//! `space_warp_jump` — Warp navigation, Stargate jumps, Docking/Undocking, and Session-Change Timers (`SP-3`, `SP-4`, `SP-5`).

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use glam::DVec3;
use std::collections::HashMap;

use super::{
    check_session_change_timer_active, compute_ship_warp_speed_au_s, ctx_scope_id,
    record_session_change_timer, sync_ship_cloak_and_mjfg_from_fitting,
    wormhole_item_id_for_signature, ACTIVE_WARPS, AU_IN_METERS, STRUCTURE_DOCKING_ACL_DENIED,
};

const BEYONCE_SVC: &str = "beyonce";

/// Zarzakh solar system ID (mirrors the V24 client constant `solarSystemZarzakh`).
const SOLAR_SYSTEM_ZARZAKH: u32 = 30_100_000;
/// Zarzakh entry-gate lock duration: 6 hours (EVE wiki `Zarzakh.md`).
const ZARZAKH_GATE_LOCK_SECS: u64 = 21_600;

/// Per-character Zarzakh entry-gate lock: `(entry stargate in Zarzakh, lock expiry unix)`.
pub(crate) static ZARZAKH_GATE_LOCKS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<(usize, i64), (i64, u64)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

pub(crate) async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // parity: plan:SP-3, feat:navigation.warp-to, feat:navigation.warp-disrupted, feat:navigation.warp-capacitor-cost, feat:navigation.warp-exit-invulnerability
        ("beyonce", "CmdWarpToStuff") | ("beyonce", "CmdWarpToStuffAutopilot") => Some(
            handle_cmd_warp_to_stuff(
                ctx,
                session,
                bound_entry,
                args,
                kwargs,
                method == "CmdWarpToStuffAutopilot",
            )
            .await,
        ),
        // parity: plan:SP-4, feat:navigation.stargate-capital-restriction, feat:navigation.session-change-timer-triggers, feat:navigation.jump-cloak-immunities
        ("beyonce", "CmdStargateJump") | ("beyonce", "StargateJump") => {
            Some(handle_cmd_stargate_jump(ctx, session, bound_entry, args).await)
        }
        // parity: plan:SP-4, plan:SP-5, feat:navigation.dock-station, feat:navigation.dock-structure, feat:navigation.undock, feat:navigation.session-change-timer-triggers
        // parity: rpc:beyonce.SetSessionCheck
        ("beyonce", "Dock") | ("beyonce", "CmdDock") => {
            Some(handle_dock(ctx, session, bound_entry, args).await)
        }
        ("beyonce", "Undock") => handle_undock(ctx, session, args, kwargs).await,
        ("beyonce", "SetSessionCheck") => {
            Some(handle_set_session_check(ctx, session, args).await)
        }
        _ => {
            if service == BEYONCE_SVC && method.ends_with("ToPoint") {
                return Some(handle_legacy_warp_to_point(ctx, session, bound_entry, args).await);
            }
            if service == BEYONCE_SVC && method.ends_with("ChangeTimer") {
                return Some(handle_get_session_change_timer(ctx, session).await);
            }
            None
        }
    }
}

