//! `nav_warp_interdiction` — Autopilot Waypoints, Celestial/Gate Warp Landing Points,
//! and Cargo Warp Vector Data Consumption (`feat:navigation.route-waypoints`,
//! `feat:navigation.warp-vector`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::cluster_macho_misc_services::{persist_misc_state, with_misc_state};
use eve_db::InventoryFlag;
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

/// Zero Point Envelope landing standoff (`2,000 m` outside target ball radius, inside the `radius + 2,500 m` `CmdDock` docking perimeter).
pub const STATION_DOCKING_RING_STANDOFF_M: f64 = 2_000.0;

/// Per-pilot reactivation cooldown (`300 s` = 5 min) after consuming a Warp Vector Data item.
pub const WARP_VECTOR_COOLDOWN_SECS: u64 = 300;

/// Mirrors `CharacterRecord.autopilot_waypoints` into `CharacterSettingsSvc` (`"waypoints"`),
/// removing the key when empty so fresh characters retain an empty settings dictionary.
pub fn sync_waypoints_to_char_settings(ctx: &ServerContext, char_id: i64, waypoints: &[i64]) {
    with_misc_state(ctx, |st| {
        if waypoints.is_empty() {
            if let Some(m) = st.char_settings.get_mut(&char_id) {
                m.remove("waypoints");
            }
        } else {
            let list = PyRep::List(waypoints.iter().map(|&w| PyRep::Integer(w)).collect());
            st.char_settings
                .entry(char_id)
                .or_default()
                .insert("waypoints".to_owned(), list);
        }
    });
    persist_misc_state(ctx);
}

/// Syncs `CharacterRecord.autopilot_waypoints` when a client saves `"waypoints"` via `charMgr.SaveCharacterSetting`.
pub fn sync_waypoints_from_char_settings(
    ctx: &ServerContext,
    char_id: i64,
    val: &PyRep,
) -> Result<(), BridgeRpcError> {
    let parsed: Vec<i64> = match val {
        PyRep::List(items) | PyRep::Tuple(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let dest_id = item.as_i64().unwrap_or(0);
                if !is_valid_waypoint_destination(ctx, dest_id) {
                    return Err(BridgeRpcError {
                        code: "InvalidDestination".into(),
                        message: format!(
                            "Destination {dest_id} is not a known solar system, station, or structure"
                        ),
                    });
                }
                out.push(dest_id);
            }
            out
        }
        _ => Vec::new(),
    };
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.autopilot_waypoints = parsed.clone();
        }
    });
    sync_waypoints_to_char_settings(ctx, char_id, &parsed);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnAutopilotUpdated",
        "charid",
        vec![],
    );
    Ok(())
}

/// Reconciles `CharacterRecord.autopilot_waypoints` and `char_settings["waypoints"]` on character login.
pub fn sync_waypoints_on_login(ctx: &ServerContext, char_id: i64) {
    let db_wps = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| c.autopilot_waypoints.clone())
            .unwrap_or_default()
    });
    if !db_wps.is_empty() {
        sync_waypoints_to_char_settings(ctx, char_id, &db_wps);
        return;
    }
    let settings_val = with_misc_state(ctx, |st| {
        st.char_settings
            .get(&char_id)
            .and_then(|m| m.get("waypoints"))
            .cloned()
    });
    if let Some(PyRep::List(items) | PyRep::Tuple(items)) = settings_val {
        let wps: Vec<i64> = items
            .iter()
            .filter_map(|v| v.as_i64())
            .filter(|&id| is_valid_waypoint_destination(ctx, id))
            .collect();
        if !wps.is_empty() {
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.autopilot_waypoints = wps;
                }
            });
        }
    }
}

/// Advances the character's autopilot waypoint queue upon arriving at `arrived_location_id`
/// (solar system on stargate jump, or station/structure on dock), syncing `CharacterSettingsSvc`
/// and emitting `OnAutopilotUpdated`.
pub fn advance_waypoints_on_arrival(ctx: &ServerContext, char_id: i64, arrived_location_id: i64) {
    let updated_opt = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id)?;
        let pos = ch
            .autopilot_waypoints
            .iter()
            .position(|&w| w == arrived_location_id)?;
        ch.autopilot_waypoints.drain(0..=pos);
        Some(ch.autopilot_waypoints.clone())
    });
    if let Some(updated_wps) = updated_opt {
        sync_waypoints_to_char_settings(ctx, char_id, &updated_wps);
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnAutopilotUpdated",
            "charid",
            vec![],
        );
    }
}

