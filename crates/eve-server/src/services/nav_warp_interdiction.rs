//! `nav_warp_interdiction` — Autopilot Waypoints, Celestial/Gate Warp Landing Points,
//! and Cargo Warp Vector Data Consumption (`feat:navigation.route-waypoints`,
//! `feat:navigation.warp-landing-points`, `feat:navigation.warp-vector`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::cluster_macho_misc_services::{persist_misc_state, with_misc_state};
use eve_db::InventoryFlag;
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

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
pub fn sync_waypoints_from_char_settings(ctx: &ServerContext, char_id: i64, val: &PyRep) {
    let parsed: Vec<i64> = match val {
        PyRep::List(items) | PyRep::Tuple(items) => items
            .iter()
            .filter_map(|v| v.as_i64())
            .filter(|&id| id > 0)
            .collect(),
        _ => Vec::new(),
    };
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.autopilot_waypoints = parsed.clone();
        }
    });
    sync_waypoints_to_char_settings(ctx, char_id, &parsed);
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
            .filter(|&id| id > 0)
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

/// Handles `map.SetWaypoint`, `map.SetWaypoints`, `map.RemoveWaypointIfFirst`, `map.ClearWaypoints`, and `map.GetWaypoints`.
pub fn handle_map_waypoint_rpc(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match method {
        "SetWaypoint" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let dest_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if !is_valid_waypoint_destination(ctx, dest_id) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidDestination".into(),
                    message: format!("Destination {dest_id} is not a known solar system, station, or structure"),
                }));
            }
            let clear_other = args
                .get(1)
                .and_then(|a| a.as_bool())
                .or_else(|| kwargs.get("clearOtherWaypoints").and_then(|v| v.as_bool()))
                .unwrap_or(true);
            let first = args
                .get(2)
                .and_then(|a| a.as_bool())
                .or_else(|| kwargs.get("first").and_then(|v| v.as_bool()))
                .unwrap_or(false);

            let updated_wps = ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if clear_other {
                        ch.autopilot_waypoints.clear();
                    }
                    if first {
                        ch.autopilot_waypoints.insert(0, dest_id);
                    } else {
                        ch.autopilot_waypoints.push(dest_id);
                    }
                    ch.autopilot_waypoints.clone()
                } else {
                    Vec::new()
                }
            });
            sync_waypoints_to_char_settings(ctx, char_id, &updated_wps);
            crate::services::notify_emitters_batch3::emit_destination_change(
                ctx,
                char_id,
                dest_id,
                clear_other,
                first,
            );
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnAutopilotUpdated",
                "charid",
                vec![],
            );
            Some(Ok(PyRep::List(
                updated_wps.into_iter().map(PyRep::Integer).collect(),
            )))
        }
        "SetWaypoints" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let items: Vec<i64> = match args.first() {
                Some(PyRep::List(v) | PyRep::Tuple(v)) => {
                    v.iter().filter_map(|x| x.as_i64()).collect()
                }
                _ => Vec::new(),
            };
            for &dest_id in &items {
                if !is_valid_waypoint_destination(ctx, dest_id) {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidDestination".into(),
                        message: format!(
                            "Destination {dest_id} is not a known solar system, station, or structure"
                        ),
                    }));
                }
            }
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.autopilot_waypoints = items.clone();
                }
            });
            sync_waypoints_to_char_settings(ctx, char_id, &items);
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnAutopilotUpdated",
                "charid",
                vec![],
            );
            Some(Ok(PyRep::List(
                items.into_iter().map(PyRep::Integer).collect(),
            )))
        }
        "RemoveWaypointIfFirst" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let dest_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let updated_wps = ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if ch.autopilot_waypoints.first().copied() == Some(dest_id) {
                        ch.autopilot_waypoints.remove(0);
                    }
                    ch.autopilot_waypoints.clone()
                } else {
                    Vec::new()
                }
            });
            sync_waypoints_to_char_settings(ctx, char_id, &updated_wps);
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnAutopilotUpdated",
                "charid",
                vec![],
            );
            Some(Ok(PyRep::List(
                updated_wps.into_iter().map(PyRep::Integer).collect(),
            )))
        }
        "ClearWaypoints" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.autopilot_waypoints.clear();
                }
            });
            sync_waypoints_to_char_settings(ctx, char_id, &[]);
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnAutopilotUpdated",
                "charid",
                vec![],
            );
            Some(Ok(PyRep::List(Vec::new())))
        }
        "GetWaypoints" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let wps = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.autopilot_waypoints.clone())
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::List(
                wps.into_iter().map(PyRep::Integer).collect(),
            )))
        }
        _ => None,
    }
}

