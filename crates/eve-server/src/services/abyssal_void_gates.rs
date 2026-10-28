//! `abyssal_void_gates` — Abyssal room transfer/exit conduit activation and Void Space jump/exit handlers.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::ServerContext;
use crate::services::abyssal_void_mgr::{
    err_no_active_ship, err_no_solar_system, err_ship_docked, validate_allowed_ship_hull,
    VOID_SPACE_SYSTEM_ID,
};
use crate::services::pve_special_sites_abyssal::{
    collapse_abyssal_pocket_and_pod_pilot_at, pocket_grid_offset,
    restore_abyssal_weather_dogma_modifiers,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

fn is_actual_filament_type(ctx: &ServerContext, type_id: u32) -> bool {
    crate::services::pve_special_sites::classify_abyssal_filament(type_id).is_some()
        || ctx
            .sde
            .types
            .get(&type_id)
            .map(|t| matches!(t.group_id, 1979 | 4041 | 4145 | 4743))
            .unwrap_or(false)
}

fn is_valid_void_entrance_gate(ctx: &ServerContext, target_id: i64, ball_type_id: u32) -> bool {
    // Specific Abyssal / Void Space Trace and Conduit types (Triglavian Space Trace, Origin Conduit, Void Space Gate)
    if matches!(
        ball_type_id,
        47_458 | 47_686 | 47_833 | 47_834 | 47_956 | 56_693 | 60_589 | 60_608 | 60_770 | 80_944
    ) {
        return true;
    }
    // Registered abyssal trace or void space entrance object
    let is_registered = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        reg.abyssal_traces.contains_key(&target_id)
            || reg.void_space_sessions.values().any(|s| s.target_id == target_id)
    });
    if is_registered {
        return true;
    }
    // Real SDE Abyssal Space Trace (group 1991) or Triglavian Space Portal (group 4081)
    if let Some(t) = ctx.sde.types.get(&ball_type_id) {
        if t.group_id == 1991 || t.group_id == 4081 {
            return true;
        }
    }
    false
}

enum GateInspect {
    Expired { ship_id: i64, abyss_sys: u32, now: u64 },
    Err(BridgeRpcError),
    Ok((u8, u8, i64, u32, Vec<i64>, Vec<i64>, bool, (u8, u32, i64, f64))),
}