/// Returns `true` when `dest_id` refers to a known solar system, NPC station, or Upwell structure.
pub fn is_valid_waypoint_destination(ctx: &ServerContext, dest_id: i64) -> bool {
    if dest_id <= 0 {
        return false;
    }
    let sys_hit = u32::try_from(dest_id)
        .ok()
        .is_some_and(|sid| ctx.sde.solar_systems.contains_key(&sid));
    sys_hit
        || ctx.sde.stations.contains_key(&dest_id)
        || ctx.db.with_snapshot(|snap| snap.structures.contains_key(&dest_id))
}

/// Computes the exact warp landing point for `target_id` in `sys_id`:
/// - Planets (`group 7`) and Moons (`group 8`): `2,500 m` outside celestial surface radius along approach vector.
/// - Stars (`group 6`): `100,000 m` outside star corona radius along approach vector.
/// - Stargates (`group 10` / `sde.jumps`): stargate perimeter (`gate_radius`) along approach vector (`0 m` from jump activation radius).
/// - NPC Stations (`sde.stations`) and Upwell Structures (`snap.structures`): docking ring perimeter (`2,000 m` along approach vector, inside `2,500 m` `CmdDock` range).
/// - Asteroid Belts (`group 9`), Bookmarks, Signatures, Anomalies: exact anchor position.
pub fn resolve_warp_landing_point(
    ctx: &ServerContext,
    sys_id: u32,
    target_id: i64,
    ship_pos: DVec3,
    anchor_pos: DVec3,
) -> DVec3 {
    let dir = (anchor_pos - ship_pos).normalize_or_zero();
    if dir.length_squared() <= 1e-12 {
        return anchor_pos;
    }
    if ctx
        .sde
        .jumps
        .get(&target_id)
        .is_some_and(|j| j.from_solar_system_id == sys_id)
    {
        let gate_radius = ctx
            .sde
            .types
            .get(&16)
            .map(|t| t.radius)
            .filter(|&r| r > 0.0)
            .unwrap_or(14_051.0);
        return anchor_pos - dir * gate_radius;
    }
    if let Some(st) = ctx
        .sde
        .stations
        .get(&target_id)
        .filter(|st| st.solar_system_id == sys_id)
    {
        let anchor_radius = ctx
            .sde
            .types
            .get(&st.station_type_id)
            .map(|t| t.radius)
            .filter(|&r| r > 0.0)
            .unwrap_or(0.0);
        return anchor_pos - dir * (anchor_radius + STATION_DOCKING_RING_STANDOFF_M);
    }
    if let Some(struct_type_id) = ctx.db.with_snapshot(|snap| {
        snap.structures
            .get(&target_id)
            .filter(|st| st.solar_system_id == sys_id && st.state != "Destroyed")
            .map(|st| st.type_id)
    }) {
        let anchor_radius = ctx
            .sde
            .types
            .get(&struct_type_id)
            .map(|t| t.radius)
            .filter(|&r| r > 0.0)
            .unwrap_or(0.0);
        return anchor_pos - dir * (anchor_radius + STATION_DOCKING_RING_STANDOFF_M);
    }
    if let Some(cel) = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.celestial_id == target_id)
    {
        if matches!(cel.group_id, 7 | 8) {
            let standoff = cel.radius.max(0.0) + 2_500.0;
            return anchor_pos - dir * standoff;
        }
        if cel.group_id == 6 {
            let standoff = cel.radius.max(0.0) + 100_000.0;
            return anchor_pos - dir * standoff;
        }
    }
    anchor_pos
}

