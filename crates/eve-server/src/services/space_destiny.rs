//! `space_destiny` — Domain 5: Solar System `Ballpark`, Subwarp Navigation, & Space Services Router (`§3.5`).

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EssReservePulseState {
    pub pulses_total: u32,
    pub pulses_remaining: u32,
    pub last_pulse_sec: Option<u64>,
    #[serde(default)]
    pub reserve_linked_character_id: Option<i64>,
    #[serde(default)]
    pub reserve_link_started_tick: Option<u64>,
}

pub fn get_ess_reserve_pulses_from_blobs(blobs: &HashMap<String, String>, sys_id: u32) -> EssReservePulseState {
    blobs.get(&format!("ess_reserve_pulses:{sys_id}")).and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default()
}

pub fn set_ess_reserve_pulses_in_blobs(blobs: &mut HashMap<String, String>, sys_id: u32, st: &EssReservePulseState) {
    if let Ok(j) = serde_json::to_string(st) {
        blobs.insert(format!("ess_reserve_pulses:{sys_id}"), j);
    }
}

pub fn get_ess_reserve_pulses(snap: &eve_db::WorldStateSnapshot, sys_id: u32) -> EssReservePulseState {
    get_ess_reserve_pulses_from_blobs(&snap.service_state_blobs, sys_id)
}

pub fn set_ess_reserve_pulses(snap: &mut eve_db::WorldStateSnapshot, sys_id: u32, st: &EssReservePulseState) {
    set_ess_reserve_pulses_in_blobs(&mut snap.service_state_blobs, sys_id, st);
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

#[derive(Debug, Clone)]
pub struct SessionChangeTimerState {
    pub expires_unix: u64,
    pub system_id: u32,
    pub started_tick: u64,
    pub reason: String,
    pub location_change_invul_until: u64,
}

pub fn check_session_change_timer_active(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
    current_tick_opt: Option<(u32, u64)>,
) -> Option<SessionChangeTimerState> {
    let map = ctx
        .session_change_timers
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let timer = map.get(&char_id)?;
    if now_unix >= timer.expires_unix {
        return None;
    }
    if let Some((sys_id, cur_tick)) = current_tick_opt {
        if timer.started_tick > 0 && sys_id == timer.system_id && cur_tick >= timer.started_tick.saturating_add(10) {
            return None;
        }
    }
    Some(timer.clone())
}

pub fn grant_location_change_invulnerability(ctx: &ServerContext, char_id: i64, until_unix: u64) {
    ctx.location_change_invuln
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(char_id, until_unix);
}

pub fn break_location_change_invulnerability(ctx: &ServerContext, char_id: i64) {
    ctx.location_change_invuln
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&char_id);
}

pub fn is_location_change_invulnerable(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
) -> bool {
    let map = ctx
        .location_change_invuln
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.get(&char_id)
        .map(|&until| now_unix < until)
        .unwrap_or(false)
}

pub fn get_session_change_timer(
    ctx: &ServerContext,
    char_id: i64,
) -> Option<SessionChangeTimerState> {
    let map = ctx
        .session_change_timers
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    map.get(&char_id).cloned()
}

pub fn clear_session_change_timer(ctx: &ServerContext, char_id: i64) {
    ctx.session_change_timers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&char_id);
    break_location_change_invulnerability(ctx, char_id);
}

pub fn record_session_change_timer(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
    system_id: u32,
    started_tick: u64,
    reason: &str,
) {
    record_session_change_timer_with_invuln(
        ctx,
        char_id,
        now_unix,
        system_id,
        started_tick,
        reason,
        true,
    );
}

pub fn record_client_session_check_timer(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
    system_id: u32,
    started_tick: u64,
    reason: &str,
) {
    record_session_change_timer_with_invuln(
        ctx,
        char_id,
        now_unix,
        system_id,
        started_tick,
        reason,
        false,
    );
}