pub(crate) async fn handle_abyssal_gate_activation(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let _cur_sys = session.solar_system_id.ok_or_else(err_no_solar_system)?;
    let ship_id = session.ship_id.ok_or_else(err_no_active_ship)?;

    let (
        curr_room,
        next_room,
        pid,
        abyss_sys,
        curr_room_npcs,
        next_room_npcs,
        room_already_cleared,
        (tier, filament, next_cache, difficulty),
    ) = {
        let now = ctx.now_unix();
        let inspect = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
            let p = reg.abyssal_pockets.get(&char_id)?;
            if p.completed || p.collapsed_destroyed {
                return None;
            }
            if now >= p.collapse_at_sec {
                return Some(GateInspect::Expired {
                    ship_id: p.ship_id,
                    abyss_sys: p.abyssal_solar_system_id,
                    now,
                });
            }
            if p.current_room >= 3 {
                return Some(GateInspect::Err(BridgeRpcError {
                    code: "AlreadyInFinalRoom".into(),
                    message: "Character is already in the final room of the Abyssal pocket".into(),
                }));
            }
            let curr = p.current_room;
            let next = curr + 1;
            let curr_idx = (curr - 1) as usize;
            let next_idx = (next - 1) as usize;
            let c_npcs = p.rooms[curr_idx].npc_ball_ids.clone();
            let n_npcs = p.rooms[next_idx].npc_ball_ids.clone();
            let cleared = p.rooms[curr_idx].cleared;
            let loot = (
                p.tier,
                p.filament_type_id,
                p.rooms[next_idx].cache_item_id,
                p.npc_difficulty_mult,
            );
            Some(GateInspect::Ok((
                curr,
                next,
                p.pocket_id,
                p.abyssal_solar_system_id,
                c_npcs,
                n_npcs,
                cleared,
                loot,
            )))
        });

        match inspect {
            None => {
                return Err(BridgeRpcError {
                    code: "NotInAbyssalSpace".into(),
                    message: "No active Abyssal Deadspace pocket for character".into(),
                });
            }
            Some(GateInspect::Expired { ship_id: sid, abyss_sys, now }) => {
                collapse_abyssal_pocket_and_pod_pilot_at(ctx, char_id, sid, abyss_sys, now);
                ctx.sync_session(session);
                ctx.quasar_gateway.on_session_changed(session, now);
                return Err(BridgeRpcError {
                    code: "PocketCollapsed".into(),
                    message: "Abyssal Deadspace pocket has collapsed".into(),
                });
            }
            Some(GateInspect::Err(e)) => return Err(e),
            Some(GateInspect::Ok(v)) => v,
        }
    };

    let abyss_handle = ctx.systems.get_or_spawn(abyss_sys);
    let expected_gate_id = 1_895_000_000 + pid * 10 + (curr_room as i64);
    let alt_gate_id = 1_895_000_000 + (curr_room as i64);
    if gate_id == 0 || (gate_id != expected_gate_id && gate_id != alt_gate_id) {
        return Err(BridgeRpcError {
            code: "InvalidGate".into(),
            message: format!("Gate {gate_id} is not the active transfer gate for room {curr_room}"),
        });
    }

    if !room_already_cleared && !curr_room_npcs.is_empty() {
        let mut alive_count = 0;
        for &npc_id in &curr_room_npcs {
            if abyss_handle.get_ball_snapshot(npc_id).await.ok().flatten().is_some() {
                alive_count += 1;
            }
        }
        if alive_count > 0 {
            return Err(BridgeRpcError {
                code: "GateLocked".into(),
                message: format!(
                    "Transfer gate locked: {alive_count} hostile entities remaining in room {curr_room}"
                ),
            });
        }
    }

    let actual_gate_id = if abyss_handle
        .get_ball_snapshot(expected_gate_id)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        expected_gate_id
    } else {
        alt_gate_id
    };
    let ship_ball = abyss_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "SystemError".into(),
            message: e,
        })?
        .map(|(_, b)| b);
    let gate_ball = abyss_handle
        .get_ball_snapshot(actual_gate_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "SystemError".into(),
            message: e,
        })?
        .map(|(_, b)| b);
    let (Some(sb), Some(gb)) = (ship_ball, gate_ball) else {
        return Err(BridgeRpcError {
            code: "GateNotFound".into(),
            message: "Gate or ship ball not found in space".into(),
        });
    };
    let surface_dist = (sb.position.distance(gb.position) - sb.radius - gb.radius).max(0.0);
    if surface_dist > 2_500.0 {
        return Err(BridgeRpcError {
            code: "GateOutOfRange".into(),
            message: format!("Distance {surface_dist:.1} m exceeds 2,500 m gate activation proximity"),
        });
    }

    let now_u = ctx.now_unix();
    let advance_res = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        let curr_idx = (curr_room - 1) as usize;
        let already_spawned_next = reg.abyssal_pockets.values().any(|other| {
            other.pocket_id == pid
                && !other.completed
                && !other.collapsed_destroyed
                && other.current_room >= next_room
        });
        for other in reg.abyssal_pockets.values_mut() {
            if other.pocket_id == pid {
                other.rooms[curr_idx].cleared = true;
                other.rooms[curr_idx].gate_unlocked_at_sec.get_or_insert(now_u);
            }
        }
        let p = reg.abyssal_pockets.get_mut(&char_id)?;
        p.current_room = next_room;
        let any_still_in_curr_room = reg.abyssal_pockets.values().any(|other| {
            other.pocket_id == pid
                && !other.completed
                && !other.collapsed_destroyed
                && other.current_room == curr_room
        });
        Some((already_spawned_next, any_still_in_curr_room))
    });
    let Some((already_spawned_next, any_still_in_curr_room)) = advance_res else {
        return Err(BridgeRpcError {
            code: "NotInAbyssalSpace".into(),
            message: "Abyssal pocket disappeared during gate activation".into(),
        });
    };

    for &npc_id in &curr_room_npcs {
        abyss_handle.try_remove_ball(npc_id);
    }
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        for &npc_id in &curr_room_npcs {
            rt.npc_entities.remove(&npc_id);
        }
    });
    if !any_still_in_curr_room {
        let old_marker_id = 1_895_000_000 + pid * 10 + (curr_room as i64);
        abyss_handle.try_remove_ball(old_marker_id);
        let old_cache_id = 1_896_000_000 + pid * 10 + (curr_room as i64);
        let old_subnode_id = old_cache_id + 500_000;
        abyss_handle.try_remove_ball(old_cache_id);
        abyss_handle.try_remove_ball(old_subnode_id);
        for c in 1..=3_i64 {
            abyss_handle.try_remove_ball(1_898_000_000 + pid * 100 + (curr_room as i64) * 10 + c);
        }
        for py in 1..=4_i64 {
            abyss_handle.try_remove_ball(1_899_000_000 + pid * 100 + (curr_room as i64) * 10 + py);
        }
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&old_cache_id);
            snap.item_positions.remove(&old_cache_id);
            snap.items.remove(&old_subnode_id);
            snap.item_positions.remove(&old_subnode_id);
            snap.items.retain(|_, item| {
                item.location_id != old_cache_id && item.location_id != old_subnode_id
            });
        });
    }

    let z_pos = (next_room as f64 - 1.0) * 50_000.0;
    if !already_spawned_next {
        crate::services::pve_special_sites_abyssal::spawn_abyssal_room_contents(
            ctx,
            &abyss_handle,
            pid,
            next_room,
            &next_room_npcs,
            next_cache,
            tier,
            filament,
            difficulty,
            z_pos,
        )
        .await;
    }
    let base = DVec3::from_array(pocket_grid_offset(ctx, pid));
    let _ = abyss_handle
        .set_ball_position(ship_id, base + DVec3::new(0.0, 0.0, z_pos - 10_000.0))
        .await;

    Ok(PyRep::keyval([
        ("room", PyRep::Integer(next_room as i64)),
        ("solarSystemID", PyRep::Integer(abyss_sys as i64)),
        ("pocketID", PyRep::Integer(pid)),
        ("gateID", PyRep::Integer(gate_id)),
        ("gateActivated", PyRep::Bool(true)),
    ]))
}

