//! `space_destiny` — Domain 5: Solar System `Ballpark`, Subwarp Navigation, & Space Services Router (`§3.5`).

#![forbid(unsafe_code)]

use crate::router::notify_registry::{OnReserveBankUnlock, OnWarpFinished};
use crate::router::{BoundObjectEntry, ServerContext};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use glam::DVec3;
use std::collections::HashMap;

const UPDATE_STATE_COOLDOWN_TICKS: u64 = 5;

pub(crate) fn take_pending_structure_set_state(
    ctx: &ServerContext,
    session_id: u64,
    char_id: i64,
) -> Option<i64> {
    ctx.take_pending_structure_view_outside_set_state(session_id, char_id)
}

/// Clears any per-session `beyonce.UpdateStateRequest` cooldown for `(session_id, char_id)` so a
/// lag-resync or re-attachment immediately delivers a fresh `SetState` snapshot (`#1169`).
pub(crate) fn reset_update_state_rate_limit(ctx: &ServerContext, session_id: u64, char_id: i64) {
    ctx.systems.clear_explicit_update_state_guard(session_id, char_id);
    ctx.clear_pending_structure_view_outside_set_state(session_id, char_id);
}

pub use eve_db::EssReservePulseState;

pub fn get_ess_reserve_pulses(snap: &eve_db::WorldStateSnapshot, sys_id: u32) -> EssReservePulseState {
    snap.mining_and_ess_state
        .ess_reserve_pulses
        .get(&sys_id)
        .cloned()
        .unwrap_or_default()
}

pub fn set_ess_reserve_pulses(snap: &mut eve_db::WorldStateSnapshot, sys_id: u32, st: &EssReservePulseState) {
    snap.mining_and_ess_state
        .ess_reserve_pulses
        .insert(sys_id, st.clone());
}

#[path = "space_warp_jump.rs"]
pub mod space_warp_jump;

#[path = "space_cyno_filaments.rs"]
pub mod space_cyno_filaments;

#[path = "space_docking_cloaking.rs"]
pub mod space_docking_cloaking;
pub use space_docking_cloaking::*;

#[path = "space_pochven_wormholes.rs"]
pub mod space_pochven_wormholes;
pub use space_pochven_wormholes::*;

#[path = "space_nav_travel.rs"]
pub mod space_nav_travel;

pub(crate) const AU_IN_METERS: f64 = 149_597_870_700.0;
const BEYONCE_SVC: &str = "beyonce";

pub use crate::router::session_transition::{
    check_session_change_timer_active, clear_session_change_timer, get_session_change_timer,
    record_client_session_check_timer, record_session_change_timer, SessionChangeTimerState,
    STARGATE_SESSION_CHANGE_TIMER_SECS,
};

pub(crate) fn emit_protection_break_notifications(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    events: &crate::system_actor::ProtectionBreakEvents,
) {
    let cloak_cancelled = events.should_emit_jump_cloak_cancelled();
    let invuln_cancelled = events.invuln_cancelled && ship_id > 0;
    if cloak_cancelled || invuln_cancelled {
        crate::services::notify_emitters_space_dungeons::emit_warp_cloak_or_invuln_cancelled(
            ctx, char_id, ship_id, cloak_cancelled, invuln_cancelled,
        );
    }
}

pub(crate) async fn break_ship_protection(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    reason: crate::system_actor::ProtectionBreakReason,
) -> crate::system_actor::ProtectionBreakEvents {
    if ship_id <= 0 {
        return crate::system_actor::ProtectionBreakEvents::default();
    }
    let now = ctx.now_unix();
    match sys
        .break_protection(ship_id, Some(char_id), reason, now)
        .await
    {
        Ok(events) => {
            emit_protection_break_notifications(ctx, char_id, ship_id, &events);
            events
        }
        Err(_) => crate::system_actor::ProtectionBreakEvents::default(),
    }
}

pub fn wormhole_item_id_for_signature(sys_id: u32, sig_id: &str) -> i64 {
    let hash = sig_id
        .bytes()
        .fold(0i64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as i64))
        .abs();
    760_000_000 + ((sys_id as i64 % 10_000) * 10_000) + (hash % 10_000)
}

pub fn compute_ship_warp_speed_au_s(ctx: &ServerContext, ship_type_id: u32) -> f64 {
    if let Some(t) = ctx.sde.types.get(&ship_type_id) {
        let base_warp = t
            .default_attributes
            .get(&1281)
            .copied()
            .unwrap_or(1.0)
            .max(1.0);
        let warp_mult = t
            .default_attributes
            .get(&attr_ids::WARP_SPEED_MULTIPLIER)
            .copied()
            .unwrap_or(3.0);
        let role_bonus_pct = t.default_attributes.get(&2789).copied().unwrap_or(0.0);
        base_warp * warp_mult * (1.0 + role_bonus_pct / 100.0)
    } else {
        3.0
    }
}

pub fn compute_pilot_warp_speed_au_s(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    ship_type_id: u32,
) -> f64 {
    compute_ship_warp_speed_au_s(ctx, ship_type_id)
        + crate::services::security_mgr::insurgency_pirate_warp_bonus_au_s(ctx, char_id, sys_id)
}

pub(crate) use space_nav_travel::sync_ship_cloak_and_mjfg_from_fitting;

fn handle_ess_mgr_clear_cache(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_character(session)?;
    if let Some(arg_oid) = args.first() {
        ctx.invalidate_cached_object(arg_oid);
    }
    let to_invalidate: Vec<PyRep> = {
        let cache = ctx.object_cache.read().unwrap_or_else(|e| e.into_inner());
        cache
            .iter()
            .filter(|(k, _)| k.contains("(essMgr,"))
            .map(|(_, rec)| rec.object_id.clone())
            .collect()
    };
    for oid in to_invalidate {
        ctx.invalidate_cached_object(&oid);
    }
    ctx.invalidate_cached_method("essMgr", "GetDataForClientSolarSystem");
    ctx.invalidate_cached_method("essMgr", "prime_cache_result");
    Ok(PyRep::None)
}

