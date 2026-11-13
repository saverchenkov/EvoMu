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
    check_session_change_timer_active, compute_pilot_warp_speed_au_s, record_session_change_timer,
    sync_ship_cloak_and_mjfg_from_fitting, wormhole_item_id_for_signature, AU_IN_METERS,
};
pub use crate::services::space_destiny::{
    break_location_change_invulnerability, grant_location_change_invulnerability, is_location_change_invulnerable,
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
        ("beyonce", "CmdStargateJump") | ("beyonce", "StargateJump") => Some(handle_cmd_stargate_jump(ctx, session, bound_entry, args, kwargs).await),
        // parity: plan:SP-4, plan:SP-5, feat:navigation.dock-station, feat:navigation.dock-structure, feat:navigation.undock, feat:navigation.session-change-timer-triggers
        // parity: rpc:beyonce.SetSessionCheck, rpc:beyonce.CmdDock
        ("beyonce", "CmdDock") => Some(handle_dock(ctx, session, bound_entry, args).await),
        ("beyonce", "Undock") => handle_undock(ctx, session, args, kwargs).await,
        ("beyonce", "SetSessionCheck") => {
            Some(crate::services::nav_warp_interdiction::handle_set_session_check(ctx, session, args).await)
        }
        _ => None,
    }
}

#[path = "space_warp_jump_helpers.rs"]
mod space_warp_jump_helpers;
use space_warp_jump_helpers::{
    bookmark_expired_err, emit_structure_dock_notifications, handle_undock,
    resolve_bound_or_session_sys as resolve_bound_or_session_sys_inner,
};

