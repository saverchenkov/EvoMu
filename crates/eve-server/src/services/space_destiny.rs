//! `space_destiny` — Domain 5: Solar System `Ballpark`, Subwarp Navigation, & Space Services Router (`§3.5`).

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use glam::DVec3;
use std::collections::HashMap;

#[path = "space_warp_jump.rs"]
pub mod space_warp_jump;

#[path = "space_cyno_filaments.rs"]
pub mod space_cyno_filaments;

#[path = "space_docking_cloaking.rs"]
pub mod space_docking_cloaking;
pub use space_docking_cloaking::*;

#[path = "space_nav_travel.rs"]
pub mod space_nav_travel;

pub(crate) const AU_IN_METERS: f64 = 149_597_870_700.0;
const BEYONCE_SVC: &str = "beyonce";

#[derive(Debug, Clone)]
pub(crate) struct SessionChangeTimerState {
    pub(crate) expires_unix: u64,
    pub(crate) system_id: u32,
    pub(crate) started_tick: u64,
    pub(crate) reason: String,
}

pub(crate) fn ctx_scope_id(ctx: &ServerContext) -> usize {
    std::ptr::from_ref(ctx) as usize
}

pub(crate) static SESSION_CHANGE_TIMERS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<(usize, i64), SessionChangeTimerState>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

pub(crate) static STRUCTURE_DOCKING_ACL_DENIED: std::sync::LazyLock<
    std::sync::Mutex<HashMap<(usize, i64), std::collections::HashSet<i64>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

pub(crate) static ACTIVE_WARPS: std::sync::LazyLock<std::sync::Mutex<HashMap<(usize, i64), i64>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

pub(crate) fn check_session_change_timer_active(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
    current_tick_opt: Option<(u32, u64)>,
) -> Option<SessionChangeTimerState> {
    let map = SESSION_CHANGE_TIMERS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let timer = map.get(&(ctx_scope_id(ctx), char_id))?;
    if now_unix >= timer.expires_unix {
        return None;
    }
    if let Some((sys_id, cur_tick)) = current_tick_opt {
        if sys_id == timer.system_id && cur_tick >= timer.started_tick.saturating_add(9) {
            return None;
        }
    }
    Some(timer.clone())
}