fn handle_ess_thefts_for_client_system(
    ctx: &ServerContext,
    session: &SessionState,
    is_reserve: bool,
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    let Some(sys_id) = session.solar_system_id else {
        return Ok(PyRep::List(Vec::new()));
    };
    let thefts = ctx.db.with_snapshot(|snap| {
        let map = if is_reserve {
            &snap.combat_crimewatch_state.ess_reserve_bank_thefts
        } else {
            &snap.combat_crimewatch_state.ess_main_bank_thefts
        };
        map.get(&sys_id).cloned().unwrap_or_default()
    });
    let rows: Vec<PyRep> = thefts
        .into_iter()
        .map(|(char_id, amount, ts)| {
            PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("stolenISK", PyRep::Real(amount)),
                ("timestamp", PyRep::Integer(ts as i64)),
            ])
        })
        .collect();
    Ok(PyRep::List(rows))
}

fn handle_ess_prime_cache_result(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    crate::services::encounter_surveillance_system::prime_cache_result(ctx, session, args)
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
    if service == BEYONCE_SVC
        && matches!(
            method,
            "CmdGotoDirection"
                | "CmdSteerDirection"
                | "CmdGotoPoint"
                | "CmdStop"
                | "CmdSetSpeedFraction"
                | "CmdOrbit"
                | "CmdFollowBall"
                | "CmdAlignTo"
        )
    {
        if let Some(cid) = session.character_id {
            let _ = crate::sp6_lifecycle::maybe_abort_safe_logoff_on_action(
                ctx,
                cid,
                "SAFE_LOGOFF_ABORT_MOVING",
            );
        }
    }
    match (service, method) {
        // parity: plan:P-7, plan:SP-1, plan:SP-2, plan:SP-3
        // parity: feat:navigation.destiny-updates, feat:navigation.celestials-balls, feat:navigation.bubbles-grid, feat:navigation.approach, feat:dogma.velocity-agility, feat:modules.micro-jump-drive
        // parity: rpc:beyonce.UpdateStateRequest, rpc:beyonce.AdminGetBubbleID, rpc:beyonce.GetFormations
        // parity: destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall
        // parity: destiny:GotoDirection, destiny:FollowBall, destiny:Orbit, destiny:Stop
        // parity: notify:DoDestinyUpdate
        ("beyonce", "CmdGotoDirection") | ("beyonce", "CmdSteerDirection") => {
            Some(space_nav_travel::handle_cmd_goto_direction(ctx, session, bound_entry, args).await)
        }
        // parity: destiny:GotoPoint
        ("beyonce", "CmdGotoPoint") => {
            Some(space_nav_travel::handle_cmd_goto_point(ctx, session, bound_entry, args).await)
        }
        // parity: feat:navigation.jump-fatigue, rpc:jumpTimers.GetTimers, notify:OnJumpTimersUpdated
        ("jumpTimers", "GetTimers") => {
            Some(space_nav_travel::handle_get_jump_timers(ctx, session, args))
        }
        // parity: rpc:beyonce.CmdStop, feat:navigation.stop-ship, destiny:Stop
        ("beyonce", "CmdStop") => Some(space_nav_travel::handle_cmd_stop(ctx, session, bound_entry).await),
        // parity: rpc:beyonce.CmdSetSpeedFraction
        ("beyonce", "CmdSetSpeedFraction") => {
            Some(space_nav_travel::handle_cmd_set_speed_fraction(ctx, session, bound_entry, args).await)
        }
        // parity: rpc:beyonce.CmdOrbit, rpc:beyonce.CmdFollowBall, feat:navigation.orbit, feat:navigation.approach, destiny:Orbit, destiny:FollowBall
        ("beyonce", "CmdOrbit") | ("beyonce", "CmdFollowBall") => {
            Some(space_nav_travel::handle_cmd_orbit_or_follow(ctx, session, bound_entry, method, args).await)
        }
        // parity: rpc:beyonce.CmdAlignTo, feat:navigation.align-to
        ("beyonce", "CmdAlignTo") => {
            Some(space_nav_travel::handle_cmd_align_to(ctx, session, bound_entry, args, kwargs).await)
        }
        ("beyonce", "UpdateStateRequest") => {
            Some(handle_update_state_request_inner(ctx, session, bound_entry, true).await)
        }
        ("beyonce", "AdminGetBubbleID") => {
            Some(handle_admin_get_bubble_id(ctx, session, bound_entry, args).await)
        }
        // parity: rpc:essMgr.GetDataForClientSolarSystem
        ("essMgr", "GetDataForClientSolarSystem") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Ok(PyRep::None));
            };
            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            if !(sys_sec <= 0.0 && sys_id < 31_000_000) {
                return Some(Ok(PyRep::None));
            }
            crate::services::pve_ess_reserve_banks::ensure_ess_intrusion_site_balls(ctx, sys_id).await;
            let now_u = ctx.now_unix();
            let rep = ctx.db.with_snapshot(|snap| {
                let Some(st) = snap.system_states.get(&sys_id) else { return PyRep::None };
                let pulse_st = get_ess_reserve_pulses(snap, sys_id);
                let rt_until = snap
                    .combat_crimewatch_state
                    .ess_reserve_unlocked_until
                    .get(&sys_id)
                    .copied()
                    .unwrap_or(0);
                let eff_until = pulse_st.unlocked_until_unix.max(rt_until);
                let rem_pulses = if pulse_st.unlocked_until_unix > 0 && eff_until <= now_u {
                    0
                } else {
                    pulse_st.pulses_remaining
                };
                let last_ft = pulse_st.last_pulse_sec.map(|s| PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(s))).unwrap_or(PyRep::None);
                PyRep::Dict(vec![
                    (PyRep::String("essID".into()), PyRep::Integer(820_000_000 + sys_id as i64)),
                    (PyRep::String("mainValue".into()), PyRep::Real(st.ess_main_bank.to_wire_f64())),
                    (PyRep::String("reserveValue".into()), PyRep::Real(st.ess_reserve_bank.to_wire_f64())),
                    (PyRep::String("mainBankLink".into()), PyRep::None),
                    (PyRep::String("reserveBankLastPulseInitiated".into()), last_ft),
                    (PyRep::String("reserveBankPulsesRemaining".into()), PyRep::Integer(rem_pulses as i64)),
                    (PyRep::String("reserveBankPulsesTotal".into()), PyRep::Integer(pulse_st.pulses_total as i64)),
                    (PyRep::String("reserveBankActiveLinks".into()), PyRep::Integer(i64::from(pulse_st.reserve_linked_character_id.is_some()))),
                ])
            });
            Some(Ok(rep))
        }

        ("essMgr", "GetMainBankTheftsForClientSolarSystem") => {
            Some(handle_ess_thefts_for_client_system(ctx, session, false))
        }

        ("essMgr", "GetReserveBankTheftsForClientSolarSystem") => {
            Some(handle_ess_thefts_for_client_system(ctx, session, true))
        }

        ("essMgr", "IsClientLinkedToReserveBank") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Ok(PyRep::Bool(false)));
            };
            let is_linked = ctx.db.with_snapshot(|snap| {
                get_ess_reserve_pulses(snap, sys_id).reserve_linked_character_id == Some(char_id)
            });
            Some(Ok(PyRep::Bool(is_linked)))
        }

        ("essMgr", "RequestMainBankUnlink") | ("essMgr", "RequestReserveBankUnlink") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Ok(PyRep::Bool(true)));
            };
            let is_reserve = method == "RequestReserveBankUnlink";
            let unlinked = ctx.db.with_mut_snapshot(|snap| {
                let mut pulse_st = get_ess_reserve_pulses(snap, sys_id);
                if is_reserve {
                    let was_reserve_linked =
                        pulse_st.reserve_linked_character_id == Some(char_id);
                    if was_reserve_linked {
                        pulse_st.reserve_linked_character_id = None;
                        pulse_st.reserve_link_started_tick = None;
                        pulse_st.link_started_unix = None;
                        set_ess_reserve_pulses(snap, sys_id, &pulse_st);
                    }
                    if let Some(st) = snap.system_states.get_mut(&sys_id) {
                        if st.ess_linked_character_id == Some(char_id) {
                            st.ess_linked_character_id = None;
                            st.ess_link_started_tick = None;
                            return true;
                        }
                    }
                    return was_reserve_linked;
                }
                if let Some(st) = snap.system_states.get_mut(&sys_id) {
                    if st.ess_linked_character_id == Some(char_id)
                        && pulse_st.reserve_linked_character_id != Some(char_id)
                    {
                        st.ess_linked_character_id = None;
                        st.ess_link_started_tick = None;
                        return true;
                    }
                }
                false
            });
            if is_reserve && unlinked {
                super::notify_emitters_combat_ess_progression::emit_reserve_bank_player_link_disconnected(
                    ctx, char_id, sys_id, 0,
                );
            }
            Some(Ok(PyRep::Bool(unlinked)))
        }

        ("essMgr", "RequestUnlockReserveBank") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot unlock ESS Reserve Bank while docked".into(),
                    client_error: None,
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                    client_error: None,
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                    client_error: None,
                }));
            };
            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            if !(sys_sec <= 0.0 && sys_id < 31_000_000) {
                return Some(Err(BridgeRpcError {
                    code: "EssOnlyInNullSec".into(),
                    message: format!(
                        "ESS Reserve Bank only exists in K-space null-sec systems (system {sys_id} has security {sys_sec:.2})"
                    ),
                    client_error: None,
                }));
            }
            let sys_handle_opt = ctx.systems.get_handle(sys_id);
            let ship_ball_opt = match sys_handle_opt.as_ref() {
                Some(sys_handle) => sys_handle.get_ball_snapshot(ship_id).await.ok().flatten(),
                None => None,
            };
            let (Some(sys_handle), Some((_, ship_ball))) = (sys_handle_opt, ship_ball_opt) else {
                return Some(Err(BridgeRpcError {
                    code: "OutsideEssLinkRange".into(),
                    message: "Active ship is not present in system space near the ESS beacon".into(),
                    client_error: None,
                }));
            };
            let balls = sys_handle.list_balls().await.unwrap_or_default();
            let ess_ball = balls
                .into_iter()
                .find(|b| b.type_id == 32882 || b.type_id == 56002 || b.id == 820_000_000 + sys_id as i64);
            let Some(ess) = ess_ball else {
                return Some(Err(BridgeRpcError {
                    code: "OutsideEssLinkRange".into(),
                    message: "ESS beacon is not present in system space".into(),
                    client_error: None,
                }));
            };
            let dist = ship_ball.position.distance(ess.position);
            if dist > 2_500.0 {
                return Some(Err(BridgeRpcError {
                    code: "OutsideEssLinkRange".into(),
                    message: format!("Ship is {:.1} m from ESS beacon (must be <= 2500 m)", dist),
                    client_error: None,
                }));
            }
            let now_unix = ctx.now_unix();
            let key_item_id_arg = args.first().and_then(|a| a.as_i64());
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let rt_until = snap
                    .combat_crimewatch_state
                    .ess_reserve_unlocked_until
                    .get(&sys_id)
                    .copied()
                    .unwrap_or(0);
                crate::services::pve_ess_reserve_banks::ensure_ess_reserve_bank_not_already_unlocked(
                    snap, sys_id, now_unix, rt_until,
                )?;
                let (key_item_id, key_type_id, duration_mins) =
                    crate::services::encounter_surveillance_system::select_valid_ess_reserve_key_in_cargo(
                        &ctx.sde, snap, char_id, ship_id, sys_id, key_item_id_arg,
                    )?;
                let key_opts = eve_db::ConsumeExactOptions {
                    reject_blueprint_copy: true,
                    active_ship_id: Some(ship_id),
                    ..eve_db::ConsumeExactOptions::for_owner_exact(
                        Some(char_id),
                        char_id,
                        eve_db::ItemClassPolicy::ExactBlueprintOrSingleton,
                    )
                };
                snap.consume_exact_item_quantity(key_item_id, 1, key_opts).map_err(|e| BridgeRpcError {
                    code: "InventoryError".into(),
                    message: e.to_string(),
                    client_error: None,
                })?;
                let pulses = duration_mins as u32;
                let unlock_until = now_unix + (duration_mins as u64) * 60;
                let pulse_st = EssReservePulseState {
                    pulses_total: pulses,
                    pulses_remaining: pulses,
                    last_pulse_sec: None,
                    reserve_linked_character_id: None,
                    reserve_link_started_tick: None,
                    link_started_unix: None,
                    unlocked_until_unix: unlock_until,
                };
                set_ess_reserve_pulses(snap, sys_id, &pulse_st);
                snap.combat_crimewatch_state
                    .ess_reserve_unlocked_until
                    .insert(sys_id, unlock_until);
                Ok((key_type_id, duration_mins, unlock_until))
            });
            match res {
                Ok((key_type_id, duration_mins, unlock_until)) => {
                    let reg_id = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id as i64).unwrap_or(0);
                    let unlock_payload = PyRep::keyval([
                        ("regionID", PyRep::Integer(reg_id)),
                        ("solarSystemID", PyRep::Integer(sys_id as i64)),
                        ("characterID", PyRep::Integer(char_id)),
                        ("unlockUntilUnix", PyRep::Integer(unlock_until as i64)),
                        ("durationMinutes", PyRep::Integer(duration_mins as i64)),
                    ]);
                    for (&s_id, s_info) in &ctx.sde.solar_systems {
                        if s_info.region_id as i64 == reg_id {
                            ctx.emit_typed(
                                crate::router::NotificationTarget::SolarSystem(s_id),
                                OnReserveBankUnlock {
                                    data: unlock_payload.clone(),
                                },
                            );
                        }
                    }
                    Some(Ok(PyRep::keyval([
                        ("solarSystemID", PyRep::Integer(sys_id as i64)),
                        ("unlocked", PyRep::Bool(true)),
                        ("keyTypeID", PyRep::Integer(key_type_id as i64)),
                        ("durationMinutes", PyRep::Integer(duration_mins as i64)),
                        ("unlockedUntil", PyRep::Integer(unlock_until as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("essMgr", "clear_cache") => {
            Some(handle_ess_mgr_clear_cache(ctx, session, args))
        }

        // parity: rpc:essMgr.prime_cache_result
        ("essMgr", "prime_cache_result") => {
            Some(handle_ess_prime_cache_result(ctx, session, args))
        }

        _ => {
            if service == BEYONCE_SVC && method == "CallComponentFromClient" {
                return Some(
                    Box::pin(handle_call_component_from_client(ctx, session, bound_entry, args)).await,
                );
            }
            if let Some(res) = Box::pin(space_warp_jump::handle(
                ctx,
                session,
                bound_entry,
                service,
                method,
                args,
                kwargs,
            ))
            .await
            {
                return Some(res);
            }
            if let Some(res) = Box::pin(space_cyno_filaments::handle(
                ctx,
                session,
                bound_entry,
                service,
                method,
                args,
                kwargs,
            ))
            .await
            {
                return Some(res);
            }
            Box::pin(handle_scan_and_wormhole_extras(
                ctx,
                session,
                bound_entry,
                service,
                method,
                args,
                kwargs,
            ))
            .await
        }
    }
}

pub(crate) async fn handle_update_state_request(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
) -> Result<PyRep, BridgeRpcError> {
    handle_update_state_request_inner(ctx, session, bound_entry, false).await
}

#[allow(clippy::too_many_arguments)]
async fn push_set_state_with_guard_and_bootstrap(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    sys_id: u32,
    observer_ball_id: i64,
    handle: &crate::system_actor::SystemHandle,
    is_explicit_rpc: bool,
    force_bypass_cooldown: bool,
    queue_set_state: bool,
) -> Result<(), BridgeRpcError> {
    let was_bootstrapped = ctx.is_space_ballpark_bootstrapped(session.session_id, char_id, sys_id);
    let current_tick = handle.view().current_tick;
    let guard_key = (session.session_id, char_id);
    let mut should_push_set_state = true;
    if !is_explicit_rpc {
        ctx.systems
            .clear_explicit_update_state_guard(session.session_id, char_id);
    } else {
        let needs_prune = {
            let mut map = ctx
                .systems
                .explicit_update_state_guards
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let force_bypass = !was_bootstrapped || force_bypass_cooldown;
            let in_cooldown = !force_bypass
                && map.get(&guard_key).copied().is_some_and(|prev| {
                    prev.sys_id == sys_id
                        && prev.ship_id == observer_ball_id
                        && current_tick >= prev.last_push_tick
                        && current_tick.saturating_sub(prev.last_push_tick)
                            < UPDATE_STATE_COOLDOWN_TICKS
                });
            if in_cooldown {
                should_push_set_state = false;
                false
            } else {
                let over = map.len() > 4_096;
                map.insert(
                    guard_key,
                    crate::system_actor::ExplicitUpdateStateGuard {
                        sys_id,
                        ship_id: observer_ball_id,
                        last_push_tick: current_tick,
                    },
                );
                over
            }
        };
        if needs_prune {
            // Query live per-system simulation ticks outside the leaf lock so each guard expires
            // strictly in its own solar system's tick domain (never global/wall time, #1196).
            let active_ticks = ctx.systems.active_system_ticks();
            let mut map = ctx
                .systems
                .explicit_update_state_guards
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if map.len() > 4_096 {
                map.retain(|_, v| {
                    active_ticks.get(&v.sys_id).is_some_and(|&sys_tick| {
                        sys_tick >= v.last_push_tick
                            && sys_tick.saturating_sub(v.last_push_tick) < 600
                    })
                });
            }
        }
    }
    if should_push_set_state {
        if queue_set_state {
            handle
                .push_set_state(observer_ball_id)
                .await
                .map_err(|e| BridgeRpcError {
                    code: "DestinyQueryFailed".into(),
                    message: e,
                    client_error: None,
                })?;
            ctx.clear_pending_structure_view_outside_set_state(session.session_id, char_id);
        } else {
            ctx.set_pending_structure_view_outside_set_state(
                session.session_id,
                char_id,
                observer_ball_id,
            );
        }
    } else if !queue_set_state {
        ctx.clear_pending_structure_view_outside_set_state(session.session_id, char_id);
    }
    ctx.mark_space_ballpark_bootstrapped(session.session_id, char_id, sys_id);
    crate::services::pve_incursion_lifecycle::emit_active_incursion_tale_data_for_system(
        ctx, sys_id,
    );
    Ok(())
}

async fn handle_update_state_request_inner(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    is_explicit_rpc: bool,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = match bound_entry {
        Some(_) => {
            let bound_sys = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
            if session.solar_system_id != Some(bound_sys) {
                return Err(BridgeRpcError {
                    code: "BoundObjectOwnershipViolation".into(),
                    message: format!(
                        "Bound beyonce solar system {bound_sys} does not match session solar system {:?}",
                        session.solar_system_id
                    ),
                    client_error: None,
                });
            }
            bound_sys
        }
        None => session.solar_system_id.ok_or_else(|| BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
            client_error: None,
        })?,
    };
    if session.station_id.is_some() {
        return Ok(PyRep::None);
    }
    if let Some(structure_id) = session.structure_id {
        // Upwell "View Outside" binds beyonce while docked (`structure_id.is_some()`):
        // observe the structure exterior ball itself and never spawn the pilot's docked ship ball.
        let is_valid_docked_structure = ctx.db.with_snapshot(|snap| {
            let Some(ch) = snap.characters.get(&char_id) else {
                return false;
            };
            if ch.station_id.is_some()
                || ch.structure_id != Some(structure_id)
                || ch.solar_system_id != sys_id
            {
                return false;
            }
            snap.structures.get(&structure_id).is_some_and(|st| {
                st.solar_system_id == sys_id && !st.state.eq_ignore_ascii_case("Destroyed")
            })
        });
        if !is_valid_docked_structure {
            return Ok(PyRep::None);
        }
        let handle = ctx.systems.get_or_spawn(sys_id);
        let had_ball = handle
            .get_ball_snapshot(structure_id)
            .await
            .map_err(|e| BridgeRpcError {
                code: "DestinyQueryFailed".into(),
                message: e,
                client_error: None,
            })?
            .is_some();
        if !had_ball {
            crate::services::station_structure::structure_lifecycle_rpc::ensure_structure_ball_in_space(
                ctx,
                sys_id,
                structure_id,
            )
            .await;
        }
        let has_ball = had_ball
            || handle
                .get_ball_snapshot(structure_id)
                .await
                .map_err(|e| BridgeRpcError {
                    code: "DestinyQueryFailed".into(),
                    message: e,
                    client_error: None,
                })?
                .is_some();
        if !has_ball {
            return Ok(PyRep::None);
        }
        crate::services::mobile_deployables_ext::sync_system_deployables(ctx, sys_id).await;
        crate::services::structure_weapons_ops::trigger_system_structures_auto_defense(ctx, sys_id)
            .await;
        push_set_state_with_guard_and_bootstrap(
            ctx,
            session,
            char_id,
            sys_id,
            structure_id,
            &handle,
            is_explicit_rpc,
            !had_ball,
            false,
        )
        .await?;
        return Ok(PyRep::None);
    }
    let Some(ship_id) = session.ship_id else {
        return Ok(PyRep::None);
    };
    // #1081 Req 3: Emit DoSimClockRebase once on space entry (login/undock/jump) into a lagged
    // or dilated system; never on beyonce bind and never repeatedly on normal ticks.
    let _ = crate::services::serverops_cluster::emit_sim_clock_rebase_on_entry_if_needed(
        ctx, char_id, sys_id,
    );
    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let _ = crate::services::pve_dungeon_runtime::on_dungeon_approach(
        ctx, char_id, sys_id, ship_id, None,
    )
    .await;
    let handle = ctx.systems.get_or_spawn(sys_id);
    let mut spawned_new_ball = false;
    let ship_ball = match handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
            client_error: None,
        })? {
        Some((_, b)) => b,
        None => {
            let Some(ship_item) = ctx.db.with_snapshot(|snap| {
                let ch = snap.characters.get(&char_id)?;
                if ch.station_id.is_some()
                    || ch.structure_id.is_some()
                    || ch.solar_system_id != sys_id
                    || ch.active_ship_id != ship_id
                {
                    return None;
                }
                let item = snap.items.get(&ship_id)?;
                if item.location_id != sys_id as i64 {
                    return None;
                }
                Some(item.clone())
            }) else {
                return Ok(PyRep::None);
            };
            spawned_new_ball = true;
            let ship_type = ctx.sde.types.get(&ship_item.type_id);
            let fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
            let max_vel = fit_opt
                .as_ref()
                .map(|f| f.ship.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &ctx.sde))
                .filter(|&v| v > 0.0)
                .or_else(|| {
                    ship_type
                        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::MAX_VELOCITY))
                        .copied()
                })
                .unwrap_or(100.0);
            let inertia = fit_opt
                .as_ref()
                .map(|f| f.ship.get_attribute(eve_sde::attr_ids::AGILITY, &ctx.sde))
                .filter(|&v| v > 0.0)
                .or_else(|| {
                    ship_type
                        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::AGILITY))
                        .copied()
                })
                .unwrap_or(1.0);
            let mass = fit_opt
                .as_ref()
                .map(|f| f.ship.get_attribute(eve_sde::attr_ids::MASS, &ctx.sde))
                .filter(|&v| v > 0.0)
                .or_else(|| ship_type.map(|t| t.mass))
                .unwrap_or(1_000_000.0);
            let radius = ship_type.map(|t| t.radius).unwrap_or(40.0);
            let spawn_pos = ctx
                .db
                .with_snapshot(|snap| snap.item_positions.get(&ship_id).copied())
                .map(|p| DVec3::new(p[0], p[1], p[2]))
                .or_else(|| {
                    ctx.sde
                        .solar_systems
                        .get(&sys_id)
                        .and_then(|s| s.stations.first())
                        .map(|st| st.position + DVec3::new(1_000.0, 0.0, 0.0))
                })
                .unwrap_or_else(|| DVec3::new(100_000.0, 0.0, 0.0));
            ctx.db.with_mut_snapshot(|snap| {
                snap.item_positions.insert(ship_id, [spawn_pos.x, spawn_pos.y, spawn_pos.z]);
            });
            let ball = eve_destiny::Ball {
                id: ship_id,
                type_id: ship_item.type_id,
                position: spawn_pos,
                velocity: DVec3::ZERO,
                max_velocity: max_vel,
                inertia,
                mass,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            };
            let combat_state = ctx.build_ship_combat_state(char_id, ship_id, ship_item.type_id);
            let slim_meta = ctx.build_ship_slim_meta(char_id, ship_item.type_id);
            let _ = handle
                .spawn_ball_with_meta(ball.clone(), combat_state, slim_meta)
                .await;
            ball
        }
    };
    let mut did_warp_finish = false;
    if matches!(ship_ball.mode, MovementMode::Stop) {
        if handle.consume_warp_finished(ship_id) {
            did_warp_finish = true;
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                OnWarpFinished {
                    item_id: ship_id,
                    x_or_type_id: PyRep::Real(ship_ball.position.x),
                    y: Some(ship_ball.position.y),
                    z: Some(ship_ball.position.z),
                },
            );
            crate::services::pve_dungeon_runtime::on_warp_landed_at_dungeon_pocket_if_present(
                ctx,
                char_id,
                sys_id,
                ship_ball.position.to_array(),
            )
            .await;
        }
    }
    let mut forced_stop_warp = false;
    if matches!(ship_ball.mode, MovementMode::Warp { .. }) {
        let provenance = handle.warp_provenance(ship_id);
        let is_emergency_combat_state = handle
            .view()
            .combat_state(ship_id)
            .is_some_and(|cs| cs.is_emergency_warping);
        let in_active_warps = handle.is_active_warp(ship_id);
        let is_client_or_fleet_active_warp = in_active_warps
            || (!is_emergency_combat_state
                && provenance == Some(crate::system_actor::WarpProvenance::PlayerOrFleetWarp));
        if is_client_or_fleet_active_warp {
            return Ok(PyRep::None);
        }
        let is_explicit_emergency = is_emergency_combat_state
            || provenance == Some(crate::system_actor::WarpProvenance::DisconnectedEmergencyWarp);
        let in_return_warps = handle.is_return_warp(ship_id);
        let is_reconnect_return_warp = !is_explicit_emergency
            && (in_return_warps
                || provenance == Some(crate::system_actor::WarpProvenance::ReconnectReturnWarp));
        if !is_reconnect_return_warp {
            // Untracked/orphan warp or SP-6 outbound emergency warp has neither a valid client/fleet
            // warp nor an active SP-6 reconnect return-warp; stop at current coordinates before `SetState`.
            let _ = handle.force_stop_warp(ship_id);
            clear_warp_tracking(&handle, ship_id);
            forced_stop_warp = true;
        }
    }
    if !ctx.db.with_snapshot(|s| s.dungeon_runtime.instances.is_empty()) {
        let _ = crate::services::pve_dungeon_runtime::check_dungeon_triggers(ctx, char_id, sys_id, ship_id).await;
    }
    crate::services::mobile_deployables_ext::sync_system_deployables(ctx, sys_id).await;
    crate::services::structure_weapons_ops::trigger_system_structures_auto_defense(ctx, sys_id).await;
    push_set_state_with_guard_and_bootstrap(
        ctx,
        session,
        char_id,
        sys_id,
        ship_id,
        &handle,
        is_explicit_rpc,
        spawned_new_ball || forced_stop_warp || did_warp_finish,
        true,
    )
    .await?;
    Ok(PyRep::None)
}

