//! `space_entity_ops` — Grid entity interaction, drone anchoring, behavior tree debugging, and shipping storage (`entity`).
//!
// parity: feat:drones.unanchor-deployable, feat:npc.behavior-tree-debugging, feat:npc.spawn-pool-enemy-clusters
// parity: feat:npc.extra-fleet-members, feat:cargo.shipping-unit-storage
// parity: rpc:beyonce.CmdFleetRegroup, rpc:beyonce.CmdFleetTagTarget, rpc:beyonce.CmdGotoBookmark
// parity: rpc:beyonce.CmdSelfDestructStructure, rpc:beyonce.CmdAbortSelfDestructStructure
// parity: rpc:beyonce.GetFleetLocationAndInfo, rpc:beyonce.GetFleetState, rpc:beyonce.GetRelativity
// parity: rpc:beyonce.GetContainerExpiryDate, rpc:beyonce.GetCapacityOfSiphon, rpc:beyonce.GetOperationSpawnpointPosition

#![forbid(unsafe_code)]

use std::collections::{BTreeSet, HashMap};
use glam::DVec3;
use eve_db::types::FleetRole;
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
use crate::services::inventory_fitting::fitting_containers_ships;

fn fleet_of(snap: &eve_db::WorldStateSnapshot, char_id: i64) -> Option<i64> {
    snap.fleets.values().find(|f| f.members.contains(&char_id)).map(|f| f.fleet_id)
}

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
            let target_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list) | PyRep::Tuple(list)) => list.iter().filter_map(|x| x.as_i64()).collect(),
                Some(v) => v.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            if target_ids.is_empty() {
                return Some(Err(BridgeRpcError { code: "ItemNotFound".into(), message: "No wreck/container IDs provided".into() }));
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into() }));
            };
            let res: Result<Vec<(i64, u32, u32, u32, bool, i64)>, BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
                let mut metas = Vec::new();
                for &tid in &target_ids {
                    let Some(it) = snap.items.get(&tid).cloned() else {
                        return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Item {tid} not found") });
                    };
                    if it.owner_id != char_id {
                        return Err(BridgeRpcError { code: "AccessDenied".into(), message: "Only item owner can abandon loot".into() });
                    }
                    if it.location_id != sys_id as i64 {
                        return Err(BridgeRpcError { code: "ItemNotInSpace".into(), message: "Item must be in space to be abandoned".into() });
                    }
                    let (grp, cat) = ctx.sde.types.get(&it.type_id).map(|t| (t.group_id, t.category_id)).unwrap_or((0, 0));
                    if grp != 186 && grp != 12 && grp != 340 && grp != 448 {
                        return Err(BridgeRpcError { code: "InvalidItemType".into(), message: "Only wrecks and containers in space can be abandoned".into() });
                    }
                    let is_empty = !snap.items.values().any(|c| c.location_id == tid);
                    metas.push((tid, it.type_id, grp, cat, is_empty, it.owner_id));
                }
                for &(tid, _, _, _, _, _) in &metas {
                    if let Some(it) = snap.items.get_mut(&tid) {
                        it.owner_id = 0;
                        it.version += 1;
                    }
                    let child_ids: Vec<i64> = snap.items.values().filter(|c| c.location_id == tid && c.owner_id == char_id).map(|c| c.item_id).collect();
                    for cid in child_ids {
                        if let Some(c) = snap.items.get_mut(&cid) {
                            c.owner_id = 0;
                            c.version += 1;
                        }
                    }
                }
                Ok(metas)
            });
            match res {
                Ok(metas) => {
                    with_mut_combat_runtime(ctx, |rt| {
                        for &(tid, _, _, _, _, _) in &metas {
                            if let Some(c) = rt.container_lifetimes.get_mut(&tid) {
                                c.is_abandoned = true;
                                c.owner_id = 0;
                            }
                        }
                    });
                    let sys_handle = ctx.systems.get_or_spawn(sys_id);
                    for &(tid, type_id, grp, cat, is_empty, prev_owner_id) in &metas {
                        let _ = sys_handle
                            .change_slim_item(
                                tid,
                                eve_destiny::SlimItemMeta {
                                    group_id: grp,
                                    category_id: cat,
                                    owner_id: 0,
                                    char_id: None,
                                    corp_id: 0,
                                    alliance_id: 0,
                                    security_status: 0.0,
                                    is_cloaked: false,
                                },
                            )
                            .await;
                        let build_slim_kv = |owner: i64| {
                            PyRep::keyval([
                                ("itemID", PyRep::Integer(tid)),
                                ("typeID", PyRep::Integer(type_id as i64)),
                                ("groupID", PyRep::Integer(grp as i64)),
                                ("categoryID", PyRep::Integer(cat as i64)),
                                ("ownerID", PyRep::Integer(owner)),
                                ("charID", PyRep::None),
                                ("corpID", PyRep::None),
                                ("allianceID", PyRep::None),
                                ("securityStatus", PyRep::Real(0.0)),
                                ("isEmpty", PyRep::Integer(if is_empty { 1 } else { 0 })),
                            ])
                        };
                        ctx.notify(
                            crate::router::NotificationTarget::SolarSystem(sys_id),
                            "OnSlimItemChange",
                            "clientID",
                            vec![build_slim_kv(prev_owner_id), build_slim_kv(0)],
                        );
                    }
                    let first_id = target_ids[0];
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(first_id)),
                        ("wreckIDs", PyRep::List(target_ids.into_iter().map(PyRep::Integer).collect())),
                        ("abandoned", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }
        ("beyonce", "CmdFleetRegroup") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotInSpace".into(),
                    message: "Caller must be in space".into(),
                }));
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let fleet_res = ctx.db.with_snapshot(|snap| {
                let fid = fleet_of(snap, char_id)?;
                let fleet = snap.fleets.get(&fid)?;
                let is_boss = fleet.boss_id == char_id;
                let mem = fleet.member_details.get(&char_id)?;
                let is_lead = is_boss || matches!(mem.role, FleetRole::FleetCommander | FleetRole::WingCommander | FleetRole::SquadCommander);
                if !is_lead {
                    return None;
                }
                let subordinates: Vec<(i64, i64)> = fleet.members.iter().filter_map(|&mid| {
                    if mid == char_id {
                        return None;
                    }
                    let m = fleet.member_details.get(&mid)?;
                    if !m.accepts_regroup {
                        return None;
                    }
                    let in_scope = match mem.role {
                        FleetRole::FleetCommander => is_boss || m.role != FleetRole::FleetCommander,
                        FleetRole::WingCommander => m.wing_id == mem.wing_id && !matches!(m.role, FleetRole::FleetCommander | FleetRole::WingCommander),
                        FleetRole::SquadCommander => m.squad_id == mem.squad_id && m.role == FleetRole::SquadMember,
                        _ => is_boss,
                    };
                    if !in_scope {
                        return None;
                    }
                    let ch = snap.characters.get(&mid)?;
                    if ch.solar_system_id != sys_id || ch.active_ship_id == 0 {
                        return None;
                    }
                    Some((mid, ch.active_ship_id))
                }).collect();
                Some((fid, subordinates))
            });
            let Some((_fleet_id, subordinates)) = fleet_res else {
                return Some(Err(BridgeRpcError {
                    code: "FleetAccessDenied".into(),
                    message: "Only fleet commanders can issue regroup".into(),
                }));
            };
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let cmd_pos = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position);
            for (_m_char, m_ship) in subordinates {
                let sub_pos = sys_handle.get_ball_snapshot(m_ship).await.ok().flatten().map(|(_, b)| b.position);
                let on_grid = match (cmd_pos, sub_pos) {
                    (Some(cp), Some(sp)) => cp.distance(sp) <= 500_000.0,
                    _ => false,
                };
                if on_grid {
                    let _ = sys_handle.set_movement_mode(m_ship, MovementMode::Follow { target_id: ship_id, range: 1_000.0 }).await;
                }
            }
            Some(Ok(PyRep::None))
        }
        ("beyonce", "CmdFleetTagTarget") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let tag_str = args.get(1).and_then(|a| match a {
                PyRep::String(s) => Some(s.clone()),
                PyRep::Integer(i) => Some(i.to_string()),
                _ => None,
            }).unwrap_or_default();
            let fleet_id = session.fleet_id.or_else(|| ctx.db.with_snapshot(|s| fleet_of(s, char_id)));
            let Some(fid) = fleet_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Character is not in a fleet".into(),
                }));
            };
            let is_lead = ctx.db.with_snapshot(|snap| {
                let fleet = snap.fleets.get(&fid)?;
                let is_boss = fleet.boss_id == char_id;
                let mem = fleet.member_details.get(&char_id)?;
                let role_ok = matches!(mem.role, FleetRole::FleetCommander | FleetRole::WingCommander | FleetRole::SquadCommander);
                Some(is_boss || role_ok)
            }).unwrap_or(false);
            if !is_lead {
                return Some(Err(BridgeRpcError {
                    code: "FleetAccessDenied".into(),
                    message: "Only fleet commanders can tag targets".into(),
                }));
            }
            {
                let mut ex = ctx.fleet_extras.lock().unwrap();
                let tags = ex.target_tags.entry(fid).or_default();
                if tag_str.is_empty() {
                    tags.remove(&target_id);
                } else {
                    tags.retain(|&tid, existing| tid == target_id || existing != &tag_str);
                    tags.insert(target_id, tag_str);
                }
            }
            let fleet_state = crate::services::fleet_social::fleet_social_notify::build_fleet_state_keyval(ctx, Some(fid), char_id, 1.0, 1.0, 1.0);
            ctx.notify(
                crate::router::NotificationTarget::Fleet(fid),
                "OnFleetStateChange",
                "fleetid",
                vec![fleet_state],
            );
            Some(Ok(PyRep::None))
        }
        ("beyonce", "CmdGotoBookmark") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "NotInSpace".into(),
                    message: "Docked callers cannot navigate to bookmark".into(),
                }));
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotInSpace".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            if (32_000_000..=32_999_999).contains(&sys_id) {
                return Some(Err(BridgeRpcError {
                    code: "CannotWarpInAbyssalSpace".into(),
                    message: "Bookmarks and warp navigation are disabled inside Abyssal Deadspace".into(),
                }));
            }
            if sys_id == 30_100_000 {
                return Some(Err(BridgeRpcError::user_error("DunBlacklistCannotWarp")));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            if let Err(e) = crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "align to bookmark") {
                return Some(Err(e));
            }
            let bm_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            let bm_res = ctx.db.with_snapshot(|s| -> Result<eve_db::BookmarkRecord, BridgeRpcError> {
                let Some(bm) = s.bookmarks.get(&bm_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "BookmarkNotFound".into(),
                        message: format!("Bookmark {bm_id} not found"),
                    });
                };
                if crate::services::fleet_social::is_bookmark_expired_in_snap(s, bm_id, now_unix) {
                    return Err(BridgeRpcError {
                        code: "BookmarkExpired".into(),
                        message: format!("Bookmark {bm_id} has expired"),
                    });
                }
                let caller_corp = session
                    .corporation_id
                    .or_else(|| s.characters.get(&char_id).map(|c| c.corporation_id))
                    .unwrap_or(0);
                let has_acl = bm.folder_id.and_then(|fid| s.bookmark_folders.get(&fid)).is_some_and(|f| {
                    crate::services::fleet_social::fleet_social_access_group_bookmarks::folder_access_level(
                        s, session, char_id, f,
                    ) >= crate::services::fleet_social::fleet_social_access_group_bookmarks::AccessLevel::Use
                });
                let can_access = bm.owner_id == char_id
                    || (caller_corp != 0 && caller_corp == bm.owner_id)
                    || has_acl
                    || (bm.is_shared && bm.folder_id.is_none());
                if !can_access {
                    return Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: "Cannot access bookmark".into(),
                    });
                }
                Ok(bm)
            });
            let bm = match bm_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if bm.solar_system_id != sys_id {
                return Some(Err(BridgeRpcError {
                    code: "InvalidTarget".into(),
                    message: format!("Bookmark {bm_id} is in another solar system"),
                }));
            }
            let handle = ctx.systems.get_or_spawn(sys_id);
            let dest = DVec3::new(bm.x, bm.y, bm.z);
            let (_, ball) = match handle.get_ball_snapshot(ship_id).await {
                Ok(Some(v)) => v,
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "ShipNotInSpace".into(),
                        message: "Active ship is not spawned in Ballpark".into(),
                    }));
                }
            };
            if matches!(ball.mode, MovementMode::Warp { .. }) {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship is already in warp".into(),
                }));
            }
            let cur_pos = ball.position;
            let dist = (dest - cur_pos).length();
            let _ = handle.break_gate_cloak(ship_id).await;
            if dist >= 150_000.0 {
                if let Err(e) = crate::services::combat_crimewatch::check_crimewatch_action_restriction(
                    ctx,
                    char_id,
                    ship_id,
                    sys_id,
                    "warp",
                    now_unix,
                ) {
                    return Some(Err(e));
                }
                if handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
                    return Some(Err(BridgeRpcError {
                        code: "WarpScrambled".into(),
                        message: "WarpScrambled: Warp drive disabled by warp disruption/scrambler effect".into(),
                    }));
                }
                if let Ok(Some(mut cs_check)) = handle.get_combat_state(ship_id).await {
                    if cs_check.is_cloaked && cs_check.gate_cloak_remaining_ticks == 0 && !cs_check.is_covert_cloak {
                        cs_check.is_cloaked = false;
                        let _ = handle.set_combat_state(ship_id, cs_check).await;
                    }
                }
                let warp_speed = crate::services::space_destiny::compute_pilot_warp_speed_au_s(ctx, char_id, sys_id, ball.type_id);
                let warp_dir = (dest - cur_pos).normalize_or_zero();
                let requested_dist_m = dist;
                let (base_warp_cap_need, sde_max_cap) = ctx
                    .sde
                    .types
                    .get(&ball.type_id)
                    .map(|t| {
                        let wcn = t.default_attributes.get(&153).copied().unwrap_or(1.24e-6);
                        let cap = t
                            .default_attributes
                            .get(&eve_sde::attr_ids::CAPACITOR_CAPACITY)
                            .copied()
                            .unwrap_or(250.0);
                        (wcn, cap)
                    })
                    .unwrap_or((1.24e-6, 250.0));
                let wdo_lvl = ctx.db.with_snapshot(|snap| {
                    snap.skills
                        .get(&char_id)
                        .and_then(|m| m.get(&3455))
                        .map(|s| s.level.min(5))
                        .unwrap_or(0)
                });
                let eff_warp_cap_need = base_warp_cap_need * (1.0 - 0.10 * (wdo_lvl as f64));
                let cap_per_au = eff_warp_cap_need * ball.mass;
                let cap_per_m = cap_per_au / crate::services::space_destiny::AU_IN_METERS;
                let req_cap = cap_per_m * requested_dist_m;
                let mut actual_dest = dest;
                if let Ok(Some(mut cs)) = handle.get_combat_state(ship_id).await {
                    if (cs.max_capacitor - 250.0).abs() < 1e-6
                        && (cs.capacitor - 250.0).abs() < 1e-6
                        && sde_max_cap > 250.0
                    {
                        cs.max_capacitor = sde_max_cap;
                        cs.capacitor = sde_max_cap;
                    }
                    if cs.capacitor < req_cap {
                        let affordable_dist_m = if cap_per_m > 0.0 {
                            cs.capacitor / cap_per_m
                        } else {
                            requested_dist_m
                        };
                        if affordable_dist_m < 150_000.0 {
                            return Some(Err(BridgeRpcError {
                                code: "NotEnoughCapacitor".into(),
                                message: format!(
                                    "Insufficient capacitor ({:.3} GJ) to initiate minimum 150 km warp (affordable distance {:.1} m)",
                                    cs.capacitor, affordable_dist_m
                                ),
                            }));
                        }
                        actual_dest = cur_pos + warp_dir * affordable_dist_m;
                        cs.capacitor = 0.0;
                    } else {
                        cs.capacitor = (cs.capacitor - req_cap).max(0.0);
                    }
                    let _ = handle.set_capacitor(ship_id, cs.capacitor).await;
                }
                let _ = handle
                    .set_movement_mode(ship_id, MovementMode::new_warp(cur_pos, actual_dest, warp_speed))
                    .await;
            } else {
                let _ = handle.goto_point(ship_id, dest).await;
            }
            Some(Ok(PyRep::None))
        }
        ("beyonce", "CmdSelfDestructStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let structure_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now = ctx.now_unix();
            let caller_corp = session.corporation_id.unwrap_or(0);
            let (is_ceo, db_roles) = super::inventory_fitting::get_effective_corp_roles(ctx, caller_corp, char_id);
            let roles = db_roles | session.corp_role;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get_mut(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} not found"),
                    });
                };
                let owner_corp = st.owner_corp_id;
                let has_role = session.has_gm_role()
                    || (caller_corp == owner_corp && (is_ceo || (roles & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER)) != 0));
                if !has_role {
                    return Err(BridgeRpcError {
                        code: "CrpAccessDenied".into(),
                        message: "Director, Station Manager, or CEO role required to initiate self-destruct".into(),
                    });
                }
                st.self_destruct_at_unix = Some(now + 1200);
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::None)),
                Err(e) => Some(Err(e)),
            }
        }
        ("beyonce", "CmdAbortSelfDestructStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let structure_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let caller_corp = session.corporation_id.unwrap_or(0);
            let (is_ceo, db_roles) = super::inventory_fitting::get_effective_corp_roles(ctx, caller_corp, char_id);
            let roles = db_roles | session.corp_role;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get_mut(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} not found"),
                    });
                };
                let owner_corp = st.owner_corp_id;
                let has_role = session.has_gm_role()
                    || (caller_corp == owner_corp && (is_ceo || (roles & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER)) != 0));
                if !has_role {
                    return Err(BridgeRpcError {
                        code: "CrpAccessDenied".into(),
                        message: "Director, Station Manager, or CEO role required to abort self-destruct".into(),
                    });
                }
                st.self_destruct_at_unix = None;
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::None)),
                Err(e) => Some(Err(e)),
            }
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
                let target_accepts = snap
                    .corp_governance
                    .get(&target_corp_id)
                    .map(|g| if g.accept_structures_set { g.accept_structures } else { true })
                    .unwrap_or(true);
                if !target_accepts {
                    return Err(BridgeRpcError {
                        code: "CorpDoesNotAcceptStructures".into(),
                        message: format!("Target corporation {target_corp_id} does not accept structure transfers"),
                    });
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
            if let Some(ship_id) = session.ship_id {
                if let Err(e) = crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "dock")
                    .and_then(|()| crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, ship_id, "dock"))
                {
                    return Some(Err(e));
                }
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
            ctx.invalidate_bound_objects_for_session_transition(
                char_id,
                session.solar_system_id.unwrap_or(0),
                Some(target_id),
                "turbo_dock",
            );
            Some(Ok(PyRep::keyval([
                ("locationID", PyRep::Integer(target_id)),
                ("docked", PyRep::Bool(true)),
            ])))
        }
        ("beyonce", "GetCapacityOfSiphon") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let unit_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let ratio_res = ctx.db.with_snapshot(|s| {
                let it = s.items.get(&unit_id)?;
                let cap = ctx.sde.types.get(&it.type_id).map(|t| t.capacity).unwrap_or(1200.0);
                if cap <= 0.0 {
                    return Some(0.0);
                }
                let used: f64 = s.items.values()
                    .filter(|i| i.location_id == unit_id)
                    .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.1) * (i.quantity.max(1) as f64))
                    .sum();
                Some((used / cap).clamp(0.0, 1.0))
            });
            let Some(ratio) = ratio_res else {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Siphon unit {unit_id} not found"),
                }));
            };
            Some(Ok(PyRep::Real(ratio)))
        }
        ("beyonce", "GetContainerExpiryDate") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let in_items = ctx.db.with_snapshot(|s| s.items.contains_key(&item_id));
            let expiry_unix = with_combat_runtime(ctx, |rt| {
                rt.container_lifetimes.get(&item_id).map(|c| c.created_unix + c.lifetime_secs)
            });
            let Some(exp) = expiry_unix.or_else(|| if in_items { Some(ctx.now_unix() + 7200) } else { None }) else {
                return Some(Err(BridgeRpcError {
                    code: "ContainerNotFound".into(),
                    message: format!("Container {item_id} not found or has no expiry"),
                }));
            };
            let expiry_ft = crate::sp6_lifecycle::unix_to_filetime(exp);
            Some(Ok(PyRep::Integer(expiry_ft)))
        }
        ("beyonce", "GetFleetLocationAndInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut items = Vec::new();
            if let Some(fid) = session.fleet_id.or_else(|| ctx.db.with_snapshot(|s| fleet_of(s, char_id))) {
                let member_ids = ctx.db.with_snapshot(|s| s.fleets.get(&fid).map(|f| f.members.clone())).unwrap_or_default();
                for mid in member_ids {
                    let member_info = ctx.db.with_snapshot(|s| {
                        let ch = s.characters.get(&mid)?;
                        let st = s.items.get(&ch.active_ship_id).map(|i| i.type_id).unwrap_or(0);
                        Some((ch.solar_system_id, ch.active_ship_id, st))
                    });
                    if let Some((m_sys, m_ship, m_ship_type)) = member_info {
                        let pos = if m_sys != 0 && m_ship != 0 {
                            let handle = ctx.systems.get_or_spawn(m_sys);
                            handle.get_ball_position_sync(m_ship).unwrap_or(DVec3::ZERO)
                        } else {
                            DVec3::ZERO
                        };
                        items.push((
                            PyRep::Integer(mid),
                            PyRep::keyval([
                                ("characterID", PyRep::Integer(mid)),
                                ("solarSystemID", PyRep::Integer(m_sys as i64)),
                                ("shipTypeID", PyRep::Integer(m_ship_type as i64)),
                                ("pos", PyRep::Tuple(vec![PyRep::Real(pos.x), PyRep::Real(pos.y), PyRep::Real(pos.z)])),
                            ]),
                        ));
                    }
                }
            }
            Some(Ok(PyRep::Dict(items)))
        }
        ("beyonce", "GetFleetState") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_state = ctx.db.with_snapshot(|s| {
                let fid = session.fleet_id.or_else(|| fleet_of(s, char_id))?;
                let fleet = s.fleets.get(&fid)?;
                let mem = fleet.member_details.get(&char_id)?;
                let is_boss = fleet.boss_id == char_id;
                let is_leader = is_boss || matches!(mem.role, FleetRole::FleetCommander);
                Some((
                    fid,
                    is_leader,
                    fleet.boss_id,
                    mem.wing_id.unwrap_or(0),
                    mem.squad_id.unwrap_or(0),
                    mem.role as u8,
                ))
            });
            let Some((fid, is_leader, leader_id, wing_id, squad_id, role)) = fleet_state else {
                return Some(Ok(PyRep::None));
            };
            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fid)),
                ("isLeader", PyRep::Bool(is_leader)),
                ("leaderID", PyRep::Integer(leader_id)),
                ("wingID", PyRep::Integer(wing_id)),
                ("squadID", PyRep::Integer(squad_id)),
                ("role", PyRep::Integer(role as i64)),
                ("booster", PyRep::Integer(0)),
            ])))
        }
        ("beyonce", "GetOperationSpawnpointPosition") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotInSpace".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let site_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let handle = ctx.systems.get_or_spawn(sys_id);
            if let Ok(Some((_, b))) = handle.get_ball_snapshot(site_id).await {
                return Some(Ok(PyRep::Tuple(vec![
                    PyRep::Real(b.position.x),
                    PyRep::Real(b.position.y),
                    PyRep::Real(b.position.z),
                ])));
            }
            let site_pos = ctx.db.with_snapshot(|s| {
                if let Some(st) = s.structures.get(&site_id) {
                    if st.solar_system_id == sys_id {
                        return Some(st.position);
                    }
                }
                if let Some(bm) = s.bookmarks.get(&site_id) {
                    if bm.solar_system_id == sys_id {
                        return Some([bm.x, bm.y, bm.z]);
                    }
                }
                None
            });
            if let Some([x, y, z]) = site_pos {
                return Some(Ok(PyRep::Tuple(vec![
                    PyRep::Real(x),
                    PyRep::Real(y),
                    PyRep::Real(z),
                ])));
            }
            Some(Err(BridgeRpcError {
                code: "SiteNotFound".into(),
                message: format!("Operation site {site_id} not found in system {sys_id}"),
            }))
        }
        ("beyonce", "GetRelativity") => {
            let client_stamp = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let client_time = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let _client_destiny_time = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
            let _client_bp_time = args.get(3).and_then(|a| a.as_i64()).unwrap_or(0);
            let _client_stamp2 = args.get(4).and_then(|a| a.as_i64()).unwrap_or(0);
            let sync_seq = args.get(5).and_then(|a| a.as_i64()).unwrap_or(0);
            let now_ft = ctx.now_blueos_filetime_ticks();
            let now_ms = (ctx.now_unix() * 1000) as i64;
            let latency = if client_time > 0 && now_ms > client_time { (now_ms - client_time) / 2 } else { 0 };
            let tick = ctx.now_unix() as i64;
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Integer(client_stamp),
                PyRep::Integer(now_ft),
                PyRep::Integer(now_ms),
                PyRep::Integer(latency),
                PyRep::Integer(client_time + latency),
                PyRep::Integer(sync_seq),
                PyRep::Integer(tick),
            ])))
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

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(meta) = snap.sp6_lifecycle.space_object_meta.get_mut(&target_id) {
            meta.anchor_start_unix = None;
            meta.anchor_completed = false;
        }
        crate::services::mobile_deployables::write_deployable_state_snap(snap, |st| {
            if let Some(r) = st.deployables.get_mut(&target_id) {
                r.anchor_completed = false;
                r.anchor_start_unix = 0;
                r.is_unanchored = true;
            }
        });
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
    if !session.has_gm_role() {
        return Err(BridgeRpcError {
            code: "InsufficientRole".into(),
            message: "EnableBehaviorDebugging requires GM role".into(),
        });
    }
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
    if !session.has_gm_role() {
        return Err(BridgeRpcError {
            code: "InsufficientRole".into(),
            message: "DisableBehaviorDebugging requires GM role".into(),
        });
    }
    let (_, char_id) = ServerContext::require_character(session)?;
    let entity_id = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("entityID").and_then(|v| v.as_i64()))
        .unwrap_or(char_id);

    let active_remaining = with_mut_combat_runtime(ctx, |rt| {
        rt.behavior_debugging_active.remove(&entity_id);
        !rt.behavior_debugging_active.is_empty()
    });

    tracing::info!(
        target: "ai::behavior",
        char_id,
        entity_id,
        "Disabled AI behavior debugging trace"
    );

    Ok(PyRep::keyval([
        ("behaviorDebuggingEnabled", PyRep::Bool(active_remaining)),
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
    let (_, char_id) = ServerContext::require_character(session)?;
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

    let unit_type = ctx.sde.types.get(&unit_ball.type_id);
    let is_shipping_or_container = unit_type
        .map(|t| matches!(t.group_id, 12 | 340 | 448 | 649) || t.category_id == 22)
        .unwrap_or(false);
    if !is_shipping_or_container {
        return Err(BridgeRpcError {
            code: "InvalidTarget".into(),
            message: format!("Target entity {unit_id} is not a shipping unit or deployable container"),
        });
    }

    let (unit_owner, is_authorized) = ctx.db.with_snapshot(|snap| {
        let u_owner = snap.items.get(&unit_id).map(|i| i.owner_id).unwrap_or(0);
        let (cat, grp) = snap.items.get(&unit_id)
            .and_then(|i| ctx.sde.types.get(&i.type_id))
            .map(|t| (t.category_id, t.group_id))
            .unwrap_or((0, 0));
        let is_personal = cat == 22 || grp == 340;
        let auth = session.has_gm_role()
            || u_owner == char_id
            || u_owner == 0
            || (!is_personal && snap.fleets.values().any(|f| f.members.contains(&char_id) && f.members.contains(&u_owner)));
        (u_owner, auth)
    });
    if !is_authorized {
        return Err(BridgeRpcError {
            code: "AccessDenied".into(),
            message: "You are not authorized to access this shipping unit storage".into(),
        });
    }

    if unit_owner != char_id && !session.has_gm_role() {
        let st = fitting_containers_ships::read_container_ship_state(ctx);
        if let Some(req_pw) = st.container_passwords.get(&unit_id).cloned() {
            if !req_pw.is_empty() && !st.unlocked_containers.contains(&(unit_id, char_id)) {
                let supplied_pw = kwargs
                    .get("password")
                    .and_then(|v| v.as_str())
                    .or_else(|| args.get(1).and_then(|a| a.as_str()));
                if supplied_pw == Some(req_pw.as_str()) {
                    fitting_containers_ships::write_container_ship_state(ctx, |st| {
                        st.unlocked_containers.insert((unit_id, char_id));
                    });
                } else {
                    return Err(BridgeRpcError {
                        code: "ALSCPasswordRequired".into(),
                        message: format!("Password required to open container {unit_id}"),
                    });
                }
            }
        }
    }

    let is_alsc = unit_type.map(|t| t.group_id == 448).unwrap_or(false);
    if is_alsc {
        let now_unix = ctx.now_unix_secs();
        fitting_containers_ships::write_container_ship_state(ctx, |st| {
            st.alsc_logs.entry(unit_id).or_default().push(fitting_containers_ships::AlscLogEntry {
                timestamp_unix: now_unix,
                actor_id: char_id,
                action: "ALSCActionOpen".into(),
                item_id: unit_id,
                item_type_id: 0,
                quantity: 0,
            });
        });
    }

    with_mut_combat_runtime(ctx, |rt| {
        rt.authorized_shipping_storage.insert((unit_id, char_id));
    });

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

pub(crate) use crate::services::corp_medals_bulletins::{handle_corp_bulletins, handle_corp_medals};