fn record_session_change_timer_with_invuln(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
    system_id: u32,
    started_tick: u64,
    reason: &str,
    server_initiated: bool,
) {
    let expires_unix = now_unix.saturating_add(10);
    let location_change_invul_until = if server_initiated && reason == "jump_drive" {
        let until = now_unix.saturating_add(60);
        grant_location_change_invulnerability(ctx, char_id, until);
        until
    } else {
        ctx.location_change_invuln
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&char_id)
            .copied()
            .unwrap_or(0)
    };
    ctx.session_change_timers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            char_id,
            SessionChangeTimerState {
                expires_unix,
                system_id,
                started_tick,
                reason: reason.to_string(),
                location_change_invul_until,
            },
        );
    let (change_dict, sess_rep, has_entries) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id);
        let station_id = ch.and_then(|c| c.station_id);
        let structure_id = ch.and_then(|c| c.structure_id);
        let ship_id = ch.map(|c| c.active_ship_id);
        let user_id = ch.map(|c| c.user_id as i64);
        let corp_id = ch.map(|c| c.corporation_id);
        let alliance_id = ch.and_then(|c| c.alliance_id);
        let mut entries = vec![];
        if system_id != 0 && !matches!(reason, "set_account_key" | "corp_change" | "ship_loss") {
            entries.push((
                PyRep::String("solarsystemid2".into()),
                PyRep::Tuple(vec![PyRep::None, PyRep::Integer(i64::from(system_id))]),
            ));
        }
        if station_id.is_some() || reason == "dock" || reason == "undock" {
            entries.push((
                PyRep::String("stationid".into()),
                PyRep::Tuple(vec![PyRep::None, station_id.map(PyRep::Integer).unwrap_or(PyRep::None)]),
            ));
            entries.push((
                PyRep::String("solarsystemid".into()),
                PyRep::Tuple(vec![
                    station_id.map(|_| PyRep::Integer(i64::from(system_id))).unwrap_or(PyRep::None),
                    if station_id.is_some() || structure_id.is_some() { PyRep::None } else { PyRep::Integer(i64::from(system_id)) },
                ]),
            ));
        }
        if let Some(st_id) = structure_id {
            entries.push((PyRep::String("structureid".into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(st_id)])));
        }
        if let Some(sid) = ship_id {
            entries.push((PyRep::String("shipid".into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(sid)])));
        }
        let in_space = station_id.is_none() && structure_id.is_none();
        let sess = PyRep::keyval([
            ("charid", PyRep::Integer(char_id)),
            ("userid", user_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("corpid", corp_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("allianceid", alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("stationid", station_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("structureid", structure_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("solarsystemid2", PyRep::Integer(i64::from(system_id))),
            ("solarsystemid", if in_space { PyRep::Integer(i64::from(system_id)) } else { PyRep::None }),
            ("shipid", ship_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ("nextSessionChange", PyRep::Integer(expires_unix as i64)),
        ]);
        let has = !entries.is_empty();
        (PyRep::Dict(entries), sess, has)
    });
    if reason == "podding" {
        let mut removed_oids = Vec::new();
        ctx.bound_objects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|&bid, b| {
                if b.character_id == char_id && (b.service == "beyonce" || b.service == "scanMgr") {
                    removed_oids.push(bid);
                    false
                } else {
                    true
                }
            });
        for bid in removed_oids {
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnMachoObjectDisconnect",
                "clientID",
                vec![
                    PyRep::String(format!("N=1:{bid}")),
                    PyRep::Integer(char_id),
                    PyRep::Integer(bid as i64),
                ],
            );
        }
    }
    if reason == "board_or_eject" || !has_entries {
        return;
    }
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnSessionChanged",
        "charid",
        vec![
            PyRep::Bool(false),
            sess_rep,
            change_dict,
        ],
    );
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

pub(crate) use space_nav_travel::sync_ship_cloak_and_mjfg_from_fitting;

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
                | "CmdWarpToStuff"
        )
    {
        if let Some(cid) = session.character_id {
            if method != "CmdStop" {
                break_location_change_invulnerability(ctx, cid);
            }
            if method != "CmdStop" && method != "CmdWarpToStuff" {
                if let Some(sys_id) = session.solar_system_id {
                    if let Err(e) = crate::services::tourney_service::check_tourney_move_restricted(ctx, sys_id, cid) {
                        return Some(Err(e));
                    }
                }
            }
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
        ("beyonce", "CmdGotoPoint") => Some(
            space_nav_travel::handle_cmd_goto_point(ctx, session, bound_entry, args).await,
        ),
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
            Some(handle_update_state_request(ctx, session, bound_entry).await)
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
            let rep = ctx.db.with_snapshot(|snap| {
                let Some(st) = snap.system_states.get(&sys_id) else {
                    return PyRep::None;
                };
                let pulse_st = get_ess_reserve_pulses(snap, sys_id);
                PyRep::Dict(vec![
                    (
                        PyRep::String("essID".into()),
                        PyRep::Integer(820_000_000 + sys_id as i64),
                    ),
                    (
                        PyRep::String("mainValue".into()),
                        PyRep::Real(st.ess_main_bank.to_wire_f64()),
                    ),
                    (
                        PyRep::String("reserveValue".into()),
                        PyRep::Real(st.ess_reserve_bank.to_wire_f64()),
                    ),
                    (PyRep::String("mainBankLink".into()), PyRep::None),
                    (
                        PyRep::String("reserveBankLastPulseInitiated".into()),
                        pulse_st.last_pulse_sec
                            .map(|s| PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(s)))
                            .unwrap_or(PyRep::None),
                    ),
                    (
                        PyRep::String("reserveBankPulsesRemaining".into()),
                        PyRep::Integer(pulse_st.pulses_remaining as i64),
                    ),
                    (
                        PyRep::String("reserveBankPulsesTotal".into()),
                        PyRep::Integer(pulse_st.pulses_total as i64),
                    ),
                    (
                        PyRep::String("reserveBankActiveLinks".into()),
                        PyRep::Integer(0),
                    ),
                ])
            });
            Some(Ok(rep))
        }

        ("essMgr", "GetMainBankTheftsForClientSolarSystem") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Ok(PyRep::List(Vec::new())));
            };
            let thefts = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.ess_main_bank_thefts
                    .get(&sys_id)
                    .cloned()
                    .unwrap_or_default()
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
            Some(Ok(PyRep::List(rows)))
        }

        ("essMgr", "GetReserveBankTheftsForClientSolarSystem") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Ok(PyRep::List(Vec::new())));
            };
            let thefts = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.ess_reserve_bank_thefts
                    .get(&sys_id)
                    .cloned()
                    .unwrap_or_default()
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
            Some(Ok(PyRep::List(rows)))
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
                let mut pulse_st =
                    get_ess_reserve_pulses_from_blobs(&snap.service_state_blobs, sys_id);
                if is_reserve {
                    let was_reserve_linked =
                        pulse_st.reserve_linked_character_id == Some(char_id);
                    if was_reserve_linked {
                        pulse_st.reserve_linked_character_id = None;
                        pulse_st.reserve_link_started_tick = None;
                        set_ess_reserve_pulses_in_blobs(
                            &mut snap.service_state_blobs,
                            sys_id,
                            &pulse_st,
                        );
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
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
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
                }));
            }
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
                return Some(Err(BridgeRpcError {
                    code: "OutsideEssLinkRange".into(),
                    message: "Active ship is not present in system space near the ESS beacon".into(),
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
                }));
            };
            let dist = ship_ball.position.distance(ess.position);
            if dist > 2_500.0 {
                return Some(Err(BridgeRpcError {
                    code: "OutsideEssLinkRange".into(),
                    message: format!("Ship is {:.1} m from ESS beacon (must be <= 2500 m)", dist),
                }));
            }
            let key_item_id_arg = args.first().and_then(|a| a.as_i64());
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (key_item_id, key_type_id, duration_mins) =
                    crate::services::encounter_surveillance_system::select_ess_reserve_key_in_cargo(
                        &ctx.sde,
                        snap.items.values(),
                        char_id,
                        ship_id,
                        sys_id,
                        key_item_id_arg,
                    )?;
                if let Some(it) = snap.items.get_mut(&key_item_id) {
                    if it.quantity > 1 {
                        it.quantity -= 1;
                        it.version += 1;
                    } else {
                        snap.items.remove(&key_item_id);
                    }
                }
                let pulses = duration_mins as u32;
                let prev_pulse = get_ess_reserve_pulses(snap, sys_id);
                let pulse_st = EssReservePulseState {
                    pulses_total: pulses,
                    pulses_remaining: pulses,
                    last_pulse_sec: None,
                    reserve_linked_character_id: prev_pulse.reserve_linked_character_id,
                    reserve_link_started_tick: prev_pulse.reserve_link_started_tick,
                };
                set_ess_reserve_pulses(snap, sys_id, &pulse_st);
                Ok((key_type_id, duration_mins))
            });
            match res {
                Ok((key_type_id, duration_mins)) => {
                    let now_unix = ctx.now_unix();
                    let unlock_until = now_unix + (duration_mins as u64) * 60;
                    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                        rt.ess_reserve_unlocked_until.insert(sys_id, unlock_until);
                    });
                    if let Some(reg_id) = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id) {
                        for (&s_id, s_info) in &ctx.sde.solar_systems {
                            if s_info.region_id == reg_id {
                                ctx.notify(
                                    crate::router::NotificationTarget::SolarSystem(s_id),
                                    "OnReserveBankUnlock",
                                    "solarsystemid",
                                    vec![PyRep::Integer(sys_id as i64), PyRep::Integer(char_id), PyRep::Integer(unlock_until as i64)],
                                );
                            }
                        }
                    }
                    session.push_notification(
                        "OnReserveBankUnlock",
                        vec![PyRep::Integer(sys_id as i64), PyRep::Integer(char_id), PyRep::Integer(unlock_until as i64)],
                    );
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
            // const-ok: cache invalidation notification returns None
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // const-ok: client-side memoized cache invalidation notification returns None
            Some(Ok(PyRep::None))
        }

        // parity: rpc:essMgr.prime_cache_result
        ("essMgr", "prime_cache_result") => {
            Some(crate::services::encounter_surveillance_system::prime_cache_result(ctx, session, args))
        }

        _ => {
            if service == BEYONCE_SVC && method == "CallComponentFromClient" {
                return Some(
                    handle_call_component_from_client(ctx, session, bound_entry, args).await,
                );
            }
            if let Some(res) =
                space_warp_jump::handle(ctx, session, bound_entry, service, method, args, kwargs)
                    .await
            {
                return Some(res);
            }
            if let Some(res) = space_cyno_filaments::handle(
                ctx,
                session,
                bound_entry,
                service,
                method,
                args,
                kwargs,
            )
            .await
            {
                return Some(res);
            }
            handle_scan_and_wormhole_extras(
                ctx,
                session,
                bound_entry,
                service,
                method,
                args,
                kwargs,
            )
            .await
        }
    }
}

