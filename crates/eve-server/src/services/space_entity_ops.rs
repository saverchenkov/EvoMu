//! `space_entity_ops` — Grid entity interaction, drone anchoring, behavior tree debugging, and shipping storage (`entity`).
//!
// parity: feat:drones.unanchor-deployable, feat:npc.behavior-tree-debugging, feat:npc.spawn-pool-enemy-clusters
// parity: feat:npc.extra-fleet-members, feat:cargo.shipping-unit-storage

#![forbid(unsafe_code)]

use std::collections::{BTreeSet, HashMap};
use glam::DVec3;
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !matches!(service, "entity" | "beyonce") {
        return None;
    }

    match (service, method) {
        ("entity", "CmdUnanchor") => {
            Some(handle_cmd_unanchor(ctx, session, bound_entry, args, kwargs).await)
        }
        ("entity", "EnableBehaviorDebugging") => {
            Some(handle_enable_behavior_debugging(ctx, session, args, kwargs))
        }
        ("entity", "DisableBehaviorDebugging") => {
            Some(handle_disable_behavior_debugging(ctx, session, args, kwargs))
        }
        ("entity", "GetBehaviorTreeManager") => {
            Some(handle_get_behavior_tree_manager(ctx, session, bound_entry, args, kwargs))
        }
        ("entity", "GetDebugSpawnPoolFleetEnemyClusters") => {
            Some(handle_get_debug_enemy_clusters(ctx, session, bound_entry, args, kwargs).await)
        }
        ("entity", "GetExtraFleetMembers") => {
            Some(handle_get_extra_fleet_members(ctx, session, bound_entry, args, kwargs))
        }
        ("entity", "OpenShippingUnitStorage") => {
            Some(handle_open_shipping_unit_storage(ctx, session, bound_entry, args, kwargs).await)
        }
        ("beyonce", "CmdAbandonLoot") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into() }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(it) = snap.items.get_mut(&target_id) else {
                    return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Item {target_id} not found") });
                };
                if it.owner_id != char_id {
                    return Err(BridgeRpcError { code: "AccessDenied".into(), message: "Only item owner can abandon loot".into() });
                }
                if it.location_id != sys_id as i64 {
                    return Err(BridgeRpcError { code: "ItemNotInSpace".into(), message: "Item must be in space to be abandoned".into() });
                }
                // Game mechanics: Only in-space wrecks (group 186) and containers (groups 12, 340) can be abandoned.
                // Setting owner_id to 0 makes the wreck/can free for anyone to loot without crimewatch penalty.
                let grp = ctx.sde.types.get(&it.type_id).map(|t| t.group_id).unwrap_or(0);
                if grp != 186 && grp != 12 && grp != 340 {
                    return Err(BridgeRpcError { code: "InvalidItemType".into(), message: "Only wrecks and containers in space can be abandoned".into() });
                }
                it.owner_id = 0;
                it.version += 1;
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([("itemID", PyRep::Integer(target_id)), ("abandoned", PyRep::Bool(true))]))),
                Err(e) => Some(Err(e)),
            }
        }
        ("beyonce", "CmdSelfDestructStructure")
        | ("beyonce", "CmdAbortSelfDestructStructure")
        | ("beyonce", "CmdFleetRegroup") => {
            Some(Err(BridgeRpcError {
                code: "UnimplementedRPC".into(),
                message: format!("RPC '{service}.{method}' is not implemented"),
            }))
        }
        ("beyonce", "CmdFleetTagTarget") => {
            Some(Err(BridgeRpcError {
                code: "UnimplementedRPC".into(),
                message: "beyonce.CmdFleetTagTarget is not implemented".into(),
            }))
        }
        ("beyonce", "CmdGotoBookmark") => {
            Some(Err(BridgeRpcError {
                code: "UnimplementedRPC".into(),
                message: "beyonce.CmdGotoBookmark is not implemented".into(),
            }))
        }
        ("beyonce", "CmdTransferCorpAssetOwnership") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let target_corp_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let caller_corp = session.corporation_id.unwrap_or(0);
            // Protocol security: CEO or Director role required to transfer corporate property.
            let (is_ceo, db_roles) = super::inventory_fitting::get_effective_corp_roles(ctx, caller_corp, char_id);
            let roles = db_roles | session.corp_role;
            let has_role = is_ceo || (roles & eve_db::corp_roles::ROLE_DIRECTOR) != 0;
            if !has_role || caller_corp == 0 {
                return Some(Err(BridgeRpcError { code: "CrpAccessDenied".into(), message: "Director or CEO role required to transfer corporation assets".into() }));
            }
            // Space mechanics: Caller must be actively uncloaked in space (not docked in station/structure).
            if session.solar_system_id.is_none() || session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError { code: "NotInSpace".into(), message: "Caller must be in space (not docked)".into() }));
            }
            let sys_id = session.solar_system_id.unwrap();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Invariant: Target corporation must be an existing player corporation (preventing transfer to NPC corps).
                if target_corp_id <= 0 || !snap.corporations.contains_key(&target_corp_id) || ctx.sde.npc_corporations.contains_key(&target_corp_id) {
                    return Err(BridgeRpcError { code: "InvalidTargetCorporation".into(), message: format!("Target {target_corp_id} must be an existing player corporation") });
                }
                let mut transferred = false;
                if let Some(it) = snap.items.get_mut(&item_id) {
                    if it.owner_id != caller_corp {
                        return Err(BridgeRpcError { code: "CrpAccessDenied".into(), message: "Item is not owned by caller's corporation".into() });
                    }
                    if it.location_id != sys_id as i64 || it.flag != eve_db::InventoryFlag::None {
                        return Err(BridgeRpcError { code: "ItemNotInSpace".into(), message: "Item must be in space in caller's solar system".into() });
                    }
                    it.owner_id = target_corp_id;
                    it.version += 1;
                    transferred = true;
                }
                if let Some(st) = snap.structures.get_mut(&item_id) {
                    if st.owner_corp_id != caller_corp {
                        return Err(BridgeRpcError { code: "CrpAccessDenied".into(), message: "Structure is not owned by caller's corporation".into() });
                    }
                    if st.solar_system_id != sys_id {
                        return Err(BridgeRpcError { code: "ItemNotInSpace".into(), message: "Structure must be in caller's solar system".into() });
                    }
                    st.owner_corp_id = target_corp_id;
                    transferred = true;
                }
                if !transferred {
                    return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("In-space corporation asset {item_id} not found") });
                }
                Ok(())
            });
            if let Err(e) = res {
                return Some(Err(e));
            }
            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(item_id)),
                ("ownerID", PyRep::Integer(target_corp_id)),
            ])))
        }
        ("beyonce", "CmdTurboDock") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            // Administrative command: GM role strictly required.
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError { code: "GmRoleRequired".into(), message: "CmdTurboDock requires GM role".into() }));
            }
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let is_sde_station = ctx.sde.stations.contains_key(&target_id);
            let is_struct = ctx.db.with_snapshot(|snap| snap.structures.contains_key(&target_id));
            if !is_sde_station && !is_struct {
                return Some(Err(BridgeRpcError { code: "DockTargetNotFound".into(), message: format!("Station or structure {target_id} not found") }));
            }
            // Game mechanics: Despawn active ship ball from physics simulation to avoid orphaned balls.
            if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                let _ = ctx.systems.get_or_spawn(sys_id).remove_ball(ship_id).await;
            }
            let ship_id_opt = session.ship_id;
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if is_struct {
                        ch.structure_id = Some(target_id);
                        ch.station_id = None;
                    } else {
                        ch.station_id = Some(target_id);
                        ch.structure_id = None;
                    }
                }
                if let Some(sid) = ship_id_opt {
                    if let Some(sh) = snap.items.get_mut(&sid) {
                        sh.location_id = target_id;
                        sh.flag = eve_db::InventoryFlag::Hangar;
                    }
                }
            });
            if is_struct {
                session.structure_id = Some(target_id);
                session.station_id = None;
            } else {
                session.station_id = Some(target_id);
                session.structure_id = None;
            }
            Some(Ok(PyRep::keyval([
                ("locationID", PyRep::Integer(target_id)),
                ("docked", PyRep::Bool(true)),
            ])))
        }
        ("beyonce", "GetCapacityOfSiphon")
        | ("beyonce", "GetContainerExpiryDate")
        | ("beyonce", "GetFleetLocationAndInfo")
        | ("beyonce", "GetFleetState")
        | ("beyonce", "GetOperationSpawnpointPosition")
        | ("beyonce", "GetRelativity") => {
            Some(Err(BridgeRpcError { code: "UnimplementedRPC".into(), message: format!("RPC '{service}.{method}' is not implemented") }))
        }
        _ => None,
    }
}

