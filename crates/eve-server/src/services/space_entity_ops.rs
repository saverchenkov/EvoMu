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
    if service != "entity" {
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
    let (_, _char_id) = ServerContext::require_character(session)?;
    let Some(sys_id) = session
        .solar_system_id
        .or_else(|| bound_entry.and_then(|b| u32::try_from(b.object_id).ok()))
    else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
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
