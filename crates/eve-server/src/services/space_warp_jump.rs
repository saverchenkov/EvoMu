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
    check_session_change_timer_active, compute_ship_warp_speed_au_s,
    record_session_change_timer, sync_ship_cloak_and_mjfg_from_fitting,
    wormhole_item_id_for_signature, AU_IN_METERS,
};

pub use crate::services::space_destiny::{
    break_location_change_invulnerability, grant_location_change_invulnerability,
    is_location_change_invulnerable,
};

const BEYONCE_SVC: &str = "beyonce";

/// Zarzakh solar system ID (mirrors the V24 client constant `solarSystemZarzakh`).
pub(crate) const SOLAR_SYSTEM_ZARZAKH: u32 = 30_100_000;
/// Zarzakh entry-gate lock duration: 6 hours (EVE wiki `Zarzakh.md`).
const ZARZAKH_GATE_LOCK_SECS: u64 = 21_600;

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
        // parity: rpc:beyonce.CmdWarpToStuff, rpc:beyonce.CmdWarpToStuffAutopilot
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
        // parity: rpc:beyonce.CmdStargateJump, feat:navigation.stargate-jump
        ("beyonce", "CmdStargateJump") | ("beyonce", "StargateJump") => {
            Some(handle_cmd_stargate_jump(ctx, session, bound_entry, args).await)
        }
        // parity: plan:SP-4, plan:SP-5, feat:navigation.dock-station, feat:navigation.dock-structure, feat:navigation.undock, feat:navigation.session-change-timer-triggers
        // parity: rpc:beyonce.SetSessionCheck, rpc:beyonce.CmdDock
        ("beyonce", "Dock") | ("beyonce", "CmdDock") => {
            Some(handle_dock(ctx, session, bound_entry, args).await)
        }
        // parity: feat:navigation.undock
        ("beyonce", "Undock") => handle_undock(ctx, session, args, kwargs).await,
        ("beyonce", "SetSessionCheck") => {
            Some(handle_set_session_check(ctx, session, args).await)
        }
        _ => None,
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
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Cannot warp while docked in station or structure".into(),
        });
    }
    let sys_id = ServerContext::require_bound_system(bound_entry, BEYONCE_SVC)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "warp")?;
    crate::services::tourney_service::check_tourney_warp_restricted(ctx, sys_id, char_id)?;
    crate::services::combat_crimewatch::check_crimewatch_action_restriction(
        ctx,
        char_id,
        ship_id,
        sys_id,
        "warp",
        ctx.now_unix(),
    )?;
    let (is_stuff_call, stuff_type, target_id, target_str_opt, raw_min_range) =
        if let Some(s) = args.first().and_then(|a| a.as_str()) {
            (
                true,
                s,
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                args.get(1).and_then(|a| a.as_str()),
                args.get(2).and_then(|a| a.as_f64()),
            )
        } else if is_autopilot {
            (
                true,
                "item",
                args.first().and_then(|a| a.as_i64()).unwrap_or(0),
                None,
                args.get(1).and_then(|a| a.as_f64()),
            )
        } else {
            (
                false,
                "item",
                0,
                None,
                None,
            )
        };
    if !is_stuff_call {
        return Err(BridgeRpcError {
            code: "InvalidWarpDestination".into(),
            message: "Invalid warp destination".into(),
        });
    }
    if target_id <= 0 && target_str_opt.is_none() {
        return Err(BridgeRpcError {
            code: "InvalidTarget".into(),
            message: "Warp target ID must be a positive integer".into(),
        });
    }
    if target_id == ship_id {
        return Err(BridgeRpcError {
            code: "InvalidTarget".into(),
            message: "Cannot warp to own ship".into(),
        });
    }
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
        "bookmark" | "mission" | "dungeon" | "location" => {
            let now_unix = ctx.now_unix();
            resolved_pos = ctx.db.with_snapshot(|snap| {
                if !crate::services::fleet_social::is_bookmark_expired_in_snap(snap, target_id, now_unix) {
                    if let Some(b) = snap
                        .bookmarks
                        .get(&target_id)
                        .filter(|b| b.solar_system_id == sys_id)
                    {
                        let can_use = b.owner_id == char_id
                            || b.folder_id.is_some_and(|fid| {
                                snap.bookmark_folders.get(&fid).is_some_and(|f| {
                                    crate::services::fleet_social::fleet_social_access_group_bookmarks::folder_access_level(
                                        snap, session, char_id, f,
                                    ) >= crate::services::fleet_social::fleet_social_access_group_bookmarks::AccessLevel::Use
                                })
                            });
                        if can_use {
                            return Some(DVec3::new(b.x, b.y, b.z));
                        }
                    }
                }
                if snap.active_missions.values().any(|m| {
                    m.character_id == char_id
                        && m.solar_system_id == sys_id
                        && m.state == "Accepted"
                        && (target_id == m.mission_id as i64 || target_id == m.agent_id as i64)
                }) {
                    return Some(DVec3::new(250_000_000.0, 0.0, 250_000_000.0));
                }
                None
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
                            let eff_strength =
                                ctx.effective_signature_signal_strength(char_id, sys_id, s);
                            if ((wh_id == target_id && (has_scanned || eff_strength >= 1.0))
                                || (target_str_opt == Some(s.signature_id.as_str())
                                    && eff_strength >= 1.0))
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
            // [plan:DG-4] Entering warp halts active non-warp module cycles (weapons, scrams, webs, mining lasers, etc.)
            let _ = handle.stop_non_warp_module_cycles(ship_id).await;
            crate::sp6_lifecycle::abandon_drones_on_warp_sync(ctx, char_id, ship_id, sys_id);
            let _ = crate::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_if_needed(
                ctx, char_id, sys_id, dest_pos,
            );
            ctx.active_warps
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(ship_id, char_id);
            ctx.db.with_mut_snapshot(|snap| {
                let mut ext = crate::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
                if !ext.characters_warped.contains(&char_id) {
                    ext.characters_warped.push(char_id);
                    crate::services::cosmetics_ui_ops::save_cosmetics_ui_state(snap, &ext);
                }
            });
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
            if ctx
                .sde
                .celestials_in_system(sys_id)
                .iter()
                .any(|c| c.celestial_id == target_id && c.group_id == 9)
            {
                let _ = ctx.notify_warp_to_asteroid_belt(char_id, target_id);
            }
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
    crate::services::combat_crimewatch::check_crimewatch_action_restriction(
        ctx,
        char_id,
        ship_id,
        old_sys,
        "stargate_jump",
        now_unix,
    )?;

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
        let locked_gate = ctx
            .db
            .with_snapshot(|snap| snap.zarzakh_gate_locks.get(&char_id).copied());
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
    let mut zarzakh_lock_expiry = None;
    if dest_sys_id == SOLAR_SYSTEM_ZARZAKH {
        let expiry = now_unix.saturating_add(ZARZAKH_GATE_LOCK_SECS);
        ctx.db.with_mut_snapshot(|snap| {
            snap.zarzakh_gate_locks.insert(
                char_id,
                (
                    jump.destination_stargate_id,
                    expiry,
                ),
            );
        });
        zarzakh_lock_expiry = Some(expiry);
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
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, old_sys);
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.authorized_shipping_storage.retain(|&(_, c)| c != char_id);
    });
    deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
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
    crate::services::map_service::record_solar_system_visit(ctx, char_id, dest_sys_id);
    let _ = crate::services::combat_npc_police::check_and_spawn_faction_police(ctx, dest_sys_id).await;
    ctx.quasar_gateway.on_session_changed(session, now_unix);
    // Published only after the session (and its Quasar gateway mirror) is in the destination
    // system, so the jumping pilot's own stream receives the SolarSystem-scoped notice.
    if let Some(expiry) = zarzakh_lock_expiry {
        let _ = crate::services::quasar_sov_services::publish_stargate_lock_activated_notice(
            &ctx.quasar_gateway,
            dest_sys_id,
            jump.destination_stargate_id as u64,
            expiry,
            now_unix,
        );
    }

    record_session_change_timer(
        ctx,
        char_id,
        now_unix,
        dest_sys_id,
        spawn_tick,
        "stargate_jump",
    );

    ctx.quasar_gateway
        .progression
        .daily_goals
        .on_gameplay_event(char_id as u32, 4, 1, &ctx.quasar_gateway);
    ctx.quasar_gateway
        .progression
        .career_goals
        .on_gameplay_event(char_id as u32, "travel", 1, &ctx.quasar_gateway);

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
    let Some(mut ship_id) = session.ship_id else {
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

    let now_unix = ctx.now_unix();
    let handle = ctx.systems.get_or_spawn(sys_id);
    let mut ball_snap = match handle.get_ball_snapshot(ship_id).await {
        Ok(v) => v,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "DestinyQueryFailed".into(),
                message: e,
            })
        }
    };
    if ball_snap.is_none() {
        if let Some(db_ship_id) = ctx
            .db
            .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
            .filter(|&id| id > 0 && id != ship_id)
        {
            if let Ok(Some(v)) = handle.get_ball_snapshot(db_ship_id).await {
                ship_id = db_ship_id;
                session.ship_id = Some(db_ship_id);
                ball_snap = Some(v);
            }
        }
    }
    let cur_tick_opt = ball_snap.as_ref().map(|(cur_tick, _)| (sys_id, *cur_tick));
    if let Some(active_timer) =
        check_session_change_timer_active(ctx, char_id, now_unix, cur_tick_opt)
    {
        return Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot dock while 10s session-change timer ({}) is active",
                active_timer.reason
            ),
        });
    }
    let Some((cur_tick, ball)) = ball_snap else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Ship {ship_id} is not spawned in solar system {sys_id} Ballpark"),
        });
    };

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

    if crate::services::combat_crimewatch::combat_npc_police::is_outlaw_ship_docking_refused(ctx, sys_id, char_id, ship_id) {
        return Err(BridgeRpcError {
            code: "FactionPoliceRefusedDocking".into(),
            message: "Faction police refuse docking permissions to outlaw ships in high-security space".into(),
        });
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
        if matches!(st.state.as_str(), "Unanchored" | "AnchorVulnerable" | "Anchoring") {
            return Err(BridgeRpcError {
                code: "StructureNotDockable".into(),
                message: format!("Structure {dock_id} is in state '{}' and cannot be docked at", st.state),
            });
        }
        let acl_denied = ctx.db.with_snapshot(|snap| {
            !crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap(
                snap, dock_id, char_id,
            )
        });
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
            deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
            crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, sys_id);
            let _ = handle.accept_docking(ship_id, dock_id, target_pos).await;
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
        let sys_id = session.solar_system_id.or_else(|| {
            ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
        }).unwrap_or(0);
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
    let sys_id = session.solar_system_id.or_else(|| {
        ctx.db.with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
    }).unwrap_or(0);
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

    let reason = match args.first() {
        Some(PyRep::Dict(_)) | Some(PyRep::KeyVal(_)) | Some(PyRep::Object { .. }) => {
            let check_rep = args.first().unwrap();
            if let Some(req_sys) = check_rep
                .dict_get("solarsystemid2")
                .or_else(|| check_rep.dict_get("solarsystemid"))
                .and_then(PyRep::as_i64)
            {
                if req_sys > 0 && req_sys as u32 != sys_id {
                    return Err(BridgeRpcError {
                        code: "SessionCheckMismatch".into(),
                        message: format!("Session solarsystemid2 is {sys_id}, not {req_sys}"),
                    });
                }
            }
            "session_change"
        }
        Some(other) => other.as_str().unwrap_or("session_change"),
        None => "session_change",
    };
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
            | "fleet_change"
            | "ship_board"
            | "ship_eject"
            | "board_or_eject"
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
    super::record_client_session_check_timer(ctx, char_id, now_unix, sys_id, tick, reason);
    Ok(PyRep::keyval([
        ("sessionChangeTimerSeconds", PyRep::Integer(10)),
        ("trigger", PyRep::String(reason.to_string())),
        ("expiresUnix", PyRep::Integer((now_unix + 10) as i64)),
    ]))
}

pub fn deactivate_ship_modules_on_leave_space(ctx: &ServerContext, char_id: i64, ship_id: i64) {
    let active_mods: Vec<i64> = ctx.db.with_mut_snapshot(|snap| {
        let mods: Vec<i64> = snap
            .items
            .values()
            .filter(|it| {
                it.location_id == ship_id && snap.module_active_states.contains(&it.item_id)
            })
            .map(|it| it.item_id)
            .collect();
        for mid in &mods {
            snap.module_active_states.remove(mid);
        }
        mods
    });
    if !active_mods.is_empty() {
        ctx.invalidate_ship_fitting(ship_id);
        for mid in active_mods {
            ctx.notify_godma_ship_effect(
                crate::router::NotificationTarget::Character(char_id),
                mid,
                10,
                false,
                0,
                0,
            );
        }
    }
}