pub(crate) fn record_session_change_timer(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
    system_id: u32,
    started_tick: u64,
    reason: &str,
) {
    let expires_unix = now_unix.saturating_add(10);
    SESSION_CHANGE_TIMERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            (ctx_scope_id(ctx), char_id),
            SessionChangeTimerState {
                expires_unix,
                system_id,
                started_tick,
                reason: reason.to_string(),
            },
        );
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnSessionChanged",
        "charid",
        vec![
            PyRep::Integer(char_id),
            PyRep::String(reason.to_string()),
            PyRep::Integer(10),
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

pub(crate) fn compute_ship_warp_speed_au_s(ctx: &ServerContext, ship_type_id: u32) -> f64 {
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

pub(crate) async fn sync_ship_cloak_and_mjfg_from_fitting(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    ship_id: i64,
) {
    let (has_cloak_mod, active_cloak_type, active_cap_mjfg) = ctx.db.with_snapshot(|snap| {
        let mut has_cloak = false;
        let mut active_cloak: Option<u32> = None;
        let mut cap_mjfg = false;
        for item in snap.items.values() {
            if item.owner_id == char_id && item.location_id == ship_id {
                let is_active = snap.module_active_states.contains(&item.item_id);
                if matches!(item.type_id, 11370 | 11578) {
                    has_cloak = true;
                    if is_active {
                        active_cloak = Some(item.type_id);
                    }
                }
                if item.type_id == 83485 && is_active {
                    cap_mjfg = true;
                }
            }
        }
        (has_cloak, active_cloak, cap_mjfg)
    });

    space_nav_travel::sync_tether(ctx, char_id, sys_id, ship_id).await;
    if !has_cloak_mod && !active_cap_mjfg {
        return;
    }

    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some(mut cs) = handle.get_combat_state(ship_id).await.ok().flatten() else {
        return;
    };
    let Some((_, mut ball)) = handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
        return;
    };

    let mut cs_changed = false;
    let mut ball_changed = false;
    let cloak_before = cs.is_cloaked;

    if let Some(cloak_type_id) = active_cloak_type {
        let is_covert = cloak_type_id == 11578;
        let vel_mult = if is_covert { 1.0 } else { 0.25 };
        let targeting_delay_s = if is_covert { 5.0 } else { 30.0 };
        if !cs.is_cloaked && cs.decloak_targeting_delay_remaining_s <= 0.0 {
            let orig_v = cs.cloak_original_max_velocity.unwrap_or(ball.max_velocity);
            cs.cloak_original_max_velocity = Some(orig_v);
            ball.max_velocity = orig_v * vel_mult;
            cs.is_cloaked = true;
            cs.is_covert_cloak = is_covert;
            cs.cloak_stabilization_remaining_s = 900.0;
            cs.cloak_configured_targeting_delay_s = targeting_delay_s;
            cs_changed = true;
            ball_changed = true;
        }
    } else if has_cloak_mod && cs.is_cloaked && cs.gate_cloak_remaining_ticks == 0 {
        cs.is_cloaked = false;
        cs.cloak_stabilization_remaining_s = 0.0;
        let delay = if cs.cloak_configured_targeting_delay_s > 0.0 {
            cs.cloak_configured_targeting_delay_s
        } else if cs.is_covert_cloak {
            5.0
        } else {
            30.0
        };
        cs.decloak_targeting_delay_remaining_s = delay;
        if let Some(orig_v) = cs.cloak_original_max_velocity.take() {
            ball.max_velocity = orig_v;
            ball_changed = true;
        }
        cs_changed = true;
    }

    if active_cap_mjfg && !cs.is_capital_mjfg && cs.mjfg_spool_remaining_s <= 0.0 {
        cs.is_capital_mjfg = true;
        cs.mjfg_spool_remaining_s = 9.0;
        let orig_sig = cs.mjfg_original_sig_radius.unwrap_or(cs.signature_radius);
        cs.mjfg_original_sig_radius = Some(orig_sig);
        cs.signature_radius = orig_sig * 2.5;
        cs_changed = true;
    }

    let cloak_now = cs.is_cloaked;
    if ball_changed {
        let _ = handle.spawn_ball(ball).await;
    }
    if cs_changed {
        let _ = handle.set_combat_state(ship_id, cs).await;
    }
    if cloak_now != cloak_before {
        space_nav_travel::emit_cloak_transition(ctx, sys_id, ship_id, cloak_now).await;
    }
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
    let method = if service == BEYONCE_SVC && (method == "CmdWarpToItem" || method.ends_with("WarpTo")) {
        "CmdWarpToStuff"
    } else {
        method
    };
    match (service, method) {
        // parity: plan:P-7, plan:SP-1, plan:SP-2, plan:SP-3
        // parity: feat:navigation.destiny-updates, feat:navigation.celestials-balls, feat:navigation.bubbles-grid, feat:navigation.approach, feat:dogma.velocity-agility, feat:modules.micro-jump-drive
        // parity: rpc:beyonce.CmdGotoDirection, rpc:beyonce.CmdSteerDirection, rpc:beyonce.CmdGotoPoint
        // parity: rpc:beyonce.CmdStop, rpc:beyonce.CmdSetSpeedFraction, rpc:beyonce.CmdOrbit, rpc:beyonce.CmdFollowBall, rpc:beyonce.CmdAlignTo
        // parity: rpc:beyonce.UpdateStateRequest, rpc:beyonce.AdminGetBubbleID, rpc:beyonce.GetFormations
        // parity: destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall
        // parity: destiny:GotoDirection, destiny:FollowBall, destiny:Orbit, destiny:Stop
        // parity: notify:DoDestinyUpdate
        ("beyonce", "CmdGotoDirection") | ("beyonce", "CmdSteerDirection") => {
            Some(handle_cmd_goto_direction(ctx, session, bound_entry, args).await)
        }
        // parity: destiny:GotoPoint
        ("beyonce", "CmdGotoPoint") => Some(
            space_nav_travel::handle_cmd_goto_point(ctx, session, bound_entry, args).await,
        ),
        // parity: feat:navigation.jump-fatigue, rpc:jumpTimers.GetTimers, notify:OnJumpTimersUpdated
        ("jumpTimers", "GetTimers") => {
            Some(space_nav_travel::handle_get_jump_timers(ctx, session, args))
        }
        ("beyonce", "CmdStop") => Some(handle_cmd_stop(ctx, session, bound_entry).await),
        ("beyonce", "CmdSetSpeedFraction") => {
            Some(handle_cmd_set_speed_fraction(ctx, session, bound_entry, args).await)
        }
        ("beyonce", "CmdOrbit") | ("beyonce", "CmdFollowBall") => {
            Some(handle_cmd_orbit_or_follow(ctx, session, bound_entry, method, args).await)
        }
        ("beyonce", "CmdAlignTo") => {
            Some(handle_cmd_align_to(ctx, session, bound_entry, args).await)
        }
        ("beyonce", "UpdateStateRequest") => {
            Some(handle_update_state_request(ctx, session, bound_entry).await)
        }
        ("beyonce", "AdminGetBubbleID") => {
            Some(handle_admin_get_bubble_id(ctx, session, bound_entry, args).await)
        }

        _ => {
            if service == BEYONCE_SVC
                && (method.ends_with("Approach") || method.ends_with("KeepAtRange"))
            {
                return Some(
                    handle_cmd_orbit_or_follow(ctx, session, bound_entry, method, args).await,
                );
            }
            if service == BEYONCE_SVC && method.ends_with("Align") {
                return Some(handle_cmd_align_to(ctx, session, bound_entry, args).await);
            }
            if service == BEYONCE_SVC && method.ends_with("MicroJump") {
                return Some(handle_legacy_micro_jump(ctx, session, bound_entry, args).await);
            }
            if service == BEYONCE_SVC && method.ends_with("BallSnapshot") {
                return Some(handle_get_ball_snapshot(ctx, session, bound_entry).await);
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

async fn handle_cmd_goto_direction(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let dx = args.first().and_then(|a| a.as_f64()).unwrap_or(1.0);
    let dy = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
    let dz = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);

    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle.break_gate_cloak(ship_id).await;
    handle
        .set_movement_mode(
            ship_id,
            MovementMode::GotoDirection {
                direction: DVec3::new(dx, dy, dz),
            },
        )
        .await
        .map(|tick| PyRep::Integer(tick as i64))
        .map_err(|e| BridgeRpcError {
            code: "DestinyCommandFailed".into(),
            message: e,
        })
}

async fn handle_cmd_stop(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let handle = ctx.systems.get_or_spawn(sys_id);
    handle
        .set_movement_mode(ship_id, MovementMode::Stop)
        .await
        .map(|tick| PyRep::Integer(tick as i64))
        .map_err(|e| BridgeRpcError {
            code: "DestinyCommandFailed".into(),
            message: e,
        })
}

async fn handle_cmd_set_speed_fraction(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    let fraction = args.first().and_then(|a| a.as_f64()).unwrap_or(1.0);
    if !fraction.is_finite() {
        return Err(BridgeRpcError {
            code: "InvalidSpeedFraction".into(),
            message: "Speed fraction must be a finite number".into(),
        });
    }
    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let handle = ctx.systems.get_or_spawn(sys_id);
    if fraction > 0.0 {
        let _ = handle.break_gate_cloak(ship_id).await;
    }
    handle
        .set_speed_fraction(ship_id, fraction)
        .await
        .map(|tick| PyRep::Integer(tick as i64))
        .map_err(|e| BridgeRpcError {
            code: "DestinyCommandFailed".into(),
            message: e,
        })
}

async fn handle_cmd_orbit_or_follow(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let default_range = if method.ends_with("Approach") {
        0.0
    } else if method.ends_with("KeepAtRange") {
        1_000.0
    } else if method == "CmdFollowBall" {
        0.0
    } else {
        2_500.0
    };
    let range = args.get(1).and_then(|a| a.as_f64()).unwrap_or(default_range);
    let mode = if method == "CmdOrbit" {
        MovementMode::Orbit { target_id, range }
    } else {
        MovementMode::Follow { target_id, range }
    };
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle.break_gate_cloak(ship_id).await;
    handle
        .set_movement_mode(ship_id, mode)
        .await
        .map(|tick| PyRep::Integer(tick as i64))
        .map_err(|e| BridgeRpcError {
            code: "DestinyCommandFailed".into(),
            message: e,
        })
}

async fn handle_cmd_align_to(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, current_ball)) = (match handle.get_ball_snapshot(ship_id).await {
        Ok(v) => v,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "DestinyCommandFailed".into(),
                message: e,
            })
        }
    }) else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in Ballpark".into(),
        });
    };

    let mut target_pos: Option<DVec3> = None;
    if let Ok(Some((_, tb))) = handle.get_ball_snapshot(target_id).await {
        target_pos = Some(tb.position);
    }
    if target_pos.is_none() {
        if let Some(sys_rec) = ctx.sde.solar_systems.get(&sys_id) {
            target_pos = sys_rec
                .stations
                .iter()
                .find(|st| st.station_id == target_id)
                .map(|st| st.position);
        }
    }
    if target_pos.is_none() {
        target_pos = ctx
            .sde
            .jumps
            .get(&target_id)
            .filter(|j| j.from_solar_system_id == sys_id)
            .map(|j| j.position);
    }
    if target_pos.is_none() {
        target_pos = ctx
            .sde
            .celestials_in_system(sys_id)
            .into_iter()
            .find(|c| c.celestial_id == target_id)
            .map(|c| c.position);
    }
    if target_pos.is_none() {
        target_pos = ctx.db.with_snapshot(|snap| {
            snap.bookmarks
                .get(&target_id)
                .filter(|b| b.owner_id == char_id && b.solar_system_id == sys_id)
                .map(|b| DVec3::new(b.x, b.y, b.z))
        });
    }
    let dest = target_pos.unwrap_or_else(|| current_ball.position + DVec3::X * 100_000.0);
    let dir = (dest - current_ball.position).normalize();
    let _ = handle.break_gate_cloak(ship_id).await;
    let _ = handle.set_speed_fraction(ship_id, 1.0).await;
    handle
        .set_movement_mode(ship_id, MovementMode::GotoDirection { direction: dir })
        .await
        .map(|tick| PyRep::Integer(tick as i64))
        .map_err(|e| BridgeRpcError {
            code: "DestinyCommandFailed".into(),
            message: e,
        })
}