async fn handle_legacy_warp_to_point(
    ctx: &ServerContext,
    session: &mut SessionState,
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
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "warp")?;
    let x = args.first().and_then(|a| a.as_f64()).unwrap_or(0.0);
    let y = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
    let z = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);
    if !x.is_finite() || !y.is_finite() || !z.is_finite() {
        return Err(BridgeRpcError {
            code: "InvalidWarpDestination".into(),
            message: "Warp destination coordinates must be finite".into(),
        });
    }
    let dest_pos = DVec3::new(x, y, z);

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

    let warp_dist = current_ball.position.distance(dest_pos);
    if warp_dist < 150_000.0 {
        super::SESSION_CHANGE_TIMERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(ctx_scope_id(ctx), char_id));
        return Err(BridgeRpcError {
            code: "WarpDistanceTooShort".into(),
            message: format!(
                "Cannot warp {:.1} m (minimum warp distance is 150,000 m)",
                warp_dist
            ),
        });
    }

    let mut is_valid_anchor = false;
    if let Some(sys_rec) = ctx.sde.solar_systems.get(&sys_id) {
        if sys_rec
            .stations
            .iter()
            .any(|st| st.position.distance(dest_pos) <= 100_000.0)
        {
            is_valid_anchor = true;
        }
        if !is_valid_anchor
            && ctx.sde.jumps.values().any(|j| {
                j.from_solar_system_id == sys_id && j.position.distance(dest_pos) <= 100_000.0
            })
        {
            is_valid_anchor = true;
        }
        if !is_valid_anchor {
            let cels = ctx.sde.celestials_in_system(sys_id);
            if cels
                .iter()
                .any(|c| c.position.distance(dest_pos) <= 100_000.0)
            {
                is_valid_anchor = true;
            }
        }
    }
    let has_scanned_sig = ctx.has_scanned_signature(char_id, sys_id, 750_000_001);
    let mut allowed_ball_ids: Vec<i64> = Vec::new();
    if !is_valid_anchor {
        is_valid_anchor = ctx.db.with_snapshot(|snap| {
            let near_structure = snap.structures.values().any(|st| {
                st.solar_system_id == sys_id
                    && st.state != "Destroyed"
                    && DVec3::from_array(st.position).distance(dest_pos) <= 100_000.0
            });
            let near_bookmark = snap.bookmarks.values().any(|b| {
                b.owner_id == char_id
                    && b.solar_system_id == sys_id
                    && DVec3::new(b.x, b.y, b.z).distance(dest_pos) <= 100_000.0
            });
            let near_fw_beacon = snap
                .system_states
                .get(&sys_id)
                .and_then(|s| s.fw_complex_position)
                .map(|p| DVec3::from_array(p).distance(dest_pos) <= 100_000.0)
                .unwrap_or(false);
            let near_scanned_sig = has_scanned_sig
                && snap
                    .system_states
                    .get(&sys_id)
                    .and_then(|s| s.signature_position)
                    .map(|p| DVec3::from_array(p).distance(dest_pos) <= 100_000.0)
                    .unwrap_or(false);
            let near_cosmic_sig = snap
                .cosmic_signatures
                .get(&sys_id)
                .is_some_and(|sigs| {
                    sigs.iter().any(|s| {
                        s.signal_strength >= 1.0
                            && !s.collapsed
                            && DVec3::from_array(s.position).distance(dest_pos) <= 100_000.0
                    })
                });
            for ((m_char_id, _), m) in &snap.active_missions {
                if *m_char_id == char_id
                    && m.state == "Accepted"
                    && m.solar_system_id == sys_id
                {
                    allowed_ball_ids.extend(m.spawned_npc_ball_ids.iter().copied());
                }
            }
            if let Some(fid) = session.fleet_id {
                if let Some(fl) = snap.fleets.get(&fid) {
                    for &member_id in &fl.members {
                        if member_id != char_id {
                            if let Some(m_ch) = snap.characters.get(&member_id) {
                                if m_ch.solar_system_id == sys_id && m_ch.station_id.is_none() {
                                    allowed_ball_ids.push(m_ch.active_ship_id);
                                }
                            }
                        }
                    }
                }
            }
            near_structure
                || near_bookmark
                || near_fw_beacon
                || near_scanned_sig
                || near_cosmic_sig
        });
    }
    if !is_valid_anchor && !allowed_ball_ids.is_empty() {
        if let Ok(balls) = handle.list_balls().await {
            is_valid_anchor = balls.iter().any(|b| {
                allowed_ball_ids.contains(&b.id) && b.position.distance(dest_pos) <= 100_000.0
            });
        }
    }
    if !is_valid_anchor {
        return Err(BridgeRpcError {
            code: "InvalidWarpDestination".into(),
            message: format!(
                "Destination ({x:.0}, {y:.0}, {z:.0}) does not match any known celestial, bookmark, or scanned object in system {sys_id}"
            ),
        });
    }

    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let is_covert_cloaked = handle
        .get_combat_state(ship_id)
        .await
        .ok()
        .flatten()
        .is_some_and(|cs| cs.is_cloaked && cs.is_covert_cloak && cs.gate_cloak_remaining_ticks == 0);
    let warp_speed = compute_ship_warp_speed_au_s(ctx, current_ball.type_id);
    let mode = MovementMode::new_warp(current_ball.position, dest_pos, warp_speed);
    if !is_covert_cloaked {
        let _ = handle.break_gate_cloak(ship_id).await;
    }
    match handle.set_movement_mode(ship_id, mode).await {
        Ok(tick) => Ok(PyRep::Integer(tick as i64)),
        Err(e) => {
            let code = if e.contains("WarpScrambled") {
                "WarpScrambled"
            } else if e.contains("CloakedCannotWarp") {
                "CloakedCannotWarp"
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

async fn handle_cmd_warp_to_stuff(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
    is_autopilot: bool,
) -> Result<PyRep, BridgeRpcError> {
    if args.len() == 3 && args.iter().all(|a| matches!(a, PyRep::Real(_))) {
        return handle_legacy_warp_to_point(ctx, session, bound_entry, args).await;
    }
    let (_, char_id) = ServerContext::require_character(session)?;
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "warp")?;
    let (stuff_type, target_id, target_str_opt, raw_min_range) =
        if let Some(s) = args.first().and_then(|a| a.as_str()) {
            (
                s,
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                args.get(1).and_then(|a| a.as_str()),
                args.get(2).and_then(|a| a.as_f64()),
            )
        } else {
            (
                "item",
                args.first().and_then(|a| a.as_i64()).unwrap_or(0),
                None,
                args.get(1).and_then(|a| a.as_f64()),
            )
        };
    let default_min_range = if is_autopilot { 15_000.0 } else { 0.0 };
    let min_range = raw_min_range
        .or_else(|| {
            kwargs
                .get("minRange")
                .or_else(|| kwargs.get("warpInDistance"))
                .or_else(|| kwargs.get("range"))
                .and_then(|v| v.as_f64())
        })
        .unwrap_or(default_min_range)
        .clamp(0.0, 100_000.0);
    let handle = ctx.systems.get_or_spawn(sys_id);
    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
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

    let mut resolved_pos: Option<DVec3> = None;
    let mut fleet_ship_ball_id: Option<i64> = None;
    match stuff_type {
        "bookmark" => {
            resolved_pos = ctx.db.with_snapshot(|snap| {
                snap.bookmarks
                    .get(&target_id)
                    .filter(|b| b.owner_id == char_id && b.solar_system_id == sys_id)
                    .map(|b| DVec3::new(b.x, b.y, b.z))
            });
        }
        "char" | "fleet" => {
            if let Some(fid) = session.fleet_id {
                fleet_ship_ball_id = ctx.db.with_snapshot(|snap| {
                    let is_member = snap
                        .fleets
                        .get(&fid)
                        .map(|f| f.members.contains(&target_id))
                        .unwrap_or(false);
                    if is_member && target_id != char_id {
                        snap.characters
                            .get(&target_id)
                            .filter(|c| c.solar_system_id == sys_id && c.station_id.is_none())
                            .map(|c| c.active_ship_id)
                    } else {
                        None
                    }
                });
            }
        }
        _ => {
            if let Some(sys_rec) = ctx.sde.solar_systems.get(&sys_id) {
                resolved_pos = sys_rec
                    .stations
                    .iter()
                    .find(|st| st.station_id == target_id)
                    .map(|st| st.position);
            }
            if resolved_pos.is_none() {
                resolved_pos = ctx
                    .sde
                    .jumps
                    .get(&target_id)
                    .filter(|j| j.from_solar_system_id == sys_id)
                    .map(|j| j.position);
            }
            if resolved_pos.is_none() {
                resolved_pos = ctx
                    .sde
                    .celestials_in_system(sys_id)
                    .into_iter()
                    .find(|c| c.celestial_id == target_id)
                    .map(|c| c.position);
            }
            if resolved_pos.is_none() {
                let has_scanned = ctx.has_scanned_signature(char_id, sys_id, target_id);
                resolved_pos = ctx.db.with_snapshot(|snap| {
                    if let Some(st) = snap.structures.get(&target_id) {
                        if st.solar_system_id == sys_id && st.state != "Destroyed" {
                            return Some(DVec3::from_array(st.position));
                        }
                    }
                    if target_id == 750_000_001 && has_scanned {
                        return snap
                            .system_states
                            .get(&sys_id)
                            .and_then(|s| s.signature_position)
                            .map(DVec3::from_array);
                    }
                    if let Some(sigs) = snap.cosmic_signatures.get(&sys_id) {
                        for s in sigs {
                            let wh_id = wormhole_item_id_for_signature(sys_id, &s.signature_id);
                            if ((wh_id == target_id && (has_scanned || s.signal_strength >= 1.0))
                                || (target_str_opt == Some(s.signature_id.as_str())
                                    && s.signal_strength >= 1.0))
                                && !s.collapsed
                            {
                                return Some(DVec3::from_array(s.position));
                            }
                        }
                    }
                    if let Some(anoms) = snap.cosmic_anomalies.get(&sys_id) {
                        for a in anoms {
                            if target_str_opt == Some(a.anomaly_id.as_str()) && !a.cleared {
                                return Some(DVec3::from_array(a.position));
                            }
                        }
                    }
                    None
                });
            }
        }
    }
    if resolved_pos.is_none() {
        if let Some(fb_id) = fleet_ship_ball_id {
            if let Ok(Some((_, fb))) = handle.get_ball_snapshot(fb_id).await {
                resolved_pos = Some(fb.position);
            }
        }
    }
    if resolved_pos.is_none() && target_id > 0 && target_id != ship_id {
        if let Ok(Some((_, tb))) = handle.get_ball_snapshot(target_id).await {
            if matches!(tb.mode, MovementMode::Rigid) {
                resolved_pos = Some(tb.position);
            }
        }
    }
    let Some(anchor_pos) = resolved_pos else {
        return Err(BridgeRpcError {
            code: "InvalidWarpDestination".into(),
            message: format!(
                "Warp target ({stuff_type}, {target_id}) is not a valid warpable object in system {sys_id}"
            ),
        });
    };

    let warp_dist = current_ball.position.distance(anchor_pos);
    if warp_dist < 150_000.0 {
        return Err(BridgeRpcError {
            code: "WarpDistanceTooShort".into(),
            message: format!(
                "Cannot warp {:.1} m (minimum warp distance is 150,000 m)",
                warp_dist
            ),
        });
    }

    if handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
        return Err(BridgeRpcError {
            code: "WarpScrambled".into(),
            message: "WarpScrambled: Warp drive disabled by warp disruption/scrambler effect"
                .into(),
        });
    }

    let mut is_covert_cloaked = false;
    if let Ok(Some(cs_check)) = handle.get_combat_state(ship_id).await {
        if cs_check.is_cloaked && cs_check.gate_cloak_remaining_ticks == 0 && !cs_check.is_covert_cloak {
            return Err(BridgeRpcError {
                code: "CloakedCannotWarp".into(),
                message: "Cannot initiate warp while cloaked without a Covert Ops Cloaking Device"
                    .into(),
            });
        }
        is_covert_cloaked =
            cs_check.is_cloaked && cs_check.is_covert_cloak && cs_check.gate_cloak_remaining_ticks == 0;
    }

    let warp_dir = (anchor_pos - current_ball.position).normalize_or_zero();
    let mut dest_pos = anchor_pos - warp_dir * min_range;
    let requested_dist_m = current_ball.position.distance(dest_pos);

    let (base_warp_cap_need, sde_max_cap) = ctx
        .sde
        .types
        .get(&current_ball.type_id)
        .map(|t| {
            let wcn = t.default_attributes.get(&153).copied().unwrap_or(1.24e-6);
            let cap = t
                .default_attributes
                .get(&attr_ids::CAPACITOR_CAPACITY)
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
    let cap_per_au = eff_warp_cap_need * current_ball.mass;
    let cap_per_m = cap_per_au / AU_IN_METERS;
    let req_cap = cap_per_m * requested_dist_m;

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
                return Err(BridgeRpcError {
                    code: "NotEnoughCapacitor".into(),
                    message: format!(
                        "Insufficient capacitor ({:.3} GJ) to initiate minimum 150 km warp (affordable distance {:.1} m)",
                        cs.capacitor, affordable_dist_m
                    ),
                });
            }
            dest_pos = current_ball.position + warp_dir * affordable_dist_m;
            cs.capacitor = 0.0;
        } else {
            cs.capacitor = (cs.capacitor - req_cap).max(0.0);
        }
        let _ = handle.set_capacitor(ship_id, cs.capacitor).await;
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnCapacitorChange",
            "charid",
            vec![
                PyRep::Integer(ship_id),
                PyRep::Real(cs.capacitor),
                PyRep::Real(cs.max_capacitor),
            ],
        );
    }

    let warp_speed = compute_ship_warp_speed_au_s(ctx, current_ball.type_id);
    let mode = MovementMode::new_warp(current_ball.position, dest_pos, warp_speed);
    if !is_covert_cloaked {
        let _ = handle.break_gate_cloak(ship_id).await;
    }
    match handle.set_movement_mode(ship_id, mode).await {
        Ok(tick) => {
            ACTIVE_WARPS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((ctx_scope_id(ctx), ship_id), char_id);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnWarpStarted",
                "charid",
                vec![
                    PyRep::Integer(ship_id),
                    PyRep::Real(dest_pos.x),
                    PyRep::Real(dest_pos.y),
                    PyRep::Real(dest_pos.z),
                    PyRep::Real(warp_speed),
                ],
            );
            Ok(PyRep::Integer(tick as i64))
        }
        Err(e) => {
            let code = if e.contains("WarpScrambled") {
                "WarpScrambled"
            } else if e.contains("CloakedCannotWarp") {
                "CloakedCannotWarp"
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

async fn handle_cmd_stargate_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot jump stargate while docked".into(),
        });
    }
    let old_sys = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "warp")?;
    let Some(gate_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "CmdStargateJump requires integer stargateID".into(),
        });
    };
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "jump")?;
    let Some(jump) = ctx.sde.jumps.get(&gate_id) else {
        return Err(BridgeRpcError {
            code: "UnknownStargate".into(),
            message: format!("Stargate {gate_id} does not exist in SDE"),
        });
    };
    if jump.from_solar_system_id != old_sys {
        return Err(BridgeRpcError {
            code: "StargateWrongSystem".into(),
            message: format!(
                "Stargate {gate_id} is in system {}, not current system {old_sys}",
                jump.from_solar_system_id
            ),
        });
    }

    let now_unix = ctx.now_unix();
    let has_weapons_timer = ctx
        .crimewatch
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .states
        .get(&char_id)
        .is_some_and(|st| st.is_weapon_timer_active(now_unix));
    if has_weapons_timer {
        return Err(BridgeRpcError {
            code: "WeaponsTimerActive".into(),
            message: "Cannot jump through stargate while a 60s Weapons Timer is active".into(),
        });
    }

    let old_handle = ctx.systems.get_or_spawn(old_sys);
    let Some((cur_tick, current_ball)) = (match old_handle.get_ball_snapshot(ship_id).await {
        Ok(v) => v,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "DestinyQueryFailed".into(),
                message: e,
            })
        }
    }) else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship ball is not in space".into(),
        });
    };

    if let Some(active_timer) =
        check_session_change_timer_active(ctx, char_id, now_unix, Some((old_sys, cur_tick)))
    {
        return Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot jump stargate while 10s session-change timer ({}) is active",
                active_timer.reason
            ),
        });
    }

    if matches!(current_ball.mode, MovementMode::Warp { .. }) {
        return Err(BridgeRpcError {
            code: "ShipInWarp".into(),
            message: "Cannot jump while ship is mid-warp".into(),
        });
    }

    let ship_group_id = ctx
        .sde
        .types
        .get(&current_ball.type_id)
        .map(|t| t.group_id)
        .unwrap_or(0);
    let is_restricted_capital = matches!(ship_group_id, 30 | 485 | 547 | 659 | 883 | 1538 | 4594);
    let is_zarzakh_gate =
        old_sys == SOLAR_SYSTEM_ZARZAKH || jump.to_solar_system_id == SOLAR_SYSTEM_ZARZAKH;
    let is_zarzakh_restricted = is_zarzakh_gate && matches!(ship_group_id, 513 | 902 | 941);
    if is_restricted_capital || is_zarzakh_restricted {
        return Err(BridgeRpcError {
            code: "CapitalShipCannotJumpStargate".into(),
            message: format!(
                "Capital ship type {} (group {}) cannot jump stargate {gate_id}",
                current_ball.type_id, ship_group_id
            ),
        });
    }

    if old_sys == SOLAR_SYSTEM_ZARZAKH {
        let locked_gate = ZARZAKH_GATE_LOCKS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(ctx_scope_id(ctx), char_id))
            .copied();
        if let Some((allowed_gate_id, expires_at)) = locked_gate {
            if now_unix < expires_at && gate_id != allowed_gate_id {
                return Err(BridgeRpcError {
                    code: "ZarzakhGateLocked".into(),
                    message: format!(
                        "Zarzakh 6-hour gate lock active: character {char_id} entered via gate {allowed_gate_id} and cannot exit via gate {gate_id}"
                    ),
                });
            }
        }
    }

    let gate_radius = ctx
        .sde
        .types
        .get(&16)
        .map(|t| t.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(14_051.0);
    let dist = (current_ball.position.distance(jump.position) - gate_radius).max(0.0);
    if dist > 2_500.0 {
        return Err(BridgeRpcError {
            code: "OutsideStargateJumpRange".into(),
            message: format!(
                "Ship is {:.1} m from stargate {gate_id} (must be <= 2500 m)",
                dist
            ),
        });
    }

    let dest_sys_id = jump.to_solar_system_id;
    if dest_sys_id == SOLAR_SYSTEM_ZARZAKH {
        ZARZAKH_GATE_LOCKS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                (ctx_scope_id(ctx), char_id),
                (jump.destination_stargate_id, now_unix.saturating_add(ZARZAKH_GATE_LOCK_SECS)),
            );
    }
    let dest_gate_pos = ctx
        .sde
        .jumps
        .get(&jump.destination_stargate_id)
        .map(|j| j.position)
        .unwrap_or(DVec3::ZERO);
    let arrival_pos = dest_gate_pos + DVec3::new(gate_radius + 1_500.0, 0.0, 0.0);

    let updated_ch = ctx
        .db
        .set_character_solar_system(char_id, dest_sys_id, &ctx.sde)
        .map_err(|e| BridgeRpcError {
            code: "SystemTransitionFailed".into(),
            message: e.to_string(),
        })?;
    let _ = old_handle
        .emit_destiny_event(
            ship_id,
            eve_destiny::DestinyEvent::OnClientControllerEvent {
                ball_id: gate_id,
                target_id: Some(ship_id),
                event_name: "GateActivate".into(),
                value: 1,
                delay: None,
            },
        )
        .await;
    let existing_ball = old_handle.remove_ball(ship_id).await;
    ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);

    let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let mut new_ball = existing_ball.unwrap_or(current_ball);
    new_ball.position = arrival_pos;
    new_ball.velocity = DVec3::ZERO;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = MovementMode::Stop;
    let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    combat_state.is_cloaked = true;
    combat_state.gate_cloak_remaining_ticks = 60;
    combat_state.undock_invulnerable_remaining_ticks = 60;
    combat_state.sync_invuln_with_undock_timer = true;
    let mut slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
    slim_meta.is_cloaked = true;
    let spawn_tick = new_handle
        .spawn_ball_with_meta(new_ball, combat_state, slim_meta)
        .await
        .unwrap_or(0);
    let _ = ctx
        .living_world
        .materialize_system_lod2(dest_sys_id, &ctx.systems, &ctx.sde)
        .await;
    let _ = new_handle.push_set_state(ship_id).await;

    session.solar_system_id = Some(updated_ch.solar_system_id);
    session.constellation_id = Some(updated_ch.constellation_id);
    session.region_id = Some(updated_ch.region_id);
    session.station_id = None;
    session.structure_id = None;

    record_session_change_timer(
        ctx,
        char_id,
        now_unix,
        dest_sys_id,
        spawn_tick,
        "stargate_jump",
    );

    let new_beyonce_id = ctx.bind_object(session, BEYONCE_SVC, dest_sys_id as i64)?;

    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
        ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
        ("jumpCooldownMinutes", PyRep::Real(0.0)),
        ("jumpFatigueMinutes", PyRep::Real(0.0)),
        ("sessionChangeTimerSeconds", PyRep::Integer(10)),
    ]))
}

