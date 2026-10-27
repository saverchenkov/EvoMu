//! Drone return-to-bay lifecycle, scoop proximity checks, and tick processing (`feat:combat.drones-return-orbit`, `BF-3`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, DroneSpaceState};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Returns active drones in the same system to the drone bay (`feat:combat.drones-return-orbit`, `feat:drones.recall-drones`).
pub async fn handle_cmd_return_bay(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(ship_id) = session.ship_id.or_else(|| ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))) else {
        return Err(BridgeRpcError { code: "NoActiveShip".into(), message: "Session has no active ship".into() });
    };
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError { code: "ShipNotInSpace".into(), message: "Cannot recall drones while docked in station or structure".into() });
    }
    let Some(sys_id) = session.solar_system_id else {
        return Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session has no active solar system".into() });
    };

    let filter_drone_ids: Option<Vec<i64>> = match args.first().or_else(|| kwargs.get("droneIDs")) {
        Some(PyRep::List(items) | PyRep::Tuple(items)) => {
            let v: Vec<i64> = items.iter().filter_map(|it| it.as_i64()).collect();
            if v.is_empty() { None } else { Some(v) }
        }
        Some(PyRep::Integer(id)) => Some(vec![*id]),
        _ => None,
    };

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_pos = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position).unwrap_or(glam::DVec3::ZERO);
    let max_ctrl_range = crate::services::combat_crimewatch::combat_npcs_drones::compute_drone_control_range(ctx, char_id, ship_id);

    let candidates: Vec<DroneSpaceState> = with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space.values().filter(|d| {
            d.solar_system_id == sys_id
                && (d.parent_ship_id == ship_id || (d.owner_char_id == char_id && d.parent_ship_id == 0))
                && !d.is_abandoned
                && filter_drone_ids.as_ref().map(|f| f.contains(&d.ball_id) || f.contains(&d.drone_item_id)).unwrap_or(true)
        }).cloned().collect()
    });

    // Pre-validate all drone IDs before mutating any state
    let mut drones_with_dist: Vec<(DroneSpaceState, f64)> = Vec::with_capacity(candidates.len());
    for d in candidates {
        let dist = sys_handle.get_ball_snapshot(d.ball_id).await.ok().flatten().map(|(_, b)| ship_pos.distance(b.position)).unwrap_or(0.0);
        if d.is_sentry && dist > 2_500.0 {
            return Err(BridgeRpcError {
                code: "SentryDroneOutOfScoopRange".into(),
                message: format!("Immobile sentry drone {} is {dist:.1} m away (must be within 2,500 m to return to bay)", d.ball_id),
            });
        }
        drones_with_dist.push((d, dist));
    }

    let mut recalled_items = Vec::new();
    for (d, dist) in drones_with_dist {
        if dist > max_ctrl_range {
            continue;
        }
        if dist > 2_500.0 {
            // Mobile drone returning from > 2500m: set follow and emit returning (activity_state = 4)
            let _ = sys_handle.set_movement_mode(d.ball_id, eve_destiny::MovementMode::Follow { target_id: ship_id, range: 0.0 }).await;
            with_mut_combat_runtime(ctx, |rt| {
                if let Some(st) = rt.active_drones_in_space.get_mut(&d.ball_id) {
                    st.parent_ship_id = ship_id;
                    st.target_id = Some(ship_id);
                    st.activity_state = 4;
                }
            });
            let _ = sys_handle.emit_destiny_event(
                d.ball_id,
                eve_destiny::DestinyEvent::OnDroneStateChange {
                    drone_id: d.ball_id,
                    owner_id: char_id,
                    controller_id: ship_id,
                    activity_state: 4,
                    type_id: d.type_id,
                    controller_owner_id: char_id,
                    target_id: Some(ship_id),
                },
            ).await;
            let notif_args = vec![
                PyRep::Integer(d.drone_item_id),
                PyRep::Integer(char_id),
                PyRep::Integer(ship_id),
                PyRep::Integer(4),
                PyRep::Integer(d.type_id as i64),
                PyRep::Integer(char_id),
                PyRep::Integer(0),
            ];
            ctx.notify(NotificationTarget::Character(char_id), "OnDroneStateChange", "charid", notif_args.clone());
            session.push_notification("OnDroneStateChange", notif_args);
            continue;
        }

        // Immediate scoop: check drone bay capacity
        let drone_vol = ctx.sde.types.get(&d.type_id).map(|t| t.volume).unwrap_or(0.0);
        let cap_ok = ctx.db.with_snapshot(|snap| {
            let Some(sh) = snap.items.get(&ship_id) else { return false; };
            let ship_cap = ctx.sde.types.get(&sh.type_id)
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::DRONE_CAPACITY))
                .copied()
                .unwrap_or(0.0);
            let used_vol: f64 = snap.items.values()
                .filter(|it| it.location_id == ship_id && it.flag == eve_db::InventoryFlag::DroneBay)
                .map(|it| ctx.sde.types.get(&it.type_id).map(|t| t.volume).unwrap_or(0.0) * (it.quantity as f64))
                .sum();
            ship_cap <= 0.0 || (used_vol + drone_vol <= ship_cap + 1e-4)
        });
        if !cap_ok {
            continue;
        }

        with_mut_combat_runtime(ctx, |rt| {
            rt.active_drones_in_space.remove(&d.ball_id);
            rt.active_drone_miners.remove(&d.ball_id);
        });
        let _ = sys_handle.remove_ball(d.ball_id).await;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&d.drone_item_id) {
                it.location_id = ship_id;
                it.flag = eve_db::InventoryFlag::DroneBay;
                it.version += 1;
            }
        });

        // Emit OnDroneStateChange (returned/in-bay = 0)
        let _ = sys_handle.emit_destiny_event(
            d.ball_id,
            eve_destiny::DestinyEvent::OnDroneStateChange {
                drone_id: d.ball_id,
                owner_id: char_id,
                controller_id: ship_id,
                activity_state: 0,
                type_id: d.type_id,
                controller_owner_id: char_id,
                target_id: None,
            },
        ).await;
        let notif_args = vec![
            PyRep::Integer(d.drone_item_id),
            PyRep::Integer(char_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(0),
            PyRep::Integer(d.type_id as i64),
            PyRep::Integer(char_id),
            PyRep::Integer(0),
        ];
        ctx.notify(NotificationTarget::Character(char_id), "OnDroneStateChange", "charid", notif_args.clone());
        session.push_notification("OnDroneStateChange", notif_args);

        // Emit OnItemChange
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnItemChange",
            "charid",
            vec![
                PyRep::keyval([
                    ("itemID", PyRep::Integer(d.drone_item_id)),
                    ("typeID", PyRep::Integer(d.type_id as i64)),
                    ("locationID", PyRep::Integer(ship_id)),
                    ("flagID", PyRep::Integer(eve_db::InventoryFlag::DroneBay as i64)),
                    ("quantity", PyRep::Integer(1)),
                ]),
                PyRep::Dict(vec![
                    (PyRep::Integer(3), PyRep::Integer(d.solar_system_id as i64)),
                    (PyRep::Integer(4), PyRep::Integer(0)),
                ]),
            ],
        );

        recalled_items.push(d.drone_item_id);
    }

    Ok(PyRep::Dict(Vec::new()))
}