/// `warpVectorMgr.UseVector(itemID)` (`feat:navigation.warp-vector`):
/// Validates consumable Warp Vector Data items in ship cargo (`group_id == 4599`, e.g. `77825`
/// `Jovian Symbolic Navigator`), checking proximity to the system star (`attr 5432`), applying the
/// star-centered directional vector delta `(attr 5429, attr 5430, attr 5431)` with `>= 150 km`
/// minimum warp distance, and atomically reserving 1 unit + 300s per-character cooldown.
pub async fn handle_use_warp_vector(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "ShipDocked".into(),
            message: "Cannot activate a warp vector while docked".into(),
        });
    }
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Session has no active ship in space".into(),
    })?;
    let sys_id = session.solar_system_id.ok_or_else(|| BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "Session has no active solar system".into(),
    })?;
    let target_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&id| id > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "warpVectorMgr.UseVector requires a positive vector itemID".into(),
        })?;
    if target_id == ship_id {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "Cannot activate a warp vector targeting your own active ship".into(),
        });
    }

    crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "warp")?;
    crate::services::space_navigation_advanced::check_immobilizing_module_active(
        ctx, ship_id, "warp",
    )?;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let is_scrambled = sys_handle.is_ball_scrambled_sync(ship_id)
        || sys_handle.is_ball_scrambled(ship_id).await.unwrap_or(false)
        || sys_handle
            .get_combat_state(ship_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|cs| cs.is_warp_scrambled());
    if is_scrambled {
        return Err(BridgeRpcError {
            code: "WarpScrambled".into(),
            message: "WarpScrambled: Warp drive disabled by warp disruption/scrambler effect".into(),
        });
    }
    if let Ok(Some(cs_check)) = sys_handle.get_combat_state(ship_id).await {
        if cs_check.is_cloaked
            && cs_check.gate_cloak_remaining_ticks == 0
            && !cs_check.is_covert_cloak
        {
            return Err(BridgeRpcError {
                code: "CloakedCannotWarp".into(),
                message: "Cannot initiate warp while cloaked without a Covert Ops Cloaking Device"
                    .into(),
            });
        }
    }

    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
        })?
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Active ship {ship_id} is not in space in solar system {sys_id}"),
        })?;

    // `warpVectorMgr.UseVector(itemID)` only accepts an owned Warp Vector Data item (`group 4599`) in active ship cargo.
    let star_opt = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .find(|c| c.group_id == 6);
    let star_pos = star_opt.as_ref().map(|c| c.position).unwrap_or(DVec3::ZERO);
    let has_star = star_opt.is_some();
    let sys_radius_opt = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.radius)
        .filter(|&r| r > 0.0);
    let now_unix = ctx.now_unix_secs();

    // Atomically validate the cargo item, cooldown, star proximity, and >= 150 km vector distance,
    // and reserve 1 unit + 300s per-character cooldown before awaiting `set_movement_mode`.
    let (dest_pos, dist_m, remaining_qty, prev_item, prev_cd_until) = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(vec_item) = snap.items.get(&target_id).cloned() else {
            return Err(BridgeRpcError {
                code: "InvalidVectorItem".into(),
                message: format!("Warp vector item {target_id} not found in active ship {ship_id} cargo"),
            });
        };
        if vec_item.owner_id != char_id
            || vec_item.location_id != ship_id
            || vec_item.flag != InventoryFlag::Cargo
            || vec_item.quantity <= 0
        {
            return Err(BridgeRpcError {
                code: "InvalidVectorItem".into(),
                message: format!("Warp vector item {target_id} must be in active ship {ship_id} cargo"),
            });
        }
        let mod_t = ctx.sde.types.get(&vec_item.type_id).ok_or_else(|| BridgeRpcError {
            code: "InvalidVectorItem".into(),
            message: format!("Unknown vector item type {}", vec_item.type_id),
        })?;
        if mod_t.group_id != 4599 {
            return Err(BridgeRpcError {
                code: "InvalidVectorItem".into(),
                message: format!(
                    "Item {} (group {}) is not a valid Warp Vector Data item (group 4599)",
                    mod_t.name, mod_t.group_id
                ),
            });
        }

        let mut cd_map: HashMap<i64, u64> = snap
            .service_state_blobs
            .get("warp_vector_cooldown_until")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        let prev_cd = cd_map.get(&char_id).copied();
        if prev_cd.unwrap_or(0) > now_unix {
            return Err(BridgeRpcError {
                code: "WarpVectorCooldown".into(),
                message: format!(
                    "Warp vector reactivation cooldown active for {} s",
                    prev_cd.unwrap_or(0) - now_unix
                ),
            });
        }

        let max_star_dist = vec_item
            .dynamic_attributes
            .get(&5432)
            .copied()
            .or_else(|| mod_t.default_attributes.get(&5432).copied())
            .unwrap_or(1_496_000_000_000.0);
        if has_star {
            let star_dist = ship_ball.position.distance(star_pos);
            if star_dist > max_star_dist {
                return Err(BridgeRpcError {
                    code: "WarpVectorTooFarFromStar".into(),
                    message: format!(
                        "Ship is {:.1} m from system star (maximum {:.1} m for warp vector activation)",
                        star_dist, max_star_dist
                    ),
                });
            }
        }

        let dx = vec_item
            .dynamic_attributes
            .get(&5429)
            .copied()
            .or_else(|| mod_t.default_attributes.get(&5429).copied())
            .unwrap_or(0.0);
        let dy = vec_item
            .dynamic_attributes
            .get(&5430)
            .copied()
            .or_else(|| mod_t.default_attributes.get(&5430).copied())
            .unwrap_or(0.0);
        let dz = vec_item
            .dynamic_attributes
            .get(&5431)
            .copied()
            .or_else(|| mod_t.default_attributes.get(&5431).copied())
            .unwrap_or(0.0);
        let delta = DVec3::new(dx, dy, dz);
        if !delta.is_finite() {
            return Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "Warp vector coordinates must be finite".into(),
            });
        }
        let mut dest_pos = star_pos + delta;
        if let Some(sys_radius) = sys_radius_opt {
            let rel = dest_pos - star_pos;
            if rel.length() > sys_radius {
                dest_pos = star_pos + rel.normalize_or_zero() * sys_radius;
            }
        }
        let dist_m = ship_ball.position.distance(dest_pos);
        if dist_m < 150_000.0 {
            return Err(BridgeRpcError {
                code: "WarpDistanceTooShort".into(),
                message: format!(
                    "Warp vector distance {:.1} m is below minimum 150,000 m",
                    dist_m
                ),
            });
        }

        cd_map.insert(char_id, now_unix.saturating_add(WARP_VECTOR_COOLDOWN_SECS));
        if let Ok(json) = serde_json::to_string(&cd_map) {
            snap.service_state_blobs
                .insert("warp_vector_cooldown_until".into(), json);
        }
        let mut rem = 0;
        if let Some(it) = snap.items.get_mut(&target_id) {
            it.quantity = (it.quantity - 1).max(0);
            it.version += 1;
            rem = it.quantity;
        }
        if rem == 0 {
            snap.items.remove(&target_id);
        }
        Ok((dest_pos, dist_m, rem, vec_item, prev_cd))
    })?;

    let warp_speed =
        crate::services::space_destiny::compute_ship_warp_speed_au_s(ctx, ship_ball.type_id);
    let warp_mode = MovementMode::new_warp(ship_ball.position, dest_pos, warp_speed);
    if let Err(e) = sys_handle.set_movement_mode(ship_id, warp_mode).await {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(target_id, prev_item);
            let mut cd_map: HashMap<i64, u64> = snap
                .service_state_blobs
                .get("warp_vector_cooldown_until")
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_default();
            if let Some(old_cd) = prev_cd_until {
                cd_map.insert(char_id, old_cd);
            } else {
                cd_map.remove(&char_id);
            }
            if let Ok(json) = serde_json::to_string(&cd_map) {
                snap.service_state_blobs
                    .insert("warp_vector_cooldown_until".into(), json);
            }
        });
        return Err(BridgeRpcError {
            code: "WarpFailed".into(),
            message: e,
        });
    }

    ctx.active_warps
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(ship_id, char_id);
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnWarpStarted",
        "charid",
        vec![
            PyRep::Integer(ship_id),
            PyRep::Integer(ship_ball.type_id as i64),
        ],
    );

    Ok(PyRep::keyval([
        ("shipID", PyRep::Integer(ship_id)),
        ("vectorItemID", PyRep::Integer(target_id)),
        ("itemID", PyRep::Integer(target_id)),
        ("consumed", PyRep::Bool(true)),
        ("remainingQuantity", PyRep::Integer(remaining_qty as i64)),
        ("distanceMeters", PyRep::Real(dist_m)),
        ("warpSpeedAuPerSec", PyRep::Real(warp_speed)),
        (
            "destination",
            PyRep::Tuple(vec![
                PyRep::Real(dest_pos.x),
                PyRep::Real(dest_pos.y),
                PyRep::Real(dest_pos.z),
            ]),
        ),
    ]))
}

async fn handle_get_session_change_timer(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let sys_id = session
        .solar_system_id
        .or_else(|| {
            ctx.db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
        })
        .unwrap_or(0);
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
    if let Some(active) = crate::services::space_destiny::check_session_change_timer_active(
        ctx,
        char_id,
        now_unix,
        cur_tick_opt,
    ) {
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

pub(crate) async fn handle_set_session_check(
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
    if let Some(active) = crate::services::space_destiny::check_session_change_timer_active(
        ctx,
        char_id,
        now_unix,
        cur_tick_opt,
    ) {
        return Err(BridgeRpcError {
            code: "SessionChangeInProgress".into(),
            message: format!(
                "Cannot perform session change '{reason}' while 10s timer from '{}' is active",
                active.reason
            ),
        });
    }
    let tick = cur_tick_opt.map(|(_, t)| t).unwrap_or(0);
    crate::services::space_destiny::record_client_session_check_timer(
        ctx, char_id, now_unix, sys_id, tick, reason,
    );
    Ok(PyRep::keyval([
        ("sessionChangeTimerSeconds", PyRep::Integer(10)),
        ("trigger", PyRep::String(reason.to_string())),
        ("expiresUnix", PyRep::Integer((now_unix + 10) as i64)),
    ]))
}