async fn handle_cmd_unanchor(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(sys_id) = session
        .solar_system_id
        .or_else(|| bound_entry.and_then(|b| u32::try_from(b.object_id).ok()))
    else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
        });
    };
    let Some(ship_id) = session.ship_id.or_else(|| {
        ctx.db
            .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
    }) else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };

    let target_id = kwargs
        .get("targetID")
        .and_then(|v| v.as_i64())
        .or_else(|| match (args.first(), args.get(1)) {
            (Some(PyRep::List(_items) | PyRep::Tuple(_items)), Some(PyRep::Integer(tid))) => Some(*tid),
            (Some(PyRep::Integer(id)), _) => Some(*id),
            _ => None,
        })
        .or_else(|| bound_entry.map(|b| b.object_id))
        .unwrap_or(0);

    if target_id <= 0 {
        return Err(BridgeRpcError {
            code: "TargetRequired".into(),
            message: "CmdUnanchor requires a targetID".into(),
        });
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, target_ball)) = sys_handle.get_ball_snapshot(target_id).await.ok().flatten() else {
        return Err(BridgeRpcError {
            code: "TargetNotInSpace".into(),
            message: format!("Target ball {target_id} is not in space in system {sys_id}"),
        });
    };

    let ship_pos = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or(glam::DVec3::ZERO);
    let dist = ship_pos.distance(target_ball.position);
    if dist > 5_000.0 {
        return Err(BridgeRpcError {
            code: "TargetOutOfRange".into(),
            message: format!("Target {target_id} is {dist:.1} m away (max interaction distance 5,000 m)"),
        });
    }

    let caller_corp_id = session.corporation_id;
    let is_owner = with_combat_runtime(ctx, |rt| {
        if let Some(d) = rt.active_drones_in_space.get(&target_id) {
            return Some(d.owner_char_id == char_id);
        }
        None
    })
    .unwrap_or_else(|| {
        ctx.db.with_snapshot(|snap| {
            if let Some(item) = snap.items.get(&target_id) {
                return item.owner_id == char_id
                    || caller_corp_id.is_some_and(|cid| item.owner_id == cid);
            }
            if let Some(st) = snap.structures.get(&target_id) {
                return caller_corp_id.is_some_and(|cid| st.owner_corp_id == cid);
            }
            false
        })
    });

    if !is_owner {
        return Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: format!("Character {char_id} does not own entity {target_id}"),
        });
    }

    let _ = sys_handle.set_movement_mode(target_id, MovementMode::Stop).await;
    let _ = sys_handle
        .emit_destiny_event(target_id, eve_destiny::DestinyEvent::Stop { ball_id: target_id })
        .await;

    let unanchored_drone = with_mut_combat_runtime(ctx, |rt| {
        if let Some(d) = rt.active_drones_in_space.get_mut(&target_id) {
            d.target_id = None;
            true
        } else {
            false
        }
    });

    Ok(PyRep::keyval([
        ("targetID", PyRep::Integer(target_id)),
        ("unanchored", PyRep::Bool(true)),
        ("isDrone", PyRep::Bool(unanchored_drone)),
        ("typeID", PyRep::Integer(target_ball.type_id as i64)),
    ]))
}