enum EndGateInspect {
    Expired { ship_id: i64, abyss_sys: u32, now: u64 },
    Err(BridgeRpcError),
    Ok((u32, [f64; 3], HashMap<u16, f64>, Vec<i64>, bool, i64)),
}

pub(crate) async fn handle_abyssal_end_gate_activation(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let pocket_sys = session.solar_system_id.ok_or_else(err_no_solar_system)?;
    let ship_id = session.ship_id.ok_or_else(err_no_active_ship)?;

    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err_ship_docked("Cannot activate Abyssal end gate while docked"));
    }

    let (origin_sys, origin_pos, saved_attrs, r3_npcs, r3_cleared, pid) = {
        let now = ctx.now_unix();
        let inspect = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
            let p = reg.abyssal_pockets.get(&char_id)?;
            if p.completed || p.collapsed_destroyed {
                return None;
            }
            if now >= p.collapse_at_sec {
                return Some(EndGateInspect::Expired {
                    ship_id: p.ship_id,
                    abyss_sys: p.abyssal_solar_system_id,
                    now,
                });
            }
            if p.current_room != 3 {
                return Some(EndGateInspect::Err(BridgeRpcError {
                    code: "NotInFinalRoom".into(),
                    message: format!(
                        "Exit conduit only accessible from final room (currently in room {})",
                        p.current_room
                    ),
                }));
            }
            let r3_npcs = p.rooms[2].npc_ball_ids.clone();
            let r3_cleared = p.rooms[2].cleared;
            Some(EndGateInspect::Ok((
                p.origin_solar_system_id,
                p.origin_position,
                p.saved_ship_attrs.clone(),
                r3_npcs,
                r3_cleared,
                p.pocket_id,
            )))
        });

        match inspect {
            None => {
                return Err(BridgeRpcError {
                    code: "NotInAbyssalSpace".into(),
                    message: "No active Abyssal Deadspace pocket for character".into(),
                });
            }
            Some(EndGateInspect::Expired { ship_id: sid, abyss_sys, now }) => {
                collapse_abyssal_pocket_and_pod_pilot_at(ctx, char_id, sid, abyss_sys, now);
                ctx.sync_session(session);
                ctx.quasar_gateway.on_session_changed(session, now);
                return Err(BridgeRpcError {
                    code: "PocketCollapsed".into(),
                    message: "Abyssal Deadspace pocket has collapsed".into(),
                });
            }
            Some(EndGateInspect::Err(e)) => return Err(e),
            Some(EndGateInspect::Ok(v)) => v,
        }
    };

    let abyss_handle = ctx.systems.get_or_spawn(pocket_sys);
    let expected_gate_id = 1_895_000_000 + pid * 10 + 3;
    let alt_gate_id = 1_895_000_000 + 3;
    if gate_id == 0 || (gate_id != expected_gate_id && gate_id != alt_gate_id) {
        return Err(BridgeRpcError {
            code: "InvalidGate".into(),
            message: format!("Gate {gate_id} is not the active exit conduit for room 3"),
        });
    }

    if !r3_cleared && !r3_npcs.is_empty() {
        let mut alive_count = 0;
        for &npc_id in &r3_npcs {
            if abyss_handle.get_ball_snapshot(npc_id).await.ok().flatten().is_some() {
                alive_count += 1;
            }
        }
        if alive_count > 0 {
            return Err(BridgeRpcError {
                code: "GateLocked".into(),
                message: format!("Exit conduit locked: {alive_count} hostiles remaining in room 3"),
            });
        }
    }

    let actual_gate_id = if abyss_handle
        .get_ball_snapshot(expected_gate_id)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        expected_gate_id
    } else {
        alt_gate_id
    };
    let ship_ball = abyss_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "SystemError".into(),
            message: e,
        })?
        .map(|(_, b)| b);
    let gate_ball = abyss_handle
        .get_ball_snapshot(actual_gate_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "SystemError".into(),
            message: e,
        })?
        .map(|(_, b)| b);
    let (Some(sb), Some(gb)) = (ship_ball, gate_ball) else {
        return Err(BridgeRpcError {
            code: "GateNotFound".into(),
            message: "Gate or ship ball not found in space".into(),
        });
    };
    let surface_dist = (sb.position.distance(gb.position) - sb.radius - gb.radius).max(0.0);
    if surface_dist > 2_500.0 {
        return Err(BridgeRpcError {
            code: "GateOutOfRange".into(),
            message: format!("Distance {surface_dist:.1} m exceeds 2,500 m gate activation proximity"),
        });
    }

    let now_u = ctx.now_unix();
    let (removed_pid, despawned_trace_ids) =
        crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
            let mut traces = Vec::new();
            if let Some(mut p) = reg.abyssal_pockets.remove(&char_id) {
                p.completed = true;
                p.rooms[2].cleared = true;
                let pid = p.pocket_id;
                let bound_tid = p.trace_ball_id;
                for other in reg.abyssal_pockets.values_mut() {
                    if other.pocket_id == pid {
                        other.rooms[2].cleared = true;
                        other.rooms[2].gate_unlocked_at_sec.get_or_insert(now_u);
                    }
                }
                let any_remaining = reg.abyssal_pockets.values().any(|other| {
                    other.pocket_id == pid && !other.completed && !other.collapsed_destroyed
                });
                if !any_remaining {
                    if bound_tid > 0 {
                        traces.push(bound_tid);
                    }
                    reg.abyssal_traces.retain(|&tid, t| {
                        if t.pocket_id == Some(pid) || (bound_tid > 0 && tid == bound_tid) {
                            if !traces.contains(&tid) {
                                traces.push(tid);
                            }
                            false
                        } else {
                            true
                        }
                    });
                    (Some(pid), traces)
                } else {
                    (None, traces)
                }
            } else {
                (None, traces)
            }
        });

    crate::sp6_lifecycle::clear_abyssal_session(ctx, char_id);
    abyss_handle.try_remove_ball(ship_id);
    for &npc_id in &r3_npcs {
        abyss_handle.try_remove_ball(npc_id);
    }
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        for &npc_id in &r3_npcs {
            rt.npc_entities.remove(&npc_id);
        }
    });
    if let Some(pid) = removed_pid {
        let r3_marker_id = 1_895_000_000 + pid * 10 + 3;
        let r3_cache_id = 1_896_000_000 + pid * 10 + 3;
        let r3_subnode_id = r3_cache_id + 500_000;
        abyss_handle.try_remove_ball(r3_marker_id);
        abyss_handle.try_remove_ball(r3_cache_id);
        abyss_handle.try_remove_ball(r3_subnode_id);
        for c in 1..=3_i64 {
            abyss_handle.try_remove_ball(1_898_000_000 + pid * 100 + 30 + c);
        }
        for py in 1..=4_i64 {
            abyss_handle.try_remove_ball(1_899_000_000 + pid * 100 + 30 + py);
        }
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&r3_cache_id);
            snap.item_positions.remove(&r3_cache_id);
            snap.items.remove(&r3_subnode_id);
            snap.item_positions.remove(&r3_subnode_id);
            snap.items
                .retain(|_, item| item.location_id != r3_cache_id && item.location_id != r3_subnode_id);
        });
    }

    restore_abyssal_weather_dogma_modifiers(ctx, ship_id, &saved_attrs);

    let exit_pos = [
        origin_pos[0] + 1_000.0,
        origin_pos[1] + 1_000.0,
        origin_pos[2] + 1_000.0,
    ];
    let _ = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = origin_sys;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = origin_sys as i64;
        }
        snap.item_positions.insert(ship_id, exit_pos);
        Ok::<(), BridgeRpcError>(())
    });

    let origin_sys_handle = ctx.systems.get_or_spawn(origin_sys);
    for tid in despawned_trace_ids {
        origin_sys_handle.try_remove_ball(tid);
    }
    let ship_type = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(621));
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    let ball = eve_destiny::Ball {
        id: ship_id,
        type_id: ship_type,
        position: DVec3::from_array(exit_pos),
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = origin_sys_handle.spawn_ball_with_combat(ball, cs).await;
    session.solar_system_id = Some(origin_sys);
    crate::services::map_service::record_solar_system_visit(ctx, char_id, origin_sys);

    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(origin_sys as i64)),
        ("exitedAbyssalSpace", PyRep::Bool(true)),
        ("gateID", PyRep::Integer(gate_id)),
    ]))
}

