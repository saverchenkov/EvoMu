//! `space_destiny` — Domain 5: Solar System `Ballpark`, Stargate Jumps, Probe Scanning, Wormholes, & Abyssal Filaments (`§3.5`).

#![forbid(unsafe_code)]

use crate::formulas::compute_jump_fatigue_and_cooldown_minutes;
use crate::router::{BoundObjectEntry, ServerContext};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("beyonce", "CmdGotoDirection") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match ServerContext::require_bound_system(bound_entry, "beyonce") {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let dx = args.first().and_then(|a| a.as_f64()).unwrap_or(1.0);
            let dy = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let dz = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);

            let handle = ctx.systems.get_or_spawn(sys_id);
            match handle
                .set_movement_mode(
                    ship_id,
                    MovementMode::GotoDirection {
                        direction: DVec3::new(dx, dy, dz),
                    },
                )
                .await
            {
                Ok(tick) => Some(Ok(PyRep::Integer(tick as i64))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyCommandFailed".into(),
                    message: e,
                })),
            }
        }

        ("beyonce", "CmdStop") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match ServerContext::require_bound_system(bound_entry, "beyonce") {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let handle = ctx.systems.get_or_spawn(sys_id);
            match handle.set_movement_mode(ship_id, MovementMode::Stop).await {
                Ok(tick) => Some(Ok(PyRep::Integer(tick as i64))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyCommandFailed".into(),
                    message: e,
                })),
            }
        }

        ("beyonce", "CmdSetSpeedFraction") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match ServerContext::require_bound_system(bound_entry, "beyonce") {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let fraction = args.first().and_then(|a| a.as_f64()).unwrap_or(1.0);
            if !fraction.is_finite() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSpeedFraction".into(),
                    message: "Speed fraction must be a finite number".into(),
                }));
            }
            let handle = ctx.systems.get_or_spawn(sys_id);
            match handle.set_speed_fraction(ship_id, fraction).await {
                Ok(tick) => Some(Ok(PyRep::Integer(tick as i64))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyCommandFailed".into(),
                    message: e,
                })),
            }
        }

        ("beyonce", "CmdOrbit") | ("beyonce", "CmdFollowBall") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match ServerContext::require_bound_system(bound_entry, "beyonce") {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let range = args.get(1).and_then(|a| a.as_f64()).unwrap_or(2_500.0);
            let mode = if method == "CmdOrbit" {
                MovementMode::Orbit { target_id, range }
            } else {
                MovementMode::Follow { target_id, range }
            };
            let handle = ctx.systems.get_or_spawn(sys_id);
            match handle.set_movement_mode(ship_id, mode).await {
                Ok(tick) => Some(Ok(PyRep::Integer(tick as i64))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyCommandFailed".into(),
                    message: e,
                })),
            }
        }

        ("beyonce", "CmdWarpToPoint") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match ServerContext::require_bound_system(bound_entry, "beyonce") {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let x = args.first().and_then(|a| a.as_f64()).unwrap_or(0.0);
            let y = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let z = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);
            if !x.is_finite() || !y.is_finite() || !z.is_finite() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidWarpDestination".into(),
                    message: "Warp destination coordinates must be finite".into(),
                }));
            }
            let dest_pos = DVec3::new(x, y, z);

            let handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, current_ball)) = (match handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyCommandFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship is not spawned in Ballpark".into(),
                }));
            };

            let warp_dist = current_ball.position.distance(dest_pos);
            if warp_dist < 150_000.0 {
                return Some(Err(BridgeRpcError {
                    code: "WarpDistanceTooShort".into(),
                    message: format!(
                        "Cannot warp {:.1} m (minimum warp distance is 150,000 m)",
                        warp_dist
                    ),
                }));
            }

            // Verify destination is within 100 km of a public celestial/station/stargate/structure/FW beacon,
            // a bookmark owned by `char_id`, a cosmic signature scanned to >=100% by `char_id`,
            // or a ball belonging to `char_id`'s accepted mission pocket or fleet members
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
                                        if m_ch.solar_system_id == sys_id
                                            && m_ch.station_id.is_none()
                                        {
                                            allowed_ball_ids.push(m_ch.active_ship_id);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    near_structure || near_bookmark || near_fw_beacon || near_scanned_sig
                });
            }
            if !is_valid_anchor && !allowed_ball_ids.is_empty() {
                if let Ok(balls) = handle.list_balls().await {
                    is_valid_anchor = balls.iter().any(|b| {
                        allowed_ball_ids.contains(&b.id)
                            && b.position.distance(dest_pos) <= 100_000.0
                    });
                }
            }
            if !is_valid_anchor {
                return Some(Err(BridgeRpcError {
                    code: "InvalidWarpDestination".into(),
                    message: format!(
                        "Destination ({x:.0}, {y:.0}, {z:.0}) does not match any known celestial, bookmark, or scanned object in system {sys_id}"
                    ),
                }));
            }

            let warp_speed = ctx
                .sde
                .types
                .get(&current_ball.type_id)
                .and_then(|t| t.default_attributes.get(&attr_ids::WARP_SPEED_MULTIPLIER))
                .copied()
                .unwrap_or(3.0);

            let mode =
                MovementMode::new_warp(current_ball.position, dest_pos, warp_speed);
            match handle.set_movement_mode(ship_id, mode).await {
                Ok(tick) => Some(Ok(PyRep::Integer(tick as i64))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyCommandFailed".into(),
                    message: e,
                })),
            }
        }

        ("beyonce", "CmdWarpToStuff") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match ServerContext::require_bound_system(bound_entry, "beyonce") {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let stuff_type = args.first().and_then(|a| a.as_str()).unwrap_or("item");
            let target_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, current_ball)) = (match handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyCommandFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship is not spawned in Ballpark".into(),
                }));
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
            let Some(dest_pos) = resolved_pos else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidWarpDestination".into(),
                    message: format!(
                        "Warp target ({stuff_type}, {target_id}) is not a valid warpable object in system {sys_id}"
                    ),
                }));
            };

            let warp_dist = current_ball.position.distance(dest_pos);
            if warp_dist < 150_000.0 {
                return Some(Err(BridgeRpcError {
                    code: "WarpDistanceTooShort".into(),
                    message: format!(
                        "Cannot warp {:.1} m (minimum warp distance is 150,000 m)",
                        warp_dist
                    ),
                }));
            }

            let warp_speed = ctx
                .sde
                .types
                .get(&current_ball.type_id)
                .and_then(|t| t.default_attributes.get(&attr_ids::WARP_SPEED_MULTIPLIER))
                .copied()
                .unwrap_or(3.0);

            let mode = MovementMode::new_warp(current_ball.position, dest_pos, warp_speed);
            match handle.set_movement_mode(ship_id, mode).await {
                Ok(tick) => Some(Ok(PyRep::Integer(tick as i64))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyCommandFailed".into(),
                    message: e,
                })),
            }
        }

        // parity: plan:P-7, plan:SP-1, plan:SP-2
        // parity: feat:navigation.destiny-updates, feat:navigation.celestials-balls, feat:navigation.bubbles-grid
        // parity: rpc:beyonce.AdminGetBubbleID
        // parity: destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall
        // parity: destiny:GotoDirection, destiny:FollowBall, destiny:Orbit, destiny:Stop
        // parity: notify:DoDestinyUpdate
        ("beyonce", "UpdateStateRequest") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match ServerContext::require_bound_system(bound_entry, "beyonce") {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Ok(PyRep::None));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Ok(PyRep::None));
            };
            let handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = (match handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyQueryFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Ok(PyRep::None));
            };
            if matches!(ship_ball.mode, MovementMode::Warp { .. }) {
                return Some(Ok(PyRep::None));
            }
            match handle.push_set_state(ship_id).await {
                Ok(_tick) => Some(Ok(PyRep::None)),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyQueryFailed".into(),
                    message: e,
                })),
            }
        }

        ("beyonce", "AdminGetBubbleID") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRoles".into(),
                    message: "beyonce.AdminGetBubbleID requires a GM or Admin role".into(),
                }));
            }
            let sys_id = match ServerContext::require_bound_system(bound_entry, "beyonce") {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let target_ball_id = args
                .first()
                .and_then(|a| a.as_i64())
                .unwrap_or(ship_id);
            let handle = ctx.systems.get_or_spawn(sys_id);
            if target_ball_id != ship_id {
                let visible = handle.list_balls_in_bubble(ship_id).await.unwrap_or_default();
                if !visible.iter().any(|b| b.id == target_ball_id) {
                    return Some(Err(BridgeRpcError {
                        code: "BallNotVisible".into(),
                        message: format!(
                            "Ball {target_ball_id} is not visible in caller ship {ship_id}'s grid bubble"
                        ),
                    }));
                }
            }
            match handle.get_bubble_id_for_ball(target_ball_id).await {
                Ok(Some(bid)) => Some(Ok(PyRep::Integer(bid as i64))),
                Ok(None) => Some(Err(BridgeRpcError {
                    code: "BallNotFound".into(),
                    message: format!("Ball {target_ball_id} is not present in system {sys_id}"),
                })),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyQueryFailed".into(),
                    message: e,
                })),
            }
        }

        ("beyonce", "GetBallSnapshot") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match ServerContext::require_bound_system(bound_entry, "beyonce") {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let handle = ctx.systems.get_or_spawn(sys_id);
            match handle.get_ball_snapshot(ship_id).await {
                Ok(Some((tick, ball))) => Some(Ok(PyRep::keyval([
                    ("tick", PyRep::Integer(tick as i64)),
                    ("ballID", PyRep::Integer(ball.id)),
                    ("x", PyRep::Real(ball.position.x)),
                    ("y", PyRep::Real(ball.position.y)),
                    ("z", PyRep::Real(ball.position.z)),
                    ("vx", PyRep::Real(ball.velocity.x)),
                    ("vy", PyRep::Real(ball.velocity.y)),
                    ("vz", PyRep::Real(ball.velocity.z)),
                ]))),
                Ok(None) => Some(Err(BridgeRpcError {
                    code: "BallNotFound".into(),
                    message: format!("Ball {ship_id} is not present in system {sys_id}"),
                })),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyQueryFailed".into(),
                    message: e,
                })),
            }
        }

        ("beyonce", "CmdStargateJump") | ("beyonce", "StargateJump") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot jump stargate while docked".into(),
                }));
            }
            let old_sys = match ServerContext::require_bound_system(bound_entry, "beyonce") {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(gate_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CmdStargateJump requires integer stargateID".into(),
                }));
            };
            let Some(jump) = ctx.sde.jumps.get(&gate_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownStargate".into(),
                    message: format!("Stargate {gate_id} does not exist in SDE"),
                }));
            };
            if jump.from_solar_system_id != old_sys {
                return Some(Err(BridgeRpcError {
                    code: "StargateWrongSystem".into(),
                    message: format!(
                        "Stargate {gate_id} is in system {}, not current system {old_sys}",
                        jump.from_solar_system_id
                    ),
                }));
            }

            let old_handle = ctx.systems.get_or_spawn(old_sys);
            let Some((_, current_ball)) = (match old_handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyQueryFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship ball is not in space".into(),
                }));
            };

            if matches!(current_ball.mode, MovementMode::Warp { .. }) {
                return Some(Err(BridgeRpcError {
                    code: "ShipInWarp".into(),
                    message: "Cannot jump while ship is mid-warp".into(),
                }));
            }

            let dist = current_ball.position.distance(jump.position);
            if dist > 2_500.0 {
                return Some(Err(BridgeRpcError {
                    code: "OutsideStargateJumpRange".into(),
                    message: format!(
                        "Ship is {:.1} m from stargate {gate_id} (must be <= 2500 m)",
                        dist
                    ),
                }));
            }

            let dest_sys_id = jump.to_solar_system_id;
            let dest_gate_pos = ctx
                .sde
                .jumps
                .get(&jump.destination_stargate_id)
                .map(|j| j.position)
                .unwrap_or(DVec3::ZERO);
            let arrival_pos = dest_gate_pos + DVec3::new(1_500.0, 0.0, 0.0);

            let updated_ch = match ctx
                .db
                .set_character_solar_system(char_id, dest_sys_id, &ctx.sde)
            {
                Ok(c) => c,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "SystemTransitionFailed".into(),
                        message: e.to_string(),
                    }))
                }
            };
            let existing_ball = old_handle.remove_ball(ship_id).await;
            ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);

            let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
            let mut new_ball = existing_ball.unwrap_or(current_ball);
            new_ball.position = arrival_pos;
            new_ball.velocity = DVec3::ZERO;
            new_ball.speed_fraction = 0.0;
            new_ball.mode = MovementMode::Stop;
            let combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
            let slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
            let _ = new_handle
                .spawn_ball_with_meta(new_ball, combat_state, slim_meta)
                .await;
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

            let new_beyonce_id = match ctx.bind_object(session, "beyonce", dest_sys_id as i64) {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };
            let (cd_min, fat_min) = compute_jump_fatigue_and_cooldown_minutes(0.0, 4.5, 0.0);

            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
                ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
                ("jumpCooldownMinutes", PyRep::Real(cd_min)),
                ("jumpFatigueMinutes", PyRep::Real(fat_min)),
            ])))
        }

        ("wormholeMgr", "WormholeJump") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot jump wormhole while docked".into(),
                }));
            }
            let Some(old_sys) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solarSystemID".into(),
                }));
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };

            let old_handle = ctx.systems.get_or_spawn(old_sys);
            let Some((_, current_ball)) = (match old_handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyQueryFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship ball is not in space".into(),
                }));
            };

            let Some(wh_ball_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "MissingWormholeBallId".into(),
                    message: "WormholeJump requires an explicit wormhole ballID argument".into(),
                }));
            };
            let Some((_, wh_ball)) = (match old_handle.get_ball_snapshot(wh_ball_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyQueryFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "WormholeNotPresent".into(),
                    message: format!("No wormhole present in solar system {old_sys}"),
                }));
            };
            if wh_ball.type_id != 26272 {
                return Some(Err(BridgeRpcError {
                    code: "WormholeNotPresent".into(),
                    message: format!("Ball {wh_ball_id} is not a Wormhole K162 (26272)"),
                }));
            }

            let dist = current_ball.position.distance(wh_ball.position);
            if dist > 2_500.0 {
                return Some(Err(BridgeRpcError {
                    code: "OutsideWormholeJumpRange".into(),
                    message: format!(
                        "Ship is {:.1} m from wormhole (must be <= 2500 m)",
                        dist
                    ),
                }));
            }

            let ship_mass = current_ball.mass;
            let (dest_sys_id, dest_sig_pos) = match ctx.db.try_with_mut_snapshot(|snap| {
                let Some(dest_sys) = snap
                    .system_states
                    .get(&old_sys)
                    .and_then(|st| st.wormhole_destination_system_id)
                else {
                    return Err(BridgeRpcError {
                        code: "NoWormholeDestination".into(),
                        message: format!(
                            "Solar system {old_sys} has no active wormhole destination"
                        ),
                    });
                };
                for sys in [old_sys, dest_sys] {
                    if let Some(st) = snap.system_states.get_mut(&sys) {
                        if st.wormhole_remaining_mass_kg < ship_mass {
                            return Err(BridgeRpcError {
                                code: "WormholeMassCollapsed".into(),
                                message: "Wormhole lacks remaining mass capacity for this ship"
                                    .into(),
                            });
                        }
                        st.wormhole_remaining_mass_kg -= ship_mass;
                    }
                }
                let sig_pos = snap
                    .system_states
                    .get(&dest_sys)
                    .and_then(|st| st.signature_position)
                    .map(DVec3::from_array)
                    .unwrap_or(DVec3::new(10_500.0, 0.0, 0.0));
                Ok((dest_sys, sig_pos))
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let existing_ball = old_handle.remove_ball(ship_id).await;
            let updated_ch = match ctx
                .db
                .set_character_solar_system(char_id, dest_sys_id, &ctx.sde)
            {
                Ok(c) => c,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "SystemTransitionFailed".into(),
                        message: e.to_string(),
                    }))
                }
            };
            ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);

            let arrival_pos = dest_sig_pos - DVec3::new(500.0, 0.0, 0.0);

            let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
            let mut new_ball = existing_ball.unwrap_or(current_ball);
            new_ball.position = arrival_pos;
            new_ball.velocity = DVec3::ZERO;
            new_ball.speed_fraction = 0.0;
            new_ball.mode = MovementMode::Stop;
            let combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
            let _ = new_handle.spawn_ball_with_combat(new_ball, combat_state).await;
            let _ = ctx
                .living_world
                .materialize_system_lod2(dest_sys_id, &ctx.systems, &ctx.sde)
                .await;

            // Spawn return K162 wormhole ball `wh_ball_id` (`type_id = 26272`) at destination signature_position
            let _ = new_handle
                .spawn_ball(Ball {
                    id: wh_ball_id,
                    type_id: 26272,
                    position: dest_sig_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 1_000_000_000.0,
                    radius: 1500.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Rigid,
                })
                .await;

            session.solar_system_id = Some(updated_ch.solar_system_id);
            session.constellation_id = Some(updated_ch.constellation_id);
            session.region_id = Some(updated_ch.region_id);
            session.station_id = None;
            session.structure_id = None;

            let new_beyonce_id = match ctx.bind_object(session, "beyonce", dest_sys_id as i64) {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
                ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
            ])))
        }

        ("abyssalMgr", "ActivateFilament") | ("voidSpaceMgr", "ActivateFilament") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot activate filament while docked".into(),
                }));
            }
            let Some(old_sys) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solarSystemID".into(),
                }));
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };

            let old_handle = ctx.systems.get_or_spawn(old_sys);
            let Some((_, current_ball)) = (match old_handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyQueryFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship ball is not in space".into(),
                }));
            };

            // Require and consume 1 owned filament item (type_id 47763) in active ship's Cargo
            let consumed = ctx.db.try_with_mut_snapshot(|snap| {
                let fil_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.location_id == ship_id
                            && i.flag == eve_db::InventoryFlag::Cargo
                            && i.type_id == 47763
                            && i.quantity >= 1
                    })
                    .map(|i| i.item_id);
                let Some(iid) = fil_id else {
                    return Err(BridgeRpcError {
                        code: "FilamentNotInCargo".into(),
                        message: format!(
                            "Activating a filament requires filament item (47763) in active ship {ship_id} Cargo"
                        ),
                    });
                };
                if let Some(item) = snap.items.get_mut(&iid) {
                    if item.quantity > 1 {
                        item.quantity -= 1;
                        item.version += 1;
                    } else {
                        snap.items.remove(&iid);
                    }
                }
                Ok(())
            });
            if let Err(e) = consumed {
                return Some(Err(e));
            }

            let dest_sys_id: SolarSystemId = 32_000_001;
            let updated_ch = match ctx
                .db
                .set_character_solar_system(char_id, dest_sys_id, &ctx.sde)
            {
                Ok(c) => c,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "SystemTransitionFailed".into(),
                        message: e.to_string(),
                    }))
                }
            };
            let existing_ball = old_handle.remove_ball(ship_id).await;
            ctx.sync_character_local_chat(char_id, Some(old_sys), dest_sys_id);

            let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
            let mut new_ball = existing_ball.unwrap_or(current_ball);
            new_ball.position = DVec3::new(50_000.0, 0.0, 0.0);
            new_ball.velocity = DVec3::ZERO;
            new_ball.speed_fraction = 0.0;
            new_ball.mode = MovementMode::Stop;
            let combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
            let _ = new_handle.spawn_ball_with_combat(new_ball, combat_state).await;
            let _ = ctx
                .living_world
                .materialize_system_lod2(dest_sys_id, &ctx.systems, &ctx.sde)
                .await;

            session.solar_system_id = Some(updated_ch.solar_system_id);
            session.constellation_id = Some(updated_ch.constellation_id);
            session.region_id = Some(updated_ch.region_id);
            session.station_id = None;
            session.structure_id = None;

            let new_beyonce_id = match ctx.bind_object(session, "beyonce", dest_sys_id as i64) {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
                ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
            ])))
        }

        ("structureCynoBeaconMgr", "GetBeaconFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let char_corp_id = session.corporation_id.unwrap_or_else(|| {
                ctx.db
                    .get_character(char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0)
            });
            let beacons: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                snap.structures
                    .values()
                    .filter(|st| {
                        st.owner_corp_id == char_corp_id
                            && st.state != "Destroyed"
                            && st.fuel_blocks > 0
                    })
                    .map(|st| {
                        PyRep::keyval([
                            ("structureID", PyRep::Integer(st.structure_id)),
                            ("solarSystemID", PyRep::Integer(st.solar_system_id as i64)),
                            ("fuelBlocks", PyRep::Integer(st.fuel_blocks as i64)),
                        ])
                    })
                    .collect()
            });
            Some(Ok(PyRep::List(beacons)))
        }

        ("scanMgr", "ConeScan") | ("scanMgr", "DirectionalScan") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solarSystemID".into(),
                }));
            };
            let convoys = ctx.living_world.list_convoys();
            let matches: Vec<PyRep> = convoys
                .into_iter()
                .filter(|c| c.current_system_id == sys_id && !c.destroyed)
                .map(|c| {
                    PyRep::keyval([
                        ("itemID", PyRep::Integer(c.convoy_id)),
                        ("typeID", PyRep::Integer(c.ship_type_id as i64)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(matches)))
        }

        _ => None,
    }
}
