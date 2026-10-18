//! `sov_fw_insurgency` — Domain 11: Sovereignty, Equinox Skyhooks/Mercenary Dens, Faction Warfare, & Pirate Insurgencies (`§3.8`, `§3.11`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("facWarMgr", "JoinFactionAsCharacter")
        | ("fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(faction_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidWarFaction".into(),
                    message: "Joining Factional Warfare requires an explicit integer factionID".into(),
                }));
            };
            if !matches!(
                faction_id,
                500_001 | 500_002 | 500_003 | 500_004 | 500_010 | 500_011
            ) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidWarFaction".into(),
                    message: format!("Faction {faction_id} is not a valid FW/Insurgency militia faction"),
                }));
            }
            let _ = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.war_faction_id = Some(faction_id);
                    ch.version += 1;
                }
                Ok::<_, std::convert::Infallible>(())
            });
            session.war_faction_id = Some(faction_id);
            Some(Ok(PyRep::keyval([
                ("factionID", PyRep::Integer(faction_id)),
                ("enlisted", PyRep::Bool(true)),
            ])))
        }

        ("facWarMgr", "CaptureComplex") | ("fwWarzoneSolarsystem", "CaptureComplex") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot capture FW/Insurgency complex while docked".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            if !(sys_sec > 0.0 && sys_sec < 0.45) {
                return Some(Err(BridgeRpcError {
                    code: "NotLowSecWarzoneSystem".into(),
                    message: format!(
                        "Solar system {sys_id} (security {sys_sec:.2}) is not a low-sec FW warzone system (requires 0.0 < sec < 0.45)"
                    ),
                }));
            }

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some((current_tick, ship_ball)) =
                (match sys_handle.get_ball_snapshot(ship_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "DestinyQueryFailed".into(),
                            message: e,
                        }))
                    }
                })
            else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: format!("Ship {ship_id} is not spawned in space in system {sys_id}"),
                }));
            };

            let effective_tick = current_tick.max(1);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let faction_id = snap
                    .characters
                    .get(&char_id)
                    .and_then(|c| c.war_faction_id)
                    .or(session.war_faction_id);
                let Some(fid) = faction_id else {
                    return Err(BridgeRpcError {
                        code: "NotEnlistedInFactionalWarfare".into(),
                        message:
                            "Character must enlist in Factional Warfare before capturing a complex"
                                .into(),
                    });
                };

                let mut outside_radius_dist = None;
                if let Some(st) = snap.system_states.get(&sys_id) {
                    if let Some(beacon_coords) = st.fw_complex_position {
                        let beacon_pos = glam::DVec3::from_array(beacon_coords);
                        let dist = ship_ball.position.distance(beacon_pos);
                        if dist > 30_000.0 {
                            outside_radius_dist = Some((dist, beacon_coords));
                        }
                    }
                }
                if let Some((dist, beacon_coords)) = outside_radius_dist {
                    if let Some(st) = snap.system_states.get_mut(&sys_id) {
                        st.last_fw_capture_tick = 0;
                    }
                    return Ok(Err((dist, beacon_coords)));
                }

                if let Some(st) = snap.system_states.get_mut(&sys_id) {
                    let Some(_beacon_coords) = st.fw_complex_position else {
                        return Err(BridgeRpcError {
                            code: "NoActiveComplexInSystem".into(),
                            message: format!("System {sys_id} has no active FW complex beacon"),
                        });
                    };
                    if st.fw_contested_pct >= 100.0 {
                        return Err(BridgeRpcError {
                            code: "ComplexAlreadyCaptured".into(),
                            message: format!("System {sys_id} is already 100% contested"),
                        });
                    };
                    if st.last_fw_capture_tick > 0 && effective_tick <= st.last_fw_capture_tick {
                        return Err(BridgeRpcError {
                            code: "ComplexCaptureTimerActive".into(),
                            message:
                                "Complex capture timer has already pulsed on the current simulation tick"
                                    .into(),
                        });
                    };
                    let duration = st.fw_capture_duration_seconds.max(600);
                    let tick_delta = if st.last_fw_capture_tick == 0 {
                        1
                    } else {
                        effective_tick
                            .saturating_sub(st.last_fw_capture_tick)
                            .clamp(1, 30)
                    };
                    st.last_fw_capture_tick = effective_tick;
                    st.fw_capture_seconds_elapsed = st
                        .fw_capture_seconds_elapsed
                        .saturating_add(tick_delta)
                        .min(duration);
                    st.fw_contested_pct =
                        ((st.fw_capture_seconds_elapsed as f64 / duration as f64) * 100.0)
                            .min(100.0);
                    st.insurgency_corruption_stage =
                        ((st.fw_contested_pct / 20.0).ceil() as u8).clamp(1, 5);
                    let lp_awarded = if st.fw_capture_seconds_elapsed >= duration {
                        15_000i64
                    } else {
                        0i64
                    };
                    let res_tuple = (
                        st.fw_contested_pct,
                        st.insurgency_corruption_stage,
                        lp_awarded,
                        st.fw_capture_seconds_elapsed,
                        duration,
                    );
                    if lp_awarded > 0 {
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            *ch.loyalty_points.entry(fid).or_insert(0) += lp_awarded;
                        }
                    }
                    Ok(Ok(res_tuple))
                } else {
                    Err(BridgeRpcError {
                        code: "SystemStateNotFound".into(),
                        message: format!("System {sys_id} has no FW/Insurgency state"),
                    })
                }
            });

            match res {
                Ok(Ok((contested, corruption, lp_awarded, elapsed_s, duration_s))) => {
                    Some(Ok(PyRep::keyval([
                        ("solarSystemID", PyRep::Integer(sys_id as i64)),
                        ("contestedPct", PyRep::Real(contested)),
                        ("corruptionStage", PyRep::Integer(corruption as i64)),
                        ("lpAwarded", PyRep::Integer(lp_awarded)),
                        ("captureSecondsElapsed", PyRep::Integer(elapsed_s as i64)),
                        ("captureDurationSeconds", PyRep::Integer(duration_s as i64)),
                    ])))
                }
                Ok(Err((dist, beacon_coords))) => {
                    Some(Err(BridgeRpcError {
                        code: "OutsideComplexCaptureRadius".into(),
                        message: format!(
                            "Ship is {:.1} m from complex capture beacon {:?} (must be <= 30000 m)",
                            dist, beacon_coords
                        ),
                    }))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("sovMgr", "AcquireSkyhooks") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot claim sovereignty or acquire skyhooks while docked".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            if sys_sec > 0.0 {
                return Some(Err(BridgeRpcError {
                    code: "NotNullSecSystem".into(),
                    message: format!(
                        "Solar system {sys_id} (security {sys_sec:.2}) is not a claimable null-sec system"
                    ),
                }));
            }

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some((current_tick, ship_ball)) =
                (match sys_handle.get_ball_snapshot(ship_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "DestinyQueryFailed".into(),
                            message: e,
                        }))
                    }
                })
            else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: format!("Ship {ship_id} is not spawned in space in system {sys_id}"),
                }));
            };

            let effective_tick = current_tick.max(1);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (caller_corp_id, caller_alliance_id) = snap
                    .characters
                    .get(&char_id)
                    .map(|c| (c.corporation_id, c.alliance_id.or(session.alliance_id)))
                    .unwrap_or((session.corporation_id.unwrap_or(0), session.alliance_id));
                let Some(alliance_id) = caller_alliance_id else {
                    return Err(BridgeRpcError {
                        code: "NotInAlliance".into(),
                        message:
                            "Acquiring sovereignty/skyhooks requires membership in a player alliance"
                                .into(),
                    });
                };
                if !snap.alliances.contains_key(&alliance_id) {
                    return Err(BridgeRpcError {
                        code: "AllianceNotFound".into(),
                        message: format!("Alliance {alliance_id} is not registered"),
                    });
                }

                // Require an online (`ShieldVulnerable`), quantum-core-installed, fueled (`fuel_blocks > 0`) structure in sys_id owned by the caller's player corporation or alliance
                let friendly_structs: Vec<[f64; 3]> = snap
                    .structures
                    .values()
                    .filter(|s| {
                        let same_player_corp =
                            caller_corp_id >= 98_000_000 && s.owner_corp_id == caller_corp_id;
                        let same_alliance = snap
                            .corporations
                            .get(&s.owner_corp_id)
                            .and_then(|c| c.alliance_id)
                            == Some(alliance_id);
                        s.solar_system_id == sys_id
                            && (same_player_corp || same_alliance)
                            && s.state == "ShieldVulnerable"
                            && s.quantum_core_installed
                            && s.fuel_blocks > 0
                    })
                    .map(|s| s.position)
                    .collect();
                if friendly_structs.is_empty() {
                    return Err(BridgeRpcError {
                        code: "NoFriendlySovStructureInSystem".into(),
                        message: format!(
                            "Claiming sovereignty in system {sys_id} requires an online structure owned by corporation {caller_corp_id} or alliance {alliance_id}"
                        ),
                    });
                }
                let min_dist = friendly_structs
                    .iter()
                    .map(|pos| ship_ball.position.distance(glam::DVec3::from_array(*pos)))
                    .fold(f64::INFINITY, f64::min);
                if min_dist > 50_000.0 {
                    return Err(BridgeRpcError {
                        code: "OutsideSovStructureProximity".into(),
                        message: format!(
                            "Ship is {:.1} m from nearest friendly sovereignty structure (must be <= 50000 m)",
                            min_dist
                        ),
                    });
                }

                let Some(st) = snap.system_states.get_mut(&sys_id) else {
                    return Err(BridgeRpcError {
                        code: "SystemStateNotFound".into(),
                        message: format!("System {sys_id} has no sovereignty state"),
                    });
                };
                if let Some(existing_alliance) = st.sov_alliance_id {
                    if existing_alliance != alliance_id {
                        return Err(BridgeRpcError {
                            code: "SovereigntyHeldByAnotherAlliance".into(),
                            message: format!(
                                "System {sys_id} sovereignty is held by alliance {existing_alliance}"
                            ),
                        });
                    }
                }
                if st.last_sov_claim_tick > 0 && effective_tick <= st.last_sov_claim_tick {
                    return Err(BridgeRpcError {
                        code: "SovereigntyClaimCycleActive".into(),
                        message: "Sovereignty index was already updated on the current simulation tick"
                            .into(),
                    });
                }
                st.last_sov_claim_tick = effective_tick;
                st.sov_alliance_id = Some(alliance_id);
                st.military_index = (st.military_index + 1).min(5);
                st.industrial_index = (st.industrial_index + 1).min(5);
                Ok((alliance_id, st.military_index, st.industrial_index))
            });

            match res {
                Ok((alliance_id, mil_idx, ind_idx)) => Some(Ok(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                    ("allianceID", PyRep::Integer(alliance_id)),
                    ("militaryIndex", PyRep::Integer(mil_idx as i64)),
                    ("industrialIndex", PyRep::Integer(ind_idx as i64)),
                    ("skyhookOnline", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