fn resolve_bound_or_session_sys(
    bound_entry: Option<&BoundObjectEntry>,
    session: &SessionState,
) -> Result<u32, BridgeRpcError> {
    resolve_bound_or_session_sys_inner(bound_entry, session, BEYONCE_SVC)
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
    if args.first().and_then(|a| a.as_str()) == Some("bookmark") {
        let bm_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
        if crate::services::fleet_social::is_bookmark_expired(ctx, bm_id, ctx.now_unix()) {
            return Err(bookmark_expired_err(bm_id));
        }
    }
    if let Some(ship_id) = session.ship_id {
        crate::services::structure_control_ops::reject_if_controlling_structure(ctx, ship_id, "warp")?;
    }
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Cannot warp while docked in station or structure".into(),
            client_error: None,
        });
    }
    let sys_id = resolve_bound_or_session_sys(bound_entry, session)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
            client_error: None,
        });
    };
    let is_fleet_warp_kw = kwargs
        .get("fleet")
        .is_some_and(|v| matches!(v, PyRep::Bool(true)) || v.as_i64().is_some_and(|i| i != 0) || v.as_str().is_some())
        || kwargs.contains_key("formationID");
    if is_fleet_warp_kw && !is_autopilot && session.fleet_id.is_some() {
        let mut wf_args = Vec::new();
        if args.first().and_then(|a| a.as_str()).is_some() {
            if let Some(tid) = args.get(1) {
                wf_args.push(tid.clone());
            }
            if let Some(mr) = args.get(2) {
                wf_args.push(mr.clone());
            }
        } else {
            wf_args.extend_from_slice(args);
        }
        if let Some(res) =
            crate::services::fleet_social::fleet_social_fleet_ops::handle_fleet_warp(ctx, session, &wf_args, kwargs).await
        {
            return res;
        }
    }
    let mjd_handle = ctx.systems.get_or_spawn(sys_id);
    crate::services::nav_warp_interdiction::validate_pre_warp_common(
        ctx,
        char_id,
        ship_id,
        sys_id,
        &mjd_handle,
    )
    .await?;
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
            client_error: None,
        });
    }
    if target_id <= 0 && target_str_opt.is_none() {
        return Err(BridgeRpcError {
            code: "InvalidTarget".into(),
            message: "Warp target ID must be a positive integer".into(),
            client_error: None,
        });
    }
    if target_id == ship_id {
        return Err(BridgeRpcError {
            code: "InvalidTarget".into(),
            message: "Cannot warp to own ship".into(),
            client_error: None,
        });
    }
    let default_min_range = if is_autopilot { 15_000.0 } else { 0.0 };
    let candidate_min_range = raw_min_range.or_else(|| {
        kwargs
            .get("minRange")
            .or_else(|| kwargs.get("warpInDistance"))
            .or_else(|| kwargs.get("range"))
            .and_then(|v| v.as_f64())
    });
    if let Some(r) = candidate_min_range {
        if !r.is_finite() {
            return Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "Warp range must be finite".into(),
                client_error: None,
            });
        }
    }
    let min_range = candidate_min_range
        .unwrap_or(default_min_range)
        .clamp(0.0, 100_000.0);
    tracing::info!(
        char_id,
        sys_id,
        ship_id,
        stuff_type,
        target_id,
        ?target_str_opt,
        min_range,
        "CmdWarpToStuff request"
    );
    let handle = ctx.systems.get_or_spawn(sys_id);
    sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
    let Some((_, current_ball)) = (match handle.get_ball_snapshot(ship_id).await {
        Ok(v) => v,
        Err(e) => {
            return Err(BridgeRpcError {
                code: "DestinyCommandFailed".into(),
                message: e,
                client_error: None,
            })
        }
    }) else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship is not spawned in Ballpark".into(),
            client_error: None,
        });
    };

    let needs_pve_seed = matches!(
        stuff_type,
        "epinstance" | "ep_instance" | "site" | "anomaly" | "dungeon" | "location"
    ) || target_str_opt.is_some_and(|s| {
        s.starts_with("PVE-ANOM-")
            || s.starts_with("EXP-")
            || s.starts_with("DED-")
            || s.starts_with("DSG-")
            || s.starts_with("DAT-")
            || s.starts_with("REL-")
            || s.starts_with("GHO-")
            || s.starts_with("SLC-")
    });
    if needs_pve_seed {
        crate::services::pve_exploration::ensure_system_pve_sites_seeded(ctx, sys_id).await;
    }

    let mut resolved_pos: Option<DVec3> = None;
    let mut fleet_ship_ball_id: Option<i64> = None;
    match stuff_type {
        "bookmark" => {
            if sys_id == SOLAR_SYSTEM_ZARZAKH {
                return Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::DunBlacklistCannotWarp));
            }
            let now_unix = ctx.now_unix();
            if crate::services::fleet_social::is_bookmark_expired(ctx, target_id, now_unix) {
                return Err(bookmark_expired_err(target_id));
            }
            let bm_opt = ctx.db.with_snapshot(|snap| {
                let b = snap.bookmarks.get(&target_id)?;
                if b.solar_system_id != sys_id {
                    return None;
                }
                let can_use = b.owner_id == char_id
                    || b.folder_id.is_some_and(|fid| {
                        snap.bookmark_folders.get(&fid).is_some_and(|f| {
                            crate::services::fleet_social::fleet_social_access_group_bookmarks::folder_access_level(
                                snap, session, char_id, f,
                            ) >= crate::services::fleet_social::fleet_social_access_group_bookmarks::AccessLevel::Use
                        })
                    });
                if can_use {
                    Some(DVec3::new(b.x, b.y, b.z))
                } else {
                    None
                }
            });
            let Some(pos) = bm_opt else {
                return Err(BridgeRpcError {
                    code: "BookmarkNotFound".into(),
                    message: format!("Bookmark {target_id} not found"),
                    client_error: None,
                });
            };
            resolved_pos = Some(pos);
        }
        "mission" | "dungeon" | "location" | "epinstance" | "ep_instance" | "site" | "anomaly" => {
            let deadspace_type = if stuff_type == "ep_instance" {
                "epinstance"
            } else {
                stuff_type
            };
            resolved_pos =
                crate::services::pve_dungeon_runtime::resolve_deadspace_or_escalation_warp_target(
                    ctx,
                    char_id,
                    sys_id,
                    deadspace_type,
                    target_id,
                    target_str_opt,
                )?;
            if resolved_pos.is_none() && target_str_opt.is_some() {
                resolved_pos = ctx.db.with_snapshot(|snap| {
                    if let Some(anoms) = snap.cosmic_anomalies.get(&sys_id) {
                        for a in anoms {
                            if target_str_opt == Some(a.anomaly_id.as_str()) && !a.cleared {
                                return Some(DVec3::from_array(a.position));
                            }
                        }
                    }
                    if let Some(sigs) = snap.cosmic_signatures.get(&sys_id) {
                        for s in sigs {
                            let eff_strength =
                                ctx.effective_signature_signal_strength(char_id, sys_id, s);
                            if target_str_opt == Some(s.signature_id.as_str())
                                && eff_strength >= 1.0
                                && !s.collapsed
                            {
                                return Some(
                                    crate::services::space_destiny::space_pochven_wormholes::pochven_wormhole_ball_position(
                                        &ctx.sde, s,
                                    ),
                                );
                            }
                        }
                    }
                    None
                });
            }
            if resolved_pos.is_none() && !matches!(stuff_type, "epinstance" | "ep_instance") {
                resolved_pos = ctx.db.with_snapshot(|snap| {
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
                                let base_pos = crate::services::space_destiny::space_pochven_wormholes::pochven_wormhole_ball_position(&ctx.sde, s);
                                if crate::services::wormhole_lifecycle_ops::is_drifter_wormhole(&ctx.sde, s) {
                                    return Some(base_pos + DVec3::new(80_000.0, 0.0, 0.0));
                                }
                                return Some(base_pos);
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
    let is_unresolved_sig_ball = target_id > 0
        && !ctx.has_scanned_signature(char_id, sys_id, target_id)
        && ctx.db.with_snapshot(|snap| {
            snap.cosmic_signatures.get(&sys_id).is_some_and(|sigs| {
                sigs.iter().any(|s| {
                    wormhole_item_id_for_signature(sys_id, &s.signature_id) == target_id
                        && ctx.effective_signature_signal_strength(char_id, sys_id, s) < 1.0
                })
            })
        });
    if resolved_pos.is_none() && target_id > 0 && target_id != ship_id && !is_unresolved_sig_ball {
        if let Ok(Some((_, tb))) = handle.get_ball_snapshot(target_id).await {
            let is_asteroid = ctx
                .sde
                .types
                .get(&tb.type_id)
                .is_some_and(|t| t.category_id == 25);
            if matches!(tb.mode, MovementMode::Rigid) || is_asteroid {
                let sys_wh_class = crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&ctx.sde, sys_id).0;
                let is_drifter_ball = matches!(tb.type_id, 34_494 | 35_650..=35_654)
                    || ((14..=18).contains(&sys_wh_class)
                        && ctx.sde.types.get(&tb.type_id).is_some_and(|t| t.group_id == 988));
                resolved_pos = Some(if is_drifter_ball {
                    tb.position + DVec3::new(80_000.0, 0.0, 0.0)
                } else {
                    tb.position
                });
            }
        }
    }
    if let Some(dest_pos) = resolved_pos.filter(|_| stuff_type != "bookmark") {
        crate::services::pve_dungeon_runtime_rpc::check_warp_target_blocked_by_gated_dungeon_room(
            ctx,
            sys_id,
            dest_pos,
            (target_id > 0).then_some(target_id),
        )?;
    } else if target_id > 0 && stuff_type != "bookmark" {
        if let Ok(Some((_, tb))) = handle.get_ball_snapshot(target_id).await {
            crate::services::pve_dungeon_runtime_rpc::check_warp_target_blocked_by_gated_dungeon_room(
                ctx,
                sys_id,
                tb.position,
                Some(target_id),
            )?;
        }
    }
    let Some(anchor_pos) = resolved_pos else {
        return Err(BridgeRpcError {
            code: "InvalidWarpDestination".into(),
            message: format!(
                "Warp target ({stuff_type}, {target_id}) is not a valid warpable object in system {sys_id}"
            ),
            client_error: None,
        });
    };
    crate::services::nav_warp_interdiction::ensure_not_in_active_warp(&current_ball.mode)?;

    let preview_dir = (anchor_pos - current_ball.position).normalize_or_zero();
    let preview_dest = anchor_pos - preview_dir * min_range;
    crate::services::pve_dungeon_runtime::check_deadspace_warp_allowed(
        ctx,
        sys_id,
        current_ball.position.to_array(),
        anchor_pos.to_array(),
        preview_dest.to_array(),
    )?;
    crate::services::pve_ess_reserve_banks::check_ess_warp_destination_allowed(
        ctx,
        sys_id,
        anchor_pos,
        preview_dest,
    )
    .await?;

    let landing_pos = crate::services::nav_warp_interdiction::resolve_warp_landing_point(
        ctx,
        sys_id,
        target_id,
        current_ball.position,
        anchor_pos,
    );
    let landing_dist = current_ball.position.distance(landing_pos);

    let validated = ctx
        .validate_ship_movement(
            char_id,
            ship_id,
            crate::services::space_nav_movement_guard::MovementIntent::Warp { sys_id },
        )
        .await?;
    let is_covert_cloaked = validated.is_covert_cloaked;

    let warp_dir = (landing_pos - current_ball.position).normalize_or_zero();
    let mut dest_pos = landing_pos - warp_dir * min_range;
    let requested_dist_m = current_ball.position.distance(dest_pos);
    let effective_warp_dist_m = (landing_dist - min_range).min(requested_dist_m);
    if effective_warp_dist_m < 150_000.0 {
        return Err(BridgeRpcError {
            code: "WarpDistanceTooShort".into(),
            message: format!(
                "Cannot warp {:.1} m (minimum warp distance is 150,000 m)",
                effective_warp_dist_m.max(0.0)
            ),
            client_error: None,
        });
    }

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
        let mut repaired_max_cap = false;
        if (cs.max_capacitor - 250.0).abs() < 1e-6
            && (cs.capacitor - 250.0).abs() < 1e-6
            && sde_max_cap > 250.0
        {
            cs.max_capacitor = sde_max_cap;
            cs.capacitor = sde_max_cap;
            repaired_max_cap = true;
        }
        let old_cap = cs.capacitor;
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
                    client_error: None,
                });
            }
            dest_pos = current_ball.position + warp_dir * affordable_dist_m;
            cs.capacitor = 0.0;
        } else {
            cs.capacitor = (cs.capacitor - req_cap).max(0.0);
        }
        if repaired_max_cap {
            let new_cap = cs.capacitor;
            let new_max_cap = cs.max_capacitor;
            let _ = handle
                .modify_combat_state(ship_id, move |live_cs| {
                    live_cs.max_capacitor = new_max_cap;
                    live_cs.capacitor = new_cap;
                })
                .await;
        } else {
            let _ = handle.set_capacitor(ship_id, cs.capacitor).await;
        }
        let _ = ctx.notify_ship_capacitor_change(
            char_id,
            ship_id,
            cs.capacitor,
            cs.max_capacitor,
            Some(old_cap),
        );
    }

    let warp_speed = compute_pilot_warp_speed_au_s(ctx, char_id, sys_id, current_ball.type_id);
    let mode = MovementMode::new_warp(current_ball.position, dest_pos, warp_speed);
    if !is_covert_cloaked {
        let _ = crate::services::notify_emitters_seasons_tactical::break_gate_cloak_and_notify(ctx, &handle, char_id, ship_id).await;
    }
    let target_belt_id = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 9 && (c.celestial_id == target_id || c.position.distance(dest_pos) <= 200_000.0))
        .map(|c| c.celestial_id);
    if target_belt_id.is_some() {
        let _ = handle.populate_asteroid_belt_entities().await;
    }
    match handle.set_movement_mode(ship_id, mode).await {
        Ok(tick) => {
            // [plan:DG-4] Entering warp halts active non-warp module cycles (weapons, scrams, webs, mining lasers, etc.)
            crate::services::nav_warp_interdiction::on_warp_initiated_hooks(
                ctx, &handle, char_id, ship_id, sys_id,
            )
            .await;
            let _ = crate::sp6_lifecycle::spawn_mission_wave_on_pocket_entry_if_needed(
                ctx, char_id, sys_id, dest_pos,
            );
            crate::services::pve_dungeon_runtime::on_warp_start_leave_dungeon_if_needed(
                ctx, char_id, sys_id, current_ball.position.to_array(), dest_pos.to_array(),
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
            ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(char_id as u32, &["timesWarped"], 1, ctx.now_unix(), &ctx.quasar_gateway);
            crate::services::operations_manager_helpers::record_server_operation_event(
                ctx,
                char_id,
                "warpToLocation",
                &[],
                1,
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
                client_error: None,
            })
        }
    }
}

