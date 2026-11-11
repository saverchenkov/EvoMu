//! Disconnect emergency-warp gating, active module/cyno hold timers, and ship removal cleanup (`#498`).

#![forbid(unsafe_code)]

use super::{with_mut_sp6_state, with_sp6_state, DisconnectedShipEntry, NPC_TIMER_SECS};
use crate::router::ServerContext;
use crate::system_actor::SystemHandle;

/// Returns `true` if `ship_id` has active navigation-locking effects or immobilizing modules
/// that must block emergency warp on ungraceful disconnect (`#498`).
pub(super) fn is_disconnect_emergency_warp_blocked(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    ship_id: i64,
) -> bool {
    if crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "emergency warp")
        .is_err()
        || crate::services::tactical_modules::is_clone_vat_bay_active_required(ctx, ship_id)
        || crate::services::space_navigation_advanced::check_immobilizing_module_active(
            ctx, ship_id, "warp",
        )
        .is_err()
    {
        return true;
    }
    sys_handle.actor_state.read().is_ok_and(|st| {
        st.active_module_cycles
            .iter()
            .any(|c| c.ship_id == ship_id && (c.immobilize_self || c.self_disallow_assistance))
    })
}

/// Returns `true` if `ship_id` has any non-standard active link, cyno, bridge, or immobilizer
/// that counts as active combat/tactical engagement at disconnect.
pub(super) fn has_tactical_engagement_on_disconnect(ctx: &ServerContext, ship_id: i64) -> bool {
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "disconnect")
        .is_err()
        || crate::services::tactical_modules::is_clone_vat_bay_active_required(ctx, ship_id)
        || crate::services::space_navigation_advanced::check_immobilizing_module_active(
            ctx, ship_id, "warp",
        )
        .is_err()
}

/// Computes the minimum Unix timestamp until which `ship_id` must remain in space while
/// active module cycles, cyno beacons/bridges, or navigation locks remain active (`#498`).
pub(super) fn compute_disconnect_hold_until_unix(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    now_unix: u64,
) -> u64 {
    let cyno_exp =
        crate::services::inventory_fitting::fitting_cyno_portal::ship_cyno_activity_expiry_unix(
            ctx, ship_id,
        );
    let nav_exp =
        crate::services::tactical_modules::ship_navigation_lock_expiry_unix(ctx, ship_id);
    let cycle_exp = ctx
        .systems
        .max_active_module_cycle_expiry_unix_sync(sys_id, ship_id, cyno_exp, now_unix);
    let immobilize_hold = if crate::services::tactical_modules::is_clone_vat_bay_active_required(
        ctx, ship_id,
    ) || crate::services::space_navigation_advanced::check_immobilizing_module_active(
        ctx, ship_id, "warp",
    )
    .is_err()
    {
        now_unix.saturating_add(NPC_TIMER_SECS)
    } else {
        0
    };
    cyno_exp.max(nav_exp).max(cycle_exp).max(immobilize_hold)
}

/// Extends active disconnect timers for any ship still under an active cycle, cyno beacon,
/// or navigation lock, and returns the disconnected ships whose hold timer has now expired.
pub(super) fn collect_expired_disconnected_ships_filtered(
    ctx: &ServerContext,
    now_unix: u64,
    only_non_warping: bool,
) -> Vec<DisconnectedShipEntry> {
    let active: Vec<DisconnectedShipEntry> = with_sp6_state(&ctx.db, |st| {
        st.disconnected_ships
            .values()
            .filter(|d| {
                !d.removed_from_space && (!only_non_warping || d.emergency_warp_dest.is_none())
            })
            .cloned()
            .collect()
    });
    let mut updates: Vec<(i64, u64)> = Vec::new();
    for disc in active {
        let hold_until =
            compute_disconnect_hold_until_unix(ctx, disc.solar_system_id, disc.ship_id, now_unix);
        let eff_remove = disc.remove_at_unix.max(hold_until);
        if eff_remove > disc.remove_at_unix || now_unix >= eff_remove {
            updates.push((disc.char_id, eff_remove));
        }
    }
    if updates.is_empty() {
        return Vec::new();
    }
    let mut expired = Vec::new();
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        for (cid, eff_remove) in updates {
            if let Some(d) = st.disconnected_ships.get_mut(&cid) {
                d.remove_at_unix = d.remove_at_unix.max(eff_remove);
                if !d.removed_from_space
                    && (!only_non_warping || d.emergency_warp_dest.is_none())
                    && now_unix >= d.remove_at_unix
                {
                    d.removed_from_space = true;
                    expired.push(d.clone());
                }
            }
        }
    });
    expired
}