pub(crate) async fn handle_void_space_player_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err_ship_docked("Cannot jump to Void Space while docked"));
    }
    let cur_sys = session.solar_system_id.ok_or_else(err_no_solar_system)?;
    let ship_id = session.ship_id.ok_or_else(err_no_active_ship)?;

    validate_allowed_ship_hull(ctx, ship_id)?;
    crate::sp6_lifecycle::require_ship_ready_for_filament_jump(ctx, char_id, ship_id, cur_sys)?;

    let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if target_id <= 0 {
        return Err(BridgeRpcError {
            code: "MissingRequiredArgument".into(),
            message: "VoidSpacePlayerJump requires a valid target ID".into(),
        });
    }

    let origin_handle = ctx.systems.get_or_spawn(cur_sys);
    let ship_pos = if let Ok(Some((_, ball))) = origin_handle.get_ball_snapshot(ship_id).await {
        [ball.position.x, ball.position.y, ball.position.z]
    } else {
        ctx.db.with_snapshot(|snap| {
            snap.item_positions
                .get(&ship_id)
                .copied()
                .unwrap_or([0.0, 0.0, 0.0])
        })
    };

    let is_cargo_filament = ctx.db.try_with_mut_snapshot(|snap| {
        let found = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == ship_id
                    && i.flag == eve_db::InventoryFlag::Cargo
                    && (i.item_id == target_id || i.type_id as i64 == target_id)
                    && i.quantity >= 1
                    && is_actual_filament_type(ctx, i.type_id)
            })
            .map(|i| i.item_id);
        if let Some(iid) = found {
            if let Some(item) = snap.items.get_mut(&iid) {
                if item.quantity > 1 {
                    item.quantity -= 1;
                    item.version += 1;
                } else {
                    snap.items.remove(&iid);
                }
            }
            Ok(true)
        } else {
            Ok(false)
        }
    })?;

    if !is_cargo_filament {
        if target_id == ship_id {
            return Err(BridgeRpcError {
                code: "InvalidTarget".into(),
                message: "Cannot target own ship as Void Space entrance gate".into(),
            });
        }
        let target_ball = origin_handle.get_ball_snapshot(target_id).await.ok().flatten();
        let Some((_, target_b)) = target_ball else {
            return Err(BridgeRpcError {
                code: "InvalidTarget".into(),
                message: format!(
                    "Target {target_id} is neither a filament in ship cargo nor a gate in space"
                ),
            });
        };
        if !is_valid_void_entrance_gate(ctx, target_id, target_b.type_id) {
            return Err(BridgeRpcError {
                code: "InvalidTarget".into(),
                message: format!(
                    "Target {target_id} (type {}) is not a valid Void Space entrance gate",
                    target_b.type_id
                ),
            });
        }
        let dist = DVec3::from_array(ship_pos).distance(target_b.position);
        if dist > 5_000.0 {
            return Err(BridgeRpcError {
                code: "OutOfRange".into(),
                message: format!("Void Space gate is too far ({dist:.1}m > 5000.0m)"),
            });
        }
    }

    let now_unix = ctx.now_unix();
    let void_session = crate::services::pve_special_sites::VoidSpaceSessionRecord {
        char_id,
        ship_id,
        origin_solar_system_id: cur_sys,
        origin_position: ship_pos,
        target_id,
        entered_unix: now_unix,
        expires_at_sec: now_unix.saturating_add(1_200),
    };
    crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        reg.void_space_sessions.insert(char_id, void_session);
    });
    crate::sp6_lifecycle::register_abyssal_session(
        ctx,
        char_id,
        ship_id,
        VOID_SPACE_SYSTEM_ID,
        cur_sys,
        now_unix,
        now_unix.saturating_add(1_200),
    );

    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, cur_sys);
    origin_handle.try_remove_ball(ship_id);

    let _ = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = VOID_SPACE_SYSTEM_ID;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = VOID_SPACE_SYSTEM_ID as i64;
        }
        snap.item_positions.insert(ship_id, [0.0, 0.0, 0.0]);
        Ok::<(), BridgeRpcError>(())
    });

    let void_handle = ctx.systems.get_or_spawn(VOID_SPACE_SYSTEM_ID);
    for exit_gid in [888_001_i64, 888_002_i64] {
        if void_handle.get_ball_snapshot(exit_gid).await.ok().flatten().is_none() {
            let _ = void_handle
                .spawn_ball(eve_destiny::Ball {
                    id: exit_gid,
                    type_id: 47_686,
                    position: DVec3::ZERO,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 10_000_000.0,
                    radius: 100.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                })
                .await;
        }
    }
    let ship_type = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(621));
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    let ball = eve_destiny::Ball {
        id: ship_id,
        type_id: ship_type,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = void_handle.spawn_ball_with_combat(ball, cs).await;
    session.solar_system_id = Some(VOID_SPACE_SYSTEM_ID);
    crate::services::map_service::record_solar_system_visit(ctx, char_id, VOID_SPACE_SYSTEM_ID);

    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(VOID_SPACE_SYSTEM_ID as i64)),
        ("originSolarSystemID", PyRep::Integer(cur_sys as i64)),
        ("voidSpaceJump", PyRep::Bool(true)),
        ("siteID", PyRep::Integer(target_id)),
    ]))
}