async fn handle_cmd_stargate_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if let Some(ship_id) = session.ship_id {
        crate::services::structure_control_ops::reject_if_controlling_structure(ctx, ship_id, "jump")?;
    }
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot jump stargate while docked".into(),
            client_error: None,
        });
    }
    let old_sys = resolve_bound_or_session_sys(bound_entry, session)?;
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
            client_error: None,
        });
    };
    let entry_db_state = ctx.db.with_snapshot(|s| {
        s.characters
            .get(&char_id)
            .map(|c| (c.station_id, c.structure_id, c.solar_system_id))
    });
    let entry_db_sys = entry_db_state.map(|(_, _, sys)| sys).unwrap_or(old_sys);
    let entry_db_undocked = entry_db_state.is_some_and(|(st, sr, _)| st.is_none() && sr.is_none());
    crate::services::space_nav_movement_guard::validate_pre_movement_locks(
        ctx,
        ship_id,
        &crate::services::space_nav_movement_guard::MovementIntent::StargateJump {
            from_sys: old_sys,
            to_sys: old_sys,
            gate_id: 0,
        },
    )?;
    let Some(gate_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "CmdStargateJump requires integer stargateID".into(),
            client_error: None,
        });
    };
    let Some(jump) = ctx.sde.jumps.get(&gate_id) else {
        return Err(BridgeRpcError {
            code: "UnknownStargate".into(),
            message: format!("Stargate {gate_id} does not exist in SDE"),
            client_error: None,
        });
    };
    if jump.from_solar_system_id != old_sys {
        return Err(BridgeRpcError {
            code: "StargateWrongSystem".into(),
            message: format!(
                "Stargate {gate_id} is in system {}, not current system {old_sys}",
                jump.from_solar_system_id
            ),
            client_error: None,
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
                client_error: None,
            })
        }
    }) else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship ball is not in space".into(),
            client_error: None,
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
            client_error: None,
        });
    }

    let gate_ball_radius = old_handle
        .get_ball_snapshot(gate_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.radius)
        .filter(|&r| r > 0.0);
    let gate_radius = gate_ball_radius.unwrap_or_else(|| {
        ctx.sde
            .types
            .get(&16)
            .map(|t| t.radius)
            .filter(|&r| r > 0.0)
            .unwrap_or(14_051.0)
    });
    let warp_perimeter = evaluate_stargate_jump_perimeter(&current_ball, jump.position, gate_radius);
    let allow_arriving_warp = matches!(warp_perimeter, Ok(true));

    // Pochven Triglavian Clade Home Systems & Zarzakh Horizon Siege checked after unified movement validation.
    ctx.validate_ship_movement_with_warp_arrival(
        char_id,
        ship_id,
        crate::services::space_nav_movement_guard::MovementIntent::StargateJump {
            from_sys: old_sys,
            to_sys: jump.to_solar_system_id,
            gate_id,
        },
        allow_arriving_warp,
    )
    .await?;

    // DERIVED: Pochven Triglavian Clade Home Systems (Kino, Niarja, Archee in region 10_000_070) require +6.0 Triglavian standing to jump stargates
    let is_pochven_home_system = ctx.sde.solar_systems.get(&jump.to_solar_system_id).is_some_and(|sys| {
        sys.region_id == 10_000_070 && matches!(sys.name.as_str(), "Kino" | "Niarja" | "Archee")
    });
    if is_pochven_home_system {
        let trig_standing = ctx.db.with_snapshot(|s| {
            s.characters
                .get(&char_id)
                .and_then(|c| c.standings.get(&500_026).copied())
                .unwrap_or(0.0)
        });
        if trig_standing < 6.0 {
            return Err(BridgeRpcError::user_error("SystemCheck_JumpFailed_StandingsTooLow"));
        }
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
                    client_error: None,
                });
            }
        }
    }

    let arrived_at_warp_dest = warp_perimeter?;

    let dest_sys_id = jump.to_solar_system_id;
    let dest_gate_pos = ctx
        .sde
        .jumps
        .get(&jump.destination_stargate_id)
        .map(|j| j.position)
        .unwrap_or(DVec3::ZERO);
    let arrival_pos = dest_gate_pos + DVec3::new(gate_radius + 1_500.0, 0.0, 0.0);

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
    let Some(taken) = old_handle.take_ball(ship_id).await else {
        return Err(BridgeRpcError {
            code: "SystemTransitionFailed".into(),
            message: format!("Ship {ship_id} is no longer in solar system {old_sys}"),
            client_error: None,
        });
    };
    let prev_combat_state = taken.combat_state.clone();

    // Zarzakh Gate Toll & Gate Lock (`feat:navigation.zarzakh-gate-toll`):
    // Validated and debited BEFORE mutating character solar system state (`P2` rollback safety).
    if dest_sys_id == SOLAR_SYSTEM_ZARZAKH {
        if let Err(e) = crate::services::space_navigation_advanced::process_zarzakh_gate_toll(
            ctx,
            char_id,
            ship_id,
            gate_id,
            jump.destination_stargate_id,
            &current_ball,
            args,
            kwargs,
        ) {
            let _ = old_handle.restore_ball(taken).await;
            return Err(e);
        }
    }

    let updated_ch_res = if entry_db_undocked {
        ctx.db
            .transition_character_solar_system_if_at(char_id, entry_db_sys, dest_sys_id, &ctx.sde)
            .map_err(|e| BridgeRpcError {
                code: "SystemTransitionFailed".into(),
                message: e.to_string(),
                client_error: None,
            })
    } else {
        ctx.db
            .try_with_mut_snapshot(|snap| {
                let (const_id, reg_id) = ctx
                    .sde
                    .solar_systems
                    .get(&dest_sys_id)
                    .map(|s| (s.constellation_id, s.region_id))
                    .unwrap_or((20_000_020, 10_000_002));
                let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: format!("Character {char_id} not found"),
                    client_error: None,
                })?;
                if entry_db_state.is_some_and(|e| (ch.station_id, ch.structure_id, ch.solar_system_id) != e) {
                    return Err(BridgeRpcError {
                        code: "SystemTransitionFailed".into(),
                        message: format!("Character {char_id} transitioned concurrently"),
                        client_error: None,
                    });
                }
                ch.solar_system_id = dest_sys_id;
                ch.constellation_id = const_id;
                ch.region_id = reg_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.version += 1;
                ch.system_epoch += 1;
                let active_ship_id = ch.active_ship_id;
                let res = ch.clone();
                if let Some(sh) = snap.items.get_mut(&active_ship_id) {
                    sh.location_id = dest_sys_id as i64;
                    sh.version += 1;
                }
                if ship_id != active_ship_id {
                    if let Some(sh) = snap.items.get_mut(&ship_id) {
                        sh.location_id = dest_sys_id as i64;
                        sh.version += 1;
                    }
                }
                Ok(res)
            })
    };
    let updated_ch = match updated_ch_res {
        Ok(ch) => ch,
        Err(e) => {
            let _ = old_handle.restore_ball(taken).await;
            return Err(e);
        }
    };
    ctx.db.with_mut_snapshot(|snap| {
        crate::sp6_lifecycle::leave_space_in_snap(
            snap,
            ship_id,
            prev_combat_state.as_ref(),
            Some(dest_sys_id),
            None,
        );
    });
    if arrived_at_warp_dest {
        let _ = old_handle.force_stop_warp(ship_id);
        ctx.active_warps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ship_id);
    }
    deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
    crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, ship_id);

    let mut zarzakh_lock_expiry = None;
    if dest_sys_id == SOLAR_SYSTEM_ZARZAKH {
        let expiry = now_unix.saturating_add(ZARZAKH_GATE_LOCK_SECS);
        ctx.db.with_mut_snapshot(|snap| {
            snap.zarzakh_gate_locks
                .insert(char_id, (jump.destination_stargate_id, expiry));
        });
        zarzakh_lock_expiry = Some(expiry);
    }
    crate::services::notify_emitters_space_dungeons::emit_exiting_dungeon_on_warp_if_present(
        ctx, char_id, old_sys, current_ball.position.to_array(),
    );
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, old_sys);
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.authorized_shipping_storage.retain(|&(_, c)| c != char_id);
    });
    ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);

    let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
    let mut new_ball = taken.ball;
    new_ball.position = arrival_pos;
    new_ball.velocity = DVec3::ZERO;
    new_ball.speed_fraction = 0.0;
    new_ball.mode = MovementMode::Stop;
    let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
    crate::services::r6_t1_p1_helpers::apply_preserved_combat_state(&mut combat_state, prev_combat_state);
    combat_state.is_cloaked = true;
    combat_state.gate_cloak_remaining_ticks = 60;
    combat_state.undock_invulnerable_remaining_ticks = 60;
    combat_state.sync_invuln_with_undock_timer = true;
    let mut slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
    slim_meta.is_cloaked = true;
    let spawn_tick = new_handle.spawn_ball_with_meta(new_ball, combat_state, slim_meta).await.unwrap_or(0);
    let _ = ctx.living_world.materialize_system_lod2(dest_sys_id, &ctx.systems, &ctx.sde).await;
    let _ = new_handle.push_set_state(ship_id).await;
    crate::services::notify_emitters_space_dungeons::emit_jump_cloak_started(ctx, char_id, ship_id);
    crate::services::notify_emitters_seasons_tactical::emit_restoring_invuln_updated(ctx, char_id, ship_id, 600_000_000);

    let jump_spec = crate::router::SessionTransitionSpec::for_kind(
        char_id,
        crate::router::TransitionKind::JumpSystem { solar_system_id: updated_ch.solar_system_id },
        "stargate_jump",
    )
    .with_apply_db(false)
    .with_cleanup_departing_space(false)
    .with_sync_local_chat(false)
    .with_emit_on_session_changed(false)
    .with_preserve_session_ship(true);
    let _ = ctx.execute_session_transition_sync(Some(session), jump_spec);
    crate::services::map_service::record_solar_system_jump(ctx, char_id, dest_sys_id);
    crate::services::nav_warp_interdiction::advance_waypoints_on_arrival(
        ctx,
        char_id,
        dest_sys_id as i64,
    );
    let _ = crate::services::combat_npc_police::check_and_spawn_faction_police(ctx, dest_sys_id).await;
    crate::services::combat_crimewatch::schedule_highsec_criminal_concord_if_needed(
        ctx,
        char_id,
        ship_id,
        dest_sys_id,
        now_unix,
    );
    if let Some(expiry) = zarzakh_lock_expiry {
        let _ = crate::services::quasar_sov_services::publish_stargate_lock_activated_notice(
            &ctx.quasar_gateway, char_id, dest_sys_id, jump.destination_stargate_id as u64, expiry, now_unix,
        );
    }

    record_session_change_timer(ctx, char_id, now_unix, dest_sys_id, spawn_tick, "stargate_jump");

    ctx.quasar_gateway
        .progression
        .daily_goals
        .on_gameplay_event(char_id as u32, 4, 1, &ctx.quasar_gateway);
    ctx.quasar_gateway
        .progression
        .career_goals
        .on_gameplay_event(char_id as u32, "travel", 1, &ctx.quasar_gateway);
    ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(char_id as u32, &["stargateJumps"], 1, now_unix, &ctx.quasar_gateway);

    let new_beyonce_id = ctx.bind_object(session, BEYONCE_SVC, dest_sys_id as i64)?;

    Ok(PyRep::keyval([
        ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
        ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
        ("jumpCooldownMinutes", PyRep::Real(0.0)),
        ("jumpFatigueMinutes", PyRep::Real(0.0)),
        ("sessionChangeTimerSeconds", PyRep::Integer(10)),
    ]))
}