async fn handle_legacy_micro_jump(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let dist = args.first().and_then(|a| a.as_f64()).unwrap_or(100_000.0);
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle.break_gate_cloak(ship_id).await;
    match handle.micro_jump(ship_id, dist).await {
        Ok(new_pos) => Ok(PyRep::keyval([
            ("x", PyRep::Real(new_pos.x)),
            ("y", PyRep::Real(new_pos.y)),
            ("z", PyRep::Real(new_pos.z)),
        ])),
        Err(e) => {
            let code = if e.contains("WarpScrambled") {
                "WarpScrambled"
            } else {
                "DestinyCommandFailed"
            };
            Err(BridgeRpcError {
                code: code.into(),
                message: e,
            })
        }
    }
}

async fn handle_update_state_request(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Ok(PyRep::None);
    }
    let Some(ship_id) = session.ship_id else {
        return Ok(PyRep::None);
    };
    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, ship_ball)) = handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
        })?
    else {
        return Ok(PyRep::None);
    };
    if matches!(ship_ball.mode, MovementMode::Stop) {
        let finished_warp = ACTIVE_WARPS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(ctx_scope_id(ctx), ship_id))
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

async fn handle_get_ball_snapshot(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let handle = ctx.systems.get_or_spawn(sys_id);
    match handle.get_ball_snapshot(ship_id).await {
        Ok(Some((tick, ball))) => {
            if matches!(ball.mode, MovementMode::Stop) {
                let finished_warp = ACTIVE_WARPS
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&(ctx_scope_id(ctx), ship_id))
                    .is_some();
                if finished_warp {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnWarpFinished",
                        "charid",
                        vec![
                            PyRep::Integer(ship_id),
                            PyRep::Real(ball.position.x),
                            PyRep::Real(ball.position.y),
                            PyRep::Real(ball.position.z),
                        ],
                    );
                }
            }
            Ok(PyRep::keyval([
                ("tick", PyRep::Integer(tick as i64)),
                ("ballID", PyRep::Integer(ball.id)),
                ("x", PyRep::Real(ball.position.x)),
                ("y", PyRep::Real(ball.position.y)),
                ("z", PyRep::Real(ball.position.z)),
                ("vx", PyRep::Real(ball.velocity.x)),
                ("vy", PyRep::Real(ball.velocity.y)),
                ("vz", PyRep::Real(ball.velocity.z)),
            ]))
        }
        Ok(None) => Err(BridgeRpcError {
            code: "BallNotFound".into(),
            message: format!("Ball {ship_id} is not present in system {sys_id}"),
        }),
        Err(e) => Err(BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
        }),
    }
}
