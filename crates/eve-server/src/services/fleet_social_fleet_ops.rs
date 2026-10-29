//! Fleet Operations, Options, Broadcasts, Fleet Warp & Fleet Finder (`SO-2`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{FleetMemberRecord, FleetRole};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

use super::fleet_social_fleets::role_to_i64;
use super::fleet_social_notify as notify;
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
            // Client (FleetSvc.SendGlobal/System/BubbleBroadcast): `(name, broadcastScope, itemID, typeID)`;
            // the RPC method picks the location scope, `broadcastScope` the command-chain group.
            let broadcast_name = args.first().and_then(|a| a.as_str()).unwrap_or("Target");
            let scope = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let location = match method {
                "BroadcastToBubble" => 2,
                "BroadcastToSystem" => 1,
                _ => 0,
            };
            let item_id = args.get(2).and_then(|a| a.as_i64());
            let type_id = args.get(3).and_then(|a| a.as_i64());

            // OnFleetBroadcast carries the sender's solarSystemID: no session system, no broadcast.
            // parity: feat:fleet.broadcast-scope-and-filter
            let Some(sender_sys) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotInSolarSystem".into(),
                    message: "Fleet broadcasts require a session solar system".into(),
                }));
            };
            let (sender_record, members_raw) = match ctx.db.with_snapshot(|snap| {
                let fleet = snap.fleets.get(&fleet_id).ok_or_else(|| BridgeRpcError {
                    code: "FleetNotFound".into(),
                    message: format!("Fleet {fleet_id} not found"),
                })?;
                let sender = fleet.member_details.get(&char_id).ok_or_else(|| BridgeRpcError {
                    code: "NotInFleet".into(),
                    message: "Caller is not in fleet".into(),
                })?;

                let is_authorized = fleet.boss_id == char_id
                    || (sender.job & notify::FLEET_JOB_CREATOR) != 0
                    || matches!(
                        sender.role,
                        FleetRole::FleetCommander | FleetRole::WingCommander | FleetRole::SquadCommander
                    );
                let is_unrestricted = matches!(
                    broadcast_name,
                    "NeedArmor"
                        | "NeedShield"
                        | "NeedCapacitor"
                        | "NeedBackup"
                        | "InPosition"
                        | "HoldPosition"
                        | "NeedRepairs"
                        | "Location"
                        | "Travel"
                );
                if !is_authorized && !is_unrestricted {
                    return Err(BridgeRpcError {
                        code: "CannotBroadcastInFleet".into(),
                        message: "Only fleet commanders and authorized pilots can broadcast in fleet".into(),
                    });
                }

                let members: Vec<(FleetMemberRecord, u32, i64)> = fleet
                    .member_details
                    .values()
                    .filter_map(|m| {
                        let ch = snap.characters.get(&m.character_id)?;
                        Some((m.clone(), ch.solar_system_id, ch.active_ship_id))
                    })
                    .collect();
                Ok((sender.clone(), members))
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let now_unix = ctx.now_unix();
            let muted_set = {
                let mut ex = ctx.fleet_extras.lock().unwrap();
                if let Some((last_ts, last_name, last_scope, last_loc, last_item)) =
                    ex.last_broadcast.get(&char_id)
                {
                    if now_unix > 0
                        && now_unix < last_ts.saturating_add(2)
                        && last_name == broadcast_name
                        && *last_scope == scope
                        && *last_loc == location
                        && *last_item == item_id
                    {
                        return Some(Err(BridgeRpcError {
                            code: "BroadcastSpamCooldown".into(),
                            message: "Broadcast spam cooldown is active (2s)".into(),
                        }));
                    }
                }
                ex.last_broadcast.insert(
                    char_id,
                    (now_unix, broadcast_name.to_string(), scope, location, item_id),
                );
                ex.broadcast_history.entry(fleet_id).or_default().push((
                    now_unix,
                    char_id,
                    broadcast_name.to_string(),
                    scope,
                    item_id,
                    type_id,
                ));
                ex.broadcast_filters.clone()
            };

            let members_info: Vec<(i64, u32, i64)> = members_raw
                .into_iter()
                .filter(|(m, _, _)| notify::broadcast_group_allows(scope, &sender_record, m))
                .filter(|(m, _, _)| {
                    !muted_set
                        .get(&m.character_id)
                        .is_some_and(|s| s.contains(broadcast_name))
                })
                .map(|(m, sys, ship)| (m.character_id, sys, ship))
                .collect();
            // FleetSvc.OnFleetBroadcast(name, scope, charID, solarSystemID, itemID, typeID)
            let opt = |v: Option<i64>| v.map(PyRep::Integer).unwrap_or(PyRep::None);
            let payload = vec![
                PyRep::String(broadcast_name.to_string()),
                PyRep::Integer(scope),
                PyRep::Integer(char_id),
                PyRep::Integer(sender_sys as i64),
                opt(item_id),
                opt(type_id),
            ];
            let mut recipient_ids = Vec::new();
            if location == 0 {
                for (m_id, _, _) in &members_info {
                    recipient_ids.push(*m_id);
                    ctx.notify(NotificationTarget::Character(*m_id), "OnFleetBroadcast", "charid", payload.clone());
                }
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
                    if location == 2 && m_id != char_id {
                        // Without a sender ball there is no bubble to share: only the sender sees it.
                        let Some(s_pos) = sender_pos else { continue };
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
                ("targetID", opt(item_id)),
                ("scope", PyRep::Integer(scope)),
                ("itemID", opt(item_id)),
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
                if !fleet.member_details.contains_key(&char_id) {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not in fleet".into() });
                }
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
            if let Err(e) = crate::services::tactical_modules::check_ship_navigation_lock(ctx, caller_ship_id, "warp") {
                return Some(Err(e));
            }
            if let Err(e) = crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, caller_ship_id, "warp") {
                return Some(Err(e));
            }
            if caller_ball.max_velocity <= 0.0 || sys.is_ball_scrambled(caller_ship_id).await.unwrap_or(false) {
                return Some(Err(BridgeRpcError {
                    code: "WarpScrambled".into(),
                    message: "Cannot warp to fleet member while warp scrambled or immobilized".into(),
                }));
            }
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
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };

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

            // parity: feat:fleet.takes-fleet-warp-toggle, feat:fleet.stored-formations-and-warp-shapes
            let (eligible_ship_ids, skipped_opt_out_members) = match ctx.db.with_snapshot(|snap| {
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
                let mut skipped = Vec::new();
                for m in fleet.member_details.values() {
                    let in_scope = match caller_member.role {
                        FleetRole::FleetCommander => true,
                        FleetRole::WingCommander => m.wing_id == caller_member.wing_id,
                        FleetRole::SquadCommander => m.squad_id == caller_member.squad_id,
                        FleetRole::SquadMember => fleet.boss_id == char_id,
                    };
                    if !in_scope {
                        continue;
                    }
                    if !m.takes_fleet_warp && m.character_id != char_id {
                        skipped.push(m.character_id);
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
                Ok((ships, skipped))
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            super::fleet_social_finder_tactical::ensure_fleet_formations_loaded(ctx);
            let stored_form = ctx
                .fleet_extras
                .lock()
                .unwrap()
                .stored_formations
                .get(&char_id)
                .copied();
            let formation_id = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("formationID").and_then(|a| a.as_i64()))
                .or_else(|| stored_form.map(|(fid, _, _)| fid))
                .unwrap_or(0);
            let formation_spacing = args
                .get(3)
                .and_then(|a| a.as_f64())
                .or_else(|| kwargs.get("formationSpacing").and_then(|a| a.as_f64()))
                .or_else(|| stored_form.map(|(_, sp, _)| sp))
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
            let now_unix = ctx.now_unix();
            let dest_pos = if let Some(pos) = explicit_dest {
                pos
            } else if let Ok(Some((_, b))) = sys.get_ball_snapshot(dest_id).await {
                b.position
            } else if let Some(bm_res) = ctx.db.with_snapshot(|snap| {
                let bm = snap.bookmarks.get(&dest_id)?;
                if sys_id == 30_100_000 || bm.solar_system_id == 30_100_000 {
                    return Some(Err(BridgeRpcError::user_error("DunBlacklistCannotWarp")));
                }
                if super::is_bookmark_expired_in_snap(snap, dest_id, now_unix)
                    || bm.solar_system_id != sys_id
                {
                    return Some(Err(BridgeRpcError {
                        code: "BookmarkNotFound".into(),
                        message: format!("Bookmark {dest_id} not found or inaccessible"),
                    }));
                }
                let can_use = bm.owner_id == char_id
                    || bm.folder_id.is_some_and(|fid| {
                        snap.bookmark_folders.get(&fid).is_some_and(|f| {
                            crate::services::fleet_social::fleet_social_access_group_bookmarks::folder_access_level(
                                snap, session, char_id, f,
                            ) >= crate::services::fleet_social::fleet_social_access_group_bookmarks::AccessLevel::Use
                        })
                    });
                if !can_use {
                    return Some(Err(BridgeRpcError {
                        code: "BookmarkNotFound".into(),
                        message: format!("Bookmark {dest_id} not found or inaccessible"),
                    }));
                }
                Some(Ok(DVec3::new(bm.x, bm.y, bm.z)))
            }) {
                match bm_res {
                    Ok(p) => p,
                    Err(e) => return Some(Err(e)),
                }
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
                    if crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "warp").is_err()
                        || crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, ship_id, "warp").is_err()
                    {
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

            for &opt_out_cid in &skipped_opt_out_members {
                ctx.notify(
                    NotificationTarget::Character(char_id),
                    "OnFleetMoveFailed",
                    "charid",
                    vec![PyRep::Integer(opt_out_cid)],
                );
            }

            Some(Ok(PyRep::keyval([
                ("status", PyRep::String("FleetWarpInitiated".into())),
                ("fleetID", PyRep::Integer(fleet_id)),
                ("destinationID", PyRep::Integer(dest_id)),
                ("formationID", PyRep::Integer(formation_id)),
                ("formationSpacing", PyRep::Real(formation_spacing)),
                ("warpCount", PyRep::Integer(warped_members.len() as i64)),
                ("warpedMembers", PyRep::Integer(warped_members.len() as i64)),
                ("warpedMemberIDs", PyRep::List(warped_members)),
                (
                    "skippedOptOutMemberIDs",
                    PyRep::List(skipped_opt_out_members.into_iter().map(PyRep::Integer).collect()),
                ),
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
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found") });
                };
                let Some(caller_m) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not in fleet".into() });
                };
                if fleet.boss_id != char_id && role_to_i64(caller_m.role) > 1 {
                    return Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only Fleet Boss or Fleet Commander can set MOTD".into() });
                }
                fleet.motd = motd.clone();
                Ok(())
            });
            match res {
                Ok(()) => {
                    notify::emit_motd(ctx, NotificationTarget::Fleet(fleet_id), &motd, true);
                    Some(Ok(PyRep::keyval([("fleetID", PyRep::Integer(fleet_id)), ("motd", PyRep::String(motd))])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "GetMotd") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found") });
                };
                if !fleet.member_details.contains_key(&char_id) {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not a member of this fleet".into() });
                }
                Ok(fleet.motd.clone())
            });
            match res {
                Ok(motd) => Some(Ok(PyRep::String(motd))),
                Err(e) => {
                    session.fleet_id = None;
                    Some(Err(e))
                }
            }
        }

        ("fleetObjectHandler", "SetOptions") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let kv_bool = |key: &str| -> Option<bool> {
                if let Some(PyRep::KeyVal(entries)) = args.first() {
                    entries.iter().find(|(k, _)| k.as_str() == key).and_then(|(_, v)| match v {
                        PyRep::Bool(b) => Some(*b),
                        PyRep::Integer(i) => Some(*i != 0),
                        _ => None,
                    })
                } else {
                    None
                }
            };
            let is_free_move = match args.first() {
                Some(PyRep::Bool(b)) => *b,
                Some(PyRep::Integer(i)) => *i != 0,
                _ => kv_bool("isFreeMove").unwrap_or(false),
            };
            let is_registered = match args.get(1) {
                Some(PyRep::Bool(b)) => Some(*b),
                Some(PyRep::Integer(i)) => Some(*i != 0),
                _ => kv_bool("isRegistered"),
            };
            let is_voice_enabled = match args.get(2) {
                Some(PyRep::Bool(b)) => Some(*b),
                Some(PyRep::Integer(i)) => Some(*i != 0),
                _ => kv_bool("isVoiceEnabled"),
            };
            let is_loot_logging = match args.get(3) {
                Some(PyRep::Bool(b)) => Some(*b),
                Some(PyRep::Integer(i)) => Some(*i != 0),
                _ => kv_bool("isLootLogging"),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get_mut(&fleet_id) else {
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found") });
                };
                if fleet.boss_id != char_id {
                    return Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only Fleet Boss can change fleet options".into() });
                }
                let old = (fleet.free_move, fleet.is_registered);
                fleet.free_move = is_free_move;
                if let Some(reg) = is_registered {
                    fleet.is_registered = reg;
                }
                Ok((old, fleet.is_registered))
            });
            match res {
                Ok(((old_free, old_reg), reg)) => {
                    let auto = {
                        let mut ex = ctx.fleet_extras.lock().unwrap();
                        if let Some(ve) = is_voice_enabled {
                            ex.voice_enabled.insert(fleet_id, ve);
                        }
                        if let Some(ll) = is_loot_logging {
                            ex.loot_logging.insert(fleet_id, ll);
                        }
                        ex.auto_join_squads.get(&fleet_id).copied().flatten()
                    };
                    notify::sync_fleet_options_to_db(ctx);
                    let old = notify::options_keyval(old_free, old_reg, auto);
                    notify::emit_options_changed(ctx, fleet_id, old, notify::options_keyval(is_free_move, reg, auto));
                    crate::services::notify_emitters_batch3::emit_fleet_respawn_points_if_any(ctx, fleet_id);
                    Some(Ok(PyRep::keyval([("isFreeMove", PyRep::Bool(is_free_move)), ("isRegistered", PyRep::Bool(reg))])))
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
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found") });
                };
                if fleet.boss_id != char_id {
                    return Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only Fleet Boss can set auto-join squad".into() });
                }
                Ok((fleet.free_move, fleet.is_registered))
            });
            match res {
                Ok((free, reg)) => {
                    let old_auto = ctx.fleet_extras.lock().unwrap().auto_join_squads.insert(fleet_id, squad_id).flatten();
                    let old = notify::options_keyval(free, reg, old_auto);
                    notify::emit_options_changed(ctx, fleet_id, old, notify::options_keyval(free, reg, squad_id));
                    Some(Ok(PyRep::keyval([("autoJoinSquadID", squad_id.map(PyRep::Integer).unwrap_or(PyRep::None))])))
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
            let max_size = args.first().and_then(|a| a.as_i64()).unwrap_or(256).clamp(1, 256) as u16;
            let res = ctx.db.with_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found") });
                };
                if fleet.boss_id != char_id {
                    return Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only Fleet Boss can change fleet max size".into() });
                }
                Ok(())
            });
            match res {
                Ok(()) => {
                    ctx.fleet_extras.lock().unwrap().max_sizes.insert(fleet_id, max_size);
                    Some(Ok(PyRep::keyval([("maxSize", PyRep::Integer(max_size as i64))])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("fleetObjectHandler", "GetFleetMaxSize") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            if !ctx.db.with_snapshot(|snap| snap.fleets.get(&fleet_id).is_some_and(|f| f.member_details.contains_key(&char_id))) {
                session.fleet_id = None;
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not a member of this fleet".into() }));
            }
            let max_size = ctx.fleet_extras.lock().unwrap().max_sizes.get(&fleet_id).copied().unwrap_or(256);
            Some(Ok(PyRep::Integer(max_size as i64)))
        }

        ("fleetObjectHandler", "SetTakesFleetWarp")
        | ("fleetMgr", "SetTakesFleetWarp") => {
            handle_fleet_member_preference(ctx, session, args, 0)
        }

        ("fleetObjectHandler", "SetAcceptsRegroupValue")
        | ("fleetMgr", "SetAcceptsRegroupValue") => {
            handle_fleet_member_preference(ctx, session, args, 1)
        }

        ("fleetObjectHandler", "SetAcceptsConduitJumpsValue")
        | ("fleetMgr", "SetAcceptsConduitJumpsValue") => {
            handle_fleet_member_preference(ctx, session, args, 2)
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
        | ("fleetObjectHandler", "GetInitState") => {
            super::handle_get_init_state(ctx, session)
        }
        ("fleetMgr", "GetWings")
        | ("fleetObjectHandler", "GetWings") => {
            super::handle_get_wings(ctx, session)
        }
        ("fleetMgr", "GetFleet")
        | ("fleetObjectHandler", "GetFleet") => {
            super::handle_get_init_state(ctx, session)
        }

        ("fleetMgr", "AddToWatchlist")
        | ("fleetMgr", "RemoveFromWatchlist")
        | ("fleetMgr", "RegisterForDamageUpdates")
        | ("fleetObjectHandler", "AddToWatchlist")
        | ("fleetObjectHandler", "RemoveFromWatchlist")
        | ("fleetObjectHandler", "RegisterForDamageUpdates") => {
            super::handle_fleet_watchlist_ops(ctx, session, method, args).await
        }

        // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
        ("fleetObjectHandler", "SetBroadcastFilter") | ("fleetMgr", "SetBroadcastFilter") => {
            // parity: feat:fleet.broadcast-scope-and-filter
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let b_name = args.first().and_then(|a| a.as_str()).unwrap_or("Target");
            let enabled = args.get(1).and_then(|a| match a { PyRep::Bool(b) => Some(*b), PyRep::Integer(i) => Some(*i != 0), _ => None }).unwrap_or(true);
            let muted_list = {
                let mut ex = ctx.fleet_extras.lock().unwrap();
                let set = ex.broadcast_filters.entry(char_id).or_default();
                if enabled { set.remove(b_name); } else { set.insert(b_name.to_string()); }
                let mut v: Vec<String> = set.iter().cloned().collect();
                v.sort();
                v
            };
            Some(Ok(PyRep::keyval([
                ("broadcastType", PyRep::String(b_name.to_string())),
                ("enabled", PyRep::Bool(enabled)),
                ("mutedBroadcasts", PyRep::List(muted_list.into_iter().map(PyRep::String).collect())),
            ])))
        }

        // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
        ("fleetObjectHandler", "GetBroadcastFilter") | ("fleetMgr", "GetBroadcastFilter") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let muted_list = {
                let ex = ctx.fleet_extras.lock().unwrap();
                let mut v: Vec<String> = ex.broadcast_filters.get(&char_id).map(|s| s.iter().cloned().collect()).unwrap_or_default();
                v.sort();
                v
            };
            Some(Ok(PyRep::keyval([("mutedBroadcasts", PyRep::List(muted_list.into_iter().map(PyRep::String).collect()))])))
        }

        ("fleetObjectHandler", "GetBroadcastHistory") | ("fleetMgr", "GetBroadcastHistory") => {
            let (_, _char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let history = ctx.fleet_extras.lock().unwrap().broadcast_history.get(&fleet_id).cloned().unwrap_or_default();
            let entries: Vec<PyRep> = history.into_iter().map(|(ts, sender, name, scope, item_id, type_id)| {
                PyRep::keyval([
                    ("timestamp", PyRep::Integer(ts as i64)),
                    ("senderID", PyRep::Integer(sender)),
                    ("broadcast", PyRep::String(name)),
                    ("scope", PyRep::Integer(scope)),
                    ("itemID", item_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ("typeID", type_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ])
            }).collect();
            Some(Ok(PyRep::List(entries)))
        }

        ("fleetMgr", "Regroup") | ("fleetObjectHandler", "Regroup") => {
            // parity: feat:fleet.member-movement-optins
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into() }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let caller_ship_id = session.ship_id.unwrap_or(0);
            let res = ctx.db.with_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found") });
                };
                let Some(caller_m) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller not in fleet".into() });
                };
                if fleet.boss_id != char_id && role_to_i64(caller_m.role) > 3 {
                    return Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only Fleet Commanders can issue Regroup".into() });
                }
                let mut candidates = Vec::new();
                let mut skipped = Vec::new();
                for m in fleet.member_details.values() {
                    if m.character_id == char_id { continue; }
                    let in_scope = match caller_m.role {
                        FleetRole::FleetCommander => true,
                        FleetRole::WingCommander => m.wing_id == caller_m.wing_id,
                        FleetRole::SquadCommander => m.squad_id == caller_m.squad_id,
                        FleetRole::SquadMember => fleet.boss_id == char_id,
                    };
                    if !in_scope { continue; }
                    if !m.accepts_regroup { skipped.push(m.character_id); continue; }
                    if let Some(ch) = snap.characters.get(&m.character_id) {
                        if ch.solar_system_id == sys_id && ch.station_id.is_none() && ch.structure_id.is_none() {
                            candidates.push((m.character_id, ch.active_ship_id));
                        }
                    }
                }
                Ok((candidates, skipped))
            });
            let (candidates, skipped) = match res { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let cmd_pos = sys_handle.get_ball_snapshot(caller_ship_id).await.ok().flatten().map(|(_, b)| b.position);
            let mut regrouped = Vec::new();
            for (m_cid, m_ship) in candidates {
                let sub_pos = sys_handle.get_ball_snapshot(m_ship).await.ok().flatten().map(|(_, b)| b.position);
                let on_grid = match (cmd_pos, sub_pos) { (Some(cp), Some(sp)) => cp.distance(sp) <= 500_000.0, _ => true };
                if on_grid {
                    let _ = sys_handle.set_movement_mode(m_ship, MovementMode::Follow { target_id: caller_ship_id, range: 1_000.0 }).await;
                    regrouped.push(PyRep::Integer(m_cid));
                }
            }
            Some(Ok(PyRep::keyval([
                ("fleetID", PyRep::Integer(fleet_id)),
                ("regroupedMemberIDs", PyRep::List(regrouped)),
                ("skippedOptOutMemberIDs", PyRep::List(skipped.into_iter().map(PyRep::Integer).collect())),
            ])))
        }

        _ => {
            super::fleet_social_finder_tactical::handle_finder_and_tactical(
                ctx, session, service, method, args, kwargs,
            )
            .await
        }
    }
}

fn handle_fleet_member_preference(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    pref_type: u8,
) -> Option<Result<PyRep, BridgeRpcError>> {
    // parity: feat:fleet.member-movement-optins, feat:fleet.takes-fleet-warp-toggle
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
        let only = fleet.members.len() == 1;
        let Some(member) = fleet.member_details.get_mut(&char_id) else {
            return Err(BridgeRpcError {
                code: "MemberNotFound".into(),
                message: "Character not in fleet".into(),
            });
        };
        let before = member.clone();
        match pref_type {
            0 => member.takes_fleet_warp = flag,
            1 => member.accepts_regroup = flag,
            _ => member.accepts_conduit_jumps = flag,
        }
        let after = member.clone();
        Ok((before, after, only))
    });
    match res {
        Ok((before, after, only)) => {
            notify::emit_member_changed(ctx, fleet_id, &before, &after, only);
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("takesFleetWarp", PyRep::Bool(after.takes_fleet_warp)),
                ("acceptsRegroup", PyRep::Bool(after.accepts_regroup)),
                ("acceptsConduitJumps", PyRep::Bool(after.accepts_conduit_jumps)),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}
