//! Drone return-to-bay lifecycle, scoop proximity checks, and tick processing (`feat:combat.drones-return-orbit`, `BF-3`).

#![forbid(unsafe_code)]

use crate::router::notify_registry::{
    push_typed_session_notification, OnDroneControlLost, OnDroneStateChange,
};
use crate::router::{NotificationTarget, ServerContext};
use crate::services::combat_crimewatch::DroneSpaceState;
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

    let mut candidates: Vec<DroneSpaceState> = sys_handle
        .view()
        .active_drones_in_space
        .values()
        .filter(|d| {
            d.ball_id > 0
                && d.solar_system_id == sys_id
                && (d.parent_ship_id == ship_id || (d.owner_char_id == char_id && d.parent_ship_id == 0))
                && !d.is_abandoned
                && filter_drone_ids.as_ref().map(|f| f.contains(&d.ball_id) || f.contains(&d.drone_item_id)).unwrap_or(true)
        })
        .cloned()
        .collect();
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
            let target_bid = d.ball_id;
            let still_controlled = sys_handle
                .mutate_active_drones_with_claims(move |drones, _, _, claimed| {
                    if claimed.contains_key(&target_bid) {
                        return (false, false);
                    }
                    if let Some(st) = drones.get_mut(&target_bid) {
                        if !st.is_abandoned
                            && st.solar_system_id == sys_id
                            && (st.parent_ship_id == ship_id
                                || (st.owner_char_id == char_id && st.parent_ship_id == 0))
                        {
                            st.parent_ship_id = ship_id;
                            st.target_id = Some(ship_id);
                            st.activity_state = 4;
                            return (true, true);
                        }
                    }
                    (false, false)
                })
                .await
                .unwrap_or(false);
            if !still_controlled {
                continue;
            }
            let _ = sys_handle.remove_drone_miner(d.ball_id).await;
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
            if let Some(scene) = crate::router::notify_registry::BallparkScene::try_solar_system(sys_id) {
                ctx.emit_typed(
                    NotificationTarget::Character(char_id),
                    OnDroneStateChange {
                        scene,
                        drone_id: d.drone_item_id,
                        owner_id: char_id,
                        controller_id: ship_id,
                        activity_state: 4,
                        drone_type_id: i64::from(d.type_id),
                        controller_owner_id: char_id,
                        target_id: Some(ship_id),
                    },
                );
            }
            crate::services::notify_emitters_space_dungeons::emit_drone_activity_change(ctx, char_id, d.drone_item_id, 4, Some("returning"));
            continue;
        }

        if !try_atomic_scoop_drone(ctx, &sys_handle, &d, ship_id).await {
            let still_controlled = sys_handle.active_drone(d.ball_id).is_some_and(|st| {
                !st.is_abandoned
                    && st.solar_system_id == sys_id
                    && (st.parent_ship_id == ship_id
                        || (st.owner_char_id == char_id && st.parent_ship_id == 0))
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

fn build_abandoned_drone_state_change(
    sys_id: u32,
    drone_id: i64,
    type_id: u32,
) -> Option<OnDroneStateChange> {
    let scene = crate::router::notify_registry::BallparkScene::try_solar_system(sys_id)?;
    Some(OnDroneStateChange {
        scene,
        drone_id,
        owner_id: 0,
        controller_id: 0,
        activity_state: 0,
        drone_type_id: i64::from(type_id),
        controller_owner_id: 0,
        target_id: None,
    })
}

/// Emits V24 drone-control teardown notifications in client-safe order (`#1564`):
/// `OnDroneStateChange(0, 0)` clears `michelle.stateByDroneID` before `OnDroneControlLost`
/// unloads the item from `clientDogmaLocation.dogmaItems`, preventing `DroneGroupHeader` tooltip `KeyError`.
fn emit_drone_control_lost_sync(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    drone_id: i64,
    type_id: u32,
) {
    if char_id <= 0 {
        return;
    }
    if let Some(abandoned) = build_abandoned_drone_state_change(sys_id, drone_id, type_id) {
        ctx.emit_typed(NotificationTarget::Character(char_id), abandoned);
    }
    crate::services::notify_emitters_space_dungeons::emit_drone_activity_change(
        ctx, char_id, drone_id, 0, None,
    );
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        OnDroneControlLost { drone_id },
    );
}

/// Completes post-actor teardown and notifications for a deferred disconnect/abandon
/// executed when a failing scoop releases its transient claim (`ReleaseClaim`).
pub(crate) async fn finish_pending_scoop_abandon(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    ball_id: i64,
    applied: crate::system_actor::PendingScoopAbandon,
) {
    let _ = sys_handle.remove_module_cycle(ball_id, -ball_id).await;
    if applied.kind == crate::system_actor::PendingScoopAbandonKind::Abandon {
        sys_handle.try_stop_ball(ball_id);
    }
    let _ = sys_handle
        .emit_destiny_event(
            ball_id,
            eve_destiny::DestinyEvent::OnDroneStateChange {
                drone_id: applied.drone_item_id,
                owner_id: 0,
                controller_id: 0,
                activity_state: 0,
                type_id: applied.type_id,
                controller_owner_id: 0,
                target_id: None,
            },
        )
        .await;
    emit_drone_control_lost_sync(
        ctx,
        applied.solar_system_id,
        applied.notify_char_id,
        applied.drone_item_id,
        applied.type_id,
    );
}

/// Releases a transient scoop claim and executes any deferred abandon/disconnect stamped while held.
pub(crate) async fn release_scoop_claim_and_apply_pending(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    ball_id: i64,
) {
    if let Ok(crate::system_actor::DroneScoopClaimOutcome::ReleasedWithPending(applied)) =
        sys_handle
            .drone_scoop_claim_op(
                ball_id,
                crate::system_actor::DroneScoopClaimAction::ReleaseClaim,
            )
            .await
    {
        finish_pending_scoop_abandon(ctx, sys_handle, ball_id, applied).await;
    }
}

/// Returns `true` when `ship_id` is currently in `MovementMode::Warp` or tracked in `active_warps`.
pub(crate) fn is_ship_ball_warping(
    sys_handle: &crate::system_actor::SystemHandle,
    ship_ball_opt: Option<&eve_destiny::Ball>,
    ship_id: i64,
) -> bool {
    if ship_ball_opt.is_some_and(|sb| matches!(sb.mode, eve_destiny::MovementMode::Warp { .. })) {
        return true;
    }
    let view = sys_handle.view();
    view.active_warps.contains_key(&ship_id)
        || view
            .balls
            .get(&ship_id)
            .is_some_and(|sb| matches!(sb.mode, eve_destiny::MovementMode::Warp { .. }))
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
    let is_destroyed = ctx.db.with_snapshot(|snap| {
        snap.combat_crimewatch_state
            .is_recently_destroyed_item_id(bid)
            || snap
                .combat_crimewatch_state
                .is_recently_destroyed_item_id(item_id)
    });
    let restored = if is_destroyed {
        false
    } else {
        drone.is_abandoned = true;
        drone.owner_char_id = 0;
        drone.parent_ship_id = 0;
        drone.target_id = None;
        drone.activity_state = 0;
        drone.assist_char_id = None;
        drone.guard_char_id = None;
        sys_handle.upsert_active_drone(drone).await.is_ok()
    };
    let _ = sys_handle.clear_drone_runtime(bid).await;
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
            None
                if crate::services::combat_crimewatch::combat_npcs_drones::is_recently_destroyed_in_persisted(
                    &snap.combat_crimewatch_state,
                    bid,
                    item_id,
                ) =>
            {
                false
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
        let _ = sys_handle.remove_active_drone(bid).await;
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
    emit_drone_control_lost_sync(ctx, sys_id, prev_owner, item_id, type_id);
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
                || c.solar_system_id != solar_system_id
                || snap
                    .sp6_lifecycle
                    .disconnected_ships
                    .contains_key(&c.character_id))
    });
    let ship_disconnected = snap
        .sp6_lifecycle
        .disconnected_ships
        .values()
        .any(|d| d.ship_id == target_ship_id);
    ship_in_this_system && !pilot_docked_or_elsewhere && !ship_disconnected
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

/// Atomically claims a drone from `SystemActor::active_drones_in_space`, verifies live range,
/// parent ship validity, and remaining drone bay capacity, moves the item into `DroneBay`,
/// or abandons the drone in space if its parent ship is no longer available in-system (`#437`).
async fn try_atomic_scoop_drone(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    d: &DroneSpaceState,
    target_ship_id: i64,
) -> bool {
    let not_destroyed = ctx.db.with_snapshot(|snap| {
        !snap
            .combat_crimewatch_state
            .is_recently_destroyed_item_id(d.ball_id)
            && !snap
                .combat_crimewatch_state
                .is_recently_destroyed_item_id(d.drone_item_id)
    });
    let pre_ok = not_destroyed
        && sys_handle
            .active_drone(d.ball_id)
            .is_some_and(|existing| is_scoopable_controlled_drone(&existing, d, target_ship_id));
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
        let should_purge = ctx.db.with_snapshot(|snap| {
            snap.combat_crimewatch_state
                .is_recently_destroyed_item_id(d.ball_id)
                || snap
                    .combat_crimewatch_state
                    .is_recently_destroyed_item_id(d.drone_item_id)
        });
        if should_purge {
            let _ = sys_handle.remove_active_drone(d.ball_id).await;
            let _ = sys_handle.clear_drone_runtime(d.ball_id).await;
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
    let ship_warping_pre =
        is_ship_ball_warping(sys_handle, ship_ball_opt.as_ref(), target_ship_id);
    if ship_warping_pre
        || ship_ball_opt
            .as_ref()
            .is_none_or(|sb| sb.surface_distance(db) > 2_500.0)
    {
        let ship_valid = !ship_warping_pre
            && ship_ball_opt.is_some()
            && ctx.db.with_snapshot(|snap| {
                is_ship_available_for_scoop_in_snap(snap, target_ship_id, d.solar_system_id)
            });
        if !ship_valid {
            if let Ok(crate::system_actor::DroneScoopClaimOutcome::TakeForAbandon(Some(
                claimed_drone,
            ))) = sys_handle
                .drone_scoop_claim_op(
                    d.ball_id,
                    crate::system_actor::DroneScoopClaimAction::TakeUnclaimedForAbandon {
                        expected_owner: d.owner_char_id,
                        target_ship_id,
                        require_returning: false,
                    },
                )
                .await
            {
                abandon_returning_drone_in_space(ctx, sys_handle, claimed_drone).await;
            }
        }
        return false;
    }

    let Ok(crate::system_actor::DroneScoopClaimOutcome::Claimed(Some(existing))) = sys_handle
        .drone_scoop_claim_op(
            d.ball_id,
            crate::system_actor::DroneScoopClaimAction::ClaimReturnScoop {
                expected_owner: d.owner_char_id,
                target_ship_id,
            },
        )
        .await
    else {
        return false;
    };

    enum ScoopOutcome {
        Moved(Box<DroneSpaceState>),
        BayFull,
        ShipUnavailable,
        NotInSpace,
        ItemMissing,
        Destroyed,
    }
    let outcome = ctx.db.with_mut_snapshot(|snap| -> ScoopOutcome {
        if snap
            .combat_crimewatch_state
            .is_recently_destroyed_item_id(existing.ball_id)
            || snap
                .combat_crimewatch_state
                .is_recently_destroyed_item_id(existing.drone_item_id)
        {
            crate::services::combat_crimewatch::combat_npcs_drones::purge_destroyed_drone_from_snap(
                snap,
                Some(existing.drone_item_id),
                existing.ball_id,
            );
            return ScoopOutcome::Destroyed;
        }
        if crate::services::combat_crimewatch::combat_npcs_drones::is_recently_destroyed_in_persisted(
            &snap.combat_crimewatch_state,
            existing.ball_id,
            existing.drone_item_id,
        ) {
            snap.combat_crimewatch_state
                .record_destroyed_item_id(existing.ball_id);
            snap.combat_crimewatch_state
                .record_destroyed_item_id(existing.drone_item_id);
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
            return ScoopOutcome::NotInSpace;
        }
        let ship_opt = (!is_ship_ball_warping(sys_handle, None, target_ship_id)
            && is_ship_available_for_scoop_in_snap(
                snap,
                target_ship_id,
                existing.solar_system_id,
            ))
        .then(|| snap.items.get(&target_ship_id))
        .flatten();
        let Some(sh) = ship_opt else {
            return ScoopOutcome::ShipUnavailable;
        };
        if !snap.items.contains_key(&existing.drone_item_id) {
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
            return ScoopOutcome::NotInSpace;
        }
        ScoopOutcome::Moved(Box::new(existing))
    });
    let claimed_drone = match outcome {
        ScoopOutcome::Moved(claimed) => {
            let _ = sys_handle
                .drone_scoop_claim_op(
                    claimed.ball_id,
                    crate::system_actor::DroneScoopClaimAction::ConsumeScoop,
                )
                .await;
            *claimed
        }
        ScoopOutcome::BayFull => {
            release_scoop_claim_and_apply_pending(ctx, sys_handle, d.ball_id).await;
            return false;
        }
        ScoopOutcome::NotInSpace => {
            let _ = sys_handle
                .drone_scoop_claim_op(
                    d.ball_id,
                    crate::system_actor::DroneScoopClaimAction::ConsumeScoop,
                )
                .await;
            let _ = sys_handle.clear_drone_runtime(d.ball_id).await;
            return false;
        }
        ScoopOutcome::ShipUnavailable => {
            match sys_handle
                .drone_scoop_claim_op(
                    d.ball_id,
                    crate::system_actor::DroneScoopClaimAction::ReleaseOrTakeForAbandon {
                        char_id_opt: None,
                        ship_id: target_ship_id,
                    },
                )
                .await
            {
                Ok(crate::system_actor::DroneScoopClaimOutcome::TakeForAbandon(Some(
                    live_drone,
                ))) => {
                    abandon_returning_drone_in_space(ctx, sys_handle, live_drone).await;
                }
                Ok(crate::system_actor::DroneScoopClaimOutcome::ReleasedWithPending(applied)) => {
                    finish_pending_scoop_abandon(ctx, sys_handle, d.ball_id, applied).await;
                }
                _ => {}
            }
            return false;
        }
        ScoopOutcome::ItemMissing | ScoopOutcome::Destroyed => {
            let _ = sys_handle
                .drone_scoop_claim_op(
                    d.ball_id,
                    crate::system_actor::DroneScoopClaimAction::ConsumeScoop,
                )
                .await;
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
    if let Some(scene) = crate::router::notify_registry::BallparkScene::try_solar_system(
        claimed_drone.solar_system_id,
    ) {
        ctx.emit_typed(
            NotificationTarget::Character(claimed_drone.owner_char_id),
            OnDroneStateChange {
                scene,
                drone_id: claimed_drone.drone_item_id,
                owner_id: claimed_drone.owner_char_id,
                controller_id: target_ship_id,
                activity_state: 0,
                drone_type_id: i64::from(claimed_drone.type_id),
                controller_owner_id: claimed_drone.owner_char_id,
                target_id: None,
            },
        );
    }
    crate::services::notify_emitters_space_dungeons::emit_drone_activity_change(
        ctx,
        claimed_drone.owner_char_id,
        claimed_drone.drone_item_id,
        0,
        None,
    );
    true
}

/// Periodic tick scoop for returning mobile drones (`activity_state == 4`).
pub async fn step_returning_drones(ctx: &ServerContext) {
    let returning_drones: Vec<(crate::system_actor::SystemHandle, DroneSpaceState)> = ctx
        .systems
        .all_active_drones()
        .into_iter()
        .filter(|(_, d)| d.ball_id > 0 && d.activity_state == 4 && !d.is_abandoned)
        .collect();

    for (sys_handle, d) in returning_drones {
        let Some((_, drone_ball)) = sys_handle.get_ball_snapshot(d.ball_id).await.ok().flatten() else {
            continue;
        };
        let ship_ball = sys_handle
            .get_ball_snapshot(d.parent_ship_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b);
        let ship_warping = is_ship_ball_warping(&sys_handle, ship_ball.as_ref(), d.parent_ship_id);
        let ship_explicitly_invalid = ship_warping
            || !ctx.db.with_snapshot(|snap| {
                is_ship_available_for_scoop_in_snap(snap, d.parent_ship_id, d.solar_system_id)
            });
        if ship_ball.is_none() || ship_explicitly_invalid {
            if let Ok(crate::system_actor::DroneScoopClaimOutcome::TakeForAbandon(Some(
                claimed_drone,
            ))) = sys_handle
                .drone_scoop_claim_op(
                    d.ball_id,
                    crate::system_actor::DroneScoopClaimAction::TakeUnclaimedForAbandon {
                        expected_owner: d.owner_char_id,
                        target_ship_id: d.parent_ship_id,
                        require_returning: true,
                    },
                )
                .await
            {
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

fn try_stamp_pending_scoop_abandon(
    claimed: &mut HashMap<i64, Option<crate::system_actor::PendingScoopAbandon>>,
    d: &DroneSpaceState,
    kind: crate::system_actor::PendingScoopAbandonKind,
    fallback_char_id: i64,
    fallback_sys_id: u32,
) -> bool {
    let Some(slot) = claimed.get_mut(&d.ball_id) else {
        return false;
    };
    let notify_char_id = if d.owner_char_id > 0 {
        d.owner_char_id
    } else {
        fallback_char_id
    };
    let solar_system_id = if d.solar_system_id > 0 {
        d.solar_system_id
    } else {
        fallback_sys_id
    };
    crate::system_actor::PendingScoopAbandon::merge_into(
        slot,
        crate::system_actor::PendingScoopAbandon {
            kind,
            notify_char_id,
            drone_item_id: d.drone_item_id,
            type_id: d.type_id,
            solar_system_id,
        },
    );
    true
}

/// Disconnects active drones when ship initiates in-system warp without abandoning ownership.
pub fn disconnect_drones_for_ship_on_warp_sync(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) {
    let mut lost_balls: Vec<(crate::system_actor::SystemHandle, i64, i64, u32, u32)> = Vec::new();
    for (_actor_sys_id, h) in ctx.systems.active_system_handles() {
        let has_match = h.view().active_drones_in_space.values().any(|d| {
            d.ball_id > 0
                && (d.owner_char_id == char_id || d.parent_ship_id == ship_id)
                && !d.is_abandoned
        });
        if !has_match {
            continue;
        }
        if let Ok(rows) = h.mutate_active_drones_with_claims_sync(move |drones, _, _, claimed| {
            let mut changed = Vec::new();
            for d in drones.values_mut() {
                if d.ball_id > 0
                    && (d.owner_char_id == char_id || d.parent_ship_id == ship_id)
                    && !d.is_abandoned
                {
                    if try_stamp_pending_scoop_abandon(
                        claimed,
                        d,
                        crate::system_actor::PendingScoopAbandonKind::Disconnect,
                        char_id,
                        sys_id,
                    ) {
                        continue;
                    }
                    d.parent_ship_id = 0;
                    d.target_id = None;
                    d.activity_state = 0;
                    changed.push((d.ball_id, d.drone_item_id, d.type_id, d.solar_system_id));
                }
            }
            (!changed.is_empty(), changed)
        }) {
            for (bid, did, tid, d_sys) in rows {
                lost_balls.push((h.clone(), bid, did, tid, d_sys));
            }
        }
    }
    for (sys_handle, bid, did, tid, d_sys) in lost_balls {
        let eff_sys = if d_sys > 0 { d_sys } else { sys_id };
        if eff_sys > 0 {
            let _ = sys_handle.remove_drone_miner_sync(bid);
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
        emit_drone_control_lost_sync(ctx, eff_sys, char_id, did, tid);
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
    let sys_handle_opt = ctx.systems.get_or_spawn_if_runtime(sys_id);
    let abandoned_drones = if let Some(ref sys_handle) = sys_handle_opt {
        sys_handle
            .mutate_active_drones_with_claims_sync(move |drones, _, _, claimed| {
                let mut rows = Vec::new();
                for d in drones.values_mut() {
                    if d.ball_id > 0
                        && d.solar_system_id == sys_id
                        && (d.owner_char_id == char_id || d.parent_ship_id == ship_id)
                        && !d.is_abandoned
                    {
                        if try_stamp_pending_scoop_abandon(
                            claimed,
                            d,
                            crate::system_actor::PendingScoopAbandonKind::Abandon,
                            char_id,
                            sys_id,
                        ) {
                            continue;
                        }
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
                        rows.push((d.ball_id, d.drone_item_id, d.type_id, notify_char_id));
                    }
                }
                (!rows.is_empty(), rows)
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    if !abandoned_drones.is_empty() {
        if let Some(ref sys_handle) = sys_handle_opt {
            let abandoned_bids: Vec<i64> = abandoned_drones.iter().map(|&(bid, _, _, _)| bid).collect();
            let _ = sys_handle.clear_drone_runtime_batch_sync(abandoned_bids);
        }
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
            emit_drone_control_lost_sync(ctx, sys_id, notify_char_id, did, tid);
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
    let fg_snap = if let Some(ref sys_handle) = sys_handle_opt {
        let _ = ctx.systems.ensure_live_fighters_hydrated_sync(sys_id, sys_handle);
        sys_handle.view().fighters.clone()
    } else {
        ctx.systems.snapshot_fighters_for_system(sys_id)
    };
    let candidate_fighter_items: Vec<(i64, bool)> = fg_snap
        .active_fighters
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
        .collect();
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
    let (abandoned_fighters, pos_updates): (Vec<(i64, i64, usize)>, Vec<(i64, i64, [f64; 3])>) =
        if let Some(ab) = sys_handle_opt.as_ref().and_then(|sh| match sh.abandon_ship_fighters_sync(char_id, ship_id, in_sys_item_ids.clone()) {
            Ok(ab) => Some((sh, ab)),
            Err(err) => { tracing::warn!(solar_system_id = sys_id, char_id, ship_id, error = %err, "Live fighter abandon failed; retrying cold/guarded fallback"); None }
        }) {
            let (sys_handle, ab) = ab;
            let mut pos_by_bid: std::collections::HashMap<i64, [f64; 3]> =
                std::collections::HashMap::with_capacity(ab.abandoned_positions.len());
            for (bid, pos_opt) in ab.abandoned_positions {
                if let Some(pos) = pos_opt.or_else(|| sys_handle.get_ball_position_sync(bid)) { pos_by_bid.insert(bid, [pos.x, pos.y, pos.z]); }
            }
            let fighters_vec: Vec<(i64, i64, usize)> = ab.abandoned.into_iter().map(|(bid, iid, tidx)| (iid, bid, tidx)).collect();
            let updates: Vec<(i64, i64, [f64; 3])> = fighters_vec.iter().filter_map(|&(iid, bid, _)| pos_by_bid.get(&bid).map(|&arr| (iid, bid, arr))).collect();
            (fighters_vec, updates)
        } else {
            let ab = match ctx.systems.abandon_fighters_in_cold_db(sys_id, char_id, ship_id, &in_sys_item_ids) {
                Ok(v) => v,
                Err(err) => { tracing::error!(solar_system_id = sys_id, char_id, ship_id, error = %err, "Failed to abandon fighters during session end"); Vec::new() }
            };
            (ab.into_iter().map(|(bid, iid, tidx)| (iid, bid, tidx)).collect(), Vec::new())
        };
    if sys_handle_opt.is_some() {
        ctx.systems.flush_system_fighter_checkpoint_sync(sys_id);
    }
    if !abandoned_fighters.is_empty() {
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
    let pending: Vec<(crate::system_actor::SystemHandle, crate::system_actor::DroneMiningState)> = ctx
        .systems
        .all_active_drone_miners()
        .into_iter()
        .filter(|(_, m)| now_unix >= m.next_cycle_unix)
        .collect();

    for (owner_sys, m) in pending {
        let sys = if owner_sys.solar_system_id == m.solar_system_id {
            owner_sys.clone()
        } else if let Some(h) = ctx.systems.get_handle(m.solar_system_id) {
            h
        } else {
            let _ = owner_sys.remove_drone_miner(m.drone_id).await;
            continue;
        };
        let valid = sys.active_drone(m.drone_id).is_some_and(|d| {
            !d.is_abandoned && d.solar_system_id == m.solar_system_id && d.owner_char_id == m.owner_char_id
        });
        if !valid {
            let _ = owner_sys.remove_drone_miner(m.drone_id).await;
            continue;
        }
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
            let _ = owner_sys.remove_drone_miner(m.drone_id).await;
            let did = m.drone_id;
            let _ = sys
                .mutate_active_drones(move |drones, _, _| {
                    if let Some(d) = drones.get_mut(&did) {
                        d.target_id = None;
                        (true, ())
                    } else {
                        (false, ())
                    }
                })
                .await;
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
            let _ = owner_sys.remove_drone_miner(m.drone_id).await;
            let did = m.drone_id;
            let _ = sys
                .mutate_active_drones(move |drones, _, _| {
                    if let Some(d) = drones.get_mut(&did) {
                        d.target_id = None;
                        d.activity_state = 0;
                        (true, ())
                    } else {
                        (false, ())
                    }
                })
                .await;
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
                let _ = owner_sys.remove_drone_miner(m.drone_id).await;
                let did = m.drone_id;
                let _ = sys
                    .mutate_active_drones(move |drones, _, _| {
                        if let Some(d) = drones.get_mut(&did) {
                            d.target_id = None;
                            d.activity_state = 0;
                            (true, ())
                        } else {
                            (false, ())
                        }
                    })
                    .await;
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

        if extraction.depleted {
            let _ = owner_sys.remove_drone_miner(m.drone_id).await;
            let did = m.drone_id;
            let _ = sys
                .mutate_active_drones(move |drones, _, _| {
                    if let Some(d) = drones.get_mut(&did) {
                        d.target_id = None;
                        d.activity_state = 0;
                        (true, ())
                    } else {
                        (false, ())
                    }
                })
                .await;
        } else {
            let _ = owner_sys.reschedule_drone_miner(m.drone_id, now_unix).await;
        }
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

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let target_ids_for_mut = target_ids;
    let abandoned_drones = sys_handle
        .mutate_active_drones_with_claims(move |drones, _, _, claimed| {
            let mut abandoned = Vec::new();
            for d in drones.values_mut() {
                if d.ball_id > 0
                    && d.solar_system_id == sys_id
                    && d.owner_char_id == char_id
                    && !d.is_abandoned
                {
                    let matches_filter = target_ids_for_mut.is_empty()
                        || target_ids_for_mut.contains(&d.ball_id)
                        || target_ids_for_mut.contains(&d.drone_item_id);
                    if matches_filter {
                        if try_stamp_pending_scoop_abandon(
                            claimed,
                            d,
                            crate::system_actor::PendingScoopAbandonKind::Abandon,
                            char_id,
                            sys_id,
                        ) {
                            continue;
                        }
                        d.is_abandoned = true;
                        d.owner_char_id = 0;
                        d.parent_ship_id = 0;
                        d.target_id = None;
                        d.activity_state = 0;
                        d.assist_char_id = None;
                        d.guard_char_id = None;
                        abandoned.push((d.ball_id, d.drone_item_id, d.type_id));
                    }
                }
            }
            (!abandoned.is_empty(), abandoned)
        })
        .await
        .unwrap_or_default();
    let _ = sys_handle
        .clear_drone_runtime_batch(abandoned_drones.iter().map(|&(bid, _, _)| bid).collect())
        .await;
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
        emit_drone_control_lost_sync(ctx, sys_id, char_id, did, tid);
        if let Some(abandoned) = build_abandoned_drone_state_change(sys_id, did, tid) {
            push_typed_session_notification(session, abandoned);
        }
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
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let view = sys_handle.view();
    let (mut cur_count, mut cur_bw) = {
        let active: Vec<&DroneSpaceState> = view
            .active_drones_in_space
            .values()
            .filter(|d| d.parent_ship_id == ship_id && !d.is_abandoned)
            .collect();
        (active.len(), active.iter().map(|d| d.bandwidth_used).sum::<f64>())
    };
    let candidates: Vec<(i64, i64, f64, u32)> = view
        .active_drones_in_space
        .values()
        .filter(|d| {
            d.ball_id > 0
                && d.solar_system_id == sys_id
                && ((d.is_abandoned && d.original_owner_char_id == char_id)
                    || (d.owner_char_id == char_id && d.parent_ship_id == 0))
                && (target_ids.is_empty()
                    || target_ids.contains(&d.ball_id)
                    || target_ids.contains(&d.drone_item_id))
        })
        .map(|d| (d.ball_id, d.drone_item_id, d.bandwidth_used, d.type_id))
        .collect();
    drop(view);
    let ship_pos = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(glam::DVec3::ZERO);

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

        let updated = sys_handle
            .mutate_active_drones_with_claims(move |drones, _, _, claimed| {
                if claimed.contains_key(&bid) {
                    return (false, false);
                }
                if let Some(d) = drones.get_mut(&bid) {
                    if d.solar_system_id == sys_id
                        && ((d.is_abandoned && d.original_owner_char_id == char_id)
                            || (d.owner_char_id == char_id && d.parent_ship_id == 0))
                    {
                        d.is_abandoned = false;
                        d.owner_char_id = char_id;
                        d.parent_ship_id = ship_id;
                        d.activity_state = 0;
                        d.target_id = None;
                        return (true, true);
                    }
                }
                (false, false)
            })
            .await
            .unwrap_or(false);
        if updated {
            cur_count += 1;
            cur_bw += bw;
            reconnected.push((bid, did, tid));
        }
    }

    super::drone_reconnect_finish::finish_reconnect_drones(ctx, &sys_handle, char_id, ship_id, &reconnected)
        .await;

    let errors_py: Vec<(PyRep, PyRep)> = errors
        .into_iter()
        .map(|(k, v)| (PyRep::String(k), PyRep::List(v.into_iter().map(PyRep::Integer).collect())))
        .collect();
    Ok(PyRep::Dict(errors_py))
}

fn abandon_drone_state_for_restart(d: &mut DroneSpaceState) -> bool {
    let changed = !d.is_abandoned
        || d.owner_char_id != 0
        || d.parent_ship_id != 0
        || d.target_id.is_some()
        || d.activity_state != 0
        || d.assist_char_id.is_some()
        || d.guard_char_id.is_some();
    if changed {
        d.is_abandoned = true;
        d.owner_char_id = 0;
        d.parent_ship_id = 0;
        d.target_id = None;
        d.activity_state = 0;
        d.assist_char_id = None;
        d.guard_char_id = None;
    }
    changed
}

/// Seeds abandoned [`DroneSpaceState`] entries for any persisted in-space drones (`category_id == 18`,
/// `location_id` in `30_000_000..=34_000_000`) and transitions all persisted `KIND_ACTIVE_DRONES`
/// checkpoint rows to abandoned state when a [`ServerContext`] initializes from SQLite
/// (`feat:lifecycle.drones-in-space-persist-restart`, `#1541`).
// parity: feat:lifecycle.drones-in-space-persist-restart
pub fn seed_persisted_abandoned_drones(ctx: &ServerContext) {
    let mut sqlite_sys_ids: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    let now_unix = ctx.db.clock().now_unix_secs();
    let sqlite_rows = ctx
        .db
        .with_sqlite_conn(|conn| {
            eve_db::sqlite_store::system_checkpoint::load_all_checkpoints_for_kind::<DroneSpaceState>(
                conn,
                eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
            )
        })
        .unwrap_or_default();

    let mut per_sys: std::collections::BTreeMap<
        u32,
        (bool, Vec<(String, DroneSpaceState)>),
    > = std::collections::BTreeMap::new();
    let mut stale_ids_by_sys: std::collections::HashMap<u32, std::collections::HashSet<i64>> =
        std::collections::HashMap::new();
    let mut drones = Vec::new();
    let mut fighters = Vec::new();
    ctx.db.with_snapshot(|snap| {
        for (sys_id, key, d) in sqlite_rows {
            let mut d = d.in_solar_system(sys_id);
            let is_destroyed =
                crate::services::combat_crimewatch::combat_npcs_drones::is_recently_destroyed_in_persisted(
                    &snap.combat_crimewatch_state,
                    d.ball_id,
                    d.drone_item_id,
                );
            let moved_out_of_space = snap.items.get(&d.drone_item_id).is_some_and(|it| {
                it.location_id != i64::from(sys_id) || it.flag != eve_db::InventoryFlag::None
            });
            if is_destroyed || moved_out_of_space {
                stale_ids_by_sys.entry(sys_id).or_default().insert(d.ball_id);
                per_sys.entry(sys_id).or_insert((true, Vec::new())).0 = true;
                continue;
            }
            sqlite_sys_ids.insert(sys_id);
            let changed = abandon_drone_state_for_restart(&mut d);
            let entry = per_sys.entry(sys_id).or_insert((false, Vec::new()));
            entry.0 |= changed;
            entry.1.push((key, d));
        }

        for item in snap.items.values() {
            if !(30_000_000..=34_000_000).contains(&item.location_id) {
                continue;
            }
            let Some(t) = ctx.sde.types.get(&item.type_id) else {
                continue;
            };
            let ball_id = snap
                .sp6_lifecycle
                .space_object_meta
                .get(&item.item_id)
                .and_then(|m| m.drone_ball_id)
                .unwrap_or(item.item_id);
            if item.flag != eve_db::InventoryFlag::None {
                continue;
            }
            if t.category_id == 87 {
                let max_squadron_size = t
                    .default_attributes
                    .get(&2215)
                    .copied()
                    .map(|v| v as u32)
                    .unwrap_or(9);
                fighters.push(crate::router::context::ActiveFighterSquadron {
                    ball_id,
                    item_id: item.item_id,
                    type_id: item.type_id,
                    owner_char_id: item.owner_id,
                    parent_ship_id: 0,
                    tube_index: 0,
                    squadron_size: (item.quantity.max(1) as u32).min(max_squadron_size),
                    target_id: None,
                    is_abandoned: true,
                    mwd_active: false,
                });
                continue;
            }
            if t.category_id != 18 {
                continue;
            }
            let attr = |aid: u16| -> Option<f64> {
                item.dynamic_attributes
                    .get(&aid)
                    .or_else(|| t.default_attributes.get(&aid))
                    .copied()
            };
            let bw_used = attr(1272).unwrap_or(5.0);
            let max_vel = attr(37).unwrap_or(1_500.0);
            let opt_range = attr(54).unwrap_or(5_000.0);
            let dmg_mult = attr(64).unwrap_or(1.0);
            let dmg_vec = eve_destiny::DamageVector {
                em: attr(114).unwrap_or(0.0) * dmg_mult,
                explosive: attr(116).unwrap_or(0.0) * dmg_mult,
                kinetic: attr(117).unwrap_or(0.0) * dmg_mult,
                thermal: attr(118).unwrap_or(0.0) * dmg_mult,
            };
            drones.push(DroneSpaceState {
                ball_id,
                drone_item_id: item.item_id,
                type_id: item.type_id,
                group_id: t.group_id,
                owner_char_id: 0,
                original_owner_char_id: item.owner_id,
                parent_ship_id: 0,
                solar_system_id: item.location_id as u32,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: true,
                is_sentry: max_vel < 1.0,
                bandwidth_used: bw_used,
                max_velocity: max_vel,
                optimal_range: opt_range,
                damage_vector: dmg_vec,
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            });
        }
    });
    if per_sys.values().any(|(changed, _)| *changed) {
        let _ = ctx.db.with_sqlite_conn(|conn| {
            for (sys_id, (changed, sys_rows)) in &per_sys {
                if *changed {
                    eve_db::sqlite_store::system_checkpoint::replace_system_checkpoints_for_kind(
                        conn,
                        *sys_id,
                        eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
                        sys_rows,
                        now_unix,
                    )?;
                }
            }
            Ok::<(), String>(())
        });
    }
    if !drones.is_empty() {
        let mut by_sys: HashMap<u32, Vec<DroneSpaceState>> = HashMap::new();
        for d in drones {
            by_sys.entry(d.solar_system_id).or_default().push(d);
        }
        for (sys_id, sys_drones) in by_sys {
            sqlite_sys_ids.remove(&sys_id);
            let stale_ids = stale_ids_by_sys.remove(&sys_id).unwrap_or_default();
            let handle = ctx.systems.wake_for_drone_seed(sys_id);
            let _ = handle.mutate_active_drones_sync(move |active, _, _| {
                let mut dirty = false;
                for sid in stale_ids {
                    if active.remove(&sid).is_some() && sid > 0 {
                        dirty = true;
                    }
                }
                for existing in active.values_mut() {
                    if abandon_drone_state_for_restart(existing) && existing.ball_id > 0 {
                        dirty = true;
                    }
                }
                for d in sys_drones {
                    let mut d = d.in_solar_system(sys_id);
                    abandon_drone_state_for_restart(&mut d);
                    let bid = d.ball_id;
                    if let std::collections::hash_map::Entry::Vacant(slot) = active.entry(bid) {
                        slot.insert(d);
                        if bid > 0 {
                            dirty = true;
                        }
                    }
                }
                (dirty, ())
            });
        }
    }
    let remaining_sys: Vec<u32> = sqlite_sys_ids
        .into_iter()
        .chain(stale_ids_by_sys.keys().copied())
        .collect();
    for sys_id in remaining_sys {
        if let Some(handle) = ctx.systems.get_handle(sys_id) {
            let stale_ids = stale_ids_by_sys.remove(&sys_id).unwrap_or_default();
            let _ = handle.mutate_active_drones_sync(move |active, _, _| {
                let mut dirty = false;
                for sid in stale_ids {
                    if active.remove(&sid).is_some() && sid > 0 {
                        dirty = true;
                    }
                }
                for existing in active.values_mut() {
                    if abandon_drone_state_for_restart(existing) && existing.ball_id > 0 {
                        dirty = true;
                    }
                }
                (dirty, ())
            });
        }
    }
    if !fighters.is_empty() {
        if let Ok(mut guard) = ctx.active_fighters.write() {
            for f in fighters {
                guard.insert(f.ball_id, f);
            }
        }
    }
}

#[cfg(test)]
#[path = "drone_recovery_tests.rs"]
mod drone_recovery_tests;

#[cfg(test)]
#[path = "drone_return_extra_tests.rs"]
mod drone_return_extra_tests;