/// Records a player or fleet warp in the owning system actor while clearing any stale
/// reconnect-return or deferred-landing markers (`#1175`, `#1541`).
pub(crate) fn track_player_or_fleet_warp(
    sys_handle: &crate::system_actor::SystemHandle,
    ship_id: i64,
    char_id: i64,
) {
    sys_handle.track_player_or_fleet_warp(ship_id, char_id);
}

/// Clears all active, reconnect-return, deferred-finish, and actor warp provenance tracking for
/// `ship_id` (`#1175`, `#1541`).
pub(crate) fn clear_warp_tracking(
    sys_handle: &crate::system_actor::SystemHandle,
    ship_id: i64,
) {
    sys_handle.clear_warp_tracking(ship_id);
}

async fn handle_admin_get_bubble_id(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    if !session.has_gm_role() {
        return Err(BridgeRpcError::insufficient_role_with_code(
            "InsufficientRoles",
            eve_machonet::service_roles::ROLE_GM_MASK,
            session.role,
            "AdminGetBubbleID",
            "AdminGetBubbleID requires a GM or Admin role",
        ));
    }
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
            client_error: None,
        });
    };
    let target_ball_id = args
        .first()
        .and_then(|a| a.as_i64())
        .unwrap_or(ship_id);
    let handle_opt = ctx.systems.get_handle(sys_id);
    if target_ball_id != ship_id {
        let visible = match handle_opt.as_ref() {
            Some(handle) => handle.list_balls_in_bubble(ship_id).await.unwrap_or_default(),
            None => Vec::new(),
        };
        if !visible.iter().any(|b| b.id == target_ball_id) {
            return Err(BridgeRpcError {
                code: "BallNotVisible".into(),
                message: format!(
                    "Ball {target_ball_id} is not visible in caller ship {ship_id}'s grid bubble"
                ),
                client_error: None,
            });
        }
    }
    let bubble_res = match handle_opt {
        Some(handle) => handle.get_bubble_id_for_ball(target_ball_id).await,
        None => Ok(None),
    };
    match bubble_res {
        Ok(Some(bid)) => Ok(PyRep::Integer(bid as i64)),
        Ok(None) => Err(BridgeRpcError {
            code: "BallNotFound".into(),
            message: format!("Ball {target_ball_id} is not present in system {sys_id}"),
            client_error: None,
        }),
        Err(e) => Err(BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
            client_error: None,
        }),
    }
}

