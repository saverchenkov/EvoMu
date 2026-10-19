//! `space_destiny` — Domain 5: Solar System `Ballpark`, Stargate Jumps, Probe Scanning, Wormholes, & Abyssal Filaments (`§3.5`).

#![forbid(unsafe_code)]

use crate::formulas::compute_jump_fatigue_and_cooldown_minutes;
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::types::InventoryFlag;
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
            let _ = handle.break_gate_cloak(ship_id).await;
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
            if fraction > 0.0 {
                let _ = handle.break_gate_cloak(ship_id).await;
            }
            match handle.set_speed_fraction(ship_id, fraction).await {
                Ok(tick) => Some(Ok(PyRep::Integer(tick as i64))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyCommandFailed".into(),
                    message: e,
                })),
            }
        }

        // parity: plan:SP-3, feat:navigation.approach, feat:dogma.velocity-agility, feat:modules.micro-jump-drive
        ("beyonce", "CmdOrbit")
        | ("beyonce", "CmdFollowBall")
        | ("beyonce", "CmdApproach")
        | ("beyonce", "CmdKeepAtRange") => {
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
            let default_range = if method == "CmdApproach" {
                0.0
            } else if method == "CmdKeepAtRange" {
                1_000.0
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
            match handle.set_movement_mode(ship_id, mode).await {
                Ok(tick) => Some(Ok(PyRep::Integer(tick as i64))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyCommandFailed".into(),
                    message: e,
                })),
            }
        }

        ("beyonce", "CmdAlign") | ("beyonce", "CmdAlignTo") => {
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
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
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
            match handle
                .set_movement_mode(ship_id, MovementMode::GotoDirection { direction: dir })
                .await
            {
                Ok(tick) => Some(Ok(PyRep::Integer(tick as i64))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyCommandFailed".into(),
                    message: e,
                })),
            }
        }

        ("beyonce", "CmdMicroJump") => {
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
            let dist = args.first().and_then(|a| a.as_f64()).unwrap_or(100_000.0);
            let handle = ctx.systems.get_or_spawn(sys_id);
            let _ = handle.break_gate_cloak(ship_id).await;
            match handle.micro_jump(ship_id, dist).await {
                Ok(new_pos) => Some(Ok(PyRep::keyval([
                    ("x", PyRep::Real(new_pos.x)),
                    ("y", PyRep::Real(new_pos.y)),
                    ("z", PyRep::Real(new_pos.z)),
                ]))),
                Err(e) => {
                    let code = if e.contains("WarpScrambled") {
                        "WarpScrambled"
                    } else {
                        "DestinyCommandFailed"
                    };
                    Some(Err(BridgeRpcError {
                        code: code.into(),
                        message: e,
                    }))
                }
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
            let _ = handle.break_gate_cloak(ship_id).await;
            match handle.set_movement_mode(ship_id, mode).await {
                Ok(tick) => Some(Ok(PyRep::Integer(tick as i64))),
                Err(e) => {
                    let code = if e.contains("WarpScrambled") {
                        "WarpScrambled"
                    } else {
                        "DestinyCommandFailed"
                    };
                    Some(Err(BridgeRpcError {
                        code: code.into(),
                        message: e,
                    }))
                }
            }
        }

        // parity: plan:SP-3
        // parity: rpc:beyonce.CmdWarpTo, rpc:beyonce.CmdWarpToStuff
        ("beyonce", "CmdWarpTo") | ("beyonce", "CmdWarpToStuff") => {
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
            let (stuff_type, target_id) = if let Some(s) = args.first().and_then(|a| a.as_str()) {
                (s, args.get(1).and_then(|a| a.as_i64()).unwrap_or(0))
            } else {
                ("item", args.first().and_then(|a| a.as_i64()).unwrap_or(0))
            };
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
            let _ = handle.break_gate_cloak(ship_id).await;
            match handle.set_movement_mode(ship_id, mode).await {
                Ok(tick) => Some(Ok(PyRep::Integer(tick as i64))),
                Err(e) => {
                    let code = if e.contains("WarpScrambled") {
                        "WarpScrambled"
                    } else {
                        "DestinyCommandFailed"
                    };
                    Some(Err(BridgeRpcError {
                        code: code.into(),
                        message: e,
                    }))
                }
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

            let gate_radius = ctx
                .sde
                .types
                .get(&16)
                .map(|t| t.radius)
                .filter(|&r| r > 0.0)
                .unwrap_or(14_051.0);
            let dist = (current_ball.position.distance(jump.position) - gate_radius).max(0.0);
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
            let arrival_pos = dest_gate_pos + DVec3::new(gate_radius + 1_500.0, 0.0, 0.0);

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
            let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
            combat_state.is_cloaked = true;
            combat_state.gate_cloak_remaining_ticks = 60;
            let mut slim_meta = ctx.build_ship_slim_meta(char_id, new_ball.type_id);
            slim_meta.is_cloaked = true;
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

            let arrival_pos = dest_sig_pos + DVec3::new(2_000.0, 0.0, 0.0);

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

        // parity: plan:S-3, feat:navigation.pharolux-beacon
        ("structureCynoBeaconMgr", "CmdJumpToStructureBeacon") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot jump to cyno beacon while docked".into(),
                }));
            }
            let Some(cur_sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solarSystemID".into(),
                }));
            };
            let (target_sys_hint, target_struct_id) = if args.len() >= 2 {
                (
                    args.first().and_then(|a| a.as_i64()).map(|s| s as u32),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                )
            } else {
                (None, args.first().and_then(|a| a.as_i64()).unwrap_or(0))
            };
            let cur_sys_handle = ctx.systems.get_or_spawn(cur_sys_id);
            let Some((_, current_ball)) = (match cur_sys_handle.get_ball_snapshot(ship_id).await {
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
                    message: "Active ship is not spawned in space".into(),
                }));
            };
            if matches!(current_ball.mode, MovementMode::Warp { .. }) {
                return Some(Err(BridgeRpcError {
                    code: "ShipInWarp".into(),
                    message: "Cannot jump while in warp".into(),
                }));
            }
            if cur_sys_handle
                .is_ball_scrambled(ship_id)
                .await
                .unwrap_or(false)
            {
                return Some(Err(BridgeRpcError {
                    code: "WarpScrambled".into(),
                    message: "Cannot jump while warp scrambled".into(),
                }));
            }

            let beacon_res = ctx.db.with_snapshot(|snap| {
                let Some(st) = snap.structures.get(&target_struct_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "BeaconNotFound".into(),
                        message: format!("Beacon structure {target_struct_id} not found"),
                    });
                };
                if st.state == "Destroyed" {
                    return Err(BridgeRpcError {
                        code: "BeaconDestroyed".into(),
                        message: format!("Beacon structure {target_struct_id} is destroyed"),
                    });
                }
                if st.fuel_blocks <= 0 {
                    return Err(BridgeRpcError {
                        code: "BeaconOutOfFuel".into(),
                        message: format!(
                            "Beacon structure {target_struct_id} has no fuel blocks"
                        ),
                    });
                }
                if let Some(hint_sys) = target_sys_hint {
                    if st.solar_system_id != hint_sys {
                        return Err(BridgeRpcError {
                            code: "BeaconWrongSystem".into(),
                            message: format!(
                                "Beacon {target_struct_id} is in system {}, expected {hint_sys}",
                                st.solar_system_id
                            ),
                        });
                    }
                }
                Ok(st)
            });
            let target_struct = match beacon_res {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let target_sys_id = target_struct.solar_system_id;
            let target_sec = ctx
                .sde
                .solar_systems
                .get(&target_sys_id)
                .map(|s| s.security_status)
                .unwrap_or(0.0);
            if target_sec >= 0.45 {
                return Some(Err(BridgeRpcError {
                    code: "CannotJumpToHighSecBeacon".into(),
                    message: format!(
                        "Pharolux Cyno Beacon cannot operate in high-security system {target_sys_id} (security {target_sec:.2} >= 0.45)"
                    ),
                }));
            }

            // Calculate distance in Light-Years
            let from_pos = ctx
                .sde
                .solar_systems
                .get(&cur_sys_id)
                .map(|s| s.galactic_pos_ly)
                .unwrap_or([0.0, 0.0, 0.0]);
            let to_pos = ctx
                .sde
                .solar_systems
                .get(&target_sys_id)
                .map(|s| s.galactic_pos_ly)
                .unwrap_or([0.0, 0.0, 0.0]);
            let dist_ly = ((from_pos[0] - to_pos[0]).powi(2)
                + (from_pos[1] - to_pos[1]).powi(2)
                + (from_pos[2] - to_pos[2]).powi(2))
            .sqrt();
            let max_jump_range = 6.0; // standard capital jump drive range in LY
            if dist_ly > max_jump_range {
                return Some(Err(BridgeRpcError {
                    code: "JumpDistanceOutOfRange".into(),
                    message: format!(
                        "Distance {dist_ly:.2} LY exceeds maximum jump range {max_jump_range:.2} LY"
                    ),
                }));
            }

            // Fuel check & consumption
            let fuel_needed = (dist_ly * 150.0).round().max(50.0) as i32;
            let fuel_deduct_res = ctx.db.try_with_mut_snapshot(|snap| {
                let fuel_items: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.location_id == ship_id
                            && (i.flag == InventoryFlag::Cargo
                                || i.flag == InventoryFlag::SpecializedFuelBay)
                    })
                    .map(|i| i.item_id)
                    .collect();
                let mut total_avail = 0;
                for &fid in &fuel_items {
                    if let Some(item) = snap.items.get(&fid) {
                        total_avail += item.quantity;
                    }
                }
                if total_avail < fuel_needed {
                    return Err(BridgeRpcError {
                        code: "InsufficientFuel".into(),
                        message: format!(
                            "Jump requires {fuel_needed} fuel units, but only {total_avail} available in ship cargo/fuel bay"
                        ),
                    });
                }
                let mut rem = fuel_needed;
                for fid in fuel_items {
                    if rem <= 0 {
                        break;
                    }
                    if let Some(item) = snap.items.get_mut(&fid) {
                        if item.quantity <= rem {
                            rem -= item.quantity;
                            item.quantity = 0;
                        } else {
                            item.quantity -= rem;
                            rem = 0;
                        }
                    }
                }
                if let Some(st) = snap.structures.get_mut(&target_struct_id) {
                    st.fuel_blocks = st.fuel_blocks.saturating_sub(1);
                }
                Ok(())
            });
            if let Err(e) = fuel_deduct_res {
                return Some(Err(e));
            }

            // Fatigue & Cooldown calculation
            let cur_fatigue = ctx
                .db
                .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.jump_fatigue_minutes).unwrap_or(0.0));
            let (cd_min, fat_min) =
                compute_jump_fatigue_and_cooldown_minutes(cur_fatigue, dist_ly, 0.0);
            let now = ctx.now_unix();
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.jump_fatigue_minutes = fat_min;
                    ch.jump_cooldown_until_unix = now + (cd_min * 60.0).round() as u64;
                    ch.solar_system_id = target_sys_id;
                }
                snap.character_last_jump_unix.insert(char_id, now);
            });

            // System handoff
            let _ = cur_sys_handle.remove_ball(ship_id).await;
            let target_pos =
                DVec3::from_array(target_struct.position) + DVec3::new(5_000.0, 0.0, 0.0);
            let dest_handle = ctx.systems.get_or_spawn(target_sys_id);
            let ball = Ball {
                id: ship_id,
                type_id: current_ball.type_id,
                position: target_pos,
                velocity: DVec3::ZERO,
                max_velocity: current_ball.max_velocity,
                inertia: current_ball.inertia,
                mass: current_ball.mass,
                radius: current_ball.radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            };
            let combat_state = ctx.build_ship_combat_state(char_id, ship_id, current_ball.type_id);
            let slim_meta = ctx.build_ship_slim_meta(char_id, current_ball.type_id);
            let _ = dest_handle
                .spawn_ball_with_meta(ball, combat_state, slim_meta)
                .await;
            session.solar_system_id = Some(target_sys_id);
            let new_beyonce_id = match ctx.bind_object(session, "beyonce", target_sys_id as i64) {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };

            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(target_sys_id as i64)),
                ("structureID", PyRep::Integer(target_struct_id)),
                ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
                ("jumpFatigueMinutes", PyRep::Real(fat_min)),
                ("jumpCooldownMinutes", PyRep::Real(cd_min)),
                ("distanceLY", PyRep::Real(dist_ly)),
            ])))
        }

        // parity: plan:S-3, feat:navigation.pharolux-beacon
        ("structureCynoBeaconMgr", "CmdBridgeToStructureBeacon") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(_ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(cur_sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solarSystemID".into(),
                }));
            };
            let (_target_sys_hint, target_struct_id) = if args.len() >= 2 {
                (
                    args.first().and_then(|a| a.as_i64()).map(|s| s as u32),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                )
            } else {
                (None, args.first().and_then(|a| a.as_i64()).unwrap_or(0))
            };
            let target_struct = match ctx
                .db
                .with_snapshot(|snap| snap.structures.get(&target_struct_id).cloned())
            {
                Some(s) => s,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "BeaconNotFound".into(),
                        message: format!("Beacon {target_struct_id} not found"),
                    }))
                }
            };
            if target_struct.state == "Destroyed" {
                return Some(Err(BridgeRpcError {
                    code: "BeaconDestroyed".into(),
                    message: "Beacon is destroyed".into(),
                }));
            }
            if target_struct.fuel_blocks <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "BeaconOutOfFuel".into(),
                    message: "Beacon has no fuel blocks".into(),
                }));
            }
            let from_pos = ctx
                .sde
                .solar_systems
                .get(&cur_sys_id)
                .map(|s| s.galactic_pos_ly)
                .unwrap_or([0.0, 0.0, 0.0]);
            let to_pos = ctx
                .sde
                .solar_systems
                .get(&target_struct.solar_system_id)
                .map(|s| s.galactic_pos_ly)
                .unwrap_or([0.0, 0.0, 0.0]);
            let dist_ly = ((from_pos[0] - to_pos[0]).powi(2)
                + (from_pos[1] - to_pos[1]).powi(2)
                + (from_pos[2] - to_pos[2]).powi(2))
            .sqrt();
            if dist_ly > 6.0 {
                return Some(Err(BridgeRpcError {
                    code: "JumpDistanceOutOfRange".into(),
                    message: format!("Distance {dist_ly:.2} LY exceeds maximum range 6.0 LY"),
                }));
            }

            Some(Ok(PyRep::keyval([
                ("bridgeActive", PyRep::Bool(true)),
                (
                    "solarSystemID",
                    PyRep::Integer(target_struct.solar_system_id as i64),
                ),
                ("structureID", PyRep::Integer(target_struct_id)),
                ("durationSeconds", PyRep::Integer(60)),
                ("distanceLY", PyRep::Real(dist_ly)),
            ])))
        }

        // parity: plan:S-3, feat:navigation.pharolux-beacon
        ("structureCynoBeaconMgr", "CmdGroupJumpToStructureBeacon") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(_ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(cur_sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solarSystemID".into(),
                }));
            };
            let (_target_sys_hint, target_struct_id) = if args.len() >= 2 {
                (
                    args.first().and_then(|a| a.as_i64()).map(|s| s as u32),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                )
            } else {
                (None, args.first().and_then(|a| a.as_i64()).unwrap_or(0))
            };
            let target_struct = match ctx
                .db
                .with_snapshot(|snap| snap.structures.get(&target_struct_id).cloned())
            {
                Some(s) => s,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "BeaconNotFound".into(),
                        message: format!("Beacon {target_struct_id} not found"),
                    }))
                }
            };
            if target_struct.state == "Destroyed" {
                return Some(Err(BridgeRpcError {
                    code: "BeaconDestroyed".into(),
                    message: "Beacon is destroyed".into(),
                }));
            }
            if target_struct.fuel_blocks <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "BeaconOutOfFuel".into(),
                    message: "Beacon has no fuel blocks".into(),
                }));
            }
            let from_pos = ctx
                .sde
                .solar_systems
                .get(&cur_sys_id)
                .map(|s| s.galactic_pos_ly)
                .unwrap_or([0.0, 0.0, 0.0]);
            let to_pos = ctx
                .sde
                .solar_systems
                .get(&target_struct.solar_system_id)
                .map(|s| s.galactic_pos_ly)
                .unwrap_or([0.0, 0.0, 0.0]);
            let dist_ly = ((from_pos[0] - to_pos[0]).powi(2)
                + (from_pos[1] - to_pos[1]).powi(2)
                + (from_pos[2] - to_pos[2]).powi(2))
            .sqrt();
            if dist_ly > 6.0 {
                return Some(Err(BridgeRpcError {
                    code: "JumpDistanceOutOfRange".into(),
                    message: format!("Distance {dist_ly:.2} LY exceeds maximum range 6.0 LY"),
                }));
            }

            Some(Ok(PyRep::keyval([
                ("groupJumpActive", PyRep::Bool(true)),
                (
                    "solarSystemID",
                    PyRep::Integer(target_struct.solar_system_id as i64),
                ),
                ("structureID", PyRep::Integer(target_struct_id)),
                ("jumpedCount", PyRep::Integer(1)),
                ("distanceLY", PyRep::Real(dist_ly)),
            ])))
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

        ("beyonce", "CmdMine")
        | ("beyonce", "CmdMineRepeatedly")
        | ("dogmaIM", "Mine")
        | ("droneMgr", "Mine")
        | ("droneMgr", "MineAsteroid")
        | ("miningMgr", "Mine")
        | ("miningMgr", "MineAsteroid") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match ServerContext::require_bound_system(bound_entry, "beyonce")
                .or_else(|_| session.solar_system_id.ok_or_else(|| BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solarSystemID".into(),
                }))
            {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };

            let asteroid_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("asteroidID").and_then(|v| v.as_i64()))
                .or_else(|| _kwargs.get("targetID").and_then(|v| v.as_i64()))
                .unwrap_or(0);
            if asteroid_id == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CmdMine requires target asteroidID".into(),
                }));
            }
            let module_id = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("moduleID").and_then(|v| v.as_i64()))
                .or_else(|| _kwargs.get("droneID").and_then(|v| v.as_i64()));

            let handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = handle.get_ball_snapshot(ship_id).await.ok().flatten() else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship is not spawned in Ballpark".into(),
                }));
            };

            let Some(ast_state) = handle.get_asteroid_state(asteroid_id).await.ok().flatten() else {
                return Some(Err(BridgeRpcError {
                    code: "AsteroidNotFound".into(),
                    message: format!("Asteroid {asteroid_id} not found in system {sys_id}"),
                }));
            };

            if ast_state.remaining_units == 0 || ast_state.depleted {
                return Some(Err(BridgeRpcError {
                    code: "AsteroidDepleted".into(),
                    message: format!("Asteroid {asteroid_id} is depleted"),
                }));
            }

            // Target range validation
            if let Some((_, ast_ball)) = handle.get_ball_snapshot(asteroid_id).await.ok().flatten() {
                let surface_dist = (ship_ball.position.distance(ast_ball.position) - ship_ball.radius - ast_ball.radius).max(0.0);
                let max_range = 15_000.0;
                if surface_dist > max_range {
                    return Some(Err(BridgeRpcError {
                        code: "OutOfRange".into(),
                        message: format!("Asteroid {asteroid_id} is {surface_dist:.1} m away (max range {max_range} m)"),
                    }));
                }
            }

            // Check if this extraction is from a mining drone or excavator drone
            let drone_item = module_id.and_then(|mid| ctx.db.get_item(mid));
            let is_drone = drone_item.as_ref().map(|i| {
                ctx.sde.types.get(&i.type_id).map(|t| t.group_id == 101).unwrap_or(false)
            }).unwrap_or(false) || _kwargs.get("isDrone").and_then(|v| v.as_bool()).unwrap_or(false);

            // Dogma yield & residue calculation via ShipFittingEngine (`MN-01`, `MN-02`)
            let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
            let (base_yield_m3, waste_prob) = if is_drone {
                let drone_type_id = drone_item.as_ref().map(|i| i.type_id).unwrap_or(2173);
                let drone_t = ctx.sde.types.get(&drone_type_id);
                let d_yield = drone_t.and_then(|t| t.default_attributes.get(&77).copied()).unwrap_or(25.0);
                let (drone_op_lvl, drone_int_lvl) = ctx.db.with_snapshot(|snap| {
                    let sk = snap.skills.get(&char_id);
                    let op = sk.and_then(|m| m.get(&3439)).map(|s| s.level).unwrap_or(0);
                    let int = sk.and_then(|m| m.get(&3436)).map(|s| s.level).unwrap_or(0);
                    (op, int)
                });
                let drone_mult = (1.0 + 0.05 * (drone_op_lvl as f64)) * (1.0 + 0.10 * (drone_int_lvl as f64));
                (d_yield * drone_mult, 0.0)
            } else if let (Some(fit), Some(mod_id)) = (&fit, module_id) {
                let y = fit.get_module_effective_attribute(mod_id, 77, &ctx.sde);
                let w = fit.get_module_effective_attribute(mod_id, 3154, &ctx.sde);
                (y, w)
            } else if let Some(fit) = &fit {
                let m = fit.modules.values().find(|m| {
                    ctx.sde.types.get(&m.type_id).map(|t| matches!(t.group_id, 54 | 464 | 483 | 737)).unwrap_or(false)
                });
                if let Some(m) = m {
                    let y = fit.get_module_effective_attribute(m.item_id, 77, &ctx.sde);
                    let w = fit.get_module_effective_attribute(m.item_id, 3154, &ctx.sde);
                    (y, w)
                } else {
                    (40.0, 0.0)
                }
            } else {
                (40.0, 0.0)
            };

            // Mining crits check (dogma attrs 5967 / 5969)
            let crit_chance = fit.as_ref()
                .and_then(|f| module_id.map(|mid| f.get_module_effective_attribute(mid, 5967, &ctx.sde)))
                .unwrap_or(0.01);
            let crit_bonus = fit.as_ref()
                .and_then(|f| module_id.map(|mid| f.get_module_effective_attribute(mid, 5969, &ctx.sde)))
                .filter(|&b| b > 0.0)
                .unwrap_or(2.0);
            let force_crit = _kwargs.get("forceCrit").and_then(|v| v.as_bool())
                .or_else(|| _kwargs.get("crit").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            let is_crit = force_crit || (crit_chance > 0.0 && ((asteroid_id + module_id.unwrap_or(1)) % 100 == 0));
            let yield_m3 = if is_crit { base_yield_m3 * crit_bonus } else { base_yield_m3 };

            let unit_vol = ast_state.unit_volume.max(0.001);
            let requested_units = ((yield_m3 / unit_vol).floor() as u64).max(1);

            let extraction = match handle.mine_asteroid(asteroid_id, requested_units, waste_prob).await {
                Ok(e) => e,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "MiningFailed".into(),
                        message: e,
                    }));
                }
            };

            // Determine target hold: SpecializedOreHold (133), SpecializedGasHold (134), SpecializedIceHold (181), or Cargo (5)
            let ore_type_rec = ctx.sde.types.get(&extraction.ore_type_id);
            let ore_group_id = ore_type_rec.map(|t| t.group_id).unwrap_or(0);
            let ore_category_id = ore_type_rec.map(|t| t.category_id).unwrap_or(0);

            let ship_type_id = ctx
                .db
                .get_item(ship_id)
                .map(|i| i.type_id)
                .unwrap_or(ship_ball.type_id);
            let ship_attrs = ctx.sde.types.get(&ship_type_id).map(|t| &t.default_attributes);
            let has_ore_hold = ship_attrs.is_some_and(|a| a.get(&1556).copied().unwrap_or(0.0) > 0.0 || a.get(&3227).copied().unwrap_or(0.0) > 0.0);
            let has_gas_hold = ship_attrs.is_some_and(|a| a.get(&1557).copied().unwrap_or(0.0) > 0.0);
            let has_ice_hold = ship_attrs.is_some_and(|a| a.get(&3136).copied().unwrap_or(0.0) > 0.0);

            let target_flag = if ore_group_id == 465 && has_ice_hold {
                InventoryFlag::SpecializedIceHold
            } else if ore_group_id == 711 && has_gas_hold {
                InventoryFlag::SpecializedGasHold
            } else if ore_category_id == 25 && has_ore_hold {
                InventoryFlag::SpecializedOreHold
            } else {
                InventoryFlag::Cargo
            };

            let to_jetcan = _kwargs.get("mineToJetcan").and_then(|v| v.as_bool())
                .or_else(|| _kwargs.get("toJetcan").and_then(|v| v.as_bool()))
                .unwrap_or(false);

            let mut jetcan_id = 0i64;
            if extraction.mined_units > 0 {
                if to_jetcan {
                    jetcan_id = ctx.db.with_mut_snapshot(|snap| {
                        let existing = snap.items.values().find(|i| {
                            i.owner_id == char_id && i.type_id == 23 && i.flag == InventoryFlag::None
                        }).map(|i| i.item_id);

                        let cid = match existing {
                            Some(c) => c,
                            None => {
                                snap.next_entity_id += 1;
                                let new_c = snap.next_entity_id;
                                snap.items.insert(
                                    new_c,
                                    eve_db::InventoryItemRecord {
                                        item_id: new_c,
                                        type_id: 23,
                                        owner_id: char_id,
                                        location_id: sys_id as i64,
                                        flag: InventoryFlag::None,
                                        quantity: 1,
                                        singleton: true,
                                        version: 1,
                                        ..Default::default()
                                    },
                                );
                                new_c
                            }
                        };

                        let existing_ore = snap.items.values_mut().find(|i| {
                            i.location_id == cid && i.type_id == extraction.ore_type_id && !i.singleton
                        });
                        if let Some(existing_ore) = existing_ore {
                            existing_ore.quantity += extraction.mined_units as i32;
                            existing_ore.version += 1;
                        } else {
                            snap.next_entity_id += 1;
                            let new_id = snap.next_entity_id;
                            snap.items.insert(
                                new_id,
                                eve_db::InventoryItemRecord {
                                    item_id: new_id,
                                    type_id: extraction.ore_type_id,
                                    owner_id: char_id,
                                    location_id: cid,
                                    flag: InventoryFlag::Cargo,
                                    quantity: extraction.mined_units as i32,
                                    singleton: false,
                                    version: 1,
                                    ..Default::default()
                                },
                            );
                        }
                        cid
                    });
                } else {
                    let _ = ctx.db.add_or_stack_item(
                        char_id,
                        ship_id,
                        target_flag,
                        extraction.ore_type_id,
                        extraction.mined_units as i64,
                    );
                }

                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let _ = ctx.db.record_mining_ledger_entry(
                    char_id,
                    sys_id,
                    extraction.ore_type_id,
                    extraction.mined_units as i64,
                    now_unix,
                );
            }

            Some(Ok(PyRep::keyval([
                ("asteroidID", PyRep::Integer(asteroid_id)),
                ("typeID", PyRep::Integer(extraction.ore_type_id as i64)),
                ("minedUnits", PyRep::Integer(extraction.mined_units as i64)),
                ("wastedUnits", PyRep::Integer(extraction.wasted_units as i64)),
                ("remainingUnits", PyRep::Integer(extraction.remaining_units as i64)),
                ("flagID", PyRep::Integer(if to_jetcan { InventoryFlag::None as i64 } else { target_flag as i64 })),
                ("depleted", PyRep::Integer(if extraction.depleted { 1 } else { 0 })),
                ("isCrit", PyRep::Bool(is_crit)),
                ("critYieldMultiplier", PyRep::Real(if is_crit { crit_bonus } else { 1.0 })),
                ("isDrone", PyRep::Bool(is_drone)),
                ("toJetcan", PyRep::Bool(to_jetcan)),
                ("jetcanID", PyRep::Integer(jetcan_id)),
            ])))
        }

        ("charMiningLedger", "GetMiningLedger") | ("charMiningLedger", "GetLedger") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let records = ctx.db.get_mining_ledger(char_id);
            let items: Vec<PyRep> = records
                .into_iter()
                .map(|r| {
                    PyRep::keyval([
                        ("date", PyRep::String(r.date)),
                        ("solarSystemID", PyRep::Integer(r.solar_system_id as i64)),
                        ("typeID", PyRep::Integer(r.type_id as i64)),
                        ("quantity", PyRep::Integer(r.quantity as i64)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(items)))
        }

        _ => None,
    }
}