/// Evaluates the unconditional `gate_radius + 2,500 m` EVE stargate jump perimeter,
/// mirroring `evaluate_dock_perimeter`:
///
/// - When `ball.mode` is `MovementMode::Warp`, jumping is permitted (`Ok(true)`, signalling
///   that the active warp must be terminated on jump) only when `destination` lies inside
///   `gate_radius + 2,500 m` of `gate_pos` AND the ship is either already inside that range or
///   within the final `<= 4.0 s` of `WarpPhase::Decelerating` (`#1001`); all other warp states
///   return `ShipInWarp`.
/// - For non-warp movement modes, returns `OutsideStargateJumpRange` when `(dist - gate_radius).max(0.0) > 2_500.0`
///   and `Ok(false)` otherwise.
pub(crate) fn evaluate_stargate_jump_perimeter(
    ball: &eve_destiny::Ball,
    gate_pos: DVec3,
    gate_radius: f64,
) -> Result<bool, BridgeRpcError> {
    let max_jump_dist = gate_radius + 2_500.0;
    let dist = ball.position.distance(gate_pos);
    match ball.mode {
        MovementMode::Warp { destination, .. } => {
            if destination.distance(gate_pos) <= max_jump_dist
                && (dist <= max_jump_dist
                    || eve_destiny::physics::is_warp_finishing_within(ball, 4.0))
            {
                Ok(true)
            } else {
                Err(BridgeRpcError {
                    code: "ShipInWarp".into(),
                    message: "Cannot jump stargate while ship is still in warp".into(),
                    client_error: None,
                })
            }
        }
        _ => {
            let surface_dist = (dist - gate_radius).max(0.0);
            if surface_dist > 2_500.0 {
                Err(BridgeRpcError {
                    code: "OutsideStargateJumpRange".into(),
                    message: format!(
                        "Ship is {:.1} m from stargate (must be <= 2500 m)",
                        surface_dist
                    ),
                    client_error: None,
                })
            } else {
                Ok(false)
            }
        }
    }
}