// parity: feat:wallet.item-trader
async fn handle_call_component_from_client(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = bound_entry
        .and_then(|b| u32::try_from(b.object_id).ok())
        .or(session.solar_system_id)
        .ok_or_else(|| BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
            client_error: None,
        })?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Must be in space to interact with a space component".into(),
            client_error: None,
        });
    }
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Must have an active ship to interact with an item trader".into(),
        client_error: None,
    })?;
    let trader_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| BridgeRpcError {
        code: "InvalidArgument".into(),
        message: "CallComponentFromClient requires target itemID".into(),
        client_error: None,
    })?;
    let comp_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
    let comp_method = args.get(2).and_then(|a| a.as_str()).unwrap_or("");
    if comp_name == "microJumpDriver"
        && matches!(comp_method, "ActivateMicroJumpDrive" | "StartMicroJumpDriveForShip" | "")
    {
        return crate::services::mobile_deployables_ext::handle_mmju_activate(
            ctx, session, char_id, ship_id, sys_id, trader_id,
        )
        .await;
    }
    if comp_name == "crabBeacon"
        || (comp_name == "linkWithShip"
            && crate::services::pve_crab_beacons::is_crab_beacon_item(ctx, trader_id))
    {
        return crate::services::pve_crab_beacons::handle_crab_component_call(
            ctx, session, sys_id, char_id, ship_id, trader_id, comp_name, comp_method, args,
        )
        .await;
    }
    if matches!(comp_name, "linkWithShip" | "orbitalSkyhook") {
        return crate::services::sov_gameplay::handle_skyhook_component_call(
            ctx, session, sys_id, char_id, ship_id, trader_id, comp_name, comp_method, args,
        )
        .await;
    }
    if comp_name == "mercenaryDen" {
        return crate::services::sov_gameplay::handle_mercden_component_call(
            ctx, session, sys_id, char_id, ship_id, trader_id, comp_name, comp_method, args,
        )
        .await;
    }
    if comp_name == "underConstruction" && comp_method == "DepositItemsFromShip" {
        return crate::services::shipcaster_ops::deposit_items_from_ship(ctx, session, trader_id, args.get(3)).await;
    }
    if comp_name != "itemTrader" || comp_method != "ProcessTrade" {
        return Err(BridgeRpcError {
            code: "UnsupportedComponentCall".into(),
            message: format!("Unsupported component call {comp_name}.{comp_method}"),
            client_error: None,
        });
    }
    let recipe_id = args
        .get(3)
        .and_then(|a| a.as_i64())
        .filter(|&r| r > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidRecipeID".into(),
            message: "ProcessTrade requires a positive recipeID".into(),
            client_error: None,
        })? as u32;
    let raw_mult = args.get(4).and_then(|a| a.as_i64()).unwrap_or(1);
    let multiplier = u32::try_from(raw_mult)
        .ok()
        .filter(|&m| (1..=100_000).contains(&m))
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidMultiplier".into(),
            message: "ProcessTrade multiplier must be between 1 and 100,000".into(),
            client_error: None,
        })? as i32;

    let handle_opt = ctx.systems.get_handle(sys_id);
    let (ship_ball, trader_ball) = if let Some(ref handle) = handle_opt {
        (
            handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b),
            handle
                .get_ball_snapshot(trader_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b),
        )
    } else {
        (None, None)
    };
    let (trader_type_id, trader_pos, trader_radius) = if let Some(tb) = trader_ball {
        (tb.type_id, Some(tb.position), tb.radius)
    } else {
        let item_opt = ctx.db.with_snapshot(|snap| {
            let tid = snap
                .items
                .get(&trader_id)
                .filter(|it| it.location_id == i64::from(sys_id))
                .map(|it| it.type_id)?;
            let pos = snap
                .item_positions
                .get(&trader_id)
                .copied()
                .map(glam::DVec3::from_array);
            Some((tid, pos))
        });
        let Some((tid, pos)) = item_opt else {
            return Err(BridgeRpcError {
                code: "ItemTraderNotFoundInSystem".into(),
                message: format!("Item trader {trader_id} is not present in system {sys_id}"),
                client_error: None,
            });
        };
        let rad = ctx.sde.types.get(&tid).map(|t| t.radius).unwrap_or(500.0);
        (tid, pos, rad)
    };

    let (allowed_recipes, range_m) = ctx
        .sde
        .item_trader_component_for_type(trader_type_id)
        .map_err(|e| BridgeRpcError {
            code: "SdeError".into(),
            message: e.to_string(),
            client_error: None,
        })?
        .ok_or_else(|| BridgeRpcError {
            code: "NotAnItemTrader".into(),
            message: format!("Type {trader_type_id} does not have an itemTrader component"),
            client_error: None,
        })?;
    let (Some(sb), Some(tpos)) = (&ship_ball, trader_pos) else {
        return Err(BridgeRpcError {
            code: "ItemTraderOutOfRange".into(),
            message: format!("Ship or item trader {trader_id} does not have a valid position in system {sys_id}"),
            client_error: None,
        });
    };
    let surface_dist = (sb.position.distance(tpos) - sb.radius - trader_radius).max(0.0);
    if surface_dist > range_m {
        return Err(BridgeRpcError {
            code: "ItemTraderOutOfRange".into(),
            message: format!(
                "Ship is {surface_dist:.0} m from item trader {trader_id} (max {range_m:.0} m)"
            ),
            client_error: None,
        });
    }
    if !allowed_recipes.contains(&recipe_id) {
        return Err(BridgeRpcError {
            code: "RecipeNotAvailableAtTrader".into(),
            message: format!(
                "Recipe {recipe_id} is not offered by item trader type {trader_type_id}"
            ),
            client_error: None,
        });
    }
    crate::services::insurgency_complex_ops::check_insurgency_laundering_access(
        ctx,
        char_id,
        sys_id,
        trader_type_id,
    )?;

    let recipes_table = ctx
        .sde
        .load_cfsd_nested("res:/staticdata/itemtraderrecipes.fsdbinary")
        .map_err(|e| BridgeRpcError {
            code: "SdeError".into(),
            message: e.to_string(),
            client_error: None,
        })?;
    let recipe_val = recipes_table
        .get(&recipe_id.to_string())
        .ok_or_else(|| BridgeRpcError {
            code: "RecipeNotFound".into(),
            message: format!("Item trader recipe {recipe_id} not found in SDE"),
            client_error: None,
        })?;
    let input_isk = recipe_val
        .get("inputIsk")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        * f64::from(multiplier);
    let mut req_inputs: Vec<(u32, i32)> = Vec::new();
    if let Some(in_obj) = recipe_val.get("inputItems").and_then(|v| v.as_object()) {
        for (k, v) in in_obj {
            if let (Ok(tid), Some(qty)) = (k.parse::<u32>(), v.as_i64()) {
                let q = i32::try_from(qty)
                    .ok()
                    .filter(|&q| q > 0)
                    .and_then(|q| q.checked_mul(multiplier))
                    .ok_or_else(|| BridgeRpcError {
                        code: "InvalidMultiplier".into(),
                        message: "Item trader input quantity overflow".into(),
                        client_error: None,
                    })?;
                req_inputs.push((tid, q));
            }
        }
    }
    let mut out_items: Vec<(u32, i32)> = Vec::new();
    if let Some(out_obj) = recipe_val.get("outputItems").and_then(|v| v.as_object()) {
        for (k, v) in out_obj {
            if let (Ok(tid), Some(qty)) = (k.parse::<u32>(), v.as_i64()) {
                let q = i32::try_from(qty)
                    .ok()
                    .filter(|&q| q > 0)
                    .and_then(|q| q.checked_mul(multiplier))
                    .ok_or_else(|| BridgeRpcError {
                        code: "InvalidMultiplier".into(),
                        message: "Item trader output quantity overflow".into(),
                        client_error: None,
                    })?;
                out_items.push((tid, q));
            }
        }
    }

    let eval_cargo_cap = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .ok()
        .map(|f| f.evaluated_context(&ctx.sde).ship.get_attribute(38, &ctx.sde))
        .filter(|&c| c > 0.0);
    let cost_cents = eve_db::IskCents::from_f64_isk(input_isk);
    let trade_rep = ctx.db.try_with_mut_snapshot(|snap| {
        if cost_cents.0 > 0 {
            let bal = snap
                .characters
                .get(&char_id)
                .map(|c| c.balance)
                .unwrap_or(eve_db::IskCents(0));
            if bal < cost_cents {
                return Err(BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: format!("Item trader recipe {recipe_id} requires {input_isk:.2} ISK"),
                    client_error: None,
                });
            }
        }
        for &(in_tid, req_qty) in &req_inputs {
            let have_qty: i32 = snap
                .items
                .values()
                .filter(|it| {
                    it.owner_id == char_id
                        && it.location_id == ship_id
                        && it.flag == eve_db::InventoryFlag::Cargo
                        && it.type_id == in_tid
                        && !snap.is_item_locked_in_contract_or_job(it.item_id)
                })
                .map(|it| it.quantity)
                .sum();
            if have_qty < req_qty {
                return Err(BridgeRpcError {
                    code: "InsufficientInputItems".into(),
                    message: format!(
                        "Recipe {recipe_id} requires {req_qty}x type {in_tid} in ship cargo (have {have_qty})"
                    ),
                    client_error: None,
                });
            }
        }
        if let Some(ship_item) = snap.items.get(&ship_id) {
            if let Some(st) = ctx.sde.types.get(&ship_item.type_id) {
                let cargo_cap =
                    crate::services::fitting_saved_apply::get_ship_hold_capacity(5, st, eval_cargo_cap);
                if cargo_cap > 0.0
                {
                    let used_vol: f64 = snap
                        .items
                        .values()
                        .filter(|it| it.location_id == ship_id && it.flag == eve_db::InventoryFlag::Cargo)
                        .map(|it| {
                            ctx.sde
                                .types
                                .get(&it.type_id)
                                .map(|t| t.volume.max(0.0))
                                .unwrap_or(1.0)
                                * f64::from(it.quantity.max(1))
                        })
                        .sum();
                    let in_vol: f64 = req_inputs
                        .iter()
                        .map(|&(tid, q)| {
                            ctx.sde
                                .types
                                .get(&tid)
                                .map(|t| t.volume.max(0.0))
                                .unwrap_or(1.0)
                                * f64::from(q)
                        })
                        .sum();
                    let out_vol: f64 = out_items
                        .iter()
                        .map(|&(tid, q)| {
                            ctx.sde
                                .types
                                .get(&tid)
                                .map(|t| t.volume.max(0.0))
                                .unwrap_or(1.0)
                                * f64::from(q)
                        })
                        .sum();
                    let projected_vol = (used_vol - in_vol).max(0.0) + out_vol;
                    if projected_vol > cargo_cap + 1e-4 {
                        let available_vol = (cargo_cap - (used_vol - in_vol).max(0.0)).max(0.0);
                        return Err(BridgeRpcError::with_client_error(
                            "NotEnoughCargoSpace",
                            format!(
                                "Item trader output volume ({projected_vol:.1} m3) exceeds ship cargo capacity ({cargo_cap:.1} m3)"
                            ),
                            eve_proto::ClientError::hold_capacity_error(
                                eve_proto::UserErrorKey::NotEnoughCargoSpace,
                                out_vol,
                                available_vol,
                            ),
                        ));
                    }
                }
            }
        }
        if cost_cents.0 > 0 {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.balance = ch.balance.checked_sub(cost_cents).unwrap_or(eve_db::IskCents(0));
                let bal_after = ch.balance;
                snap.append_journal(
                    char_id,
                    "ItemTraderPayment",
                    cost_cents,
                    bal_after,
                    format!("NPC item trader exchange for recipe {recipe_id} (x{multiplier})"),
                );
            }
        }
        for &(in_tid, mut rem) in &req_inputs {
            let mut stack_ids: Vec<i64> = snap
                .items
                .values()
                .filter(|it| {
                    it.owner_id == char_id
                        && it.location_id == ship_id
                        && it.flag == eve_db::InventoryFlag::Cargo
                        && it.type_id == in_tid
                        && !snap.is_item_locked_in_contract_or_job(it.item_id)
                })
                .map(|it| it.item_id)
                .collect();
            stack_ids.sort_unstable();
            for iid in stack_ids {
                if rem <= 0 {
                    break;
                }
                if let Some(it) = snap.items.get_mut(&iid) {
                    if it.quantity <= rem {
                        rem -= it.quantity;
                        snap.items.remove(&iid);
                    } else {
                        it.quantity -= rem;
                        it.version += 1;
                        rem = 0;
                    }
                }
            }
        }
        let mut delivered_ids = Vec::with_capacity(out_items.len());
        for (out_tid, out_qty) in out_items {
            snap.next_entity_id += 1;
            let nid = snap.next_entity_id;
            snap.items.insert(
                nid,
                eve_db::InventoryItemRecord {
                    item_id: nid,
                    type_id: out_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: eve_db::InventoryFlag::Cargo,
                    quantity: out_qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
            delivered_ids.push(PyRep::Integer(nid));
        }
        Ok(PyRep::Tuple(vec![
            PyRep::Bool(true),
            PyRep::List(delivered_ids),
        ]))
    })?;
    crate::services::insurgency_complex_ops::on_insurgency_laundering_trade_completed(
        ctx,
        char_id,
        sys_id,
        trader_type_id,
        multiplier,
    );
    Ok(trade_rep)
}