async fn handle_dock(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "AlreadyDocked".into(),
            message: "Character is already docked in a station or structure".into(),
        });
    }
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Cannot dock without an active ship in space".into(),
        });
    };
    let Some(dock_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "MissingLocationId".into(),
            message: "Dock requires an explicit station or structure ID argument".into(),
        });
    };
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "dock")?;
    let Some(sys_id) = bound_entry
        .map(|b| b.object_id as u32)
        .or(session.solar_system_id)
    else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solarSystemID".into(),
        });
    };

    let handle = ctx.systems.get_or_spawn(sys_id);
    let Some((cur_tick, ball)) = (match handle.get_ball_snapshot(ship_id).await {
        Ok(v) => v,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "DestinyQueryFailed".into(),
                message: e,
            })
        }
    }) else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Ship {ship_id} is not spawned in solar system {sys_id} Ballpark"),
        });
    };

    let now_unix = ctx.now_unix();
    if let Some(active_timer) =
        check_session_change_timer_active(ctx, char_id, now_unix, Some((sys_id, cur_tick)))
    {
        return Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot dock while 10s session-change timer ({}) is active",
                active_timer.reason
            ),
        });
    }

    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    let ship_group_id = ctx
        .sde
        .types
        .get(&ball.type_id)
        .map(|t| t.group_id)
        .unwrap_or(0);
    let is_capsule = ball.type_id == 670 || ship_group_id == 29;

    {
        let cw = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner());
        if let Some(cw_state) = cw.states.get(&char_id) {
            if cw_state.is_weapon_timer_active(now_unix) {
                return Err(BridgeRpcError {
                    code: "WeaponsTimerActive".into(),
                    message: "Cannot dock while a 60s Weapons Timer is active".into(),
                });
            }
            if sys_sec >= 0.45 && cw_state.is_criminal(now_unix) && !is_capsule {
                return Err(BridgeRpcError {
                    code: "CriminalTimerActive".into(),
                    message: "Cannot dock a non-capsule ship in high-sec while a Criminal Timer is active".into(),
                });
            }
        }
    }

    let is_structure = dock_id >= 1_000_000_000_000;
    let target_pos = if is_structure {
        let st_opt = ctx
            .db
            .with_snapshot(|snap| snap.structures.get(&dock_id).cloned());
        let Some(st) = st_opt else {
            return Err(BridgeRpcError {
                code: "StructureNotFound".into(),
                message: format!("Structure {dock_id} does not exist"),
            });
        };
        if st.solar_system_id != sys_id {
            return Err(BridgeRpcError {
                code: "DockTargetWrongSystem".into(),
                message: format!(
                    "Structure {dock_id} is in system {}, but pilot is in {sys_id}",
                    st.solar_system_id
                ),
            });
        }
        if st.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "StructureDestroyed".into(),
                message: format!("Structure {dock_id} is destroyed"),
            });
        }
        let char_corp_id = session.corporation_id.unwrap_or_else(|| {
            ctx.db
                .get_character(char_id)
                .map(|c| c.corporation_id)
                .unwrap_or(1_000_001)
        });
        let acl_denied = STRUCTURE_DOCKING_ACL_DENIED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(ctx_scope_id(ctx), dock_id))
            .is_some_and(|set| set.contains(&char_id))
            || (st.owner_corp_id >= 98_000_000 && st.owner_corp_id != char_corp_id);
        if acl_denied {
            return Err(BridgeRpcError {
                code: "StructureDockingAccessDenied".into(),
                message: format!(
                    "Character {char_id} is denied docking access by structure {dock_id} ACL"
                ),
            });
        }
        let is_medium_structure = matches!(st.type_id, 35832 | 35825 | 35835);
        let is_large_structure = matches!(st.type_id, 35833 | 35826 | 35836);
        let is_capital_ship =
            matches!(ship_group_id, 30 | 485 | 513 | 547 | 659 | 883 | 902 | 1538 | 4594);
        let is_supercapital_ship = matches!(ship_group_id, 30 | 659);
        if (is_medium_structure && is_capital_ship)
            || (is_large_structure && is_supercapital_ship)
        {
            return Err(BridgeRpcError {
                code: "ShipTooLargeForStructure".into(),
                message: format!(
                    "Ship type {} (group {}) is too large to dock at structure {} (type {})",
                    ball.type_id, ship_group_id, dock_id, st.type_id
                ),
            });
        }
        DVec3::from_array(st.position)
    } else {
        let st_opt = ctx
            .sde
            .stations
            .get(&dock_id)
            .filter(|st| st.solar_system_id == sys_id)
            .cloned();
        let Some(st) = st_opt else {
            return Err(BridgeRpcError {
                code: "DockTargetWrongSystem".into(),
                message: format!(
                    "Station {dock_id} does not exist in current solar system {sys_id}"
                ),
            });
        };
        st.position
    };

    if matches!(ball.mode, MovementMode::Warp { .. }) {
        return Err(BridgeRpcError {
            code: "ShipInWarp".into(),
            message: "Cannot dock while ship is still in warp".into(),
        });
    }
    let dist = ball.position.distance(target_pos);
    if dist > 2_500.0 {
        return Err(BridgeRpcError {
            code: "OutsideDockingPerimeter".into(),
            message: format!(
                "Ship is {:.1} m from docking perimeter (must be <= 2500 m)",
                dist
            ),
        });
    }
    match ctx.db.dock_character(char_id, dock_id, is_structure) {
        Ok(_) => {
            let _ = handle.remove_ball(ship_id).await;
            if is_structure {
                session.structure_id = Some(dock_id);
                session.station_id = None;
            } else {
                session.station_id = Some(dock_id);
                session.structure_id = None;
            }
            record_session_change_timer(ctx, char_id, now_unix, sys_id, cur_tick, "dock");
            Ok(PyRep::keyval([
                ("locationID", PyRep::Integer(dock_id)),
                ("isStructure", PyRep::Bool(is_structure)),
                ("sessionChangeTimerSeconds", PyRep::Integer(10)),
            ]))
        }
        Err(e) => Err(BridgeRpcError {
            code: "DockFailed".into(),
            message: e.to_string(),
        }),
    }
}

