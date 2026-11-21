//! Drone return-to-bay lifecycle, scoop proximity checks, and tick processing (`feat:combat.drones-return-orbit`, `BF-3`).

#![forbid(unsafe_code)]

use crate::router::notify_registry::{
    push_typed_session_notification, OnDroneControlLost, OnDroneStateChange, OnItemChange,
};
use crate::router::{NotificationTarget, ServerContext};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, DroneSpaceState};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum DroneReturnErrorCode {
    NoActiveShip,
    ShipNotInSpace,
    NoSolarSystem,
    SentryDroneOutOfScoopRange,
}

impl DroneReturnErrorCode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NoActiveShip => "NoActiveShip",
            Self::ShipNotInSpace => "ShipNotInSpace",
            Self::NoSolarSystem => "NoSolarSystem",
            Self::SentryDroneOutOfScoopRange => "SentryDroneOutOfScoopRange",
        }
    }
}

fn drone_return_rpc_error(code: DroneReturnErrorCode, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.as_str().into(),
        message: message.into(),
        client_error: None,
    }
}

fn extract_drone_target_ids(args: &[PyRep], kwargs: &HashMap<String, PyRep>) -> Vec<i64> {
    match crate::router::rpc_args::RpcArgs::new(args, kwargs).get_raw(0, "droneIDs") {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            list.iter().filter_map(|x| x.as_i64()).collect()
        }
        Some(v) => v.as_i64().into_iter().collect(),
        None => Vec::new(),
    }
}

/// Returns active drones in the same system to the drone bay (`feat:combat.drones-return-orbit`, `feat:drones.recall-drones`).
pub async fn handle_cmd_return_bay(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(ship_id) = session.ship_id.or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))) else {
        return Err(drone_return_rpc_error(DroneReturnErrorCode::NoActiveShip, "Session has no active ship"));
    };
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(drone_return_rpc_error(DroneReturnErrorCode::ShipNotInSpace, "Cannot recall drones while docked in station or structure"));
    }
    let Some(sys_id) = session.solar_system_id else {
        return Err(drone_return_rpc_error(DroneReturnErrorCode::NoSolarSystem, "Session has no active solar system"));
    };

    let parsed_ids = extract_drone_target_ids(args, kwargs);
    let filter_drone_ids = if parsed_ids.is_empty() { None } else { Some(parsed_ids) };

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return Err(drone_return_rpc_error(
            DroneReturnErrorCode::ShipNotInSpace,
            format!("Active ship {ship_id} is not in space"),
        ));
    };
    let max_ctrl_range = crate::services::combat_crimewatch::combat_npcs_drones::compute_drone_control_range(ctx, char_id, ship_id);

    let mut candidates: Vec<DroneSpaceState> = with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space.values().filter(|d| {
            d.solar_system_id == sys_id
                && (d.parent_ship_id == ship_id || (d.owner_char_id == char_id && d.parent_ship_id == 0))
                && !d.is_abandoned
                && filter_drone_ids.as_ref().map(|f| f.contains(&d.ball_id) || f.contains(&d.drone_item_id)).unwrap_or(true)
        }).cloned().collect()
    });
    candidates.sort_by_key(|d| d.ball_id);

    // Pre-validate all drone IDs before mutating any state
    let mut drones_with_dist: Vec<(DroneSpaceState, f64)> = Vec::with_capacity(candidates.len());
    for d in candidates {
        let Some((_, drone_ball)) = sys_handle.get_ball_snapshot(d.ball_id).await.ok().flatten() else {
            continue;
        };
        let dist = ship_ball.surface_distance(&drone_ball);
        if d.is_sentry && dist > 2_500.0 {
            return Err(drone_return_rpc_error(
                DroneReturnErrorCode::SentryDroneOutOfScoopRange,
                format!("Immobile sentry drone {} is {dist:.1} m away (must be within 2,500 m to return to bay)", d.ball_id),
            ));
        }
        drones_with_dist.push((d, dist));
    }

    let mut recalled_items = Vec::new();
    for (d, dist) in drones_with_dist {
        if dist > max_ctrl_range {
            continue;
        }
        if dist > 2_500.0 {
            let still_controlled = with_mut_combat_runtime(ctx, |rt| {
                if let Some(st) = rt.active_drones_in_space.get_mut(&d.ball_id) {
                    if !st.is_abandoned
                        && st.solar_system_id == sys_id
                        && (st.parent_ship_id == ship_id
                            || (st.owner_char_id == char_id && st.parent_ship_id == 0))
                    {
                        st.parent_ship_id = ship_id;
                        st.target_id = Some(ship_id);
                        st.activity_state = 4;
                        rt.active_drone_miners.remove(&d.ball_id);
                        return true;
                    }
                }
                false
            });
            if !still_controlled {
                continue;
            }
            let _ = sys_handle.remove_module_cycle(d.ball_id, -d.ball_id).await;
            // Mobile drone returning from > 2500m: restore full speed fraction (e.g. if previously Stopped)
            // before Follow so the drone does not remain stuck at 0.0 m/s.
            let _ = sys_handle.set_speed_fraction(d.ball_id, 1.0).await;
            let _ = sys_handle.set_movement_mode(d.ball_id, eve_destiny::MovementMode::Follow { target_id: ship_id, range: 0.0 }).await;
            let _ = sys_handle.emit_destiny_event(
                d.ball_id,
                eve_destiny::DestinyEvent::OnDroneStateChange {
                    drone_id: d.drone_item_id,
                    owner_id: char_id,
                    controller_id: ship_id,
                    activity_state: 4,
                    type_id: d.type_id,
                    controller_owner_id: char_id,
                    target_id: Some(ship_id),
                },
            ).await;
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                OnDroneStateChange {
                    drone_id: d.drone_item_id,
                    owner_id: char_id,
                    controller_id: ship_id,
                    activity_state: 4,
                    drone_type_id: i64::from(d.type_id),
                    controller_owner_id: char_id,
                    target_id: Some(ship_id),
                },
            );
            crate::services::notify_emitters_space_dungeons::emit_drone_activity_change(ctx, char_id, d.drone_item_id, 4, Some("returning"));
            continue;
        }

        if !try_atomic_scoop_drone(ctx, &sys_handle, &d, ship_id).await {
            let still_controlled = with_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space.get(&d.ball_id).is_some_and(|st| {
                    !st.is_abandoned
                        && st.solar_system_id == sys_id
                        && (st.parent_ship_id == ship_id
                            || (st.owner_char_id == char_id && st.parent_ship_id == 0))
                })
            });
            if still_controlled {
                let _ = sys_handle.remove_module_cycle(d.ball_id, -d.ball_id).await;
            }
            continue;
        }
        recalled_items.push(d.drone_item_id);
    }

    Ok(PyRep::Dict(Vec::new()))
}

