//! `space_destiny` — Domain 5: Solar System `Ballpark`, Stargate Jumps, Probe Scanning, Wormholes, & Abyssal Filaments (`§3.5`).

#![forbid(unsafe_code)]

use crate::formulas::{compute_jump_fatigue_and_cooldown_minutes, compute_probe_scan_signal_pct};
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

        ("beyonce", "CmdWarpToPoint") | ("beyonce", "CmdWarpToStuff") => {
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
            let x = args.first().and_then(|a| a.as_f64()).unwrap_or(0.0);
            let y = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let z = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);

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

            let warp_speed = ctx
                .sde
                .types
                .get(&current_ball.type_id)
                .and_then(|t| t.default_attributes.get(&attr_ids::WARP_SPEED_MULTIPLIER))
                .copied()
                .unwrap_or(3.0);

            let mode =
                MovementMode::new_warp(current_ball.position, DVec3::new(x, y, z), warp_speed);
            match handle.set_movement_mode(ship_id, mode).await {
                Ok(tick) => Some(Ok(PyRep::Integer(tick as i64))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyCommandFailed".into(),
                    message: e,
                })),
            }
        }

        ("beyonce", "StepSimulation") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match ServerContext::require_bound_system(bound_entry, "beyonce") {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let dt = args.first().and_then(|a| a.as_f64()).unwrap_or(1.0);
            let handle = ctx.systems.get_or_spawn(sys_id);
            match handle.step_manual(dt).await {
                Ok(bcast) => Some(Ok(PyRep::Integer(bcast.tick as i64))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyStepFailed".into(),
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
                Ok(None) => Some(Ok(PyRep::None)),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DestinyQueryFailed".into(),
                    message: e,
                })),
            }
        }

        // Stargate Jump, Cyno Jump, Wormhole Traversal, and Abyssal Filament Transition
        ("beyonce", "CmdStargateJump")
        | ("jumpbeaconsvc", "JumpToBeacon")
        | ("wormholeMgr", "WormholeJump")
        | ("abyssalMgr", "ActivateFilament")
        | ("voidSpaceMgr", "ActivateFilament")
        | ("pvpFilamentMgr", "ActivateFilament")
        | ("randomJumpMgr", "ActivateFilament") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let dest_sys_id: SolarSystemId = if method == "CmdStargateJump" {
                let gate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(50001248);
                ctx.sde
                    .jumps
                    .get(&gate_id)
                    .map(|j| j.to_solar_system_id)
                    .or_else(|| args.get(1).and_then(|a| a.as_i64()).map(|v| v as u32))
                    .unwrap_or(30_000_144)
            } else if service == "wormholeMgr" {
                args.first()
                    .and_then(|a| a.as_i64())
                    .map(|v| v as u32)
                    .unwrap_or(31_000_005)
            } else if service == "abyssalMgr" || service == "voidSpaceMgr" {
                32_000_001
            } else {
                args.first()
                    .and_then(|a| a.as_i64())
                    .map(|v| v as u32)
                    .unwrap_or(30_000_144)
            };

            let old_sys = session.solar_system_id.unwrap_or(30_000_142);
            let ship_id = session.ship_id.unwrap_or(100_000_001);
            let old_handle = ctx.systems.get_or_spawn(old_sys);
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

            let new_handle = ctx.systems.get_or_spawn(dest_sys_id);
            let mut new_ball = existing_ball.unwrap_or(Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(15_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 3.45,
                mass: 1_067_000.0,
                radius: 37.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            });
            new_ball.position = DVec3::new(15_000.0, 0.0, 0.0);
            new_ball.velocity = DVec3::ZERO;
            new_ball.mode = MovementMode::Stop;
            let _ = new_handle.spawn_ball(new_ball).await;

            // Update wormhole mass if jumping through wormhole
            if service == "wormholeMgr" {
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(st) = snap.system_states.get_mut(&dest_sys_id) {
                        st.wormhole_remaining_mass_kg =
                            (st.wormhole_remaining_mass_kg - 1_067_000.0).max(0.0);
                    }
                });
            }

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

        // Probe Scanning (`scanSvc`) & Directional Scanner (`directionalScanner`)
        ("scanSvc", "RequestScans") | ("sensorSuite", "ScanSignatures") => {
            let range_au = args.first().and_then(|a| a.as_f64()).unwrap_or(0.25);
            let dist_au = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let sig_pct = compute_probe_scan_signal_pct(40.0, 10.0, range_au, dist_au);
            Some(Ok(PyRep::keyval([
                ("signatureID", PyRep::String("ABC-123".into())),
                ("signalStrengthPct", PyRep::Real(sig_pct)),
                ("warpable", PyRep::Bool(sig_pct >= 100.0)),
                ("wormholeSystemID", PyRep::Integer(31_000_005)),
            ])))
        }

        ("directionalScanner", "DirectionalScan") | ("scanSvc", "ConeScan") => {
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
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

        ("keeper", _)
        | ("space", _)
        | ("spaceMgr", _)
        | ("michelle", _)
        | ("map", _)
        | ("solarsystemInterferenceMgr", _) => Some(Ok(PyRep::Bool(true))),

        _ => None,
    }
}