async fn handle_undock(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let now_unix = ctx.now_unix();
    if let Some(active_timer) = check_session_change_timer_active(ctx, char_id, now_unix, None) {
        return Some(Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot undock while 10s session-change timer ({}) is active",
                active_timer.reason
            ),
        }));
    }
    let res = crate::services::station_structure::handle(
        ctx,
        session,
        "stationSvc",
        "Undock",
        args,
        kwargs,
    )
    .await?;
    if let Ok(ref rep) = res {
        let sys_id = session.solar_system_id.unwrap_or(30_000_142);
        let spawn_tick = rep
            .dict_get("spawnTick")
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as u64;
        record_session_change_timer(ctx, char_id, now_unix, sys_id, spawn_tick, "undock");
    }
    Some(res)
}

async fn handle_get_session_change_timer(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let sys_id = session.solar_system_id.unwrap_or(30_000_142);
    let cur_tick_opt = if let Some(ship_id) = session.ship_id {
        let h = ctx.systems.get_or_spawn(sys_id);
        h.get_ball_snapshot(ship_id)
            .await
            .ok()
            .flatten()
            .map(|(t, _)| (sys_id, t))
    } else {
        None
    };
    if let Some(active) = check_session_change_timer_active(ctx, char_id, now_unix, cur_tick_opt) {
        let rem = active.expires_unix.saturating_sub(now_unix);
        return Ok(PyRep::keyval([
            ("active", PyRep::Bool(true)),
            ("remainingSeconds", PyRep::Integer(rem as i64)),
            ("trigger", PyRep::String(active.reason)),
        ]));
    }
    Ok(PyRep::keyval([
        ("active", PyRep::Bool(false)),
        ("remainingSeconds", PyRep::Integer(0)),
        ("trigger", PyRep::None),
    ]))
}