fn build_abandoned_drone_state_change(drone_id: i64, type_id: u32) -> OnDroneStateChange {
    OnDroneStateChange {
        drone_id,
        owner_id: 0,
        controller_id: 0,
        activity_state: 0,
        drone_type_id: i64::from(type_id),
        controller_owner_id: 0,
        target_id: None,
    }
}

/// Emits V24 drone-control teardown notifications in client-safe order (`#1564`):
/// `OnDroneStateChange(0, 0)` clears `michelle.stateByDroneID` before `OnDroneControlLost`
/// unloads the item from `clientDogmaLocation.dogmaItems`, preventing `DroneGroupHeader` tooltip `KeyError`.
fn emit_drone_control_lost_sync(ctx: &ServerContext, char_id: i64, drone_id: i64, type_id: u32) {
    if char_id <= 0 {
        return;
    }
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        build_abandoned_drone_state_change(drone_id, type_id),
    );
    crate::services::notify_emitters_space_dungeons::emit_drone_activity_change(
        ctx, char_id, drone_id, 0, None,
    );
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDroneControlLost { drone_id },
    );
}

/// Abandons a returning or scoop-interrupted drone in place when its parent ship is destroyed,
/// docked, or no longer in the drone's solar system, leaving both the space ball and item intact
/// unless the item was already moved out of space.
pub(crate) async fn abandon_returning_drone_in_space(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    mut drone: DroneSpaceState,
) {
    let bid = drone.ball_id;
    let item_id = drone.drone_item_id;
    let sys_id = drone.solar_system_id;
    let type_id = drone.type_id;
    let prev_owner = if drone.owner_char_id > 0 {
        drone.owner_char_id
    } else {
        drone.original_owner_char_id
    };
    let restored = with_mut_combat_runtime(ctx, |rt| {
        rt.active_drone_miners.remove(&bid);
        rt.drone_next_ready_unix.remove(&bid);
        if rt.is_recently_destroyed_item_id(bid) || rt.is_recently_destroyed_item_id(item_id) {
            return false;
        }
        drone.is_abandoned = true;
        drone.owner_char_id = 0;
        drone.parent_ship_id = 0;
        drone.target_id = None;
        drone.activity_state = 0;
        drone.assist_char_id = None;
        drone.guard_char_id = None;
        rt.active_drones_in_space.insert(bid, drone);
        true
    });
    let _ = sys_handle.remove_module_cycle(bid, -bid).await;
    if !restored {
        return;
    }
    sys_handle.try_stop_ball(bid);
    let _ = sys_handle
        .set_movement_mode(bid, eve_destiny::MovementMode::Stop)
        .await;
    let live_pos = sys_handle.get_ball_position_sync(bid);
    let still_in_space = ctx
        .db
        .with_mut_snapshot(|snap| match snap.items.get_mut(&item_id) {
            Some(it) if it.location_id != sys_id as i64 || it.flag != eve_db::InventoryFlag::None => {
                false
            }
            Some(it) => {
                it.location_id = sys_id as i64;
                it.flag = eve_db::InventoryFlag::None;
                it.version += 1;
                super::record_drone_ball_meta_in_snap(snap, item_id, bid);
                if let Some(pos) = live_pos {
                    let pos_arr = [pos.x, pos.y, pos.z];
                    snap.item_positions.insert(item_id, pos_arr);
                    snap.item_positions.insert(bid, pos_arr);
                }
                true
            }
            None => {
                super::record_drone_ball_meta_in_snap(snap, item_id, bid);
                if let Some(pos) = live_pos {
                    let pos_arr = [pos.x, pos.y, pos.z];
                    snap.item_positions.insert(item_id, pos_arr);
                    snap.item_positions.insert(bid, pos_arr);
                }
                true
            }
        });
    if !still_in_space {
        with_mut_combat_runtime(ctx, |rt| {
            rt.active_drones_in_space.remove(&bid);
        });
        return;
    }
    let _ = sys_handle
        .emit_destiny_event(
            bid,
            eve_destiny::DestinyEvent::OnDroneStateChange {
                drone_id: item_id,
                owner_id: 0,
                controller_id: 0,
                activity_state: 0,
                type_id,
                controller_owner_id: 0,
                target_id: None,
            },
        )
        .await;
    emit_drone_control_lost_sync(ctx, prev_owner, item_id, type_id);
}

fn is_ship_available_for_scoop_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    target_ship_id: i64,
    solar_system_id: u32,
) -> bool {
    let ship_in_this_system = snap.items.get(&target_ship_id).is_some_and(|sh| {
        sh.location_id == solar_system_id as i64 && sh.flag == eve_db::InventoryFlag::None
    });
    let pilot_docked_or_elsewhere = snap.characters.values().any(|c| {
        c.active_ship_id == target_ship_id
            && (c.station_id.is_some()
                || c.structure_id.is_some()
                || c.solar_system_id != solar_system_id)
    });
    ship_in_this_system && !pilot_docked_or_elsewhere
}