fn handle_enable_behavior_debugging(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let entity_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("entityID").and_then(|v| v.as_i64()))
        .unwrap_or(char_id);

    with_mut_combat_runtime(ctx, |rt| {
        rt.behavior_debugging_active.insert(entity_id);
    });

    tracing::info!(
        target: "ai::behavior",
        char_id,
        entity_id,
        "Enabled AI behavior debugging trace"
    );

    Ok(PyRep::keyval([
        ("behaviorDebuggingEnabled", PyRep::Bool(true)),
        ("targetEntityID", PyRep::Integer(entity_id)),
    ]))
}

fn handle_disable_behavior_debugging(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let entity_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("entityID").and_then(|v| v.as_i64()))
        .unwrap_or(char_id);

    with_mut_combat_runtime(ctx, |rt| {
        rt.behavior_debugging_active.remove(&entity_id);
    });

    tracing::info!(
        target: "ai::behavior",
        char_id,
        entity_id,
        "Disabled AI behavior debugging trace"
    );

    Ok(PyRep::keyval([
        ("behaviorDebuggingEnabled", PyRep::Bool(false)),
        ("targetEntityID", PyRep::Integer(entity_id)),
    ]))
}

fn handle_get_behavior_tree_manager(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    _args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, _char_id) = ServerContext::require_character(session)?;
    let sys_id = session
        .solar_system_id
        .or_else(|| bound_entry.and_then(|b| u32::try_from(b.object_id).ok()))
        .unwrap_or(30_000_142);

    let (active_trees, archetypes) = with_combat_runtime(ctx, |rt| {
        let matching: Vec<&crate::services::combat_crimewatch::ExtendedNpcEntity> = rt
            .npc_entities
            .values()
            .filter(|e| e.solar_system_id == sys_id)
            .collect();
        let archs: BTreeSet<String> = matching.iter().map(|e| e.archetype.clone()).collect();
        (matching.len(), archs)
    });

    let arch_reps: Vec<PyRep> = archetypes.into_iter().map(PyRep::String).collect();
    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(sys_id as i64)),
        ("activeTreesCount", PyRep::Integer(active_trees as i64)),
        ("archetypes", PyRep::List(arch_reps)),
        ("managerStatus", PyRep::String("Active".into())),
    ]))
}