/// Computes the exact warp landing point for `target_id` in `sys_id` (`feat:navigation.warp-landing-points`):
/// - Planets (`group 7`) and Moons (`group 8`): `2,500 m` outside celestial surface radius along approach vector.
/// - Stars (`group 6`): `100,000 m` outside star corona radius along approach vector.
/// - Stargates (`group 10` / `sde.jumps`): stargate perimeter (`gate_radius`) along approach vector (`0 m` from jump activation radius).
/// - Stations, Upwell Structures, Asteroid Belts (`group 9`), Bookmarks, Signatures, Anomalies: exact anchor position.
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
/// Supports both:
/// 1. Consumable Warp Vector Data items in ship cargo (`group_id == 4599`, e.g. `77825` `Jovian Symbolic Navigator`),
///    validating proximity to the system star (`attr 5432`), applying the directional vector delta
///    `(attr 5429, attr 5430, attr 5431)` with `>= 150 km` minimum warp distance, and consuming 1 unit.
/// 2. Local rigid warp vector beacon entities in space within `100 km`.
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

    // Check if `target_id` is an inventory item (consumable Warp Vector Data `group 4599` in ship cargo).
    let cargo_item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&target_id).cloned());
    if let Some(vec_item) = cargo_item_opt {
        if vec_item.owner_id != char_id
            || vec_item.location_id != ship_id
            || vec_item.flag != InventoryFlag::Cargo
            || vec_item.quantity <= 0
        {
            return Err(BridgeRpcError {
                code: "InvalidVectorItem".into(),
                message: format!(
                    "Warp vector item {target_id} must be in active ship {ship_id} cargo"
                ),
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

        let max_star_dist = vec_item
            .dynamic_attributes
            .get(&5432)
            .copied()
            .or_else(|| mod_t.default_attributes.get(&5432).copied())
            .unwrap_or(1_496_000_000_000.0);
        if let Some(star) = ctx
            .sde
            .celestials_in_system(sys_id)
            .into_iter()
            .find(|c| c.group_id == 6)
        {
            let star_dist = ship_ball.position.distance(star.position);
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
        let dist_m = delta.length();
        if !delta.is_finite() || dist_m < 150_000.0 {
            return Err(BridgeRpcError {
                code: "WarpDistanceTooShort".into(),
                message: format!(
                    "Warp vector distance {:.1} m is below minimum 150,000 m",
                    dist_m
                ),
            });
        }

        let dest_pos = ship_ball.position + delta;
        let warp_speed =
            crate::services::space_destiny::compute_ship_warp_speed_au_s(ctx, ship_ball.type_id);
        let warp_mode = MovementMode::new_warp(ship_ball.position, dest_pos, warp_speed);
        sys_handle
            .set_movement_mode(ship_id, warp_mode)
            .await
            .map_err(|e| BridgeRpcError {
                code: "WarpFailed".into(),
                message: e,
            })?;

        let remaining_qty = ctx.db.with_mut_snapshot(|snap| {
            let mut rem = 0;
            if let Some(it) = snap.items.get_mut(&target_id) {
                it.quantity = (it.quantity - 1).max(0);
                it.version += 1;
                rem = it.quantity;
            }
            if rem == 0 {
                snap.items.remove(&target_id);
            }
            rem
        });

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

        return Ok(PyRep::keyval([
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
        ]));
    }

    // Fallback: Space warp vector beacon entity within 100 km (`sov_services.rs` legacy beacon path).
    let (_, target_ball) = sys_handle
        .get_ball_snapshot(target_id)
        .await
        .map_err(|e| BridgeRpcError {
            code: "DestinyQueryFailed".into(),
            message: e,
        })?
        .ok_or_else(|| BridgeRpcError {
            code: "VectorItemNotFound".into(),
            message: format!("Warp vector entity {target_id} is not in space in solar system {sys_id}"),
        })?;

    if ctx
        .sde
        .types
        .get(&target_ball.type_id)
        .is_some_and(|t| t.category_id == 6)
    {
        return Err(BridgeRpcError {
            code: "InvalidVectorTarget".into(),
            message: format!("Entity {target_id} is a ship, not a valid warp vector celestial"),
        });
    }

    let dist = (target_ball.position - ship_ball.position).length();
    const MAX_VECTOR_PROXIMITY_M: f64 = 100_000.0;
    if dist > MAX_VECTOR_PROXIMITY_M {
        return Err(BridgeRpcError {
            code: "TargetNotInRange".into(),
            message: format!(
                "Warp vector entity {target_id} is out of activation range ({dist:.1} m > {MAX_VECTOR_PROXIMITY_M:.1} m)"
            ),
        });
    }

    const MAX_VECTOR_DISPLACEMENT_M: f64 = 100_000.0;
    let explicit_vec = match args.get(1) {
        Some(PyRep::Tuple(seq) | PyRep::List(seq)) if seq.len() >= 3 => {
            let v = DVec3::new(
                seq[0].as_f64().unwrap_or(0.0),
                seq[1].as_f64().unwrap_or(0.0),
                seq[2].as_f64().unwrap_or(0.0),
            );
            if !v.is_finite() {
                return Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Vector coordinates must be finite".into(),
                });
            }
            Some(v)
        }
        _ => None,
    };

    let dest_pos = if let Some(v) = explicit_vec.filter(|v| v.length() > 1e-6) {
        if v.length() > MAX_VECTOR_DISPLACEMENT_M {
            ship_ball.position + v.normalize() * MAX_VECTOR_DISPLACEMENT_M
        } else if v.length() >= 1_000.0 {
            ship_ball.position + v
        } else {
            ship_ball.position + v.normalize() * MAX_VECTOR_DISPLACEMENT_M
        }
    } else {
        let delta = target_ball.position - ship_ball.position;
        if delta.length() > 1.0 {
            target_ball.position + delta.normalize() * MAX_VECTOR_DISPLACEMENT_M
        } else {
            target_ball.position + DVec3::new(MAX_VECTOR_DISPLACEMENT_M, 0.0, 0.0)
        }
    };

    let warp_speed =
        crate::services::space_destiny::compute_ship_warp_speed_au_s(ctx, ship_ball.type_id);
    let warp_mode = MovementMode::new_warp(ship_ball.position, dest_pos, warp_speed);
    let _ = sys_handle.set_movement_mode(ship_id, warp_mode).await;

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