fn is_scoopable_controlled_drone(
    existing: &DroneSpaceState,
    d: &DroneSpaceState,
    target_ship_id: i64,
) -> bool {
    !existing.is_abandoned
        && existing.solar_system_id == d.solar_system_id
        && existing.owner_char_id == d.owner_char_id
        && (existing.parent_ship_id == target_ship_id
            || (existing.parent_ship_id == 0 && d.owner_char_id > 0))
}

/// Atomically claims a drone from `rt.active_drones_in_space`, verifies live range,
/// parent ship validity, and remaining drone bay capacity, moves the item into `DroneBay`,
/// or abandons the drone in space if its parent ship is no longer available in-system (`#437`).
async fn try_atomic_scoop_drone(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    d: &DroneSpaceState,
    target_ship_id: i64,
) -> bool {
    let pre_ok = with_combat_runtime(ctx, |rt| {
        !rt.is_recently_destroyed_item_id(d.ball_id)
            && !rt.is_recently_destroyed_item_id(d.drone_item_id)
            && rt
                .active_drones_in_space
                .get(&d.ball_id)
                .is_some_and(|existing| is_scoopable_controlled_drone(existing, d, target_ship_id))
    });
    if !pre_ok {
        return false;
    }

    let drone_ball_opt = sys_handle
        .get_ball_snapshot(d.ball_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b);
    let ship_ball_opt = sys_handle
        .get_ball_snapshot(target_ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b);

    let Some(ref db) = drone_ball_opt else {
        // Fail closed when the drone ball is no longer in space (destroyed/removed).
        let _ = sys_handle
            .remove_module_cycle(d.ball_id, -d.ball_id)
            .await;
        let should_purge = with_mut_combat_runtime(ctx, |rt| {
            if rt.is_recently_destroyed_item_id(d.ball_id)
                || rt.is_recently_destroyed_item_id(d.drone_item_id)
            {
                rt.active_drones_in_space.remove(&d.ball_id);
                rt.active_drone_miners.remove(&d.ball_id);
                return true;
            }
            false
        });
        if should_purge {
            ctx.db.with_mut_snapshot(|snap| {
                crate::services::combat_crimewatch::combat_npcs_drones::purge_destroyed_drone_from_snap(
                    snap,
                    Some(d.drone_item_id),
                    d.ball_id,
                );
            });
        }
        return false;
    };
    if ship_ball_opt
        .as_ref()
        .is_none_or(|sb| sb.surface_distance(db) > 2_500.0)
    {
        let ship_valid = ship_ball_opt.is_some()
            && ctx.db.with_snapshot(|snap| {
                is_ship_available_for_scoop_in_snap(snap, target_ship_id, d.solar_system_id)
            });
        if !ship_valid {
            let claimed = with_mut_combat_runtime(ctx, |rt| {
                let st = rt.active_drones_in_space.get(&d.ball_id)?;
                if !is_scoopable_controlled_drone(st, d, target_ship_id) {
                    return None;
                }
                rt.active_drone_miners.remove(&d.ball_id);
                rt.active_drones_in_space.remove(&d.ball_id)
            });
            if let Some(claimed_drone) = claimed {
                abandon_returning_drone_in_space(ctx, sys_handle, claimed_drone).await;
            }
        }
        return false;
    }

    enum ScoopOutcome {
        Moved(Box<DroneSpaceState>),
        BayFull,
        ShipUnavailable(Box<DroneSpaceState>),
        NotControlled,
        NotInSpace,
        ItemMissing,
        Destroyed,
    }
    let outcome = crate::services::combat_crimewatch::with_mut_combat_runtime_tx_locks(
        &ctx.db,
        &*ctx.combat_ephemeral,
        |snap, rt| -> ScoopOutcome {
            if rt.is_recently_destroyed_item_id(d.ball_id)
                || rt.is_recently_destroyed_item_id(d.drone_item_id)
            {
                let stale_item = rt
                    .active_drones_in_space
                    .remove(&d.ball_id)
                    .map(|st| st.drone_item_id)
                    .or(Some(d.drone_item_id));
                rt.active_drone_miners.remove(&d.ball_id);
                rt.drone_next_ready_unix.remove(&d.ball_id);
                crate::services::combat_crimewatch::combat_npcs_drones::purge_destroyed_drone_from_snap(
                    snap,
                    stale_item,
                    d.ball_id,
                );
                return ScoopOutcome::Destroyed;
            }
            let Some(existing) = rt.active_drones_in_space.get(&d.ball_id).cloned() else {
                return ScoopOutcome::NotControlled;
            };
            if !is_scoopable_controlled_drone(&existing, d, target_ship_id) {
                return ScoopOutcome::NotControlled;
            }
            if crate::services::combat_crimewatch::combat_npcs_drones::is_recently_destroyed_in_persisted(
                &snap.combat_crimewatch_state,
                existing.ball_id,
                existing.drone_item_id,
            ) {
                rt.active_drones_in_space.remove(&d.ball_id);
                rt.active_drone_miners.remove(&d.ball_id);
                rt.drone_next_ready_unix.remove(&d.ball_id);
                rt.record_destroyed_item_id(existing.ball_id);
                rt.record_destroyed_item_id(existing.drone_item_id);
                crate::services::combat_crimewatch::combat_npcs_drones::purge_destroyed_drone_from_snap(
                    snap,
                    Some(existing.drone_item_id),
                    existing.ball_id,
                );
                return ScoopOutcome::Destroyed;
            }
            if snap.items.get(&existing.drone_item_id).is_some_and(|drone_item| {
                drone_item.location_id != existing.solar_system_id as i64
                    || drone_item.flag != eve_db::InventoryFlag::None
            }) {
                rt.active_drones_in_space.remove(&d.ball_id);
                rt.active_drone_miners.remove(&d.ball_id);
                rt.drone_next_ready_unix.remove(&d.ball_id);
                return ScoopOutcome::NotInSpace;
            }
            let ship_opt = is_ship_available_for_scoop_in_snap(
                snap,
                target_ship_id,
                existing.solar_system_id,
            )
            .then(|| snap.items.get(&target_ship_id))
            .flatten();
            let Some(sh) = ship_opt else {
                let claimed = rt.active_drones_in_space.remove(&d.ball_id).unwrap_or(existing);
                rt.active_drone_miners.remove(&d.ball_id);
                rt.drone_next_ready_unix.remove(&d.ball_id);
                return ScoopOutcome::ShipUnavailable(Box::new(claimed));
            };
            if !snap.items.contains_key(&existing.drone_item_id) {
                rt.active_drones_in_space.remove(&d.ball_id);
                rt.active_drone_miners.remove(&d.ball_id);
                rt.drone_next_ready_unix.remove(&d.ball_id);
                return ScoopOutcome::ItemMissing;
            }
            let drone_vol = ctx
                .sde
                .types
                .get(&existing.type_id)
                .map(|t| t.volume)
                .unwrap_or(0.0);
            let ship_cap = ctx
                .sde
                .types
                .get(&sh.type_id)
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::DRONE_CAPACITY))
                .copied()
                .unwrap_or(0.0);
            let used_vol: f64 = snap
                .items
                .values()
                .filter(|it| {
                    it.item_id != existing.drone_item_id
                        && it.location_id == target_ship_id
                        && it.flag == eve_db::InventoryFlag::DroneBay
                })
                .map(|it| {
                    ctx.sde.types.get(&it.type_id).map(|t| t.volume).unwrap_or(0.0)
                        * (it.quantity as f64)
                })
                .sum();
            if ship_cap > 0.0 && used_vol + drone_vol > ship_cap + 1e-4 {
                return ScoopOutcome::BayFull;
            }
            if !crate::services::combat_crimewatch::combat_npcs_drones::move_drone_item_to_bay_in_snap(
                snap,
                existing.drone_item_id,
                existing.ball_id,
                existing.solar_system_id,
                None,
                target_ship_id,
                true,
            ) {
                rt.active_drones_in_space.remove(&d.ball_id);
                rt.active_drone_miners.remove(&d.ball_id);
                rt.drone_next_ready_unix.remove(&d.ball_id);
                return ScoopOutcome::NotInSpace;
            }
            let claimed = rt.active_drones_in_space.remove(&d.ball_id).unwrap_or(existing);
            rt.active_drone_miners.remove(&d.ball_id);
            rt.drone_next_ready_unix.remove(&d.ball_id);
            ScoopOutcome::Moved(Box::new(claimed))
        },
    );
    let claimed_drone = match outcome {
        ScoopOutcome::Moved(claimed) => *claimed,
        ScoopOutcome::BayFull | ScoopOutcome::NotControlled | ScoopOutcome::NotInSpace => {
            return false;
        }
        ScoopOutcome::ShipUnavailable(claimed) => {
            abandon_returning_drone_in_space(ctx, sys_handle, *claimed).await;
            return false;
        }
        ScoopOutcome::ItemMissing | ScoopOutcome::Destroyed => {
            let _ = sys_handle.remove_module_cycle(d.ball_id, -d.ball_id).await;
            let _ = sys_handle.remove_ball(d.ball_id).await;
            return false;
        }
    };

    let _ = sys_handle.remove_module_cycle(claimed_drone.ball_id, -claimed_drone.ball_id).await;
    let _ = sys_handle.remove_ball(claimed_drone.ball_id).await;

    let _ = sys_handle
        .emit_destiny_event(
            claimed_drone.ball_id,
            eve_destiny::DestinyEvent::OnDroneStateChange {
                drone_id: claimed_drone.drone_item_id,
                owner_id: claimed_drone.owner_char_id,
                controller_id: target_ship_id,
                activity_state: 0,
                type_id: claimed_drone.type_id,
                controller_owner_id: claimed_drone.owner_char_id,
                target_id: None,
            },
        )
        .await;
    ctx.emit_typed(
        NotificationTarget::Character(claimed_drone.owner_char_id),
        OnDroneStateChange {
            drone_id: claimed_drone.drone_item_id,
            owner_id: claimed_drone.owner_char_id,
            controller_id: target_ship_id,
            activity_state: 0,
            drone_type_id: i64::from(claimed_drone.type_id),
            controller_owner_id: claimed_drone.owner_char_id,
            target_id: None,
        },
    );
    crate::services::notify_emitters_space_dungeons::emit_drone_activity_change(
        ctx,
        claimed_drone.owner_char_id,
        claimed_drone.drone_item_id,
        0,
        None,
    );
    let (group_id, category_id) = ctx
        .sde
        .types
        .get(&claimed_drone.type_id)
        .map(|t| (t.group_id, t.category_id))
        .unwrap_or((0, 0));
    let (owner_id, singleton, qty_wire) = ctx
        .db
        .get_item(claimed_drone.drone_item_id)
        .map(|i| (i.owner_id, i.singleton, if i.singleton { -1 } else { i.quantity as i64 }))
        .unwrap_or((claimed_drone.owner_char_id, true, -1));
    ctx.emit_typed(
        NotificationTarget::Character(claimed_drone.owner_char_id),
        OnItemChange {
            item: PyRep::keyval([
                ("itemID", PyRep::Integer(claimed_drone.drone_item_id)),
                ("typeID", PyRep::Integer(claimed_drone.type_id as i64)),
                ("ownerID", PyRep::Integer(owner_id)),
                ("locationID", PyRep::Integer(target_ship_id)),
                ("flagID", PyRep::Integer(eve_db::InventoryFlag::DroneBay as i64)),
                ("quantity", PyRep::Integer(qty_wire)),
                ("groupID", PyRep::Integer(i64::from(group_id))),
                ("categoryID", PyRep::Integer(i64::from(category_id))),
                ("singleton", PyRep::Bool(singleton)),
            ]),
            change: PyRep::Dict(vec![
                (
                    PyRep::Integer(3),
                    PyRep::Integer(claimed_drone.solar_system_id as i64),
                ),
                (PyRep::Integer(4), PyRep::Integer(0)),
            ]),
            location: PyRep::None,
        },
    );
    true
}

