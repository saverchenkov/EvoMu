//! Fleet Operations, Options, Broadcasts, Fleet Warp & Fleet Finder (`SO-2`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{FleetMemberRecord, FleetRole};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

use super::fleet_social_fleets::{
    ensure_fleet_channel_member, role_to_i64, FleetAdvertState, FLEET_EXTRAS,
};
use super::parse_dest_id;

const WARP_MIN_DISTANCE_M: f64 = 150_000.0;

pub(super) async fn handle_fleet_ops(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("fleetObjectHandler", "SendBroadcast")
        | ("fleetMgr", "SendBroadcast")
        | ("fleetMgr", "BroadcastToBubble")
        | ("fleetObjectHandler", "BroadcastToBubble")
        | ("fleetMgr", "BroadcastToSystem")
        | ("fleetObjectHandler", "BroadcastToSystem") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            let broadcast_name = args.first().and_then(|a| a.as_str()).unwrap_or("Target");
            let scope = if method == "BroadcastToBubble" {
                2
            } else if method == "BroadcastToSystem" {
                1
            } else {
                args.get(1).and_then(|a| a.as_i64()).unwrap_or(0)
            };
            let item_id = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                .unwrap_or(0);

            let sender_sys = session.solar_system_id.unwrap_or(30_000_142);
            let members_info: Vec<(i64, u32, i64)> = ctx.db.with_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get(&fleet_id) else {
                    return Vec::new();
                };
                fleet
                    .members
                    .iter()
                    .map(|&m_id| {
                        let ch = snap.characters.get(&m_id);
                        (
                            m_id,
                            ch.map(|c| c.solar_system_id).unwrap_or(sender_sys),
                            ch.map(|c| c.active_ship_id).unwrap_or(0),
                        )
                    })
                    .collect()
            });
            let payload = vec![
                PyRep::String(broadcast_name.to_string()),
                PyRep::Integer(scope),
                PyRep::Integer(char_id),
                PyRep::Integer(item_id),
            ];
            let mut recipient_ids = Vec::new();
            if scope == 0 {
                ctx.notify(
                    NotificationTarget::Fleet(fleet_id),
                    "OnFleetBroadcast",
                    "fleetid",
                    payload,
                );
                recipient_ids = members_info.iter().map(|(id, _, _)| *id).collect();
            } else {
                let sys = ctx.systems.get_or_spawn(sender_sys);
                let sender_ship = session.ship_id.unwrap_or(0);
                let sender_pos = sys
                    .get_ball_snapshot(sender_ship)
                    .await
                    .ok()
                    .flatten()
                    .map(|(_, b)| b.position);
                for (m_id, m_sys, m_ship) in members_info {
                    if m_sys != sender_sys {
                        continue;
                    }
                    if scope == 2 && m_id != char_id {
                        if let Some(s_pos) = sender_pos {
                            let in_bubble = sys
                                .get_ball_snapshot(m_ship)
                                .await
                                .ok()
                                .flatten()
                                .is_some_and(|(_, b)| b.position.distance(s_pos) <= 250_000.0);
                            if !in_bubble {
                                continue;
                            }
                        }
                    }
                    recipient_ids.push(m_id);
                    ctx.notify(
                        NotificationTarget::Character(m_id),
                        "OnFleetBroadcast",
                        "charid",
                        payload.clone(),
                    );
                }
            }

            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fleet_id)),
                ("broadcast", PyRep::String(broadcast_name.to_string())),
                ("broadcastType", PyRep::String(broadcast_name.to_string())),
                ("senderID", PyRep::Integer(char_id)),
                ("targetID", PyRep::Integer(item_id)),
                ("scope", PyRep::Integer(scope)),
                ("itemID", PyRep::Integer(item_id)),
                (
                    "recipientIDs",
                    PyRep::List(recipient_ids.into_iter().map(PyRep::Integer).collect()),
                ),
            ])))
        }

        ("fleetMgr", "WarpToMember") | ("fleetObjectHandler", "WarpToMember") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            let Some(target_char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "WarpToMember requires target characterID".into(),
                }));
            };
            let min_range = args
                .get(1)
                .and_then(|a| a.as_f64())
                .or_else(|| kwargs.get("minRange").and_then(|a| a.as_f64()))
                .unwrap_or(0.0)
                .clamp(0.0, 100_000.0);

            let (caller_ship_id, target_ship_id, sys_id) = match ctx.db.with_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                if !fleet.member_details.contains_key(&target_char_id) {
                    return Err(BridgeRpcError {
                        code: "MemberNotFound".into(),
                        message: format!("Character {target_char_id} is not in fleet {fleet_id}"),
                    });
                }
                let caller = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: "Caller character not found".into(),
                })?;
                let target = snap
                    .characters
                    .get(&target_char_id)
                    .ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: "Target character not found".into(),
                    })?;
                if caller.solar_system_id != target.solar_system_id {
                    return Err(BridgeRpcError {
                        code: "DifferentSolarSystem".into(),
                        message: "Cannot warp to a fleet member in another solar system".into(),
                    });
                }
                Ok((
                    session.ship_id.unwrap_or(caller.active_ship_id),
                    target.active_ship_id,
                    caller.solar_system_id,
                ))
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let sys = ctx.systems.get_or_spawn(sys_id);
            let Some((_, target_ball)) = sys.get_ball_snapshot(target_ship_id).await.ok().flatten()
            else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotInSpace".into(),
                    message: "Target fleet member is not in space".into(),
                }));
            };
            let Some((_, caller_ball)) = sys.get_ball_snapshot(caller_ship_id).await.ok().flatten()
            else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Caller ship is not in space".into(),
                }));
            };
            let dist = caller_ball.position.distance(target_ball.position);
            if dist < WARP_MIN_DISTANCE_M {
                return Some(Err(BridgeRpcError {
                    code: "WarpDistanceTooClose".into(),
                    message: format!("Fleet member is within minimum warp distance ({dist:.0} m)"),
                }));
            }
            let dir = (target_ball.position - caller_ball.position).normalize_or_zero();
            let dest = target_ball.position - dir * min_range;
            let warp_mode = MovementMode::new_warp(caller_ball.position, dest, 3.0);
            let _ = sys.set_movement_mode(caller_ship_id, warp_mode).await;

            Some(Ok(PyRep::keyval([
                ("status", PyRep::String("Warping".into())),
                ("targetCharacterID", PyRep::Integer(target_char_id)),
                ("destX", PyRep::Real(dest.x)),
                ("destY", PyRep::Real(dest.y)),
                ("destZ", PyRep::Real(dest.z)),
                ("destinationX", PyRep::Real(dest.x)),
                ("destinationY", PyRep::Real(dest.y)),
                ("destinationZ", PyRep::Real(dest.z)),
                ("distance", PyRep::Real(min_range)),
            ])))
        }

        ("fleetMgr", "FleetWarp")
        | ("fleetObjectHandler", "FleetWarp")
        | ("fleetMgr", "WarpFleet")
        | ("fleetObjectHandler", "WarpFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);

            let (dest_id, explicit_dest, min_range) = if args.len() >= 3
                && args.first().and_then(|a| a.as_f64()).is_some()
                && args.get(1).and_then(|a| a.as_f64()).is_some()
                && args.get(2).and_then(|a| a.as_f64()).is_some()
            {
                let x = args.first().and_then(|a| a.as_f64()).unwrap_or(0.0);
                let y = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
                let z = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);
                let d = args.get(3).and_then(|a| a.as_f64()).unwrap_or(0.0);
                (0_i64, Some(DVec3::new(x, y, z)), d.clamp(0.0, 100_000.0))
            } else {
                let Some(did) = parse_dest_id(args, kwargs) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "FleetWarp requires destination entity ID".into(),
                    }));
                };
                let d = args
                    .get(1)
                    .and_then(|a| a.as_f64())
                    .or_else(|| kwargs.get("minRange").and_then(|a| a.as_f64()))
                    .unwrap_or(0.0)
                    .clamp(0.0, 100_000.0);
                (did, None, d)
            };

            let eligible_ship_ids = match ctx.db.with_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let Some(caller_member) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Caller not in fleet".into(),
                    });
                };
                let caller_role = role_to_i64(caller_member.role);
                if caller_role > 3 && fleet.boss_id != char_id {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Fleet, Wing, or Squad Commanders can initiate Fleet Warp".into(),
                    });
                }
                let mut ships = Vec::new();
                for m in fleet.member_details.values() {
                    if !m.takes_fleet_warp && m.character_id != char_id {
                        continue;
                    }
                    let in_scope = match caller_member.role {
                        FleetRole::FleetCommander => true,
                        FleetRole::WingCommander => m.wing_id == caller_member.wing_id,
                        FleetRole::SquadCommander => m.squad_id == caller_member.squad_id,
                        FleetRole::SquadMember => fleet.boss_id == char_id,
                    };
                    if !in_scope {
                        continue;
                    }
                    if let Some(ch) = snap.characters.get(&m.character_id) {
                        if ch.solar_system_id == sys_id
                            && ch.station_id.is_none()
                            && ch.structure_id.is_none()
                        {
                            ships.push((m.character_id, ch.active_ship_id));
                        }
                    }
                }
                Ok(ships)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let formation_id = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("formationID").and_then(|a| a.as_i64()))
                .unwrap_or(0);
            let formation_spacing = args
                .get(3)
                .and_then(|a| a.as_f64())
                .or_else(|| kwargs.get("formationSpacing").and_then(|a| a.as_f64()))
                .unwrap_or(match formation_id {
                    1 | 3 | 4 => 5_000.0,
                    2 => 10_000.0,
                    _ => 0.0,
                })
                .clamp(0.0, 100_000.0);

            let sys = ctx.systems.get_or_spawn(sys_id);
            let caller_ship_id = session.ship_id.unwrap_or(0);
            let caller_pos = sys
                .get_ball_snapshot(caller_ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position);
            let dest_pos = if let Some(pos) = explicit_dest {
                pos
            } else if let Ok(Some((_, b))) = sys.get_ball_snapshot(dest_id).await {
                b.position
            } else if let Some(bm_pos) = ctx.db.with_snapshot(|snap| {
                snap.bookmarks
                    .get(&dest_id)
                    .map(|bm| DVec3::new(bm.x, bm.y, bm.z))
            }) {
                bm_pos
            } else if let Some(st) = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .and_then(|s| s.stations.iter().find(|st| st.station_id == dest_id))
            {
                DVec3::new(st.position.x, st.position.y, st.position.z)
            } else {
                DVec3::new(150_000_000.0, 0.0, 0.0)
            };

            let mut warped_members = Vec::new();
            let mut landing_positions = Vec::new();
            for (idx, (member_char_id, ship_id)) in eligible_ship_ids.into_iter().enumerate() {
                if let Ok(Some((_, ball))) = sys.get_ball_snapshot(ship_id).await {
                    if ball.max_velocity <= 0.0 {
                        continue;
                    }
                    if sys.is_ball_scrambled(ship_id).await.unwrap_or(false) {
                        continue;
                    }
                    if let Some(c_pos) = caller_pos {
                        if member_char_id != char_id && ball.position.distance(c_pos) > 8_000_000.0
                        {
                            continue;
                        }
                    }
                    let dist = ball.position.distance(dest_pos);
                    if dist >= WARP_MIN_DISTANCE_M {
                        let dir = (dest_pos - ball.position).normalize_or_zero();
                        let i = idx as f64;
                        let offset = match formation_id {
                            1 => DVec3::new((idx % 5) as f64 * formation_spacing, 0.0, (idx / 5) as f64 * formation_spacing),
                            2 => DVec3::new(i.cos() * formation_spacing, (i * 0.7).sin() * formation_spacing, i.sin() * formation_spacing),
                            3 => DVec3::new(((idx + 1) / 2) as f64 * if idx % 2 == 0 { 1.0 } else { -1.0 } * formation_spacing, 0.0, -(((idx + 1) / 2) as f64) * formation_spacing),
                            4 => DVec3::new((idx % 5) as f64 * formation_spacing, ((idx / 5) as f64) * formation_spacing, 0.0),
                            5 => caller_pos.map(|cp| (ball.position - cp).clamp_length_max(100_000.0)).unwrap_or(DVec3::ZERO),
                            _ => DVec3::ZERO,
                        };
                        let landing = dest_pos - dir * min_range + offset;
                        let warp_mode = MovementMode::new_warp(ball.position, landing, 3.0);
                        if sys.set_movement_mode(ship_id, warp_mode).await.is_ok() {
                            warped_members.push(PyRep::Integer(member_char_id));
                            landing_positions.push(PyRep::Tuple(vec![
                                PyRep::Integer(member_char_id),
                                PyRep::Real(landing.x),
                                PyRep::Real(landing.y),
                                PyRep::Real(landing.z),
                            ]));
                        }
                    }
                }
            }

            ctx.notify(
                NotificationTarget::Fleet(fleet_id),
                "OnFleetBroadcast",
                "fleetid",
                vec![
                    PyRep::String("FleetWarp".into()),
                    PyRep::Integer(char_id),
                    PyRep::Real(dest_pos.x),
                    PyRep::Real(dest_pos.y),
                    PyRep::Real(dest_pos.z),
                ],
            );

            Some(Ok(PyRep::keyval([
                ("status", PyRep::String("FleetWarpInitiated".into())),
                ("fleetID", PyRep::Integer(fleet_id)),
                ("destinationID", PyRep::Integer(dest_id)),
                ("formationID", PyRep::Integer(formation_id)),
                ("formationSpacing", PyRep::Real(formation_spacing)),
                ("warpCount", PyRep::Integer(warped_members.len() as i64)),
                ("warpedMembers", PyRep::Integer(warped_members.len() as i64)),
                ("warpedMemberIDs", PyRep::List(warped_members)),
                ("landingPositions", PyRep::List(landing_positions)),
                ("destinationX", PyRep::Real(dest_pos.x)),
                ("destinationY", PyRep::Real(dest_pos.y)),
                ("destinationZ", PyRep::Real(dest_pos.z)),
            ])))
        }

        ("fleetMgr", "SetMotd")
        | ("fleetObjectHandler", "SetMotd")
        | ("fleetObjectHandler", "SetMotdEx") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            let motd = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let caller_role = fleet
                    .member_details
                    .get(&char_id)
                    .map(|m| role_to_i64(m.role))
                    .unwrap_or(4);
                if fleet.boss_id != char_id && caller_role > 1 {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Fleet Boss or Fleet Commander can set MOTD".into(),
                    });
                }
                fleet.motd = motd.clone();
                Ok(())
            });
            match res {
                Ok(()) => {
                    ctx.notify(
                        NotificationTarget::Fleet(fleet_id),
                        "OnFleetMotdChanged",
                        "fleetid",
                        vec![PyRep::String(motd.clone())],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("motd", PyRep::String(motd)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "GetMotd") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = session.fleet_id.unwrap_or(0);
            let motd = ctx.db.with_snapshot(|snap| {
                snap.fleets
                    .get(&fleet_id)
                    .map(|f| f.motd.clone())
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::String(motd)))
        }

        ("fleetObjectHandler", "SetOptions") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            let is_free_move = match args.first() {
                Some(PyRep::Bool(b)) => *b,
                Some(PyRep::Integer(i)) => *i != 0,
                Some(PyRep::KeyVal(entries)) => entries
                    .iter()
                    .find(|(k, _)| k.as_str() == "isFreeMove")
                    .and_then(|(_, v)| match v {
                        PyRep::Bool(b) => Some(*b),
                        PyRep::Integer(i) => Some(*i != 0),
                        _ => None,
                    })
                    .unwrap_or(false),
                _ => false,
            };
            let is_registered = match args.get(1) {
                Some(PyRep::Bool(b)) => Some(*b),
                Some(PyRep::Integer(i)) => Some(*i != 0),
                _ => None,
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                if fleet.boss_id != char_id {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Fleet Boss can change fleet options".into(),
                    });
                }
                fleet.free_move = is_free_move;
                if let Some(reg) = is_registered {
                    fleet.is_registered = reg;
                }
                Ok(fleet.is_registered)
            });
            match res {
                Ok(reg) => {
                    ctx.notify(
                        NotificationTarget::Fleet(fleet_id),
                        "OnFleetOptionsChanged",
                        "fleetid",
                        vec![PyRep::Bool(is_free_move), PyRep::Bool(reg)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("isFreeMove", PyRep::Bool(is_free_move)),
                        ("isRegistered", PyRep::Bool(reg)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "SetAutoJoinSquadID") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = session.fleet_id.unwrap_or(0);
            let squad_id = args.first().and_then(|a| a.as_i64());
            let res = ctx.db.with_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                if fleet.boss_id != char_id {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Fleet Boss can set auto-join squad".into(),
                    });
                }
                Ok(())
            });
            match res {
                Ok(()) => {
                    FLEET_EXTRAS
                        .lock()
                        .unwrap()
                        .auto_join_squads
                        .insert(fleet_id, squad_id);
                    Some(Ok(PyRep::keyval([(
                        "autoJoinSquadID",
                        squad_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    )])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "SetFleetMaxSize") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = session.fleet_id.unwrap_or(0);
            let max_size = args
                .first()
                .and_then(|a| a.as_i64())
                .unwrap_or(256)
                .clamp(1, 256) as u16;
            let res = ctx.db.with_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                if fleet.boss_id != char_id {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only Fleet Boss can change fleet max size".into(),
                    });
                }
                Ok(())
            });
            match res {
                Ok(()) => {
                    FLEET_EXTRAS
                        .lock()
                        .unwrap()
                        .max_sizes
                        .insert(fleet_id, max_size);
                    Some(Ok(PyRep::keyval([(
                        "maxSize",
                        PyRep::Integer(max_size as i64),
                    )])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "GetFleetMaxSize") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = session.fleet_id.unwrap_or(0);
            let max_size = FLEET_EXTRAS
                .lock()
                .unwrap()
                .max_sizes
                .get(&fleet_id)
                .copied()
                .unwrap_or(256);
            Some(Ok(PyRep::Integer(max_size as i64)))
        }

        ("fleetObjectHandler", "SetTakesFleetWarp")
        | ("fleetObjectHandler", "SetAcceptsRegroupValue")
        | ("fleetObjectHandler", "SetAcceptsConduitJumpsValue") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            let flag = match args.first() {
                Some(PyRep::Bool(b)) => *b,
                Some(PyRep::Integer(i)) => *i != 0,
                _ => true,
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                let Some(member) = fleet.member_details.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "MemberNotFound".into(),
                        message: "Character not in fleet".into(),
                    });
                };
                member.takes_fleet_warp = flag;
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("characterID", PyRep::Integer(char_id)),
                    ("takesFleetWarp", PyRep::Bool(flag)),
                    ("acceptsRegroup", PyRep::Bool(flag)),
                    ("acceptsConduitJumps", PyRep::Bool(flag)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "LoadFleetSetup") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            let setup_name = args.first().and_then(|a| a.as_str()).unwrap_or("Default Setup");
            let new_motd = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("Loaded Fleet Setup MOTD");
            let is_free_move =
                matches!(args.get(2), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let max_size = args
                .get(3)
                .and_then(|a| a.as_i64())
                .unwrap_or(256)
                .clamp(1, 256) as u16;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                if fleet.boss_id != char_id {
                    return Err(BridgeRpcError {
                        code: "FleetPermissionDenied".into(),
                        message: "Only the Fleet Boss can load a fleet setup".into(),
                    });
                }
                fleet.motd = new_motd.to_string();
                fleet.free_move = is_free_move;
                Ok(())
            });
            match res {
                Ok(()) => {
                    {
                        let mut ex = FLEET_EXTRAS.lock().unwrap();
                        ex.fleet_names.insert(fleet_id, setup_name.to_string());
                        ex.max_sizes.insert(fleet_id, max_size);
                    }
                    ctx.notify(
                        NotificationTarget::Fleet(fleet_id),
                        "OnFleetSetupChanged",
                        "fleetid",
                        vec![PyRep::String(setup_name.to_string())],
                    );
                    ctx.notify(
                        NotificationTarget::Fleet(fleet_id),
                        "OnFleetMotdChanged",
                        "fleetid",
                        vec![PyRep::String(new_motd.to_string())],
                    );
                    ctx.notify(
                        NotificationTarget::Fleet(fleet_id),
                        "OnFleetOptionsChanged",
                        "fleetid",
                        vec![PyRep::Bool(is_free_move), PyRep::Bool(true)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("setupName", PyRep::String(setup_name.to_string())),
                        ("motd", PyRep::String(new_motd.to_string())),
                        ("isFreeMove", PyRep::Bool(is_free_move)),
                        ("maxSize", PyRep::Integer(max_size as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "GetFleetComposition") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            let is_authorized = ctx.db.with_snapshot(|snap| {
                snap.fleets.get(&fleet_id).is_some_and(|fl| {
                    if fl.boss_id == char_id {
                        return true;
                    }
                    if let Some(m) = fl.member_details.get(&char_id) {
                        matches!(
                            m.role,
                            FleetRole::FleetCommander
                                | FleetRole::WingCommander
                                | FleetRole::SquadCommander
                        )
                    } else {
                        false
                    }
                })
            });
            if !is_authorized {
                return Some(Err(BridgeRpcError {
                    code: "FleetRoleDenied".into(),
                    message: "Only fleet commanders can view fleet composition".into(),
                }));
            }
            let members_info = ctx.db.with_snapshot(|snap| {
                let Some(fl) = snap.fleets.get(&fleet_id) else {
                    return Vec::new();
                };
                fl.member_details
                    .values()
                    .map(|m| {
                        let (sys_id, ship_id, ship_type_id) = snap
                            .characters
                            .get(&m.character_id)
                            .map(|c| {
                                let st = snap
                                    .items
                                    .get(&c.active_ship_id)
                                    .map(|i| i.type_id)
                                    .unwrap_or(606);
                                (c.solar_system_id, c.active_ship_id, st)
                            })
                            .unwrap_or((30_000_142, 0, 606));
                        (
                            m.character_id,
                            ship_type_id,
                            sys_id,
                            ship_id,
                            m.wing_id.unwrap_or(1),
                            m.squad_id.unwrap_or(1),
                            role_to_i64(m.role),
                            m.takes_fleet_warp,
                        )
                    })
                    .collect::<Vec<_>>()
            });

            let mut rows = Vec::with_capacity(members_info.len());
            for (cid, ship_type_id, sys_id, ship_id, wid, sid, role_num, takes_warp) in members_info
            {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let pos = sys
                    .get_ball_snapshot(ship_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|(_, b)| b.position)
                    .unwrap_or(DVec3::ZERO);

                rows.push(vec![
                    PyRep::Integer(cid),
                    PyRep::Integer(ship_type_id as i64),
                    PyRep::Integer(sys_id as i64),
                    PyRep::Integer(wid),
                    PyRep::Integer(sid),
                    PyRep::Integer(role_num),
                    PyRep::Bool(takes_warp),
                    PyRep::Real(pos.x),
                    PyRep::Real(pos.y),
                    PyRep::Real(pos.z),
                ]);
            }
            Some(Ok(PyRep::rowset(
                vec![
                    "characterID",
                    "shipTypeID",
                    "solarSystemID",
                    "wingID",
                    "squadID",
                    "role",
                    "takesFleetWarp",
                    "x",
                    "y",
                    "z",
                ],
                rows,
            )))
        }

        ("fleetMgr", "GetInitState")
        | ("fleetObjectHandler", "GetInitState")
        | ("fleetMgr", "GetWings")
        | ("fleetObjectHandler", "GetWings") => {
            handle_get_init_state(ctx, session)
        }
        ("fleetMgr", m) | ("fleetObjectHandler", m) if m.eq_ignore_ascii_case("getfleet") => {
            handle_get_init_state(ctx, session)
        }

        ("fleetMgr", "AddToWatchlist")
        | ("fleetMgr", "RemoveFromWatchlist")
        | ("fleetMgr", "RegisterForDamageUpdates") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let watched = method != "RemoveFromWatchlist";
            {
                let mut ex = FLEET_EXTRAS.lock().unwrap();
                let set = ex.watchlist.entry(char_id).or_default();
                if watched {
                    set.insert(target_id);
                } else {
                    set.remove(&target_id);
                }
            }
            let (target_sys, target_ship) = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&target_id)
                    .map(|c| (c.solar_system_id, c.active_ship_id))
                    .unwrap_or((session.solar_system_id.unwrap_or(30_000_142), 0))
            });
            let (mut s_ratio, mut a_ratio, mut h_ratio) = (1.0, 1.0, 1.0);
            if target_ship > 0 {
                let sys = ctx.systems.get_or_spawn(target_sys);
                if let Ok(Some(cs)) = sys.get_combat_state(target_ship).await {
                    s_ratio = (cs.shield_hp / cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0);
                    a_ratio = (cs.armor_hp / cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0);
                    h_ratio = (cs.structure_hp / cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0);
                }
            }
            if watched {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnFleetStateChange",
                    "charid",
                    vec![
                        PyRep::Integer(target_id),
                        PyRep::Real(s_ratio),
                        PyRep::Real(a_ratio),
                        PyRep::Real(h_ratio),
                    ],
                );
            }
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(target_id)),
                ("watched", PyRep::Bool(watched)),
                ("shieldRatio", PyRep::Real(s_ratio)),
                ("armorRatio", PyRep::Real(a_ratio)),
                ("hullRatio", PyRep::Real(h_ratio)),
            ])))
        }

        ("fleetProxy", "AddFleetFinderAdvert")
        | ("fleetProxy", "UpdateAdvertInfo")
        | ("fleetProxy", "UpdateAdvertAllowedEntities")
        | ("fleetProxy", "UpdateFleetAdvertWithNewLeader") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = match session.fleet_id {
                Some(fid) => fid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotInFleet".into(),
                        message: "Character is not in a fleet".into(),
                    }))
                }
            };
            if method == "UpdateAdvertAllowedEntities" {
                let mut entities = std::collections::HashSet::new();
                if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.first() {
                    for v in list {
                        if let Some(id) = v.as_i64() {
                            entities.insert(id);
                        }
                    }
                }
                if let Some(ad) = FLEET_EXTRAS.lock().unwrap().adverts.get_mut(&fleet_id) {
                    ad.allowed_entities = entities.clone();
                }
                return Some(Ok(PyRep::keyval([
                    ("fleetID", PyRep::Integer(fleet_id)),
                    ("allowedCount", PyRep::Integer(entities.len() as i64)),
                ])));
            }
            let fleet_name = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("Public Standing Fleet")
                .to_string();
            let description = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("Open to corp/alliance/public")
                .to_string();
            let invite_scope = args.get(2).and_then(|a| a.as_i64()).unwrap_or(3) as u8;
            let auto_join = args
                .get(3)
                .and_then(|a| match a {
                    PyRep::Bool(b) => Some(*b),
                    PyRep::Integer(i) => Some(*i != 0),
                    _ => None,
                })
                .unwrap_or(true);
            let min_standing = args.get(4).and_then(|a| a.as_f64()).unwrap_or(-10.0);
            let min_security = args.get(5).and_then(|a| a.as_f64()).unwrap_or(-10.0);
            let now_unix = ctx.now_unix();
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);

            ctx.db
                .try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                    if let Some(f) = snap.fleets.get_mut(&fleet_id) {
                        f.is_registered = true;
                    }
                    Ok(())
                })
                .unwrap();

            FLEET_EXTRAS.lock().unwrap().adverts.insert(
                fleet_id,
                FleetAdvertState {
                    fleet_id,
                    leader_id: char_id,
                    fleet_name: fleet_name.clone(),
                    description: description.clone(),
                    solar_system_id: sys_id,
                    invite_scope,
                    auto_join,
                    min_standing,
                    min_security,
                    allowed_entities: std::collections::HashSet::new(),
                    _created_unix: now_unix,
                },
            );

            ctx.notify(
                NotificationTarget::Fleet(fleet_id),
                "OnFleetFinderAdvertChanged",
                "fleetid",
                vec![PyRep::Integer(fleet_id), PyRep::String(fleet_name.clone())],
            );

            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fleet_id)),
                ("leaderID", PyRep::Integer(char_id)),
                ("fleetName", PyRep::String(fleet_name)),
                ("description", PyRep::String(description)),
                ("inviteScope", PyRep::Integer(invite_scope as i64)),
                ("autoJoin", PyRep::Bool(auto_join)),
                ("minStanding", PyRep::Real(min_standing)),
                ("minSecurity", PyRep::Real(min_security)),
            ])))
        }

        ("fleetProxy", "GetAvailableFleetAds") | ("fleetProxy", "GetMyFleetFinderAdvert") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ads: Vec<FleetAdvertState> = FLEET_EXTRAS
                .lock()
                .unwrap()
                .adverts
                .values()
                .cloned()
                .collect();
            let rows = ctx.db.with_snapshot(|snap| {
                let caller = snap.characters.get(&char_id);
                let caller_corp = caller.map(|c| c.corporation_id).unwrap_or(0);
                let caller_ally = caller.and_then(|c| c.alliance_id).unwrap_or(0);
                let caller_sec = caller.map(|c| c.security_status).unwrap_or(0.0);
                ads.into_iter()
                    .filter(|ad| {
                        if method == "GetMyFleetFinderAdvert" || ad.leader_id == char_id {
                            return true;
                        }
                        if caller_sec < ad.min_security {
                            return false;
                        }
                        if !ad.allowed_entities.is_empty()
                            && !ad.allowed_entities.contains(&char_id)
                            && !ad.allowed_entities.contains(&caller_corp)
                            && (caller_ally == 0 || !ad.allowed_entities.contains(&caller_ally))
                        {
                            return false;
                        }
                        let leader = snap.characters.get(&ad.leader_id);
                        let leader_corp = leader.map(|c| c.corporation_id).unwrap_or(0);
                        let leader_ally = leader.and_then(|c| c.alliance_id).unwrap_or(0);
                        if ad.min_standing > -10.0 {
                            let standing = super::fleet_social_corps::CORP_GOV
                                .lock()
                                .unwrap()
                                .get(&leader_corp)
                                .and_then(|g| {
                                    g.contacts
                                        .get(&char_id)
                                        .or_else(|| g.contacts.get(&caller_corp))
                                        .map(|(s, _)| *s)
                                })
                                .unwrap_or(0.0);
                            if standing < ad.min_standing {
                                return false;
                            }
                        }
                        match ad.invite_scope {
                            1 => caller_corp == leader_corp,
                            2 => caller_corp == leader_corp || (caller_ally != 0 && caller_ally == leader_ally),
                            _ => true,
                        }
                    })
                    .map(|ad| {
                        let num_members = snap
                            .fleets
                            .get(&ad.fleet_id)
                            .map(|f| f.members.len() as i64)
                            .unwrap_or(1);
                        vec![
                            PyRep::Integer(ad.fleet_id),
                            PyRep::Integer(ad.leader_id),
                            PyRep::String(ad.fleet_name),
                            PyRep::String(ad.description),
                            PyRep::Integer(ad.solar_system_id as i64),
                            PyRep::Integer(ad.invite_scope as i64),
                            PyRep::Bool(ad.auto_join),
                            PyRep::Integer(num_members),
                        ]
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::rowset(
                vec![
                    "fleetID",
                    "leaderID",
                    "fleetName",
                    "description",
                    "solarSystemID",
                    "inviteScope",
                    "autoJoin",
                    "numMembers",
                ],
                rows,
            )))
        }

        ("fleetProxy", "RemoveFleetFinderAdvert") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fleet_id = session.fleet_id.unwrap_or(0);
            FLEET_EXTRAS.lock().unwrap().adverts.remove(&fleet_id);
            ctx.db
                .try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                    if let Some(f) = snap.fleets.get_mut(&fleet_id) {
                        f.is_registered = false;
                    }
                    Ok(())
                })
                .unwrap();
            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fleet_id)),
                ("removed", PyRep::Bool(true)),
            ])))
        }

        ("fleetProxy", "ApplyToJoinFleet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(fleet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "ApplyToJoinFleet requires fleetID".into(),
                }));
            };
            let (advert, auto_join, max_size, auto_squad) = {
                let ex = FLEET_EXTRAS.lock().unwrap();
                let ad = ex.adverts.get(&fleet_id).cloned();
                let aj = ad.as_ref().map(|a| a.auto_join).unwrap_or(true);
                let ms = ex.max_sizes.get(&fleet_id).copied().unwrap_or(256);
                let asq = ex.auto_join_squads.get(&fleet_id).copied().flatten();
                (ad, aj, ms, asq)
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(ad) = &advert {
                    let caller = snap.characters.get(&char_id);
                    let caller_corp = caller.map(|c| c.corporation_id).unwrap_or(0);
                    let caller_ally = caller.and_then(|c| c.alliance_id).unwrap_or(0);
                    let caller_sec = caller.map(|c| c.security_status).unwrap_or(0.0);
                    if caller_sec < ad.min_security {
                        return Err(BridgeRpcError {
                            code: "FleetJoinDenied".into(),
                            message: "Security status below fleet minimum".into(),
                        });
                    }
                    if !ad.allowed_entities.is_empty()
                        && !ad.allowed_entities.contains(&char_id)
                        && !ad.allowed_entities.contains(&caller_corp)
                        && (caller_ally == 0 || !ad.allowed_entities.contains(&caller_ally))
                    {
                        return Err(BridgeRpcError {
                            code: "FleetJoinDenied".into(),
                            message: "Not in allowed entities for fleet advert".into(),
                        });
                    }
                    let leader = snap.characters.get(&ad.leader_id);
                    let leader_corp = leader.map(|c| c.corporation_id).unwrap_or(0);
                    let leader_ally = leader.and_then(|c| c.alliance_id).unwrap_or(0);
                    let scope_ok = match ad.invite_scope {
                        1 => caller_corp == leader_corp,
                        2 => caller_corp == leader_corp || (caller_ally != 0 && caller_ally == leader_ally),
                        _ => true,
                    };
                    if !scope_ok {
                        return Err(BridgeRpcError {
                            code: "FleetJoinDenied".into(),
                            message: "Outside fleet advert scope".into(),
                        });
                    }
                }
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                    });
                };
                if fleet.members.len() >= max_size as usize {
                    return Err(BridgeRpcError {
                        code: "FleetFull".into(),
                        message: "Fleet is full".into(),
                    });
                }
                let wid = fleet.wings.first().copied().unwrap_or(1);
                let sid = auto_squad
                    .or_else(|| {
                        fleet
                            .wing_details
                            .get(&wid)
                            .and_then(|w| w.squad_ids.first().copied())
                    })
                    .or_else(|| fleet.squads.first().copied())
                    .unwrap_or(1);
                let boss_id = fleet.boss_id;
                let motd = fleet.motd.clone();
                if auto_join {
                    if !fleet.members.contains(&char_id) {
                        fleet.members.push(char_id);
                    }
                    fleet.member_details.insert(
                        char_id,
                        FleetMemberRecord {
                            character_id: char_id,
                            role: FleetRole::SquadMember,
                            wing_id: Some(wid),
                            squad_id: Some(sid),
                            takes_fleet_warp: true,
                            job: 0,
                            role_name: "Squad Member".into(),
                            join_time_unix: ctx.now_unix(),
                        },
                    );
                    ensure_fleet_channel_member(&mut snap.chat_channels, fleet_id, boss_id, char_id);
                    Ok((true, wid, sid, boss_id, motd))
                } else {
                    fleet.invites.insert(char_id);
                    Ok((false, wid, sid, boss_id, motd))
                }
            });

            match res {
                Ok((joined, wid, sid, boss_id, motd)) => {
                    if joined {
                        session.fleet_id = Some(fleet_id);
                        ctx.notify(
                            NotificationTarget::Fleet(fleet_id),
                            "OnFleetJoin",
                            "fleetid",
                            vec![
                                PyRep::Integer(char_id),
                                PyRep::Integer(wid),
                                PyRep::Integer(sid),
                                PyRep::Integer(4),
                            ],
                        );
                        if !motd.is_empty() {
                            ctx.notify(
                                NotificationTarget::Character(char_id),
                                "OnFleetMotdChanged",
                                "charid",
                                vec![PyRep::String(motd)],
                            );
                        }
                    } else {
                        FLEET_EXTRAS.lock().unwrap().invite_details.insert(
                            (fleet_id, char_id),
                            (wid, sid, FleetRole::SquadMember),
                        );
                        ctx.notify(
                            NotificationTarget::Character(boss_id),
                            "OnFleetJoinRequest",
                            "charid",
                            vec![PyRep::Integer(char_id), PyRep::Integer(fleet_id)],
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("fleetID", PyRep::Integer(fleet_id)),
                        ("joined", PyRep::Bool(joined)),
                        ("wingID", PyRep::Integer(wid)),
                        ("squadID", PyRep::Integer(sid)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        _ => super::handle_fleet_ops_tail(ctx, session, service, method, args),
    }
}

fn handle_get_init_state(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    super::handle_get_init_state(ctx, session)
}