/// Periodic tick scoop for returning mobile drones (`activity_state == 4`).
pub async fn step_returning_drones(ctx: &ServerContext) {
    let returning_drones: Vec<DroneSpaceState> = with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space.values().filter(|d| d.activity_state == 4).cloned().collect()
    });

    for d in returning_drones {
        let sys_handle = ctx.systems.get_or_spawn(d.solar_system_id);
        let ship_pos = sys_handle.get_ball_position_sync(d.parent_ship_id);
        let drone_pos = sys_handle.get_ball_position_sync(d.ball_id);
        let Some((sp, dp)) = ship_pos.zip(drone_pos) else { continue; };
        let dist = sp.distance(dp);
        if dist > 2_500.0 {
            continue;
        }

        let drone_vol = ctx.sde.types.get(&d.type_id).map(|t| t.volume).unwrap_or(0.0);
        let cap_ok = ctx.db.with_snapshot(|snap| {
            let Some(sh) = snap.items.get(&d.parent_ship_id) else { return false; };
            let ship_cap = ctx.sde.types.get(&sh.type_id)
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::DRONE_CAPACITY))
                .copied()
                .unwrap_or(0.0);
            let used_vol: f64 = snap.items.values()
                .filter(|it| it.location_id == d.parent_ship_id && it.flag == eve_db::InventoryFlag::DroneBay)
                .map(|it| ctx.sde.types.get(&it.type_id).map(|t| t.volume).unwrap_or(0.0) * (it.quantity as f64))
                .sum();
            ship_cap <= 0.0 || (used_vol + drone_vol <= ship_cap + 1e-4)
        });
        if !cap_ok {
            continue;
        }

        with_mut_combat_runtime(ctx, |rt| {
            rt.active_drones_in_space.remove(&d.ball_id);
            rt.active_drone_miners.remove(&d.ball_id);
        });
        let _ = sys_handle.remove_ball(d.ball_id).await;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&d.drone_item_id) {
                it.location_id = d.parent_ship_id;
                it.flag = eve_db::InventoryFlag::DroneBay;
                it.version += 1;
            }
        });

        // Emit OnDroneStateChange (returned/in-bay = 0)
        let _ = sys_handle.emit_destiny_event(
            d.ball_id,
            eve_destiny::DestinyEvent::OnDroneStateChange {
                drone_id: d.ball_id,
                owner_id: d.owner_char_id,
                controller_id: d.parent_ship_id,
                activity_state: 0,
                type_id: d.type_id,
                controller_owner_id: d.owner_char_id,
                target_id: None,
            },
        ).await;
        let notif_args = vec![
            PyRep::Integer(d.drone_item_id),
            PyRep::Integer(d.owner_char_id),
            PyRep::Integer(d.parent_ship_id),
            PyRep::Integer(0),
            PyRep::Integer(d.type_id as i64),
            PyRep::Integer(d.owner_char_id),
            PyRep::Integer(0),
        ];
        ctx.notify(NotificationTarget::Character(d.owner_char_id), "OnDroneStateChange", "charid", notif_args);

        // Emit OnItemChange
        ctx.notify(
            NotificationTarget::Character(d.owner_char_id),
            "OnItemChange",
            "charid",
            vec![
                PyRep::keyval([
                    ("itemID", PyRep::Integer(d.drone_item_id)),
                    ("typeID", PyRep::Integer(d.type_id as i64)),
                    ("locationID", PyRep::Integer(d.parent_ship_id)),
                    ("flagID", PyRep::Integer(eve_db::InventoryFlag::DroneBay as i64)),
                    ("quantity", PyRep::Integer(1)),
                ]),
                PyRep::Dict(vec![
                    (PyRep::Integer(3), PyRep::Integer(d.solar_system_id as i64)),
                    (PyRep::Integer(4), PyRep::Integer(0)),
                ]),
            ],
        );
    }
}