/// Periodic tick scoop for returning mobile drones (`activity_state == 4`).
pub async fn step_returning_drones(ctx: &ServerContext) {
    let returning_drones: Vec<DroneSpaceState> = with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .filter(|d| d.activity_state == 4 && !d.is_abandoned)
            .cloned()
            .collect()
    });

    for d in returning_drones {
        let sys_handle = ctx.systems.get_or_spawn(d.solar_system_id);
        let Some((_, drone_ball)) = sys_handle.get_ball_snapshot(d.ball_id).await.ok().flatten() else {
            continue;
        };
        let ship_ball = sys_handle
            .get_ball_snapshot(d.parent_ship_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b);
        let ship_explicitly_invalid = ctx.db.with_snapshot(|snap| {
            let ship_bad = snap.items.get(&d.parent_ship_id).is_some_and(|sh| {
                sh.location_id != d.solar_system_id as i64 || sh.flag != eve_db::InventoryFlag::None
            });
            let pilot_bad = snap.characters.values().any(|c| {
                c.active_ship_id == d.parent_ship_id
                    && (c.station_id.is_some()
                        || c.structure_id.is_some()
                        || c.solar_system_id != d.solar_system_id)
            });
            ship_bad || pilot_bad
        });
        if ship_ball.is_none() || ship_explicitly_invalid {
            let claimed = with_mut_combat_runtime(ctx, |rt| {
                let st = rt.active_drones_in_space.get(&d.ball_id)?;
                if st.activity_state == 4
                    && !st.is_abandoned
                    && st.parent_ship_id == d.parent_ship_id
                {
                    rt.active_drones_in_space.remove(&d.ball_id)
                } else {
                    None
                }
            });
            if let Some(claimed_drone) = claimed {
                abandon_returning_drone_in_space(ctx, &sys_handle, claimed_drone).await;
            }
            continue;
        }
        let Some(ref sb) = ship_ball else {
            continue;
        };
        if sb.surface_distance(&drone_ball) > 2_500.0 {
            continue;
        }
        let _ = try_atomic_scoop_drone(ctx, &sys_handle, &d, d.parent_ship_id).await;
    }
}