use crate::router::rpc_registry::{
    rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry, RpcSceneClass,
};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle(ctx, session, bound_entry, service, method, args, kwargs))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("beyonce", "CmdGotoDirection", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("beyonce", "CmdSteerDirection", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("beyonce", "CmdGotoPoint", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("jumpTimers", "GetTimers", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("beyonce", "CmdStop", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("beyonce", "CmdSetSpeedFraction", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("beyonce", "CmdOrbit", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("beyonce", "CmdFollowBall", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("beyonce", "CmdAlignTo", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("beyonce", "UpdateStateRequest", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("beyonce", "CallComponentFromClient", RpcDispatchHandler::BoundAsync(dispatch_rpc)).with_scene(RpcSceneClass::InSpace),
    rpc_entry("beyonce", "AdminGetBubbleID", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("essMgr", "GetDataForClientSolarSystem", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("essMgr", "GetMainBankTheftsForClientSolarSystem", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("essMgr", "GetReserveBankTheftsForClientSolarSystem", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("essMgr", "IsClientLinkedToReserveBank", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("essMgr", "RequestMainBankUnlink", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("essMgr", "RequestReserveBankUnlink", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("essMgr", "RequestUnlockReserveBank", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("essMgr", "clear_cache", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("essMgr", "prime_cache_result", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
];

#[cfg(test)]
#[path = "space_destiny_tests.rs"]
mod space_destiny_tests;

