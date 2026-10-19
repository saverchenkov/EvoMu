//! `space_destiny` — Domain 5: Solar System `Ballpark`, Stargate Jumps, Probe Scanning, Wormholes, & Abyssal Filaments (`§3.5`).

#![forbid(unsafe_code)]

use crate::formulas::compute_jump_fatigue_and_cooldown_minutes;
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::types::{CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;

const AU_IN_METERS: f64 = 149_597_870_700.0;

pub fn wormhole_item_id_for_signature(sys_id: u32, sig_id: &str) -> i64 {
    let hash = sig_id
        .bytes()
        .fold(0i64, |acc, b| acc.wrapping_mul(31).wrapping_add(b as i64))
        .abs();
    760_000_000 + ((sys_id as i64 % 10_000) * 10_000) + (hash % 10_000)
}

fn compute_ship_warp_speed_au_s(ctx: &ServerContext, ship_type_id: u32) -> f64 {
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

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let method = if service == "beyonce" && method == "CmdWarpToItem" {
        "CmdWarpToStuff"
    } else {
        method
    };
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

            let warp_speed = compute_ship_warp_speed_au_s(ctx, current_ball.type_id);

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
            let (stuff_type, target_id, target_str_opt) =
                if let Some(s) = args.first().and_then(|a| a.as_str()) {
                    (
                        s,
                        args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                        args.get(1).and_then(|a| a.as_str()),
                    )
                } else {
                    ("item", args.first().and_then(|a| a.as_i64()).unwrap_or(0), None)
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
                            if let Some(sigs) = snap.cosmic_signatures.get(&sys_id) {
                                for s in sigs {
                                    let wh_id = wormhole_item_id_for_signature(sys_id, &s.signature_id);
                                    if ((wh_id == target_id && (has_scanned || s.signal_strength >= 1.0))
                                        || (target_str_opt == Some(s.signature_id.as_str()) && s.signal_strength >= 1.0))
                                        && !s.collapsed
                                    {
                                        return Some(DVec3::from_array(s.position));
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

            let warp_speed = compute_ship_warp_speed_au_s(ctx, current_ball.type_id);

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

        // parity: feat:navigation.wormhole-jump, feat:navigation.wormhole-lifetime, feat:navigation.wormhole-statics, feat:navigation.wormhole-thera
        // parity: rpc:wormholeMgr.WormholeJump
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

            let wh_arg_i64 = args.first().and_then(|a| a.as_i64());
            let wh_arg_str = args.first().and_then(|a| a.as_str());
            if wh_arg_i64 != Some(750_000_001) {
                if let Some(res) = execute_cosmic_wormhole_jump(
                    ctx,
                    session,
                    char_id,
                    old_sys,
                    ship_id,
                    current_ball.clone(),
                    wh_arg_i64,
                    wh_arg_str,
                )
                .await
                {
                    return Some(res);
                }
            }

            let Some(wh_ball_id) = wh_arg_i64 else {
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
                    ch.jump_fatigue_until_unix = now + (fat_min * 60.0).round() as u64;
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

        // parity: feat:scanning.dscan
        // parity: rpc:scanMgr.ConeScan
        ("scanMgr", "ConeScan") | ("scanMgr", "DirectionalScan") => {
            if method == "DirectionalScan" {
                return None;
            }
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match bound_entry
                .map(|b| b.object_id as u32)
                .or(session.solar_system_id)
            {
                Some(s) => s,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NoSolarSystem".into(),
                        message: "Session has no active solarSystemID".into(),
                    }))
                }
            };
            let scan_angle = args
                .first()
                .and_then(|a| a.as_f64())
                .unwrap_or(360.0)
                .clamp(5.0, 360.0);
            let max_scan_range = 14.3 * AU_IN_METERS;
            let scan_range = args
                .get(1)
                .and_then(|a| a.as_f64())
                .unwrap_or(max_scan_range)
                .clamp(0.0, max_scan_range);

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let caller_ship = session.ship_id;
            let caller_ball = if let Some(sid) = caller_ship {
                sys_handle
                    .get_ball_snapshot(sid)
                    .await
                    .ok()
                    .flatten()
                    .map(|(_, b)| b)
            } else {
                None
            };
            let caller_pos = caller_ball
                .as_ref()
                .map(|b| b.position)
                .unwrap_or(DVec3::ZERO);

            let explicit_dir = if let (Some(dx), Some(dy), Some(dz)) = (
                args.get(2).and_then(|a| a.as_f64()),
                args.get(3).and_then(|a| a.as_f64()),
                args.get(4).and_then(|a| a.as_f64()),
            ) {
                Some(DVec3::new(dx, dy, dz))
            } else if let Some(PyRep::Tuple(coords) | PyRep::List(coords)) = args.get(2) {
                if coords.len() >= 3 {
                    Some(DVec3::new(
                        coords[0].as_f64().unwrap_or(0.0),
                        coords[1].as_f64().unwrap_or(0.0),
                        coords[2].as_f64().unwrap_or(1.0),
                    ))
                } else {
                    None
                }
            } else {
                None
            };

            let caller_dir = explicit_dir
                .filter(|d| d.length_squared() > 1e-9)
                .map(|d| d.normalize())
                .or_else(|| {
                    caller_ball.as_ref().map(|b| {
                        if b.velocity.length_squared() > 0.001 {
                            b.velocity.normalize()
                        } else {
                            DVec3::new(0.0, 0.0, 1.0)
                        }
                    })
                })
                .unwrap_or(DVec3::new(0.0, 0.0, 1.0));

            let cos_half_angle = (scan_angle.to_radians() * 0.5).cos();
            let balls = sys_handle.list_balls().await.unwrap_or_default();
            let mut matches: Vec<PyRep> = Vec::new();
            let mut seen_ids = std::collections::HashSet::new();

            for ball in balls {
                if Some(ball.id) == caller_ship {
                    continue;
                }
                let dist = ball.position.distance(caller_pos);
                if dist > scan_range {
                    continue;
                }
                if scan_angle < 360.0 {
                    let to_target = (ball.position - caller_pos).normalize_or_zero();
                    if to_target.dot(caller_dir) < cos_half_angle {
                        continue;
                    }
                }
                let group_id = ctx
                    .sde
                    .types
                    .get(&ball.type_id)
                    .map(|t| t.group_id)
                    .unwrap_or(0);
                // Combat Recon Ships (group_id == 906: Huginn, Rook, Lachesis, Curse) are immune to D-Scan
                if group_id == 906 {
                    continue;
                }
                // Cloaked ships do not appear on D-Scan
                if let Ok(Some(cs)) = sys_handle.get_combat_state(ball.id).await {
                    if cs.is_cloaked {
                        continue;
                    }
                }
                seen_ids.insert(ball.id);
                matches.push(PyRep::keyval([
                    ("id", PyRep::Integer(ball.id)),
                    ("itemID", PyRep::Integer(ball.id)),
                    ("ballID", PyRep::Integer(ball.id)),
                    ("typeID", PyRep::Integer(ball.type_id as i64)),
                    ("groupID", PyRep::Integer(group_id as i64)),
                    ("distance", PyRep::Real(dist)),
                    ("x", PyRep::Real(ball.position.x)),
                    ("y", PyRep::Real(ball.position.y)),
                    ("z", PyRep::Real(ball.position.z)),
                ]));
            }

            let convoys = ctx.living_world.list_convoys();
            for c in convoys
                .into_iter()
                .filter(|c| c.current_system_id == sys_id && !c.destroyed)
            {
                if !seen_ids.contains(&c.convoy_id) {
                    let group_id = ctx
                        .sde
                        .types
                        .get(&c.ship_type_id)
                        .map(|t| t.group_id)
                        .unwrap_or(0);
                    matches.push(PyRep::keyval([
                        ("id", PyRep::Integer(c.convoy_id)),
                        ("itemID", PyRep::Integer(c.convoy_id)),
                        ("ballID", PyRep::Integer(c.convoy_id)),
                        ("typeID", PyRep::Integer(c.ship_type_id as i64)),
                        ("groupID", PyRep::Integer(group_id as i64)),
                    ]));
                }
            }
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

        _ => {
            handle_scan_and_wormhole_extras(
                ctx,
                session,
                bound_entry,
                service,
                method,
                args,
                _kwargs,
            )
            .await
        }
    }
}

pub fn system_wormhole_profile(sys_id: SolarSystemId) -> (u8, bool, bool, &'static str) {
    let is_wh = (31_000_000..=31_002_605).contains(&sys_id);
    let is_thera = sys_id == 31_000_005;
    let is_shattered = is_thera || (is_wh && (sys_id % 10 == 0));
    let wh_class = if is_thera {
        12
    } else if is_shattered {
        13
    } else if is_wh {
        (((sys_id - 31_000_000) % 6) + 1) as u8
    } else {
        0
    };
    let env_effect = if !is_wh || is_thera {
        "None"
    } else {
        match sys_id % 6 {
            0 => "Pulsar",
            1 => "Wolf-Rayet",
            2 => "Magnetar",
            3 => "Red Giant",
            4 => "Cataclysmic Variable",
            _ => "Black Hole",
        }
    };
    (wh_class, is_thera, is_shattered, env_effect)
}

fn static_wormhole_template(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    now_unix: u64,
    seq: u32,
) -> Vec<CosmicSignatureRecord> {
    if sys_id == 31_000_005 {
        // Thera (31000005): multiple static wormholes to High-Sec (Jita 30000142), Low-Sec (Tama 30002813), Null-Sec (VFK-IV 30002904)
        // Capital ships are disallowed in Thera (`max_jump_mass_kg = 375_000_000.0`).
        let hs_wh = ctx.sde.wormhole_types.get(&30647);
        let ls_wh = ctx.sde.wormhole_types.get(&30648);
        let ns_wh = ctx.sde.wormhole_types.get(&30649);
        vec![
            CosmicSignatureRecord {
                signature_id: format!("THS-{}", 100 + ((sys_id + seq) % 900)),
                solar_system_id: sys_id,
                sig_group: "Wormhole".into(),
                type_name: hs_wh
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| "Wormhole B449".into()),
                position: [10.0 * AU_IN_METERS, 2.0 * AU_IN_METERS, -3.0 * AU_IN_METERS],
                signal_strength: 0.0,
                wormhole_type_id: Some(30647),
                destination_system_id: Some(30_000_142),
                created_unix: now_unix,
                lifetime_seconds: hs_wh
                    .map(|w| (w.max_stable_time_min * 60.0) as u64)
                    .unwrap_or(57_600),
                max_mass_kg: hs_wh.map(|w| w.max_stable_mass).unwrap_or(2_000_000_000.0),
                remaining_mass_kg: hs_wh.map(|w| w.max_stable_mass).unwrap_or(2_000_000_000.0),
                max_jump_mass_kg: 375_000_000.0,
                collapsed: false,
            },
            CosmicSignatureRecord {
                signature_id: format!("TLS-{}", 200 + ((sys_id + seq) % 700)),
                solar_system_id: sys_id,
                sig_group: "Wormhole".into(),
                type_name: ls_wh
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| "Wormhole N944".into()),
                position: [-9.0 * AU_IN_METERS, 4.0 * AU_IN_METERS, 6.0 * AU_IN_METERS],
                signal_strength: 0.0,
                wormhole_type_id: Some(30648),
                destination_system_id: Some(30_002_813),
                created_unix: now_unix,
                lifetime_seconds: ls_wh
                    .map(|w| (w.max_stable_time_min * 60.0) as u64)
                    .unwrap_or(86_400),
                max_mass_kg: ls_wh.map(|w| w.max_stable_mass).unwrap_or(3_000_000_000.0),
                remaining_mass_kg: ls_wh.map(|w| w.max_stable_mass).unwrap_or(3_000_000_000.0),
                max_jump_mass_kg: 375_000_000.0,
                collapsed: false,
            },
            CosmicSignatureRecord {
                signature_id: format!("TNS-{}", 300 + ((sys_id + seq) % 600)),
                solar_system_id: sys_id,
                sig_group: "Wormhole".into(),
                type_name: ns_wh
                    .map(|w| w.name.clone())
                    .unwrap_or_else(|| "Wormhole S199".into()),
                position: [6.0 * AU_IN_METERS, -8.0 * AU_IN_METERS, 5.0 * AU_IN_METERS],
                signal_strength: 0.0,
                wormhole_type_id: Some(30649),
                destination_system_id: Some(30_002_904),
                created_unix: now_unix,
                lifetime_seconds: ns_wh
                    .map(|w| (w.max_stable_time_min * 60.0) as u64)
                    .unwrap_or(57_600),
                max_mass_kg: ns_wh.map(|w| w.max_stable_mass).unwrap_or(3_000_000_000.0),
                remaining_mass_kg: ns_wh.map(|w| w.max_stable_mass).unwrap_or(3_000_000_000.0),
                max_jump_mass_kg: 375_000_000.0,
                collapsed: false,
            },
        ]
    } else if (31_000_000..=31_002_605).contains(&sys_id) {
        let (wh_class, _, is_shattered, _) = system_wormhole_profile(sys_id);
        let (wh_type_id, dest_sys): (u32, u32) = match wh_class {
            1 => (30579, 30_000_142), // Z971 -> C1 / High-Sec
            2 => (30583, 31_000_001), // R943 -> C2
            3 => (30584, 30_002_813), // X702 -> C3 / Low-Sec
            4 => (30642, 31_000_003), // O128 -> C4
            5 => (30643, 31_000_004), // N432 -> C5 (allows capitals)
            6 => (30645, 31_000_006), // B041 -> C6 (allows capitals)
            _ => (30660, 30_000_142), // H121 -> C13 shattered (small/medium ships only)
        };
        let wh_info = ctx.sde.wormhole_types.get(&wh_type_id);
        let max_mass = wh_info
            .map(|w| w.max_stable_mass)
            .unwrap_or(2_000_000_000.0);
        let max_jump = if is_shattered {
            62_000_000.0
        } else {
            wh_info.map(|w| w.max_jump_mass).unwrap_or(375_000_000.0)
        };
        let life_s = wh_info
            .map(|w| (w.max_stable_time_min * 60.0) as u64)
            .unwrap_or(86_400);
        vec![CosmicSignatureRecord {
            signature_id: format!("WST-{}", 100 + ((sys_id + seq * 17) % 900)),
            solar_system_id: sys_id,
            sig_group: "Wormhole".into(),
            type_name: wh_info
                .map(|w| w.name.clone())
                .unwrap_or_else(|| "Unstable Wormhole".into()),
            position: [12.0 * AU_IN_METERS, 2.5 * AU_IN_METERS, -4.0 * AU_IN_METERS],
            signal_strength: 0.0,
            wormhole_type_id: Some(wh_type_id),
            destination_system_id: Some(dest_sys),
            created_unix: now_unix,
            lifetime_seconds: life_s,
            max_mass_kg: max_mass,
            remaining_mass_kg: max_mass,
            max_jump_mass_kg: max_jump,
            collapsed: false,
        }]
    } else {
        let sig_prefix = if seq == 0 { "JMM" } else { "STA" };
        vec![CosmicSignatureRecord {
            signature_id: format!("{sig_prefix}-{}", 100 + ((sys_id + seq * 37) % 900)),
            solar_system_id: sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Unstable Wormhole (K162)".into(),
            position: [12.0 * AU_IN_METERS, 2.5 * AU_IN_METERS, -4.0 * AU_IN_METERS],
            signal_strength: 0.0,
            wormhole_type_id: Some(26272),
            destination_system_id: Some(31_000_005),
            created_unix: now_unix,
            lifetime_seconds: 86_400,
            max_mass_kg: 2_000_000_000.0,
            remaining_mass_kg: 2_000_000_000.0,
            max_jump_mass_kg: 375_000_000.0,
            collapsed: false,
        }]
    }
}