/// Disconnects active drones when ship initiates in-system warp without abandoning ownership.
pub fn disconnect_drones_for_ship_on_warp_sync(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) {
    let mut lost_balls = Vec::new();
    with_mut_combat_runtime(ctx, |rt| {
        for d in rt.active_drones_in_space.values_mut() {
            if (d.owner_char_id == char_id || d.parent_ship_id == ship_id) && !d.is_abandoned {
                d.parent_ship_id = 0;
                d.target_id = None;
                d.activity_state = 0;
                lost_balls.push((d.ball_id, d.drone_item_id, d.type_id, d.solar_system_id));
                rt.active_drone_miners.remove(&d.ball_id);
            }
        }
    });
    for (bid, did, tid, d_sys) in lost_balls {
        if let Some(ref sys_handle) = ctx.systems.get_or_spawn_if_runtime(if d_sys > 0 { d_sys } else { sys_id }) {
            sys_handle.try_remove_module_cycle(bid, -bid);
            let _ = sys_handle.try_emit_destiny_event(
                bid,
                eve_destiny::DestinyEvent::OnDroneStateChange {
                    drone_id: did,
                    owner_id: 0,
                    controller_id: 0,
                    activity_state: 0,
                    type_id: tid,
                    controller_owner_id: 0,
                    target_id: None,
                },
            );
        }
        emit_drone_control_lost_sync(ctx, char_id, did, tid);
    }
}

/// Abandons active in-space drones (in `sys_id` only) when the controlling ship warps (`feat:lifecycle.drones-leave-grid-abandon`).
/// Keeps carrier fighter squadrons under control and preserves DB item `owner_id` so `CmdReconnectToDrones` works seamlessly.
pub fn abandon_drones_on_warp_sync(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) {
    let mut abandoned_drones = Vec::new();
    with_mut_combat_runtime(ctx, |rt| {
        for d in rt.active_drones_in_space.values_mut() {
            if d.solar_system_id == sys_id
                && (d.owner_char_id == char_id || d.parent_ship_id == ship_id)
                && !d.is_abandoned
            {
                let notify_char_id = if d.owner_char_id > 0 {
                    d.owner_char_id
                } else {
                    char_id
                };
                d.is_abandoned = true;
                d.owner_char_id = 0;
                d.parent_ship_id = 0;
                d.target_id = None;
                d.activity_state = 0;
                d.assist_char_id = None;
                d.guard_char_id = None;
                abandoned_drones.push((d.ball_id, d.drone_item_id, d.type_id, notify_char_id));
                rt.active_drone_miners.remove(&d.ball_id);
                rt.drone_next_ready_unix.remove(&d.ball_id);
            }
        }
    });
    if !abandoned_drones.is_empty() {
        let sys_handle_opt = ctx.systems.get_or_spawn_if_runtime(sys_id);
        for &(bid, did, tid, notify_char_id) in &abandoned_drones {
            if let Some(ref sys_handle) = sys_handle_opt {
                sys_handle.try_remove_module_cycle(bid, -bid);
                sys_handle.try_stop_ball(bid);
                let _ = sys_handle.try_emit_destiny_event(
                    bid,
                    eve_destiny::DestinyEvent::OnDroneStateChange {
                        drone_id: did,
                        owner_id: 0,
                        controller_id: 0,
                        activity_state: 0,
                        type_id: tid,
                        controller_owner_id: 0,
                        target_id: None,
                    },
                );
            }
            emit_drone_control_lost_sync(ctx, notify_char_id, did, tid);
        }
    }
}