async fn handle_get_debug_enemy_clusters(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    _args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, _char_id) = ServerContext::require_character(session)?;
    let sys_id = session
        .solar_system_id
        .or_else(|| bound_entry.and_then(|b| u32::try_from(b.object_id).ok()))
        .unwrap_or(30_000_142);

    let clusters_data = with_combat_runtime(ctx, |rt| {
        let mut grouped: HashMap<u32, Vec<&crate::services::combat_crimewatch::ExtendedNpcEntity>> =
            HashMap::new();
        for entity in rt.npc_entities.values() {
            if entity.solar_system_id == sys_id {
                grouped.entry(entity.spawn_group_id).or_default().push(entity);
            }
        }
        let mut clusters = Vec::new();
        for (gid, members) in grouped {
            let member_ids: Vec<PyRep> = members.iter().map(|m| PyRep::Integer(m.ball_id)).collect();
            let threat: f64 = members.iter().map(|m| m.bounty_isk).sum();
            let arch = members.first().map(|m| m.archetype.as_str()).unwrap_or("Standard");
            clusters.push(PyRep::keyval([
                ("clusterID", PyRep::Integer(gid as i64)),
                ("entityIDs", PyRep::List(member_ids)),
                ("archetype", PyRep::String(arch.into())),
                ("threatScore", PyRep::Real(threat)),
            ]));
        }
        clusters
    });

    Ok(PyRep::List(clusters_data))
}

fn handle_get_extra_fleet_members(
    ctx: &ServerContext,
    session: &SessionState,
    _bound_entry: Option<&BoundObjectEntry>,
    _args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let fleet_id_opt = ctx.db.with_snapshot(|snap| {
        snap.fleets
            .values()
            .find(|f| f.members.contains(&char_id))
            .map(|f| f.fleet_id)
    });

    let extra_members: Vec<PyRep> = match fleet_id_opt {
        Some(fid) => {
            // Check if fleet has any attached NPC assistance entities or extra support members
            with_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space
                    .values()
                    .filter(|d| d.assist_char_id == Some(char_id) || d.guard_char_id == Some(char_id))
                    .map(|d| {
                        PyRep::keyval([
                            ("fleetID", PyRep::Integer(fid)),
                            ("extraMemberID", PyRep::Integer(d.ball_id)),
                            ("memberTypeID", PyRep::Integer(d.type_id as i64)),
                            ("role", PyRep::String(if d.assist_char_id.is_some() { "Assist".into() } else { "Guard".into() })),
                        ])
                    })
                    .collect()
            })
        }
        None => Vec::new(),
    };

    Ok(PyRep::List(extra_members))
}

async fn handle_open_shipping_unit_storage(
    ctx: &ServerContext,
    session: &SessionState,
    _bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, _char_id) = ServerContext::require_character(session)?;
    let unit_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("unitID").and_then(|v| v.as_i64()))
        .unwrap_or(0);

    if unit_id <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "OpenShippingUnitStorage requires a valid unitID".into(),
        });
    }

    let Some(sys_id) = session.solar_system_id else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
        });
    };

    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or(DVec3::ZERO);

    let Some((_, unit_ball)) = sys_handle.get_ball_snapshot(unit_id).await.ok().flatten() else {
        return Err(BridgeRpcError {
            code: "UnitNotInSpace".into(),
            message: format!("Shipping unit {unit_id} is not in space in system {sys_id}"),
        });
    };

    let dist = ship_pos.distance(unit_ball.position);
    if dist > 5_000.0 {
        return Err(BridgeRpcError {
            code: "TargetOutOfRange".into(),
            message: format!("Shipping unit is {dist:.1} m away (max interaction distance 5,000 m)"),
        });
    }

    let (item_count, type_id) = ctx.db.with_snapshot(|snap| {
        let tid = snap.items.get(&unit_id).map(|i| i.type_id).unwrap_or(unit_ball.type_id);
        let count = snap.items.values().filter(|i| i.location_id == unit_id).count();
        (count, tid)
    });

    Ok(PyRep::keyval([
        ("shippingUnitID", PyRep::Integer(unit_id)),
        ("typeID", PyRep::Integer(type_id as i64)),
        ("storageOpened", PyRep::Bool(true)),
        ("itemCount", PyRep::Integer(item_count as i64)),
        ("distance", PyRep::Real(dist)),
    ]))
}