pub async fn ensure_system_signatures_and_anomalies(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> (Vec<CosmicSignatureRecord>, Vec<CosmicAnomalyRecord>) {
    let now_unix = ctx.now_unix();
    let (sigs, anomalies) = ctx
        .db
        .try_with_mut_snapshot(|snap| {
            let list = snap.cosmic_signatures.entry(sys_id).or_insert_with(|| {
                let mut v = static_wormhole_template(ctx, sys_id, now_unix, 0);
                v.extend([
                    CosmicSignatureRecord {
                        signature_id: format!("REL-{}", 200 + (sys_id % 700)),
                        solar_system_id: sys_id,
                        sig_group: "Relic Site".into(),
                        type_name: "Ruined Sansha Relic Site".into(),
                        position: [-8.0 * AU_IN_METERS, 1.0 * AU_IN_METERS, 15.0 * AU_IN_METERS],
                        signal_strength: 0.0,
                        wormhole_type_id: None,
                        destination_system_id: None,
                        created_unix: now_unix,
                        lifetime_seconds: 86_400,
                        max_mass_kg: 0.0,
                        remaining_mass_kg: 0.0,
                        max_jump_mass_kg: 0.0,
                        collapsed: false,
                    },
                    CosmicSignatureRecord {
                        signature_id: format!("DAT-{}", 300 + (sys_id % 600)),
                        solar_system_id: sys_id,
                        sig_group: "Data Site".into(),
                        type_name: "Local Information Repository".into(),
                        position: [5.0 * AU_IN_METERS, -11.0 * AU_IN_METERS, 3.0 * AU_IN_METERS],
                        signal_strength: 0.0,
                        wormhole_type_id: None,
                        destination_system_id: None,
                        created_unix: now_unix,
                        lifetime_seconds: 86_400,
                        max_mass_kg: 0.0,
                        remaining_mass_kg: 0.0,
                        max_jump_mass_kg: 0.0,
                        collapsed: false,
                    },
                    CosmicSignatureRecord {
                        signature_id: format!("CBT-{}", 400 + (sys_id % 500)),
                        solar_system_id: sys_id,
                        sig_group: "Combat Site".into(),
                        type_name: "Pirate Fleet Base".into(),
                        position: [-14.0 * AU_IN_METERS, 0.0 * AU_IN_METERS, -8.0 * AU_IN_METERS],
                        signal_strength: 0.0,
                        wormhole_type_id: None,
                        destination_system_id: None,
                        created_unix: now_unix,
                        lifetime_seconds: 86_400,
                        max_mass_kg: 0.0,
                        remaining_mass_kg: 0.0,
                        max_jump_mass_kg: 0.0,
                        collapsed: false,
                    },
                    CosmicSignatureRecord {
                        signature_id: format!("GAS-{}", 500 + (sys_id % 400)),
                        solar_system_id: sys_id,
                        sig_group: "Gas Site".into(),
                        type_name: "Amber Cytoserocin Nebula".into(),
                        position: [9.0 * AU_IN_METERS, -4.0 * AU_IN_METERS, 7.0 * AU_IN_METERS],
                        signal_strength: 0.0,
                        wormhole_type_id: None,
                        destination_system_id: None,
                        created_unix: now_unix,
                        lifetime_seconds: 86_400,
                        max_mass_kg: 0.0,
                        remaining_mass_kg: 0.0,
                        max_jump_mass_kg: 0.0,
                        collapsed: false,
                    },
                    CosmicSignatureRecord {
                        signature_id: format!("ORE-{}", 600 + (sys_id % 300)),
                        solar_system_id: sys_id,
                        sig_group: "Ore Site".into(),
                        type_name: "Rare Ore Site (Jaspet & Hemorphite)".into(),
                        position: [-11.0 * AU_IN_METERS, 6.0 * AU_IN_METERS, -13.0 * AU_IN_METERS],
                        signal_strength: 0.0,
                        wormhole_type_id: None,
                        destination_system_id: None,
                        created_unix: now_unix,
                        lifetime_seconds: 86_400,
                        max_mass_kg: 0.0,
                        remaining_mass_kg: 0.0,
                        max_jump_mass_kg: 0.0,
                        collapsed: false,
                    },
                ]);
                v
            });
            let sigs_clone = list.clone();
            let anoms = snap
                .cosmic_anomalies
                .entry(sys_id)
                .or_insert_with(|| {
                    vec![CosmicAnomalyRecord {
                        anomaly_id: format!("ANO-{}", 100 + (sys_id % 900)),
                        solar_system_id: sys_id,
                        name: "Dense Veldspar Deposit".into(),
                        anomaly_type: "Ore".into(),
                        position: [4.0 * AU_IN_METERS, 3.0 * AU_IN_METERS, -2.0 * AU_IN_METERS],
                        signal_strength: 1.0,
                        ore_types: vec![1230, 1228],
                        remaining_volume: 50_000.0,
                        created_unix: now_unix,
                        lifetime_seconds: 86_400,
                        cleared: false,
                    }]
                })
                .clone();
            Ok::<_, std::convert::Infallible>((sigs_clone, anoms))
        })
        .unwrap();

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for sig in &sigs {
        if sig.sig_group == "Wormhole" && !sig.collapsed {
            let wh_item_id = wormhole_item_id_for_signature(sys_id, &sig.signature_id);
            if sys_handle
                .get_ball_snapshot(wh_item_id)
                .await
                .ok()
                .flatten()
                .is_none()
            {
                let _ = sys_handle
                    .spawn_ball(Ball {
                        id: wh_item_id,
                        type_id: sig.wormhole_type_id.unwrap_or(26272),
                        position: DVec3::from_array(sig.position),
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 1.0,
                        mass: sig.remaining_mass_kg.max(1.0),
                        radius: 1500.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Rigid,
                    })
                    .await;
            }
        }
    }

    (sigs, anomalies)
}

pub fn compute_wormhole_stages(
    wh: &CosmicSignatureRecord,
    now_unix: u64,
) -> (&'static str, f64, &'static str, i64, bool) {
    let elapsed = now_unix.saturating_sub(wh.created_unix);
    let lifetime_pct = if wh.lifetime_seconds == 0 || elapsed >= wh.lifetime_seconds {
        0.0
    } else {
        (wh.lifetime_seconds - elapsed) as f64 / wh.lifetime_seconds as f64
    };
    let (lifetime_stage, wormhole_age) = if lifetime_pct <= 0.0 {
        ("Collapsed_Expired", 3)
    } else if lifetime_pct < 0.25 {
        ("ReachingEndOfLifetime", 2)
    } else {
        ("Healthy", 1)
    };

    let mass_pct = if wh.max_mass_kg > 0.0 {
        wh.remaining_mass_kg / wh.max_mass_kg
    } else {
        0.0
    };
    let (mass_stage, wormhole_size) = if wh.remaining_mass_kg <= 0.0 {
        ("Collapsed_Exhausted", 0.0)
    } else if mass_pct < 0.10 {
        ("Critical", 0.1)
    } else if mass_pct < 0.50 {
        ("Destabilized", 0.5)
    } else {
        ("Stable", 1.0)
    };
    let is_collapsed = wh.collapsed || lifetime_pct <= 0.0 || wh.remaining_mass_kg <= 0.0;
    (
        mass_stage,
        wormhole_size,
        lifetime_stage,
        wormhole_age,
        is_collapsed,
    )
}

#[allow(clippy::too_many_arguments)]
async fn execute_cosmic_wormhole_jump(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    old_sys: SolarSystemId,
    ship_id: i64,
    current_ball: Ball,
    wh_arg_i64: Option<i64>,
    wh_arg_str: Option<&str>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let _ = ensure_system_signatures_and_anomalies(ctx, old_sys).await;
    let has_matching_sig = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures.get(&old_sys).is_some_and(|sigs| {
            sigs.iter().any(|s| {
                s.sig_group == "Wormhole"
                    && (wh_arg_str == Some(s.signature_id.as_str())
                        || wh_arg_i64 == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id)))
            })
        })
    });
    if !has_matching_sig && wh_arg_str.is_none() {
        return None;
    }

    let ship_mass = current_ball.mass;
    let now_unix = ctx.now_unix();
    let old_handle = ctx.systems.get_or_spawn(old_sys);

    let mut collapsed_ball_to_remove: Option<i64> = None;
    let mut replacement_to_spawn: Option<CosmicSignatureRecord> = None;

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(sigs) = snap.cosmic_signatures.get_mut(&old_sys) else {
            return Err(BridgeRpcError {
                code: "WormholeNotFound".into(),
                message: "No signatures in system".into(),
            });
        };
        let next_seq = sigs.len() as u32 + 1;
        let Some(wh_idx) = sigs.iter().position(|s| {
            s.sig_group == "Wormhole"
                && (wh_arg_str == Some(s.signature_id.as_str())
                    || wh_arg_i64 == Some(wormhole_item_id_for_signature(old_sys, &s.signature_id)))
        }) else {
            return Err(BridgeRpcError {
                code: "WormholeNotFound".into(),
                message: format!("Wormhole not found in system {old_sys}"),
            });
        };

        let wh_id = wormhole_item_id_for_signature(old_sys, &sigs[wh_idx].signature_id);
        let sig_id_str = sigs[wh_idx].signature_id.clone();
        let dest_sys = sigs[wh_idx].destination_system_id.unwrap_or(31_000_005);

        // Check polarization (300s timer)
        let pol_expiry = snap
            .wh_polarizations
            .get(&(char_id, dest_sys as i64))
            .copied()
            .unwrap_or(0);
        if pol_expiry > now_unix {
            return Err(BridgeRpcError {
                code: "WormholePolarized".into(),
                message: format!(
                    "Character is polarized for system {} for another {} seconds",
                    dest_sys,
                    pol_expiry.saturating_sub(now_unix)
                ),
            });
        }

        // Check distance (<= 2500 m)
        let wh_pos = DVec3::from_array(sigs[wh_idx].position);
        let dist = current_ball.position.distance(wh_pos);
        if dist > 2_500.0 {
            return Err(BridgeRpcError {
                code: "OutsideWormholeJumpRange".into(),
                message: format!(
                    "Ship is {:.1} m from wormhole (must be <= 2500 m)",
                    dist
                ),
            });
        }

        // Check lifetime expiry
        let elapsed = now_unix.saturating_sub(sigs[wh_idx].created_unix);
        if elapsed >= sigs[wh_idx].lifetime_seconds || sigs[wh_idx].collapsed {
            sigs[wh_idx].collapsed = true;
            collapsed_ball_to_remove = Some(wh_id);
            let mut replacements = static_wormhole_template(ctx, old_sys, now_unix, next_seq);
            if let Some(mut rep) = replacements.pop() {
                rep.destination_system_id = Some(dest_sys);
                replacement_to_spawn = Some(rep.clone());
                sigs.push(rep);
            }
            let code = if elapsed >= sigs[wh_idx].lifetime_seconds {
                "WormholeCollapsed"
            } else {
                "WormholeMassCollapsed"
            };
            return Err(BridgeRpcError {
                code: code.into(),
                message: "Wormhole has collapsed".into(),
            });
        }

        // Check single-jump mass limit (`max_jump_mass_kg` and Thera/shattered capital ship exclusion)
        let (dest_class, dest_is_thera, dest_is_shattered, _) = system_wormhole_profile(dest_sys);
        let max_jump = sigs[wh_idx].max_jump_mass_kg;
        if (max_jump > 0.0 && ship_mass > max_jump)
            || ((dest_is_thera || dest_is_shattered || (1..=4).contains(&dest_class))
                && ship_mass > 375_000_000.0)
        {
            return Err(BridgeRpcError {
                code: "ShipTooHeavyForWormhole".into(),
                message: format!(
                    "Ship mass {:.1} kg exceeds wormhole jump limit {:.1} kg",
                    ship_mass, max_jump
                ),
            });
        }

        // Check remaining mass before jump
        if sigs[wh_idx].remaining_mass_kg <= 0.0 {
            sigs[wh_idx].collapsed = true;
            collapsed_ball_to_remove = Some(wh_id);
            let mut replacements = static_wormhole_template(ctx, old_sys, now_unix, next_seq);
            if let Some(mut rep) = replacements.pop() {
                rep.destination_system_id = Some(dest_sys);
                replacement_to_spawn = Some(rep.clone());
                sigs.push(rep);
            }
            return Err(BridgeRpcError {
                code: "WormholeMassCollapsed".into(),
                message: "Wormhole lacks remaining mass capacity and has collapsed".into(),
            });
        }

        // Subtract mass and collapse if <= 0
        sigs[wh_idx].remaining_mass_kg -= ship_mass;
        if sigs[wh_idx].remaining_mass_kg <= 0.0 {
            sigs[wh_idx].collapsed = true;
            collapsed_ball_to_remove = Some(wh_id);
            let mut replacements = static_wormhole_template(ctx, old_sys, now_unix, next_seq);
            if let Some(mut rep) = replacements.pop() {
                rep.destination_system_id = Some(dest_sys);
                replacement_to_spawn = Some(rep.clone());
                sigs.push(rep);
            }
        }

        let (mass_stage, wh_size, life_stage, wh_age, is_collapsed) =
            compute_wormhole_stages(&sigs[wh_idx], now_unix);
        let rem_mass = sigs[wh_idx].remaining_mass_kg;

        // Record 300s polarization
        snap.wh_polarizations
            .insert((char_id, dest_sys as i64), now_unix + 300);

        Ok((
            dest_sys,
            wh_id,
            sig_id_str,
            rem_mass,
            max_jump,
            mass_stage,
            wh_size,
            life_stage,
            wh_age,
            is_collapsed,
        ))
    });

    if let Some(rem_id) = collapsed_ball_to_remove {
        let _ = old_handle.remove_ball(rem_id).await;
    }
    let mut rep_sig_id: Option<String> = None;
    let mut rep_wh_id: Option<i64> = None;
    if let Some(rep) = replacement_to_spawn {
        let new_wh_id = wormhole_item_id_for_signature(old_sys, &rep.signature_id);
        rep_sig_id = Some(rep.signature_id.clone());
        rep_wh_id = Some(new_wh_id);
        let _ = old_handle
            .spawn_ball(Ball {
                id: new_wh_id,
                type_id: rep.wormhole_type_id.unwrap_or(26272),
                position: DVec3::from_array(rep.position),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: rep.remaining_mass_kg.max(1.0),
                radius: 1500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }

    match res {
        Ok((
            dest_sys_id,
            wh_item_id,
            sig_id_str,
            remaining_mass,
            max_jump_mass,
            mass_stage,
            wh_size,
            life_stage,
            wh_age,
            is_collapsed,
        )) => {
            let _ = old_handle.remove_ball(ship_id).await;
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

            session.solar_system_id = Some(updated_ch.solar_system_id);
            session.constellation_id = Some(updated_ch.constellation_id);
            session.region_id = Some(updated_ch.region_id);
            session.station_id = None;
            session.structure_id = None;

            let _ = ensure_system_signatures_and_anomalies(ctx, dest_sys_id).await;
            let (dest_wh_class, dest_is_thera, dest_is_shattered, env_effect) =
                system_wormhole_profile(dest_sys_id);

            let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
            let mut new_ball = current_ball;
            new_ball.position = DVec3::new(10_000.0, 0.0, 0.0);
            new_ball.velocity = DVec3::ZERO;
            new_ball.speed_fraction = 0.0;
            new_ball.mode = MovementMode::Stop;

            let mut combat_state = ctx.build_ship_combat_state(char_id, ship_id, new_ball.type_id);
            // Apply W-space environmental anomaly effects
            match env_effect {
                "Pulsar" => {
                    combat_state.max_shield_hp *= 1.30;
                    combat_state.shield_hp = combat_state.max_shield_hp;
                    combat_state.signature_radius *= 1.30;
                }
                "Wolf-Rayet" => {
                    combat_state.max_armor_hp *= 1.30;
                    combat_state.armor_hp = combat_state.max_armor_hp;
                    combat_state.signature_radius *= 0.85;
                }
                "Black Hole" => {
                    new_ball.max_velocity *= 1.30;
                    new_ball.inertia *= 1.15;
                }
                "Cataclysmic Variable" => {
                    combat_state.max_capacitor *= 1.30;
                    combat_state.capacitor = combat_state.max_capacitor;
                }
                _ => {}
            }
            let _ = new_handle
                .spawn_ball_with_combat(new_ball, combat_state)
                .await;

            let new_beyonce_id = match ctx.bind_object(session, "beyonce", dest_sys_id as i64) {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };

            Some(Ok(PyRep::keyval([
                ("success", PyRep::Bool(true)),
                ("solarSystemID", PyRep::Integer(dest_sys_id as i64)),
                ("destinationSystemID", PyRep::Integer(dest_sys_id as i64)),
                ("beyonceBoundID", PyRep::Integer(new_beyonce_id as i64)),
                ("wormholeID", PyRep::Integer(wh_item_id)),
                ("signatureID", PyRep::String(sig_id_str)),
                ("remainingMassKg", PyRep::Real(remaining_mass)),
                ("maxJumpMassKg", PyRep::Real(max_jump_mass)),
                ("massStage", PyRep::String(mass_stage.into())),
                ("wormholeSize", PyRep::Real(wh_size)),
                ("lifetimeStage", PyRep::String(life_stage.into())),
                ("wormholeAge", PyRep::Integer(wh_age)),
                ("collapsed", PyRep::Bool(is_collapsed)),
                (
                    "replacementSignatureID",
                    rep_sig_id.map(PyRep::String).unwrap_or(PyRep::None),
                ),
                (
                    "replacementWormholeID",
                    rep_wh_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                ("wormholeClass", PyRep::Integer(dest_wh_class as i64)),
                ("isThera", PyRep::Bool(dest_is_thera)),
                ("isShattered", PyRep::Bool(dest_is_shattered)),
                ("delayedLocal", PyRep::Bool(dest_wh_class > 0)),
                ("environmentalEffect", PyRep::String(env_effect.into())),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}

async fn handle_scan_and_wormhole_extras(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "scanMgr" {
        return None;
    }

    // parity: feat:scanning.probe-launch, feat:scanning.probe-recall, feat:scanning.probe-scan-math
    // parity: rpc:scanMgr.GetSystemScanMgr, rpc:scanMgr.RequestScans, rpc:scanMgr.RecoverProbes, rpc:scanMgr.ReconnectToLostProbes, rpc:scanMgr.DestroyProbe
    match method {
        "GetSystemScanMgr" => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|v| v as u32)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            let now_unix = ctx.now_unix();
            let (sigs, anomalies) = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
            let bound_id = match ctx.bind_object(session, service, sys_id as i64) {
                Ok(b) => b,
                Err(e) => return Some(Err(e)),
            };
            let (wh_class, is_thera, is_shattered, env_effect) = system_wormhole_profile(sys_id);

            let sig_items: Vec<PyRep> = sigs
                .into_iter()
                .filter(|s| !s.collapsed)
                .map(|s| {
                    let wh_item_id = wormhole_item_id_for_signature(sys_id, &s.signature_id);
                    let (mass_stage, wh_size, life_stage, wh_age, is_collapsed) =
                        compute_wormhole_stages(&s, now_unix);
                    PyRep::keyval([
                        ("signatureID", PyRep::String(s.signature_id)),
                        ("itemID", PyRep::Integer(wh_item_id)),
                        ("targetID", PyRep::Integer(wh_item_id)),
                        (
                            "group",
                            PyRep::String(if s.signal_strength >= 0.25 {
                                s.sig_group
                            } else {
                                "Unknown".into()
                            }),
                        ),
                        (
                            "type",
                            PyRep::String(if s.signal_strength >= 1.0 {
                                s.type_name
                            } else {
                                "Unidentified Signature".into()
                            }),
                        ),
                        ("signalStrength", PyRep::Real(s.signal_strength)),
                        (
                            "typeID",
                            s.wormhole_type_id
                                .map(|t| PyRep::Integer(t as i64))
                                .unwrap_or(PyRep::None),
                        ),
                        (
                            "destinationSystemID",
                            s.destination_system_id
                                .map(|d| PyRep::Integer(d as i64))
                                .unwrap_or(PyRep::None),
                        ),
                        ("massStage", PyRep::String(mass_stage.into())),
                        ("wormholeSize", PyRep::Real(wh_size)),
                        ("lifetimeStage", PyRep::String(life_stage.into())),
                        ("wormholeAge", PyRep::Integer(wh_age)),
                        ("remainingMassKg", PyRep::Real(s.remaining_mass_kg)),
                        ("maxJumpMassKg", PyRep::Real(s.max_jump_mass_kg)),
                        ("collapsed", PyRep::Bool(is_collapsed)),
                        ("x", PyRep::Real(s.position[0])),
                        ("y", PyRep::Real(s.position[1])),
                        ("z", PyRep::Real(s.position[2])),
                    ])
                })
                .collect();

            let anom_items: Vec<PyRep> = anomalies
                .into_iter()
                .filter(|a| !a.cleared)
                .map(|a| {
                    PyRep::keyval([
                        ("anomalyID", PyRep::String(a.anomaly_id)),
                        ("name", PyRep::String(a.name)),
                        ("anomalyType", PyRep::String(a.anomaly_type)),
                        ("signalStrength", PyRep::Real(1.0)),
                        ("resolved", PyRep::Bool(true)),
                        ("x", PyRep::Real(a.position[0])),
                        ("y", PyRep::Real(a.position[1])),
                        ("z", PyRep::Real(a.position[2])),
                    ])
                })
                .collect();

            Some(Ok(PyRep::keyval([
                ("boundID", PyRep::Integer(bound_id as i64)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("wormholeClass", PyRep::Integer(wh_class as i64)),
                ("isThera", PyRep::Bool(is_thera)),
                ("isShattered", PyRep::Bool(is_shattered)),
                ("delayedLocal", PyRep::Bool(wh_class > 0)),
                ("allowsCapitalShips", PyRep::Bool(wh_class == 0 || (wh_class >= 5 && !is_shattered))),
                ("environmentalEffect", PyRep::String(env_effect.into())),
                ("signatures", PyRep::List(sig_items)),
                ("anomalies", PyRep::List(anom_items)),
            ])))
        }

        "GetAnomalies" => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = bound_entry
                .map(|b| b.object_id as u32)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            let (_, anomalies) = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
            let anom_items: Vec<PyRep> = anomalies
                .into_iter()
                .filter(|a| !a.cleared)
                .map(|a| {
                    PyRep::keyval([
                        ("anomalyID", PyRep::String(a.anomaly_id)),
                        ("name", PyRep::String(a.name)),
                        ("anomalyType", PyRep::String(a.anomaly_type)),
                        ("signalStrength", PyRep::Real(1.0)),
                        ("resolved", PyRep::Bool(true)),
                        ("x", PyRep::Real(a.position[0])),
                        ("y", PyRep::Real(a.position[1])),
                        ("z", PyRep::Real(a.position[2])),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(anom_items)))
        }

        "RequestScans" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let has_active_probes = ctx.active_scanner_probes.read().ok().is_some_and(|map| {
                map.values().any(|p| p.owner_char_id == char_id && p.state != 0)
            });
            // Preserve S-6 security check when unbound RequestScans is called with no active probes
            if !has_active_probes
                && bound_entry.is_none()
                && (args.is_empty() || matches!(args.first(), Some(PyRep::Real(_))))
                && _kwargs.is_empty()
            {
                return None;
            }

            let sys_id = bound_entry
                .map(|b| b.object_id as u32)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            let _ = ensure_system_signatures_and_anomalies(ctx, sys_id).await;
            let now_unix = ctx.now_unix();
            let sys_handle = ctx.systems.get_or_spawn(sys_id);

            // Optional probe updates from args.first() (`Dict` of probe_id -> settings or `List` of probe configs)
            let override_range_step = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("rangeStep").and_then(|v| v.as_i64()))
                .map(|v| (v as i32).clamp(1, 8));

            let mut moved_balls: Vec<(i64, DVec3)> = Vec::new();
            if let Ok(mut probe_map) = ctx.active_scanner_probes.write() {
                for p in probe_map.values_mut() {
                    if p.owner_char_id == char_id
                        && now_unix
                            >= p.launched_at_unix
                                .saturating_add(p.max_flight_time_s.max(0.0) as u64)
                    {
                        p.state = 0;
                    }
                }

                if let Some(PyRep::Dict(entries)) = args.first() {
                    for (k, v) in entries {
                        let Some(pid) = k.as_i64() else { continue };
                        let Some(probe) = probe_map.get_mut(&pid) else {
                            continue;
                        };
                        if probe.owner_char_id != char_id || probe.state == 0 {
                            continue;
                        }
                        let find_in_v = |key: &str| -> Option<&PyRep> {
                            match v {
                                PyRep::KeyVal(kv) => {
                                    kv.iter().find(|(ek, _)| *ek == key).map(|(_, ev)| ev)
                                }
                                PyRep::Dict(kv) => kv
                                    .iter()
                                    .find(|(ek, _)| ek.as_str() == Some(key))
                                    .map(|(_, ev)| ev),
                                _ => None,
                            }
                        };
                        if let Some(PyRep::Tuple(c) | PyRep::List(c)) =
                            find_in_v("pos").or_else(|| find_in_v("destination"))
                        {
                            if c.len() >= 3 {
                                probe.position = [
                                    c[0].as_f64().unwrap_or(probe.position[0]),
                                    c[1].as_f64().unwrap_or(probe.position[1]),
                                    c[2].as_f64().unwrap_or(probe.position[2]),
                                ];
                                probe.destination = probe.position;
                                moved_balls.push((pid, DVec3::from_array(probe.position)));
                            }
                        } else if let (Some(x), Some(y), Some(z)) = (
                            find_in_v("x").and_then(|a| a.as_f64()),
                            find_in_v("y").and_then(|a| a.as_f64()),
                            find_in_v("z").and_then(|a| a.as_f64()),
                        ) {
                            probe.position = [x, y, z];
                            probe.destination = probe.position;
                            moved_balls.push((pid, DVec3::from_array(probe.position)));
                        }
                        if let Some(step) = find_in_v("rangeStep").and_then(|a| a.as_i64()) {
                            probe.range_step = (step as i32).clamp(1, 8);
                            probe.scan_range_m =
                                0.25 * AU_IN_METERS * 2.0_f64.powi(probe.range_step - 1);
                        } else if let Some(rad_m) = find_in_v("scanRange").and_then(|a| a.as_f64())
                        {
                            probe.scan_range_m = rad_m.max(0.25 * AU_IN_METERS);
                            let au = probe.scan_range_m / (0.25 * AU_IN_METERS);
                            probe.range_step = (au.log2().round() as i32 + 1).clamp(1, 8);
                        }
                    }
                } else if let Some(PyRep::List(list)) = args.first() {
                    let mut owner_probe_ids: Vec<i64> = probe_map
                        .values()
                        .filter(|p| {
                            p.owner_char_id == char_id
                                && p.solar_system_id == sys_id
                                && p.state != 0
                        })
                        .map(|p| p.probe_id)
                        .collect();
                    owner_probe_ids.sort_unstable();
                    for (idx, item) in list.iter().enumerate() {
                        let Some(&pid) = owner_probe_ids.get(idx) else {
                            break;
                        };
                        let Some(probe) = probe_map.get_mut(&pid) else {
                            continue;
                        };
                        if let PyRep::List(coords) | PyRep::Tuple(coords) = item {
                            if coords.len() >= 3 {
                                probe.position = [
                                    coords[0].as_f64().unwrap_or(0.0),
                                    coords[1].as_f64().unwrap_or(0.0),
                                    coords[2].as_f64().unwrap_or(0.0),
                                ];
                                probe.destination = probe.position;
                                moved_balls.push((pid, DVec3::from_array(probe.position)));
                            }
                            if let Some(step) = coords
                                .get(3)
                                .and_then(|a| a.as_i64())
                                .map(|s| (s as i32).clamp(1, 8))
                                .or(override_range_step)
                            {
                                probe.range_step = step;
                                probe.scan_range_m =
                                    0.25 * AU_IN_METERS * 2.0_f64.powi(step - 1);
                            }
                        }
                    }
                } else if let Some(step) = override_range_step {
                    for p in probe_map.values_mut() {
                        if p.owner_char_id == char_id
                            && p.solar_system_id == sys_id
                            && p.state != 0
                        {
                            p.range_step = step;
                            p.scan_range_m = 0.25 * AU_IN_METERS * 2.0_f64.powi(step - 1);
                        }
                    }
                }
            }

            for (pid, new_pos) in moved_balls {
                if let Ok(Some((_, mut b))) = sys_handle.get_ball_snapshot(pid).await {
                    b.position = new_pos;
                    let _ = sys_handle.spawn_ball(b).await;
                }
            }

            let active_probes: Vec<crate::router::ActiveScannerProbe> = ctx
                .active_scanner_probes
                .read()
                .ok()
                .map(|map| {
                    map.values()
                        .filter(|p| {
                            p.owner_char_id == char_id
                                && p.solar_system_id == sys_id
                                && p.state != 0
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            if active_probes.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveProbes".into(),
                    message: "No active scanner probes in space".into(),
                }));
            }

            let (astro_lvl, rangefinding_lvl) = ctx.db.with_snapshot(|snap| {
                let sk = snap.skills.get(&char_id);
                let a = sk.and_then(|m| m.get(&3412)).map(|s| s.level).unwrap_or(3);
                let r = sk.and_then(|m| m.get(&25810)).map(|s| s.level).unwrap_or(0);
                (a, r)
            });
            let skill_mult =
                (1.0 + 0.05 * astro_lvl as f64) * (1.0 + 0.05 * rangefinding_lvl as f64);

            let scan_results = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    let sigs = snap.cosmic_signatures.entry(sys_id).or_default();
                    let mut out = Vec::new();
                    for sig in sigs.iter_mut() {
                        if sig.collapsed {
                            continue;
                        }
                        let sig_pos = DVec3::from_array(sig.position);
                        let mut covering_probes = 0usize;
                        let mut total_effective_strength = 0.0;

                        for probe in &active_probes {
                            let p_pos = DVec3::from_array(probe.position);
                            let dist = p_pos.distance(sig_pos);
                            let radius = probe.scan_range_m.max(0.25 * AU_IN_METERS);
                            if dist <= radius {
                                covering_probes += 1;
                                let step_factor = 0.5_f64.powi(probe.range_step - 1);
                                let dist_factor = (-(dist / radius).powi(2)).exp();
                                let probe_s = probe.base_sensor_strength
                                    * skill_mult
                                    * step_factor
                                    * dist_factor;
                                total_effective_strength += probe_s;
                            }
                        }

                        // 4-probe trilateration requirement:
                        // 1 probe -> sphere (max 0.24), 2 probes -> circle (max 0.49),
                        // 3 probes -> 2 points (max 0.74), >=4 probes -> 3D lock (up to 1.0)
                        let trilateration_cap = match covering_probes {
                            0 => 0.0,
                            1 => 0.24,
                            2 => 0.49,
                            3 => 0.74,
                            _ => 1.0,
                        };
                        let raw_strength =
                            (total_effective_strength / 25.0).min(trilateration_cap);
                        sig.signal_strength = raw_strength.clamp(0.0, trilateration_cap);
                        let resolved = sig.signal_strength >= 1.0;
                        let wh_item_id =
                            wormhole_item_id_for_signature(sys_id, &sig.signature_id);
                        out.push((sig.clone(), wh_item_id, covering_probes, resolved));
                    }
                    Ok::<_, std::convert::Infallible>(out)
                })
                .unwrap();

            let mut rows = Vec::new();
            for (sig, wh_item_id, covering_probes, resolved) in scan_results {
                if resolved {
                    ctx.mark_signature_scanned(char_id, sys_id, wh_item_id);
                }
                let (mass_stage, wh_size, life_stage, wh_age, is_collapsed) =
                    compute_wormhole_stages(&sig, now_unix);
                rows.push(PyRep::keyval([
                    ("id", PyRep::String(sig.signature_id.clone())),
                    ("signatureID", PyRep::String(sig.signature_id)),
                    ("itemID", PyRep::Integer(wh_item_id)),
                    ("targetID", PyRep::Integer(wh_item_id)),
                    (
                        "group",
                        PyRep::String(if sig.signal_strength >= 0.25 {
                            sig.sig_group
                        } else {
                            "Unknown".into()
                        }),
                    ),
                    (
                        "type",
                        PyRep::String(if resolved {
                            sig.type_name
                        } else {
                            "Unidentified Signature".into()
                        }),
                    ),
                    ("signalStrength", PyRep::Real(sig.signal_strength)),
                    ("certainty", PyRep::Real(sig.signal_strength)),
                    ("coveringProbes", PyRep::Integer(covering_probes as i64)),
                    ("resolved", PyRep::Bool(resolved)),
                    ("massStage", PyRep::String(mass_stage.into())),
                    ("wormholeSize", PyRep::Real(wh_size)),
                    ("lifetimeStage", PyRep::String(life_stage.into())),
                    ("wormholeAge", PyRep::Integer(wh_age)),
                    (
                        "x",
                        if resolved {
                            PyRep::Real(sig.position[0])
                        } else {
                            PyRep::None
                        },
                    ),
                    (
                        "y",
                        if resolved {
                            PyRep::Real(sig.position[1])
                        } else {
                            PyRep::None
                        },
                    ),
                    (
                        "z",
                        if resolved {
                            PyRep::Real(sig.position[2])
                        } else {
                            PyRep::None
                        },
                    ),
                    ("collapsed", PyRep::Bool(is_collapsed)),
                ]));
            }

            let _ = ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnSystemScanStarted",
                "charid",
                vec![
                    PyRep::Integer(now_unix as i64),
                    PyRep::Integer(4000),
                    PyRep::Integer(active_probes.len() as i64),
                ],
            );
            let _ = ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnSystemScanStopped",
                "charid",
                vec![
                    PyRep::Integer(active_probes.len() as i64),
                    PyRep::List(rows.clone()),
                    PyRep::List(Vec::new()),
                ],
            );

            Some(Ok(PyRep::List(rows)))
        }

        "RecoverProbes" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = bound_entry
                .map(|b| b.object_id as u32)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);

            let mut target_probe_ids: Vec<i64> = Vec::new();
            if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.first() {
                for item in list {
                    if let Some(pid) = item.as_i64() {
                        target_probe_ids.push(pid);
                    }
                }
            } else if let Some(pid) = args.first().and_then(|a| a.as_i64()) {
                target_probe_ids.push(pid);
            }

            let recovered_probes: Vec<crate::router::ActiveScannerProbe> =
                if let Ok(mut map) = ctx.active_scanner_probes.write() {
                    let ids: Vec<i64> = map
                        .values()
                        .filter(|p| {
                            p.owner_char_id == char_id
                                && p.state != 0
                                && (target_probe_ids.is_empty()
                                    || target_probe_ids.contains(&p.probe_id))
                        })
                        .map(|p| p.probe_id)
                        .collect();
                    ids.into_iter().filter_map(|id| map.remove(&id)).collect()
                } else {
                    Vec::new()
                };

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            for p in &recovered_probes {
                let _ = sys_handle.remove_ball(p.probe_id).await;
            }

            let count = recovered_probes.len() as i32;
            let probe_type_id = recovered_probes
                .first()
                .map(|p| p.probe_type_id)
                .unwrap_or(30013);
            let launcher_id_opt = recovered_probes.first().map(|p| p.launcher_module_id);
            let ship_id_opt = session
                .ship_id
                .or_else(|| recovered_probes.first().map(|p| p.ship_id));

            let (to_launcher, to_cargo) = if count > 0 {
                ctx.db.with_mut_snapshot(|snap| {
                    let mut remaining = count;
                    let mut in_launcher = 0i32;
                    let mut in_cargo = 0i32;

                    if let (Some(ship_id), Some(launcher_id)) = (ship_id_opt, launcher_id_opt) {
                        if let Some(launcher_item) = snap.items.get(&launcher_id).cloned() {
                            if launcher_item.location_id == ship_id {
                                let existing_clip = snap.items.values_mut().find(|i| {
                                    i.owner_id == char_id
                                        && i.location_id == ship_id
                                        && i.flag == launcher_item.flag
                                        && i.item_id != launcher_id
                                        && i.type_id == probe_type_id
                                });
                                if let Some(clip) = existing_clip {
                                    let add = (8 - clip.quantity).max(0).min(remaining);
                                    clip.quantity += add;
                                    clip.version += 1;
                                    remaining -= add;
                                    in_launcher += add;
                                } else {
                                    let add = remaining.min(8);
                                    snap.next_entity_id += 1;
                                    let new_id = snap.next_entity_id;
                                    snap.items.insert(
                                        new_id,
                                        eve_db::InventoryItemRecord {
                                            item_id: new_id,
                                            type_id: probe_type_id,
                                            owner_id: char_id,
                                            location_id: ship_id,
                                            flag: launcher_item.flag,
                                            quantity: add,
                                            singleton: false,
                                            version: 1,
                                            ..Default::default()
                                        },
                                    );
                                    remaining -= add;
                                    in_launcher += add;
                                }
                            }
                        }
                    }

                    if remaining > 0 {
                        if let Some(ship_id) = ship_id_opt {
                            if let Some(cargo_stack) = snap.items.values_mut().find(|i| {
                                i.owner_id == char_id
                                    && i.location_id == ship_id
                                    && i.flag == InventoryFlag::Cargo
                                    && i.type_id == probe_type_id
                                    && !i.singleton
                            }) {
                                cargo_stack.quantity += remaining;
                                cargo_stack.version += 1;
                            } else {
                                snap.next_entity_id += 1;
                                let new_id = snap.next_entity_id;
                                snap.items.insert(
                                    new_id,
                                    eve_db::InventoryItemRecord {
                                        item_id: new_id,
                                        type_id: probe_type_id,
                                        owner_id: char_id,
                                        location_id: ship_id,
                                        flag: InventoryFlag::Cargo,
                                        quantity: remaining,
                                        singleton: false,
                                        version: 1,
                                        ..Default::default()
                                    },
                                );
                            }
                            in_cargo += remaining;
                        }
                    }
                    (in_launcher, in_cargo)
                })
            } else {
                (0, 0)
            };

            let rec_ids: Vec<PyRep> = recovered_probes
                .iter()
                .map(|p| PyRep::Integer(p.probe_id))
                .collect();
            Some(Ok(PyRep::keyval([
                ("recoveredProbeIDs", PyRep::List(rec_ids)),
                ("recoveredCount", PyRep::Integer(count as i64)),
                ("returnedToLauncher", PyRep::Integer(to_launcher as i64)),
                ("returnedToCargo", PyRep::Integer(to_cargo as i64)),
            ])))
        }

        "ReconnectToLostProbes" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = bound_entry
                .map(|b| b.object_id as u32)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            let now_unix = ctx.now_unix();
            let ship_id = session.ship_id.unwrap_or(0);

            let mut reconnected = Vec::new();
            if let Ok(mut map) = ctx.active_scanner_probes.write() {
                for p in map.values_mut() {
                    if p.owner_char_id == char_id && p.solar_system_id == sys_id {
                        if now_unix
                            >= p.launched_at_unix
                                .saturating_add(p.max_flight_time_s.max(0.0) as u64)
                        {
                            p.state = 0;
                        } else {
                            p.state = 1;
                            if ship_id != 0 {
                                p.ship_id = ship_id;
                            }
                            reconnected.push(PyRep::Integer(p.probe_id));
                        }
                    }
                }
            }
            let count = reconnected.len() as i64;
            Some(Ok(PyRep::keyval([
                ("reconnectedProbeIDs", PyRep::List(reconnected)),
                ("reconnectedCount", PyRep::Integer(count)),
            ])))
        }

        "DestroyProbe" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = bound_entry
                .map(|b| b.object_id as u32)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            let target_pid = args.first().and_then(|a| a.as_i64());

            let destroyed_ids: Vec<i64> = if let Ok(mut map) = ctx.active_scanner_probes.write() {
                let ids: Vec<i64> = map
                    .values()
                    .filter(|p| {
                        p.owner_char_id == char_id
                            && (target_pid.is_none() || target_pid == Some(p.probe_id))
                    })
                    .map(|p| p.probe_id)
                    .collect();
                for id in &ids {
                    map.remove(id);
                }
                ids
            } else {
                Vec::new()
            };

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            for pid in &destroyed_ids {
                let _ = sys_handle.remove_ball(*pid).await;
            }

            Some(Ok(PyRep::keyval([
                (
                    "destroyedProbeID",
                    destroyed_ids
                        .first()
                        .copied()
                        .map(PyRep::Integer)
                        .unwrap_or(PyRep::None),
                ),
                ("destroyedCount", PyRep::Integer(destroyed_ids.len() as i64)),
                ("destroyed", PyRep::Bool(!destroyed_ids.is_empty())),
            ])))
        }

        _ => None,
    }
}