/// Abandons all active in-space drones and fighters in `sys_id` controlled by `char_id` or `ship_id`
/// on dock, stargate/wormhole/cyno jump, logoff, or death (`feat:lifecycle.drones-leave-grid-abandon`, `feat:lifecycle.fighters-left-in-space`).
pub fn abandon_drones_and_fighters_for_ship_sync(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) {
    abandon_drones_on_warp_sync(ctx, char_id, ship_id, sys_id);

    let mut probes_changed = false;
    if let Ok(mut probes) = ctx.active_scanner_probes.write() {
        for p in probes.values_mut() {
            if p.owner_char_id == char_id && p.solar_system_id == sys_id && p.state > 0 {
                p.state = 0;
                probes_changed = true;
            }
        }
    }
    if probes_changed {
        crate::services::space_probe_scanning::sync_scanner_probes_to_db(ctx);
    }

    let sys_handle_opt = ctx.systems.get_or_spawn_if_runtime(sys_id);
    let candidate_fighter_items: Vec<(i64, bool)> = if let Ok(fighters) = ctx.active_fighters.read() {
        fighters
            .values()
            .filter(|f| (f.owner_char_id == char_id || f.parent_ship_id == ship_id) && !f.is_abandoned)
            .map(|f| {
                (
                    f.item_id,
                    sys_handle_opt
                        .as_ref()
                        .is_some_and(|h| h.get_ball_position_sync(f.ball_id).is_some()),
                )
            })
            .collect()
    } else {
        Vec::new()
    };
    if candidate_fighter_items.is_empty() {
        return;
    }
    let in_sys_item_ids: std::collections::HashSet<i64> = ctx.db.with_snapshot(|snap| {
        candidate_fighter_items
            .iter()
            .filter(|&&(iid, ball_in_sys)| {
                ball_in_sys
                    || snap.items.get(&iid).is_some_and(|it| it.location_id == sys_id as i64 || it.location_id == ship_id)
            })
            .map(|&(iid, _)| iid)
            .collect()
    });
    let mut abandoned_fighters = Vec::new();
    if let Ok(mut fighters) = ctx.active_fighters.write() {
        for f in fighters.values_mut() {
            if in_sys_item_ids.contains(&f.item_id)
                && (f.owner_char_id == char_id || f.parent_ship_id == ship_id)
                && !f.is_abandoned
            {
                f.is_abandoned = true;
                f.parent_ship_id = 0;
                f.target_id = None;
                abandoned_fighters.push((f.item_id, f.ball_id, f.tube_index));
            }
        }
    }
    if !abandoned_fighters.is_empty() {
        if let Ok(mut loads) = ctx.fighter_tube_loads.write() {
            for &(_, _, tidx) in &abandoned_fighters {
                loads.remove(&(ship_id, tidx));
            }
        }
        let mut pos_updates = Vec::new();
        for &(iid, bid, _) in &abandoned_fighters {
            if let Some(ref sys_handle) = sys_handle_opt {
                sys_handle.try_stop_ball(bid);
                if let Some(pos) = sys_handle.get_ball_position_sync(bid) {
                    pos_updates.push((iid, bid, [pos.x, pos.y, pos.z]));
                }
            }
        }
        let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
            for &(iid, bid, _) in &abandoned_fighters {
                if let Some(it) = snap.items.get_mut(&iid) {
                    it.location_id = sys_id as i64;
                    it.flag = eve_db::InventoryFlag::None;
                    it.version += 1;
                }
                super::record_drone_ball_meta_in_snap(snap, iid, bid);
            }
            for (iid, bid, pos_arr) in pos_updates {
                snap.item_positions.insert(iid, pos_arr);
                snap.item_positions.insert(bid, pos_arr);
            }
            Ok(())
        });
    }
}