pub(crate) async fn handle_update_state_request(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
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
                });
            }
            bound_sys
        }
        None => session.solar_system_id.ok_or_else(|| BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
        })?,
    };
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Ok(PyRep::None);
    }
    let Some(ship_id) = session.ship_id else {
        return Ok(PyRep::None);
    };
    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let handle = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = match handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
        })? {
        Some((_, b)) => b,
        None => {
            let Some(ship_item) = ctx.db.with_snapshot(|snap| snap.items.get(&ship_id).cloned())
            else {
                return Ok(PyRep::None);
            };
            let ship_type = ctx.sde.types.get(&ship_item.type_id);
            let max_vel = ctx
                .get_or_rebuild_ship_fitting(char_id, ship_id)
                .ok()
                .map(|f| f.ship.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &ctx.sde))
                .or_else(|| {
                    ship_type
                        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::MAX_VELOCITY))
                        .copied()
                })
                .unwrap_or(100.0);
            let inertia = ship_type
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::AGILITY))
                .copied()
                .unwrap_or(1.0);
            let mass = ship_type.map(|t| t.mass).unwrap_or(1_000_000.0);
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
    if matches!(ship_ball.mode, MovementMode::Stop) {
        let finished_warp = ctx
            .active_warps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ship_id)
            .is_some();
        if finished_warp {
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnWarpFinished",
                "charid",
                vec![
                    PyRep::Integer(ship_id),
                    PyRep::Real(ship_ball.position.x),
                    PyRep::Real(ship_ball.position.y),
                    PyRep::Real(ship_ball.position.z),
                ],
            );
        }
    }
    if matches!(ship_ball.mode, MovementMode::Warp { .. }) {
        return Ok(PyRep::None);
    }
    handle
        .push_set_state(ship_id)
        .await
        .map(|_tick| PyRep::None)
        .map_err(|e| BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
        })
}