/// Disconnects active drones when ship initiates in-system warp without abandoning ownership.
pub fn disconnect_drones_for_ship_on_warp_sync(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    _sys_id: u32,
) {
    let mut lost_balls = Vec::new();
    with_mut_combat_runtime(ctx, |rt| {
        for d in rt.active_drones_in_space.values_mut() {
            if (d.owner_char_id == char_id || d.parent_ship_id == ship_id) && !d.is_abandoned {
                d.parent_ship_id = 0;
                d.target_id = None;
                d.activity_state = 0;
                lost_balls.push(d.ball_id);
                rt.active_drone_miners.remove(&d.ball_id);
            }
        }
    });
    for &bid in &lost_balls {
        ctx.notify(NotificationTarget::Character(char_id), "OnDroneControlLost", "charid", vec![PyRep::Integer(bid)]);
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
    let mut abandoned_balls = Vec::new();
    with_mut_combat_runtime(ctx, |rt| {
        for d in rt.active_drones_in_space.values_mut() {
            if d.solar_system_id == sys_id
                && (d.owner_char_id == char_id || d.parent_ship_id == ship_id)
                && !d.is_abandoned
            {
                d.is_abandoned = true;
                d.owner_char_id = 0;
                d.parent_ship_id = 0;
                d.target_id = None;
                d.activity_state = 0;
                abandoned_balls.push(d.ball_id);
                rt.active_drone_miners.remove(&d.ball_id);
            }
        }
    });
    if !abandoned_balls.is_empty() {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for &bid in &abandoned_balls {
            sys_handle.try_stop_ball(bid);
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnDroneControlLost",
                "charid",
                vec![PyRep::Integer(bid)],
            );
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

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let candidate_fighter_items: Vec<(i64, bool)> = if let Ok(fighters) = ctx.active_fighters.read() {
        fighters
            .values()
            .filter(|f| (f.owner_char_id == char_id || f.parent_ship_id == ship_id) && !f.is_abandoned)
            .map(|f| (f.item_id, sys_handle.get_ball_position_sync(f.ball_id).is_some()))
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
            sys_handle.try_stop_ball(bid);
            if let Some(pos) = sys_handle.get_ball_position_sync(bid) {
                pos_updates.push((iid, bid, [pos.x, pos.y, pos.z]));
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

        let extraction = match sys.mine_asteroid(m.asteroid_id, m.yield_units, m.waste_prob).await {
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

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ast) = snap.asteroids.get_mut(&m.asteroid_id) {
                ast.quantity = extraction.remaining_units as i64;
            }
        });

        let ore_vol = ctx.sde.types.get(&m.ore_type_id).map(|t| t.volume).unwrap_or(0.1);
        let needed_vol = ore_vol * (extraction.mined_units as f64);
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

        if extraction.mined_units > 0 {
            crate::services::space_mining_harvest::deposit_mined_ore_and_record_ledger(
                ctx, m.owner_char_id, m.ship_id, m.solar_system_id, m.destination_flag, m.ore_type_id, extraction.mined_units,
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