pub(crate) async fn handle_void_space_end_gate_activation(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let cur_sys = session.solar_system_id.ok_or_else(err_no_solar_system)?;
    if cur_sys != VOID_SPACE_SYSTEM_ID {
        return Err(BridgeRpcError::user_error("NotInVoidSpace"));
    }
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err_ship_docked("Cannot activate Void Space end gate while docked"));
    }
    let ship_id = session.ship_id.ok_or_else(err_no_active_ship)?;
    if gate_id <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidGate".into(),
            message: "VoidSpaceEndGateActivation requires a valid exit gateID".into(),
        });
    }

    let void_session = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        reg.void_space_sessions.get(&char_id).cloned()
    });
    let Some(vs) = void_session else {
        return Err(BridgeRpcError {
            code: "NotInVoidSpace".into(),
            message: "No active Void Space session record for character".into(),
        });
    };

    let now_unix = ctx.now_unix();
    if vs.expires_at_sec > 0 && now_unix >= vs.expires_at_sec {
        crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
            reg.void_space_sessions.remove(&char_id);
        });
        crate::sp6_lifecycle::clear_abyssal_session(ctx, char_id);
        return Err(BridgeRpcError {
            code: "PocketCollapsed".into(),
            message: "Void Space pocket has expired and collapsed".into(),
        });
    }

    if let Some(active_timer) =
        crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None)
    {
        if !matches!(active_timer.reason.as_str(), "login" | "undock") {
            return Err(BridgeRpcError {
                code: "SessionChangeInProgress".into(),
                message: format!(
                    "Cannot activate Void Space exit gate while 10s session-change timer ({}) is active",
                    active_timer.reason
                ),
            });
        }
    }

    let void_handle = ctx.systems.get_or_spawn(VOID_SPACE_SYSTEM_ID);
    if void_handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
        return Err(BridgeRpcError {
            code: "WarpScrambled".into(),
            message: "Cannot activate Void Space exit gate while warp scrambled".into(),
        });
    }

    let Some((_, gb)) = void_handle.get_ball_snapshot(gate_id).await.ok().flatten() else {
        return Err(BridgeRpcError {
            code: "InvalidGate".into(),
            message: format!("Gate {gate_id} is not present in Void Space"),
        });
    };
    if !is_valid_void_entrance_gate(ctx, gate_id, gb.type_id) {
        return Err(BridgeRpcError {
            code: "InvalidGate".into(),
            message: format!("Ball {gate_id} (type {}) is not a valid Void Space exit gate", gb.type_id),
        });
    }
    if let Some((_, sb)) = void_handle.get_ball_snapshot(ship_id).await.ok().flatten() {
        let surface_dist = (sb.position.distance(gb.position) - sb.radius - gb.radius).max(0.0);
        if surface_dist > 2_500.0 {
            return Err(BridgeRpcError {
                code: "GateOutOfRange".into(),
                message: format!("Distance {surface_dist:.1} m exceeds 2,500 m gate activation proximity"),
            });
        }
    }

    crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
        reg.void_space_sessions.remove(&char_id);
    });
    crate::sp6_lifecycle::clear_abyssal_session(ctx, char_id);
    void_handle.try_remove_ball(ship_id);

    let origin_sys = vs.origin_solar_system_id;
    let exit_pos = [
        vs.origin_position[0] + 1_000.0,
        vs.origin_position[1] + 1_000.0,
        vs.origin_position[2] + 1_000.0,
    ];
    let _ = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = origin_sys;
        }
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.location_id = origin_sys as i64;
        }
        snap.item_positions.insert(ship_id, exit_pos);
        Ok::<(), BridgeRpcError>(())
    });

    let origin_handle = ctx.systems.get_or_spawn(origin_sys);
    let ship_type = ctx
        .db
        .with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(621));
    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type);
    let ball = eve_destiny::Ball {
        id: ship_id,
        type_id: ship_type,
        position: DVec3::from_array(exit_pos),
        velocity: DVec3::ZERO,
        max_velocity: 250.0,
        inertia: 0.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = origin_handle.spawn_ball_with_combat(ball, cs).await;
    session.solar_system_id = Some(origin_sys);
    crate::services::map_service::record_solar_system_visit(ctx, char_id, origin_sys);

    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(origin_sys as i64)),
        ("exitedVoidSpace", PyRep::Bool(true)),
        ("gateID", PyRep::Integer(gate_id)),
    ]))
}