pub(super) fn collect_expired_disconnected_ships(
    ctx: &ServerContext,
    now_unix: u64,
) -> Vec<DisconnectedShipEntry> {
    collect_expired_disconnected_ships_filtered(ctx, now_unix, false)
}

/// Deactivates active ship modules and clears any cyno beacons or jump bridges when `ship_id`
/// leaves space on logoff completion.
pub(super) fn cleanup_ship_modules_and_cyno_on_despawn(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
) {
    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(
        ctx, char_id, ship_id,
    );
    crate::services::inventory_fitting::fitting_cyno_portal::on_cyno_ship_destroyed(ctx, ship_id);
}

/// Removes an expired disconnected ship from space after cleaning up active modules and cyno state.
pub(super) fn finalize_expired_disconnected_ship(
    ctx: &ServerContext,
    disc: DisconnectedShipEntry,
) {
    let handle = ctx.systems.get_or_spawn(disc.solar_system_id);
    let final_pos = handle
        .get_ball_position_sync(disc.ship_id)
        .map(|v| [v.x, v.y, v.z])
        .unwrap_or(disc.last_position);
    let final_cs = handle
        .ballpark
        .read()
        .ok()
        .and_then(|bp| bp.combat_states.get(&disc.ship_id).cloned());
    cleanup_ship_modules_and_cyno_on_despawn(ctx, disc.char_id, disc.ship_id);
    handle.try_remove_ball(disc.ship_id);
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
        if let Some(d) = snap.sp6_lifecycle.disconnected_ships.get_mut(&disc.char_id) {
            d.last_position = final_pos;
            d.removed_from_space = true;
        }
        super::leave_space_in_snap(
            snap,
            disc.ship_id,
            final_cs.as_ref(),
            None,
            Some(final_pos),
        );
        Ok(())
    });
}

