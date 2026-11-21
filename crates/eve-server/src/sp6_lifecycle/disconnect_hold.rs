//! Disconnect emergency-warp gating, active module/cyno hold timers, and ship removal cleanup (`#498`).

#![forbid(unsafe_code)]

use super::{with_mut_sp6_state, with_sp6_state, DisconnectedShipEntry, NPC_TIMER_SECS};
use crate::router::ServerContext;
use crate::system_actor::SystemHandle;

/// Returns `true` if `ship_id` has active navigation-locking effects or immobilizing modules
/// that must block emergency warp on ungraceful disconnect (`#498`).
#[cfg(test)]
pub(super) fn is_disconnect_emergency_warp_blocked(
    ctx: &ServerContext,
    sys_handle: &SystemHandle,
    ship_id: i64,
) -> bool {
    is_disconnect_emergency_warp_blocked_opt(ctx, Some(sys_handle), ship_id)
}

pub(super) fn is_disconnect_emergency_warp_blocked_opt(
    ctx: &ServerContext,
    sys_handle: Option<&SystemHandle>,
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
    sys_handle.is_some_and(|h| {
        h.actor_state.read().is_ok_and(|st| {
            st.active_module_cycles
                .iter()
                .any(|c| c.ship_id == ship_id && (c.immobilize_self || c.self_disallow_assistance))
        })
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
    with_mut_sp6_state(&ctx.db, |st| {
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

/// Returns `true` if a `disconnected_ships` record for `cid` is still backed by an offline
/// pilot undocked in the recorded solar system aboard `disc.ship_id`. When `require_char_record`
/// is `true`, missing character rows are pruned; `finalize_expired_disconnected_ship` passes
/// `false` so every character invariant still applies when a pilot record exists while synthetic
/// item-only finalization fixtures continue to work.
///
/// `prelogoff_session` carries the `(session_id, connected_at_unix)` snapshot recorded at
/// `build_disconnected_ship_entry_sync` time for direct/pre-release logoffs so an uncleaned test
/// or pre-lock-release `OnlineSession` from before disconnect does not prematurely prune a live hold.
/// `reconnecting_session` carries the `session_id` of a `SelectCharacterID` reconnect currently in
/// flight so `reconnect_character_in_space_sync`'s internal `on_advance_secs(0)` does not prune a
/// hold whose return-warp state it is about to consume.
#[allow(clippy::too_many_arguments)]
pub(super) fn is_disconnected_ship_valid(
    has_ship_item: bool,
    char_rec: Option<&eve_db::CharacterRecord>,
    sessions: &std::collections::HashMap<u64, eve_db::OnlineSessionRecord>,
    online_check: Option<&ServerContext>,
    prelogoff_session: Option<(u64, u64)>,
    reconnecting_session: Option<u64>,
    reconnect_hold_ctx: Option<&ServerContext>,
    cid: i64,
    disc: &DisconnectedShipEntry,
    require_char_record: bool,
) -> bool {
    let pilot_ok = |c: &eve_db::CharacterRecord| {
        let runtime_os = online_check
            .and_then(|ctx| ctx.db.get_online_session(c.user_id))
            .filter(|os| os.character_id == Some(cid));
        let snap_os = sessions
            .get(&c.user_id)
            .filter(|os| os.character_id == Some(cid));
        // Read `reconnect_hold_session(cid)` AFTER observing `online_sessions`/`snap.sessions`
        // so `SelectCharacterID` (`ReconnectHoldGuard::acquire` -> `acquire_online_lock`)
        // cannot be torn by an inverted pre-snapshot read (`BF-R6-02`).
        let effective_reconnect = if runtime_os.is_some() || snap_os.is_some() {
            reconnect_hold_ctx
                .and_then(|ctx| ctx.reconnect_hold_session(cid))
                .or(reconnecting_session)
        } else {
            reconnecting_session
        };
        let is_hold_exempt_session = |os: &eve_db::OnlineSessionRecord| {
            prelogoff_session == Some((os.session_id, os.connected_at_unix))
                || effective_reconnect == Some(os.session_id)
        };
        let runtime_online = runtime_os.is_some_and(|os| !is_hold_exempt_session(&os));
        let snap_online = snap_os.is_some_and(|os| !is_hold_exempt_session(os));
        c.active_ship_id == disc.ship_id
            && c.station_id.is_none()
            && c.structure_id.is_none()
            && c.solar_system_id == disc.solar_system_id
            && !runtime_online
            && !snap_online
    };
    has_ship_item
        && if require_char_record {
            char_rec.is_some_and(pilot_ok)
        } else {
            char_rec.is_none_or(pilot_ok)
        }
}

fn disc_entry_matches(d: &DisconnectedShipEntry, disc: &DisconnectedShipEntry) -> bool {
    d.solar_system_id == disc.solar_system_id && d.ship_id == disc.ship_id
}

/// Verifies pre-despawn that `disc` is still the live disconnected hold entry and is neither
/// stale nor actively held by an in-flight `SelectCharacterID` reconnect.
fn pre_despawn_gate(
    ctx: &ServerContext,
    disc: &DisconnectedShipEntry,
) -> (bool, Option<(u64, u64)>) {
    let prelogoff_session = ctx.prelogoff_hold_session(disc.char_id);
    let reconnecting_session = ctx.reconnect_hold_session(disc.char_id);
    let (entry_matches, still_valid) = ctx.db.with_snapshot(|snap| {
        let entry_matches = snap
            .sp6_lifecycle
            .disconnected_ships
            .get(&disc.char_id)
            .is_some_and(|d| disc_entry_matches(d, disc));
        // Apply `reconnecting_session` exemption only while `disc` itself still sits in `disconnected_ships`.
        let effective_reconnecting = entry_matches.then_some(reconnecting_session).flatten();
        let reconnect_ctx = entry_matches.then_some(ctx);
        let valid = entry_matches
            && is_disconnected_ship_valid(
                snap.items.contains_key(&disc.ship_id),
                snap.characters.get(&disc.char_id),
                &*snap.sessions,
                Some(ctx),
                prelogoff_session,
                effective_reconnecting,
                reconnect_ctx,
                disc.char_id,
                disc,
                false,
            );
        (entry_matches, valid)
    });
    if !entry_matches {
        return (false, None);
    }
    if !still_valid {
        let mut removed_stale = false;
        let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
            let matches_stale = snap
                .sp6_lifecycle
                .disconnected_ships
                .get(&disc.char_id)
                .is_some_and(|d| disc_entry_matches(d, disc));
            if matches_stale {
                snap.sp6_lifecycle.disconnected_ships.remove(&disc.char_id);
                removed_stale = true;
            }
            Ok(())
        });
        if removed_stale {
            if let Some(pair) = prelogoff_session {
                ctx.clear_prelogoff_hold_session_if_matches(disc.char_id, pair);
            }
        }
        return (false, None);
    }
    if reconnecting_session.is_some() || ctx.reconnect_hold_session(disc.char_id).is_some() {
        // Re-arm collection in case the reconnecting session aborts before consuming `disc`.
        let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
            if let Some(d) = snap
                .sp6_lifecycle
                .disconnected_ships
                .get_mut(&disc.char_id)
                .filter(|d| disc_entry_matches(d, disc))
            {
                d.removed_from_space = false;
            }
            Ok(())
        });
        return (false, None);
    }
    (true, prelogoff_session)
}

/// Removes an expired disconnected ship from space after cleaning up active modules and cyno state.
pub(super) fn finalize_expired_disconnected_ship(
    ctx: &ServerContext,
    disc: DisconnectedShipEntry,
) {
    let (proceed_initial, _) = pre_despawn_gate(ctx, &disc);
    if !proceed_initial {
        return;
    }
    let handle = ctx.systems.get_or_spawn(disc.solar_system_id);
    let final_pos = handle
        .get_ball_position_sync(disc.ship_id)
        .map(|v| [v.x, v.y, v.z])
        .unwrap_or(disc.last_position);
    let final_cs = handle.view().combat_state(disc.ship_id).cloned();
    let (proceed_despawn, prelogoff_session) = pre_despawn_gate(ctx, &disc);
    if !proceed_despawn {
        return;
    }
    cleanup_ship_modules_and_cyno_on_despawn(ctx, disc.char_id, disc.ship_id);
    handle.try_remove_ball(disc.ship_id);
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
        let Some(d) = snap
            .sp6_lifecycle
            .disconnected_ships
            .get_mut(&disc.char_id)
            .filter(|d| disc_entry_matches(d, &disc))
        else {
            return Ok(());
        };
        d.last_position = final_pos;
        d.removed_from_space = true;
        super::leave_space_in_snap(
            snap,
            disc.ship_id,
            final_cs.as_ref(),
            None,
            Some(final_pos),
        );
        Ok(())
    });
    if let Some(pair) = prelogoff_session {
        ctx.clear_prelogoff_hold_session_if_matches(disc.char_id, pair);
    }
}