async fn handle_admin_get_bubble_id(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let _ = ServerContext::require_character(session)?;
    if !session.has_gm_role() {
        return Err(BridgeRpcError {
            code: "InsufficientRoles".into(),
            message: "AdminGetBubbleID requires a GM or Admin role".into(),
        });
    }
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    let target_ball_id = args
        .first()
        .and_then(|a| a.as_i64())
        .unwrap_or(ship_id);
    let handle = ctx.systems.get_or_spawn(sys_id);
    if target_ball_id != ship_id {
        let visible = handle.list_balls_in_bubble(ship_id).await.unwrap_or_default();
        if !visible.iter().any(|b| b.id == target_ball_id) {
            return Err(BridgeRpcError {
                code: "BallNotVisible".into(),
                message: format!(
                    "Ball {target_ball_id} is not visible in caller ship {ship_id}'s grid bubble"
                ),
            });
        }
    }
    match handle.get_bubble_id_for_ball(target_ball_id).await {
        Ok(Some(bid)) => Ok(PyRep::Integer(bid as i64)),
        Ok(None) => Err(BridgeRpcError {
            code: "BallNotFound".into(),
            message: format!("Ball {target_ball_id} is not present in system {sys_id}"),
        }),
        Err(e) => Err(BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
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
        })?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Must be in space to interact with a space component".into(),
        });
    }
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Must have an active ship to interact with an item trader".into(),
    })?;
    let trader_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| BridgeRpcError {
        code: "InvalidArgument".into(),
        message: "CallComponentFromClient requires target itemID".into(),
    })?;
    let comp_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
    let comp_method = args.get(2).and_then(|a| a.as_str()).unwrap_or("");
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
    if comp_name != "itemTrader" || comp_method != "ProcessTrade" {
        return Err(BridgeRpcError {
            code: "UnsupportedComponentCall".into(),
            message: format!("Unsupported component call {comp_name}.{comp_method}"),
        });
    }
    let recipe_id = args
        .get(3)
        .and_then(|a| a.as_i64())
        .filter(|&r| r > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidRecipeID".into(),
            message: "ProcessTrade requires a positive recipeID".into(),
        })? as u32;
    let raw_mult = args.get(4).and_then(|a| a.as_i64()).unwrap_or(1);
    let multiplier = u32::try_from(raw_mult)
        .ok()
        .filter(|&m| (1..=100_000).contains(&m))
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidMultiplier".into(),
            message: "ProcessTrade multiplier must be between 1 and 100,000".into(),
        })? as i32;

    let handle = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b);
    let trader_ball = handle
        .get_ball_snapshot(trader_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b);
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
        })?
        .ok_or_else(|| BridgeRpcError {
            code: "NotAnItemTrader".into(),
            message: format!("Type {trader_type_id} does not have an itemTrader component"),
        })?;
    let (Some(sb), Some(tpos)) = (&ship_ball, trader_pos) else {
        return Err(BridgeRpcError {
            code: "ItemTraderOutOfRange".into(),
            message: format!("Ship or item trader {trader_id} does not have a valid position in system {sys_id}"),
        });
    };
    let surface_dist = (sb.position.distance(tpos) - sb.radius - trader_radius).max(0.0);
    if surface_dist > range_m {
        return Err(BridgeRpcError {
            code: "ItemTraderOutOfRange".into(),
            message: format!(
                "Ship is {surface_dist:.0} m from item trader {trader_id} (max {range_m:.0} m)"
            ),
        });
    }
    if !allowed_recipes.contains(&recipe_id) {
        return Err(BridgeRpcError {
            code: "RecipeNotAvailableAtTrader".into(),
            message: format!(
                "Recipe {recipe_id} is not offered by item trader type {trader_type_id}"
            ),
        });
    }

    let recipes_table = ctx
        .sde
        .load_cfsd_nested("res:/staticdata/itemtraderrecipes.fsdbinary")
        .map_err(|e| BridgeRpcError {
            code: "SdeError".into(),
            message: e.to_string(),
        })?;
    let recipe_val = recipes_table
        .get(&recipe_id.to_string())
        .ok_or_else(|| BridgeRpcError {
            code: "RecipeNotFound".into(),
            message: format!("Item trader recipe {recipe_id} not found in SDE"),
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
    ctx.db.try_with_mut_snapshot(|snap| {
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
                        return Err(BridgeRpcError {
                            code: "NotEnoughCargoSpace".into(),
                            message: format!(
                                "Item trader output volume ({projected_vol:.1} m3) exceeds ship cargo capacity ({cargo_cap:.1} m3)"
                            ),
                        });
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
    })
}