async fn handle_set_session_check(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    if args.is_empty() {
        return handle_get_session_change_timer(ctx, session).await;
    }
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let sys_id = session.solar_system_id.unwrap_or(30_000_142);
    let cur_tick_opt = if let Some(ship_id) = session.ship_id {
        let h = ctx.systems.get_or_spawn(sys_id);
        h.get_ball_snapshot(ship_id)
            .await
            .ok()
            .flatten()
            .map(|(t, _)| (sys_id, t))
    } else {
        None
    };

    let reason = args
        .first()
        .and_then(|a| a.as_str())
        .unwrap_or("session_change");
    let is_valid_trigger = matches!(
        reason,
        "login"
            | "stargate_jump"
            | "wormhole_jump"
            | "jump_bridge"
            | "jump_drive"
            | "dock"
            | "undock"
            | "fleet_join"
            | "fleet_position_change"
            | "ship_board"
            | "ship_eject"
            | "pod_transition"
            | "clone_jump"
            | "clone_swap"
            | "corp_join"
            | "corp_leave"
            | "wallet_division_change"
            | "session_change"
    );
    if !is_valid_trigger {
        return Err(BridgeRpcError {
            code: "InvalidSessionChangeTrigger".into(),
            message: format!("Unknown session-change trigger '{reason}'"),
        });
    }
    if let Some(active) = check_session_change_timer_active(ctx, char_id, now_unix, cur_tick_opt) {
        return Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot perform session change '{reason}' while 10s timer from '{}' is active",
                active.reason
            ),
        });
    }
    let tick = cur_tick_opt.map(|(_, t)| t).unwrap_or(0);
    record_session_change_timer(ctx, char_id, now_unix, sys_id, tick, reason);
    Ok(PyRep::keyval([
        ("sessionChangeTimerSeconds", PyRep::Integer(10)),
        ("trigger", PyRep::String(reason.to_string())),
        ("expiresUnix", PyRep::Integer((now_unix + 10) as i64)),
    ]))
}