/// Executes repeating drone mining cycles on the cluster tick (`rpc:entity.CmdMineRepeatedly`).
pub async fn step_repeating_drone_mining(ctx: &ServerContext, now_unix: u64) {
    let pending: Vec<crate::services::combat_crimewatch::DroneMiningState> =
        with_combat_runtime(ctx, |rt| {
            rt.active_drone_miners.values().filter(|m| now_unix >= m.next_cycle_unix).cloned().collect()
        });

    for m in pending {
        let valid = with_combat_runtime(ctx, |rt| {
            rt.active_drones_in_space.get(&m.drone_id).is_some_and(|d| {
                !d.is_abandoned && d.solar_system_id == m.solar_system_id && d.owner_char_id == m.owner_char_id
            })
        });
        if !valid {
            with_mut_combat_runtime(ctx, |rt| { rt.active_drone_miners.remove(&m.drone_id); });
            continue;
        }

        let sys = ctx.systems.get_or_spawn(m.solar_system_id);
        let ship_pos = sys.get_ball_snapshot(m.ship_id).await.ok().flatten().map(|(_, b)| b.position);
        let drone_pos = sys.get_ball_snapshot(m.drone_id).await.ok().flatten().map(|(_, b)| b.position);
        let in_space = ctx.db.with_snapshot(|snap| {
            snap.characters.get(&m.owner_char_id).is_some_and(|c| {
                c.station_id.is_none() && c.structure_id.is_none()
                    && c.solar_system_id == m.solar_system_id && c.active_ship_id == m.ship_id
            })
        });
        let ctrl_range = crate::services::combat_crimewatch::combat_npcs_drones::compute_drone_control_range(ctx, m.owner_char_id, m.ship_id);
        let in_range = matches!((ship_pos, drone_pos), (Some(sp), Some(dp)) if sp.distance(dp) <= ctrl_range);
        if !in_space || !in_range {
            with_mut_combat_runtime(ctx, |rt| {
                rt.active_drone_miners.remove(&m.drone_id);
                if let Some(d) = rt.active_drones_in_space.get_mut(&m.drone_id) { d.target_id = None; }
            });
            continue;
        }

        let yield_mult = ctx.mining_yield_multiplier();
        let effective_yield_units = if m.yield_units == 0 {
            0
        } else {
            (((m.yield_units as f64) * yield_mult)
                .floor()
                .clamp(1.0, (u64::MAX / 2) as f64)) as u64
        };
        let ore_vol = ctx.sde.types.get(&m.ore_type_id).map(|t| t.volume).unwrap_or(0.1).max(0.001);
        let needed_vol = ore_vol * (effective_yield_units as f64);
        let (hold_cap, used_vol) = ctx.db.with_snapshot(|snap| {
            let stid = snap.items.get(&m.ship_id).map(|s| s.type_id).unwrap_or(0);
            let st = ctx.sde.types.get(&stid);
            let cap = match m.destination_flag as u16 {
                134 => st.and_then(|t| t.default_attributes.get(&1556).or_else(|| t.default_attributes.get(&1770)).or_else(|| t.default_attributes.get(&3227))).copied().unwrap_or(0.0),
                135 => st.and_then(|t| t.default_attributes.get(&1557)).copied().unwrap_or(0.0),
                181 => st.and_then(|t| t.default_attributes.get(&3136)).copied().unwrap_or(0.0),
                _ => st.map(|t| t.capacity).unwrap_or(1000.0),
            };
            let used: f64 = snap.items.values()
                .filter(|i| i.location_id == m.ship_id && i.flag == m.destination_flag)
                .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.1) * (i.quantity as f64))
                .sum();
            (cap, used)
        });

        if hold_cap > 0.0 && (used_vol + needed_vol > hold_cap + 1e-4) {
            with_mut_combat_runtime(ctx, |rt| {
                rt.active_drone_miners.remove(&m.drone_id);
                if let Some(d) = rt.active_drones_in_space.get_mut(&m.drone_id) {
                    d.target_id = None;
                    d.activity_state = 0;
                }
            });
            let _ = sys.emit_destiny_event(m.drone_id, eve_destiny::DestinyEvent::OnDroneStateChange {
                drone_id: m.drone_id, owner_id: m.owner_char_id, controller_id: m.ship_id,
                activity_state: 0, type_id: 0, controller_owner_id: m.owner_char_id, target_id: None,
            }).await;
            continue;
        }

        let eff_waste = if m.waste_prob > 1.0 { m.waste_prob / 100.0 } else { m.waste_prob };
        let extraction = match sys.mine_asteroid(m.asteroid_id, effective_yield_units, eff_waste).await {
            Ok(e) => e,
            Err(_) => {
                with_mut_combat_runtime(ctx, |rt| {
                    rt.active_drone_miners.remove(&m.drone_id);
                    if let Some(d) = rt.active_drones_in_space.get_mut(&m.drone_id) {
                        d.target_id = None;
                        d.activity_state = 0;
                    }
                });
                let _ = sys.emit_destiny_event(m.drone_id, eve_destiny::DestinyEvent::OnDroneStateChange {
                    drone_id: m.drone_id, owner_id: m.owner_char_id, controller_id: m.ship_id,
                    activity_state: 0, type_id: 0, controller_owner_id: m.owner_char_id, target_id: None,
                }).await;
                continue;
            }
        };

        let ast_pos_opt = ctx.db.with_mut_snapshot(|snap| {
            let mut pos_opt = None;
            if let Some(ast) = snap.asteroids.get_mut(&m.asteroid_id) {
                ast.quantity = extraction.remaining_units as i64;
                pos_opt = Some(glam::DVec3::new(ast.position.0, ast.position.1, ast.position.2));
            }
            pos_opt
        });
        if extraction.depleted {
            if let Some(ast_pos) = ast_pos_opt {
                crate::services::mining_cycle::clear_emptied_sites(ctx, m.solar_system_id, ast_pos);
            }
        }

        if extraction.mined_units > 0 {
            let ast_pos_arr = ast_pos_opt.map(|p| [p.x, p.y, p.z]);
            crate::services::space_mining_harvest::deposit_mined_ore_for_asteroid_and_record_ledger_with_waste(
                ctx,
                m.owner_char_id,
                m.ship_id,
                m.solar_system_id,
                m.destination_flag,
                m.ore_type_id,
                extraction.mined_units,
                extraction.wasted_units,
                Some(m.asteroid_id),
                ast_pos_arr,
            );
        }

        with_mut_combat_runtime(ctx, |rt| {
            if extraction.depleted {
                rt.active_drone_miners.remove(&m.drone_id);
                if let Some(d) = rt.active_drones_in_space.get_mut(&m.drone_id) {
                    d.target_id = None;
                    d.activity_state = 0;
                }
            } else if let Some(miner_st) = rt.active_drone_miners.get_mut(&m.drone_id) {
                miner_st.next_cycle_unix = now_unix.saturating_add(miner_st.duration_secs);
            }
        });
    }
}