/// Evaluates the unconditional `target_radius + 2,500 m` EVE docking perimeter (`SP-4`) across
/// `beyonce.CmdDock`, `stationSvc.Dock`, and `structureDocking.Dock`.
///
/// - When `ball.mode` is `MovementMode::Warp`, docking is permitted (`Ok(true)`, signalling that
///   the active warp must be terminated) only when `destination` lies inside `target_radius + 2,500 m`
///   of `target_pos` AND the ship is either already inside `max_dock_dist` or within the final
///   `<= 4.0 s` of `WarpPhase::Decelerating` (`eve_destiny::physics::is_warp_finishing_within`, `#1001`);
///   all other warp states return `ShipInWarp`.
/// - For non-warp movement modes, returns `OutsideDockingPerimeter` when `dist > max_dock_dist`
///   and `Ok(false)` otherwise, regardless of `session.address`.
pub(crate) fn evaluate_dock_perimeter(
    ball: &eve_destiny::Ball,
    target_pos: DVec3,
    target_radius: f64,
) -> Result<bool, BridgeRpcError> {
    let max_dock_dist = target_radius + 2_500.0;
    let dist = ball.position.distance(target_pos);
    match ball.mode {
        MovementMode::Warp { destination, .. } => {
            if destination.distance(target_pos) <= max_dock_dist
                && (dist <= max_dock_dist
                    || eve_destiny::physics::is_warp_finishing_within(ball, 4.0))
            {
                Ok(true)
            } else {
                Err(BridgeRpcError {
                    code: "ShipInWarp".into(),
                    message: "Cannot dock while ship is still in warp".into(),
                    client_error: None,
                })
            }
        }
        _ => {
            if dist > max_dock_dist {
                Err(BridgeRpcError::outside_docking_perimeter(dist, max_dock_dist))
            } else {
                Ok(false)
            }
        }
    }
}