/// Evaluates logoff hold timers and emergency-warp vs mid-tunnel-warp preservation for an ungraceful disconnect (`#851` / `B13`).
#[allow(clippy::too_many_arguments)]
pub(super) fn build_disconnected_ship_entry_sync(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    current_pos: glam::DVec3,
    warp_speed_au_s: f64,
    now_unix: u64,
) -> DisconnectedShipEntry {
    let (weap_until, npc_until, pvp_until, crim_until) =
        super::active_timers_for_char(ctx, char_id, now_unix);
    let has_target_locks = sys_handle
        .ballpark
        .read()
        .is_ok_and(|bp| bp.target_locks.get(&ship_id).is_some_and(|l| !l.is_empty()));
    let has_active_combat_engagement = sys_handle.has_active_modules_or_cloak_sync(ship_id)
        || has_target_locks
        || has_tactical_engagement_on_disconnect(ctx, ship_id);
    let base_remove_secs = if has_active_combat_engagement {
        NPC_TIMER_SECS
    } else {
        super::UNAGGRESSED_LOGOFF_REMOVE_SECS
    };
    let hold_until = compute_disconnect_hold_until_unix(ctx, sys_id, ship_id, now_unix);
    let remove_at_unix = pvp_until
        .max(crim_until)
        .max(npc_until)
        .max(weap_until)
        .max(hold_until)
        .max(now_unix.saturating_add(base_remove_secs));

    let sys_sec = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.modified_system_securities.get(&sys_id).copied()
    })
    .unwrap_or_else(|| {
        ctx.sde
            .solar_systems
            .get(&sys_id)
            .map(|s| s.security_status)
            .unwrap_or(0.5)
    });
    let in_abyssal = (32_000_000..=32_999_999).contains(&sys_id);
    let is_ball_scrambled = sys_handle
        .ballpark
        .read()
        .is_ok_and(|bp| bp.is_ball_scrambled(ship_id))
        || ctx.systems.is_ball_scrambled_sync(sys_id, ship_id);
    let is_scrambled =
        in_abyssal || is_ball_scrambled || (sys_sec >= 0.45 && crim_until > now_unix);

    let committed_warp_dest = sys_handle.committed_warp_destination_sync(ship_id);
    if committed_warp_dest.is_none() {
        ctx.active_warps.lock().unwrap_or_else(|e| e.into_inner()).remove(&ship_id);
    }

    let (emergency_warp_dest, pre_warp_position) = if let Some(committed_dest) = committed_warp_dest
    {
        (
            Some([committed_dest.x, committed_dest.y, committed_dest.z]),
            None,
        )
    } else if !is_scrambled
        && weap_until <= now_unix
        && !is_disconnect_emergency_warp_blocked(ctx, sys_handle, ship_id)
    {
        let raw_dest = super::compute_emergency_warp_destination(current_pos, char_id, ship_id);
        let mut dir = (raw_dest - current_pos).normalize_or_zero();
        if let Ok(bp) = sys_handle.ballpark.read() {
            let ship_r = bp.balls.get(&ship_id).map(|b| b.radius).unwrap_or(50.0);
            for (&bid, b) in &bp.balls {
                if bid != ship_id && bp.is_immobile_structure_or_static(bid) {
                    let to_ship = current_pos - b.position;
                    let dist = to_ship.length();
                    if dist <= b.radius + ship_r + 2_500.0 && dist > 1.0 {
                        let outward = to_ship / dist;
                        let into_wall = dir.dot(outward);
                        if into_wall < 0.1 {
                            dir = (dir - outward * (2.0 * into_wall)).normalize_or_zero();
                        }
                    }
                }
            }
        }
        let dest = if dir.length_squared() > 0.5 {
            current_pos + dir * super::EMERGENCY_WARP_DISTANCE_M
        } else {
            raw_dest
        };
        if let Ok(mut bp) = sys_handle.ballpark.write() {
            if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
                cs.is_emergency_warping = true;
            }
            let _ = bp.set_mode(
                ship_id,
                eve_destiny::MovementMode::new_warp(current_pos, dest, warp_speed_au_s),
            );
        }
        (
            Some([dest.x, dest.y, dest.z]),
            Some([current_pos.x, current_pos.y, current_pos.z]),
        )
    } else {
        if sys_handle.is_ball_aligning_warp_sync(ship_id) {
            sys_handle.try_stop_ball(ship_id);
        }
        (None, None)
    };

    DisconnectedShipEntry {
        char_id,
        ship_id,
        ship_type_id,
        solar_system_id: sys_id,
        disconnected_at_unix: now_unix,
        remove_at_unix,
        emergency_warp_dest,
        last_position: [current_pos.x, current_pos.y, current_pos.z],
        removed_from_space: false,
        pre_warp_position,
    }
}

/// Evaluates logoff hold timers and emergency-warp vs mid-tunnel-warp preservation for an ungraceful disconnect.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) async fn build_disconnected_ship_entry(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    current_pos: glam::DVec3,
    warp_speed_au_s: f64,
    now_unix: u64,
) -> DisconnectedShipEntry {
    build_disconnected_ship_entry_sync(
        ctx,
        sys_handle,
        char_id,
        ship_id,
        ship_type_id,
        sys_id,
        current_pos,
        warp_speed_au_s,
        now_unix,
    )
}

#[cfg(test)]
#[path = "disconnect_hold_edges_tests.rs"]
mod disconnect_hold_edges_tests;