/// Abandons specified drones (or all drones) in space for the calling character (`rpc:entity.CmdAbandonDrone`).
pub async fn handle_cmd_abandon_drone(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(sys_id) = session.solar_system_id else {
        return Err(drone_return_rpc_error(
            DroneReturnErrorCode::NoSolarSystem,
            "Session has no active solar system",
        ));
    };
    let target_ids = extract_drone_target_ids(args, kwargs);

    let mut abandoned_drones = Vec::new();
    with_mut_combat_runtime(ctx, |rt| {
        for d in rt.active_drones_in_space.values_mut() {
            if d.solar_system_id == sys_id && d.owner_char_id == char_id && !d.is_abandoned {
                let matches_filter = target_ids.is_empty()
                    || target_ids.contains(&d.ball_id)
                    || target_ids.contains(&d.drone_item_id);
                if matches_filter {
                    d.is_abandoned = true;
                    d.owner_char_id = 0;
                    d.parent_ship_id = 0;
                    d.target_id = None;
                    d.activity_state = 0;
                    d.assist_char_id = None;
                    d.guard_char_id = None;
                    abandoned_drones.push((d.ball_id, d.drone_item_id, d.type_id));
                    rt.active_drone_miners.remove(&d.ball_id);
                    rt.drone_next_ready_unix.remove(&d.ball_id);
                }
            }
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for &(bid, did, tid) in &abandoned_drones {
        crate::services::combat_crimewatch::combat_npcs_drones::clear_drone_web_cycle(
            &sys_handle,
            bid,
        )
        .await;
        let _ = sys_handle
            .emit_destiny_event(
                bid,
                eve_destiny::DestinyEvent::OnDroneStateChange {
                    drone_id: did,
                    owner_id: 0,
                    controller_id: 0,
                    activity_state: 0,
                    type_id: tid,
                    controller_owner_id: 0,
                    target_id: None,
                },
            )
            .await;
    }

    for &(_, did, tid) in &abandoned_drones {
        emit_drone_control_lost_sync(ctx, char_id, did, tid);
        push_typed_session_notification(session, build_abandoned_drone_state_change(did, tid));
        push_typed_session_notification(session, OnDroneControlLost { drone_id: did });
    }

    Ok(PyRep::Dict(vec![]))
}

/// Reconnects to abandoned drones within 250km and bandwidth/control count limits (`rpc:entity.CmdReconnectToDrones`, `feat:drones.reconnect-drones`).
pub async fn handle_cmd_reconnect_to_drones(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(ship_id) = session.ship_id else {
        return Err(drone_return_rpc_error(
            DroneReturnErrorCode::NoActiveShip,
            "Session has no active ship",
        ));
    };
    let Some(sys_id) = session.solar_system_id else {
        return Err(drone_return_rpc_error(
            DroneReturnErrorCode::NoSolarSystem,
            "Session has no active solar system",
        ));
    };
    if let Err(e) = crate::services::tourney_service::check_tourney_drone_restricted(ctx, sys_id, char_id) {
        return Err(e);
    }

    let target_ids = extract_drone_target_ids(args, kwargs);

    let mut ship_fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
    let snap_type_id = ctx.db.with_snapshot(|snap| snap.items.get(&ship_id).map(|s| s.type_id)).unwrap_or(0);
    if snap_type_id > 0 && ship_fit_opt.as_ref().map(|f| f.ship.type_id) != Some(snap_type_id) {
        ctx.invalidate_ship_fitting(ship_id);
        ship_fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
    }
    let (max_drones_allowed, ship_bw, _) = crate::services::combat_crimewatch::combat_npcs_drones::pilot_drone_capacity(ctx, char_id, ship_id, ship_fit_opt.as_ref());
    let (mut cur_count, mut cur_bw) = with_combat_runtime(ctx, |rt| {
        let active: Vec<&DroneSpaceState> = rt
            .active_drones_in_space.values().filter(|d| d.parent_ship_id == ship_id && !d.is_abandoned).collect();
        (active.len(), active.iter().map(|d| d.bandwidth_used).sum::<f64>())
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(glam::DVec3::ZERO);

    let candidates: Vec<(i64, i64, f64, u32)> = with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space.values().filter(|d| {
            d.solar_system_id == sys_id
                && ((d.is_abandoned && d.original_owner_char_id == char_id) || (d.owner_char_id == char_id && d.parent_ship_id == 0))
                && (target_ids.is_empty() || target_ids.contains(&d.ball_id) || target_ids.contains(&d.drone_item_id))
        }).map(|d| (d.ball_id, d.drone_item_id, d.bandwidth_used, d.type_id)).collect()
    });

    let mut reconnected = Vec::new();
    let mut errors: HashMap<String, Vec<i64>> = HashMap::new();

    for (bid, did, bw, tid) in candidates {
        let drone_pos = sys_handle
            .get_ball_snapshot(bid)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position);
        if let Some(pos) = drone_pos {
            if pos.distance(ship_pos) > 250_000.0 {
                errors.entry("OutOfRange".into()).or_default().push(bid);
                continue;
            }
        }
        if cur_count >= max_drones_allowed {
            errors.entry("MaxDronesExceeded".into()).or_default().push(bid);
            continue;
        }
        if cur_bw + bw > ship_bw {
            errors.entry("InsufficientBandwidth".into()).or_default().push(bid);
            continue;
        }

        with_mut_combat_runtime(ctx, |rt| {
            if let Some(d) = rt.active_drones_in_space.get_mut(&bid) {
                d.is_abandoned = false;
                d.owner_char_id = char_id;
                d.parent_ship_id = ship_id;
                d.activity_state = 0;
                d.target_id = None;
                cur_count += 1;
                cur_bw += bw;
                reconnected.push((bid, did, tid));
            }
        });
    }

    super::drone_reconnect_finish::finish_reconnect_drones(ctx, &sys_handle, char_id, ship_id, &reconnected)
        .await;

    let errors_py: Vec<(PyRep, PyRep)> = errors
        .into_iter()
        .map(|(k, v)| (PyRep::String(k), PyRep::List(v.into_iter().map(PyRep::Integer).collect())))
        .collect();
    Ok(PyRep::Dict(errors_py))
}

#[cfg(test)]
#[path = "drone_recovery_tests.rs"]
mod drone_recovery_tests;

#[cfg(test)]
#[path = "drone_return_extra_tests.rs"]
mod drone_return_extra_tests;