async fn handle_dock(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if let Some(ship_id) = session.ship_id {
        crate::services::structure_control_ops::reject_if_controlling_structure(ctx, ship_id, "dock")?;
    }
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "AlreadyDocked".into(),
            message: "Character is already docked in a station or structure".into(),
            client_error: None,
        });
    }
    let Some(mut ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Cannot dock without an active ship in space".into(),
            client_error: None,
        });
    };
    let Some(dock_id) = args.first().and_then(|a| a.as_i64()) else {
        return Err(BridgeRpcError {
            code: "MissingLocationId".into(),
            message: "Dock requires an explicit station or structure ID argument".into(),
            client_error: None,
        });
    };
    let Some(sys_id) = bound_entry
        .map(|b| b.object_id as u32)
        .or(session.solar_system_id)
    else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solarSystemID".into(),
            client_error: None,
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
                client_error: None,
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
    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "dock")?;
    crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, ship_id, "dock")?;
    if args.get(1).and_then(|a| a.as_i64()).is_some_and(|req| req != ship_id) {
        return Err(BridgeRpcError {
            code: "InvalidShipId".into(),
            message: "Requested shipID does not match pilot's active ship".into(),
            client_error: None,
        });
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
            client_error: None,
        });
    }
    let Some((cur_tick, ball)) = ball_snap else {
        return Err(BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Ship {ship_id} is not spawned in solar system {sys_id} Ballpark"),
            client_error: None,
        });
    };

    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    let ship_type_id = ctx
        .db
        .with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id))
        .unwrap_or(ball.type_id);
    let ship_group_id = ctx
        .sde
        .types
        .get(&ship_type_id)
        .map(|t| t.group_id)
        .unwrap_or(0);
    let is_capsule = ship_type_id == 670 || ship_group_id == 29;

    let is_structure = dock_id >= 1_000_000_000_000
        || ctx.db.with_snapshot(|snap| snap.structures.contains_key(&dock_id));
    if let Err(e) = crate::services::fw_gameplay::check_docking_allowed(ctx, char_id, sys_id, dock_id, is_structure) {
        return Err(e);
    }

    {
        let cw = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner());
        if let Some(cw_state) = cw.states.get(&char_id) {
            if cw_state.is_weapon_timer_active(now_unix) {
                return Err(BridgeRpcError {
                    code: "WeaponsTimerActive".into(),
                    message: "Cannot dock while a 60s Weapons Timer is active".into(),
                    client_error: None,
                });
            }
            if sys_sec >= 0.45 && cw_state.is_criminal(now_unix) && !is_capsule {
                return Err(BridgeRpcError {
                    code: "CantDockGlobalCriminalFlag".into(),
                    message: "Cannot dock a non-capsule ship in high-sec while a Criminal Timer is active".into(),
                    client_error: None,
                });
            }
        }
    }

    if crate::services::combat_crimewatch::combat_npc_police::is_outlaw_ship_docking_refused(ctx, sys_id, char_id, ship_id) {
        return Err(BridgeRpcError {
            code: "FactionPoliceRefusedDocking".into(),
            message: "Faction police refuse docking permissions to outlaw ships in high-security space".into(),
            client_error: None,
        });
    }

    let (target_pos, sde_target_radius) = if is_structure {
        let st_opt = ctx
            .db
            .with_snapshot(|snap| snap.structures.get(&dock_id).cloned());
        let Some(st) = st_opt else {
            return Err(BridgeRpcError {
                code: "StructureNotFound".into(),
                message: format!("Structure {dock_id} does not exist"),
                client_error: None,
            });
        };
        if st.solar_system_id != sys_id {
            return Err(BridgeRpcError {
                code: "DockTargetWrongSystem".into(),
                message: format!(
                    "Structure {dock_id} is in system {}, but pilot is in {sys_id}",
                    st.solar_system_id
                ),
                client_error: None,
            });
        }
        if st.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "StructureDestroyed".into(),
                message: format!("Structure {dock_id} is destroyed"),
                client_error: None,
            });
        }
        if matches!(st.state.as_str(), "Unanchored" | "AnchorVulnerable" | "Anchoring") {
            return Err(BridgeRpcError {
                code: "StructureNotDockable".into(),
                message: format!("Structure {dock_id} is in state '{}' and cannot be docked at", st.state),
                client_error: None,
            });
        }
        crate::services::structure_acl_defense_rules::apply_pending_profile_applications(ctx);
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
                client_error: None,
            });
        }
        let is_medium_structure = matches!(st.type_id, 35832 | 35825 | 35835);
        let is_large_structure = matches!(st.type_id, 35833 | 35826 | 35836);
        let is_capital_ship =
            matches!(ship_group_id, 30 | 485 | 547 | 659 | 883 | 1538 | 4594); // freighters dock at all Upwell hulls
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
                client_error: None,
            });
        }
        let r = ctx
            .sde
            .types
            .get(&st.type_id)
            .map(|t| t.radius)
            .filter(|&r| r > 0.0)
            .unwrap_or(0.0);
        (DVec3::from_array(st.position), r)
    } else {
        crate::services::space_navigation_advanced::validate_npc_station_docking(ship_group_id, true)?;
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
                client_error: None,
            });
        };
        crate::services::station_structure_handlers::validate_npc_station_docking_gate(ctx, char_id, dock_id)?;
        let r = ctx
            .sde
            .types
            .get(&st.station_type_id)
            .map(|t| t.radius)
            .filter(|&r| r > 0.0)
            .unwrap_or(0.0);
        (st.position, r)
    };

    let target_radius = handle
        .get_ball_snapshot(dock_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or(sde_target_radius);
    let arrived_at_warp_dest = evaluate_dock_perimeter(&ball, target_pos, target_radius)?;
    if is_structure && handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
        return Err(BridgeRpcError {
            code: "ShipWarpScrambled".into(),
            message: "Cannot dock at an Upwell structure while ship is warp scrambled".into(),
            client_error: None,
        });
    }
    let Some(taken) = handle.take_ball_for_docking(ship_id, dock_id, target_pos).await else {
        return Err(BridgeRpcError {
            code: "DockFailed".into(),
            message: format!("Ship {ship_id} is no longer in space in solar system {sys_id}"),
            client_error: None,
        });
    };
    match ctx.db.dock_character_if_in_space(char_id, sys_id, dock_id, is_structure) {
        Ok(_) => {
            ctx.db.with_mut_snapshot(|snap| {
                crate::sp6_lifecycle::leave_space_in_snap(
                    snap,
                    ship_id,
                    taken.combat_state.as_ref(),
                    None,
                    None,
                );
            });
            if arrived_at_warp_dest {
                let _ = handle.force_stop_warp(ship_id);
                ctx.active_warps
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&ship_id);
            }
            crate::services::notify_emitters_space_dungeons::emit_exiting_dungeon_on_warp_if_present(
                ctx,
                char_id,
                sys_id,
                ball.position.to_array(),
            );
            let dock_spec = crate::router::SessionTransitionSpec::for_kind(
                char_id,
                crate::router::TransitionKind::Dock { dock_id, is_structure },
                "dock",
            )
            .with_apply_db(false)
            .with_cleanup_departing_space(false)
            .with_preserve_session_ship(true);
            let _ = ctx.execute_session_transition_sync(Some(session), dock_spec);
            deactivate_ship_modules_on_leave_space(ctx, char_id, ship_id);
            crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, ship_id);
            crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, sys_id);
            if let Some(ship_rec) = ctx.db.with_snapshot(|snap| snap.items.get(&ship_id).cloned()) {
                ctx.notify_item_record_change(
                    char_id,
                    &ship_rec,
                    vec![
                        (3, PyRep::Integer(sys_id as i64)),
                        (4, PyRep::Integer(eve_db::InventoryFlag::None as i64)),
                    ],
                );
            }
            record_session_change_timer(ctx, char_id, now_unix, sys_id, cur_tick, "dock");
            crate::services::nav_warp_interdiction::advance_waypoints_on_arrival(
                ctx, char_id, dock_id,
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnDockingAccepted",
                "charid",
                vec![PyRep::Integer(dock_id)],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnDockingFinished",
                "charid",
                vec![PyRep::Integer(dock_id)],
            );
            if is_structure {
                let (corp_id, ally_id, fac_id, st_type_id) = ctx.db.with_snapshot(|snap| {
                    let c = snap.characters.get(&char_id);
                    let corp = c.map(|ch| ch.corporation_id).unwrap_or(0);
                    let ally = c.and_then(|ch| ch.alliance_id);
                    let fac = c.and_then(|ch| ch.war_faction_id);
                    let st_tid = snap.structures.get(&dock_id).map(|s| s.type_id).unwrap_or(35832);
                    (corp, ally, fac, st_tid)
                });
                emit_structure_dock_notifications(
                    ctx, dock_id, char_id, corp_id, ally_id, fac_id, st_type_id, ship_id,
                );
            }
            ctx.quasar_gateway.on_session_changed(session, now_unix);
            ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(char_id as u32, &["dockInStation"], 1, now_unix, &ctx.quasar_gateway);
            Ok(PyRep::keyval([
                ("locationID", PyRep::Integer(dock_id)),
                ("isStructure", PyRep::Bool(is_structure)),
                ("sessionChangeTimerSeconds", PyRep::Integer(10)),
            ]))
        }
        Err(e) => {
            let _ = handle.restore_ball(taken).await;
            Err(BridgeRpcError {
                code: "DockFailed".into(),
                message: e.to_string(),
                client_error: None,
            })
        }
    }
}

pub use crate::services::space_navigation_advanced::deactivate_ship_modules_on_leave_space;

#[cfg(test)] #[path = "space_warp_jump_unit_tests.rs"] mod space_warp_jump_unit_tests;