/// Evaluates logoff hold timers and emergency-warp vs mid-tunnel-warp preservation for an ungraceful disconnect (`#851` / `B13`).
#[allow(clippy::too_many_arguments)]
pub(super) fn build_disconnected_ship_entry_sync(
    ctx: &ServerContext,
    sys_handle: Option<&SystemHandle>,
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
    let sys_view = sys_handle.map(|h| h.view());
    let has_target_locks = sys_view.as_ref().is_some_and(|view| {
        view.target_locks
            .get(&ship_id)
            .is_some_and(|l| !l.is_empty())
    });
    let has_active_combat_engagement = sys_handle
        .is_some_and(|h| h.has_active_modules_or_cloak_sync(ship_id))
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
    let is_ball_scrambled = sys_view
        .as_ref()
        .is_some_and(|view| view.is_ball_scrambled(ship_id))
        || ctx.systems.is_ball_scrambled_sync(sys_id, ship_id);
    let is_scrambled =
        in_abyssal || is_ball_scrambled || (sys_sec >= 0.45 && crim_until > now_unix);

    let committed_warp_dest = sys_handle.and_then(|h| h.committed_warp_destination_sync(ship_id));
    if committed_warp_dest.is_none() {
        if let Some(handle) = sys_handle {
            crate::services::space_destiny::clear_warp_tracking(handle, ship_id);
        }
    }

    let (emergency_warp_dest, pre_warp_position) = if let Some(committed_dest) = committed_warp_dest
    {
        (
            Some([committed_dest.x, committed_dest.y, committed_dest.z]),
            None,
        )
    } else if !is_scrambled
        && weap_until <= now_unix
        && !is_disconnect_emergency_warp_blocked_opt(ctx, sys_handle, ship_id)
    {
        let raw_dest = super::compute_emergency_warp_destination(current_pos, char_id, ship_id);
        let mut dir = (raw_dest - current_pos).normalize_or_zero();
        if let Some(view) = sys_view.as_ref() {
            let ship_r = view.ball(ship_id).map(|b| b.radius).unwrap_or(50.0);
            for (&bid, b) in &view.balls {
                let is_immobile = view.is_static_ball(bid)
                    || match view.slim_meta(bid) {
                        Some(m) => matches!(m.category_id, 2 | 3 | 22 | 23 | 40 | 46 | 65),
                        None => b.max_velocity <= 0.0,
                    };
                if bid != ship_id && is_immobile {
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
        if let Some(h) = sys_handle {
            if let Ok(mut bp) = h.ballpark.write() {
                if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
                    cs.is_emergency_warping = true;
                }
                let _ = bp.set_mode(
                    ship_id,
                    eve_destiny::MovementMode::new_warp(current_pos, dest, warp_speed_au_s),
                );
            }
            h.set_warp_provenance(
                ship_id,
                crate::system_actor::WarpProvenance::DisconnectedEmergencyWarp,
            );
        }
        (
            Some([dest.x, dest.y, dest.z]),
            Some([current_pos.x, current_pos.y, current_pos.z]),
        )
    } else {
        if let Some(h) = sys_handle {
            if h.is_ball_aligning_warp_sync(ship_id) {
                h.try_stop_ball(ship_id);
            }
        }
        (None, None)
    };

    let last_velocity = sys_handle
        .and_then(|h| h.get_ball_velocity_sync(ship_id))
        .unwrap_or([0.0, 0.0, 0.0]);

    let prelogoff_session = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id))
        .and_then(|uid| ctx.db.get_online_session(uid))
        .filter(|os| os.character_id == Some(char_id))
        .map(|os| (os.session_id, os.connected_at_unix));
    ctx.record_prelogoff_hold_session(char_id, prelogoff_session);

    DisconnectedShipEntry {
        char_id,
        ship_id,
        ship_type_id,
        solar_system_id: sys_id,
        disconnected_at_unix: now_unix,
        remove_at_unix,
        emergency_warp_dest,
        last_position: [current_pos.x, current_pos.y, current_pos.z],
        last_velocity,
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
        Some(sys_handle),
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
