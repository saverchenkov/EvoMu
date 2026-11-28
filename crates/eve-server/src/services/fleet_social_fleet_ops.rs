//! Fleet Operations, Options, Broadcasts, Fleet Warp & Fleet Finder (`SO-2`).

use crate::router::notify_registry::{OnFleetBroadcast, OnFleetMoveFailed};
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

async fn handle_send_broadcast(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
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
                client_error: None,
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
            client_error: None,
        }));
    };
    let (sender_record, members_raw) = match ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fleet_id).ok_or_else(|| BridgeRpcError {
            code: "FleetNotFound".into(),
            message: format!("Fleet {fleet_id} not found"),
            client_error: None,
        })?;
        let sender = fleet.member_details.get(&char_id).ok_or_else(|| BridgeRpcError {
            code: "NotInFleet".into(),
            message: "Caller is not in fleet".into(),
            client_error: None,
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
                client_error: None,
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
                    client_error: None,
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
    let mut recipient_ids = Vec::new();
    if location == 0 {
        for (m_id, _, _) in &members_info {
            recipient_ids.push(*m_id);
            ctx.emit_typed(
                NotificationTarget::Character(*m_id),
                OnFleetBroadcast {
                    name: broadcast_name.to_string(),
                    scope,
                    char_id,
                    solar_system_id: sender_sys as i64,
                    item_id,
                    type_id,
                },
            );
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
            ctx.emit_typed(
                NotificationTarget::Character(m_id),
                OnFleetBroadcast {
                    name: broadcast_name.to_string(),
                    scope,
                    char_id,
                    solar_system_id: sender_sys as i64,
                    item_id,
                    type_id,
                },
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

pub(super) async fn handle_fleet_ops(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("fleetObjectHandler", "SendBroadcast") => {
            handle_send_broadcast(ctx, session, "SendBroadcast", args).await
        }
        ("fleetObjectHandler", "BroadcastToBubble") => {
            handle_send_broadcast(ctx, session, "BroadcastToBubble", args).await
        }
        ("fleetObjectHandler", "BroadcastToSystem") => {
            handle_send_broadcast(ctx, session, "BroadcastToSystem", args).await
        }
        ("fleetMgr", "SendBroadcast") => {
            handle_send_broadcast(ctx, session, "SendBroadcast", args).await
        }
        ("fleetMgr", "BroadcastToBubble") => {
            handle_send_broadcast(ctx, session, "BroadcastToBubble", args).await
        }
        ("fleetMgr", "BroadcastToSystem") => {
            handle_send_broadcast(ctx, session, "BroadcastToSystem", args).await
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
                        client_error: None,
                    }))
                }
            };
            let Some(target_char_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "WarpToMember requires target characterID".into(),
                    client_error: None,
                }));
            };
            let rpc = crate::router::RpcArgs::new(args, kwargs);
            let min_range = match rpc.opt_finite_f64(1, "minRange") {
                Ok(v) => v.unwrap_or(0.0).clamp(0.0, 100_000.0),
                Err(e) => return Some(Err(e)),
            };

            let (caller_ship_id, caller_docked, target_ship_id, target_docked, sys_id) =
                match ctx.db.with_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError {
                        code: "FleetNotFound".into(),
                        message: format!("Fleet {fleet_id} not found"),
                        client_error: None,
                    });
                };
                if !fleet.member_details.contains_key(&char_id) {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not in fleet".into(), client_error: None });
                }
                if !fleet.member_details.contains_key(&target_char_id) {
                    return Err(BridgeRpcError {
                        code: "MemberNotFound".into(),
                        message: format!("Character {target_char_id} is not in fleet {fleet_id}"),
                        client_error: None,
                    });
                }
                let caller = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError {
                    code: "CharacterNotFound".into(),
                    message: "Caller character not found".into(),
                    client_error: None,
                })?;
                let target = snap
                    .characters
                    .get(&target_char_id)
                    .ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: "Target character not found".into(),
                        client_error: None,
                    })?;
                if caller.solar_system_id != target.solar_system_id {
                    return Err(BridgeRpcError {
                        code: "DifferentSolarSystem".into(),
                        message: "Cannot warp to a fleet member in another solar system".into(),
                        client_error: None,
                    });
                }
                let caller_docked = session.station_id.is_some()
                    || session.structure_id.is_some()
                    || caller.station_id.is_some()
                    || caller.structure_id.is_some();
                let target_docked = target.station_id.is_some() || target.structure_id.is_some();
                Ok((
                    session.ship_id.unwrap_or(caller.active_ship_id),
                    caller_docked,
                    target.active_ship_id,
                    target_docked,
                    caller.solar_system_id,
                ))
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let sys = ctx.systems.get_or_spawn(sys_id);
            if let Err(e) = crate::services::nav_warp_interdiction::validate_pre_warp_common(
                ctx,
                char_id,
                caller_ship_id,
                sys_id,
                &sys,
            )
            .await
            {
                return Some(Err(e));
            }
            let target_ball = if target_docked {
                None
            } else {
                sys.get_ball_snapshot(target_ship_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|(_, b)| b)
            };
            let Some(target_ball) = target_ball else {
                return Some(Err(BridgeRpcError {
                    code: "TargetNotInSpace".into(),
                    message: "Target fleet member is not in space".into(),
                    client_error: None,
                }));
            };
            if caller_docked {
                return Some(Err(BridgeRpcError::ship_not_in_space(
                    caller_ship_id,
                    Some(char_id),
                    session.solar_system_id,
                    "fleet_warp",
                    "Caller ship is docked and cannot warp",
                )));
            }
            let validated = match ctx
                .validate_ship_movement(
                    char_id,
                    caller_ship_id,
                    crate::services::space_nav_movement_guard::MovementIntent::Warp { sys_id },
                )
                .await
            {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(caller_ball) = validated.ball else {
                return Some(Err(BridgeRpcError::ship_not_in_space(
                    caller_ship_id,
                    Some(char_id),
                    session.solar_system_id,
                    "fleet_warp",
                    "Caller ship is not in space",
                )));
            };
            if caller_ball.max_velocity <= 0.0 {
                return Some(Err(BridgeRpcError {
                    code: "WarpScrambled".into(),
                    message: "Cannot warp to fleet member while warp scrambled or immobilized".into(),
                    client_error: None,
                }));
            }
            let dist = caller_ball.position.distance(target_ball.position);
            let dir = (target_ball.position - caller_ball.position).normalize_or_zero();
            let mut dest = target_ball.position - dir * min_range;
            if dist < WARP_MIN_DISTANCE_M || caller_ball.position.distance(dest) < WARP_MIN_DISTANCE_M
            {
                return Some(Err(BridgeRpcError {
                    code: "WarpDistanceTooClose".into(),
                    message: format!("Fleet member is within minimum warp distance ({dist:.0} m)"),
                    client_error: None,
                }));
            }
            if let Err(e) = crate::services::pve_dungeon_runtime::check_deadspace_warp_allowed(
                ctx,
                sys_id,
                caller_ball.position.to_array(),
                target_ball.position.to_array(),
                dest.to_array(),
            ) {
                return Some(Err(e));
            }
            if let Err(e) =
                crate::services::pve_ess_reserve_banks::check_ess_warp_destination_allowed(
                    ctx,
                    sys_id,
                    target_ball.position,
                    dest,
                )
                .await
            {
                return Some(Err(e));
            }
            dest = match crate::services::nav_warp_interdiction::check_warp_interdiction_and_adjust_dest(
                &sys,
                caller_ship_id,
                caller_ball.position,
                dest,
            ) {
                Ok(adjusted) => adjusted,
                Err(e) => return Some(Err(e)),
            };
            let warp_speed = crate::services::space_destiny::compute_pilot_warp_speed_au_s(
                ctx,
                char_id,
                sys_id,
                caller_ball.type_id,
            );
            let _ = commit_fleet_member_warp(
                ctx,
                &sys,
                char_id,
                caller_ship_id,
                sys_id,
                caller_ball.position,
                dest,
                warp_speed,
                validated.combat_state.as_ref(),
                validated.is_covert_cloaked,
                None,
            )
            .await;

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

        ("fleetMgr", "FleetWarp") | ("fleetObjectHandler", "FleetWarp") => {
            handle_fleet_warp(ctx, session, args, kwargs).await
        }
        ("fleetMgr", "WarpFleet") | ("fleetObjectHandler", "WarpFleet") => {
            handle_fleet_warp(ctx, session, args, kwargs).await
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
                        client_error: None,
                    }))
                }
            };
            let motd = args.first().and_then(|a| a.as_str()).unwrap_or("").to_string();
            match crate::domain::fleets::update_fleet_settings(
                ctx,
                crate::domain::fleets::UpdateFleetSettingsInput {
                    fleet_id,
                    caller_char_id: Some(char_id),
                    require_boss_only: false,
                    motd: Some(motd.clone()),
                    is_free_move: None,
                    is_registered: None,
                    is_voice_enabled: None,
                    is_loot_logging: None,
                },
            ) {
                Ok(_) => {
                    notify::emit_motd(ctx, NotificationTarget::Fleet(fleet_id), &motd, true);
                    Some(Ok(PyRep::keyval([("fleetID", PyRep::Integer(fleet_id)), ("motd", PyRep::String(motd))])))
                }
                Err(e) => Some(Err(crate::services::domain_err_to_rpc(e))),
            }
        }

        ("fleetObjectHandler", "GetMotd") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into(), client_error: None }));
            };
            match crate::domain::fleets::get_fleet_settings(ctx, fleet_id, Some(char_id)) {
                Ok(view) => Some(Ok(PyRep::String(view.motd))),
                Err(e) => {
                    session.fleet_id = None;
                    Some(Err(crate::services::domain_err_to_rpc(e)))
                }
            }
        }

        ("fleetObjectHandler", "SetOptions") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into(), client_error: None }));
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
            match crate::domain::fleets::update_fleet_settings(
                ctx,
                crate::domain::fleets::UpdateFleetSettingsInput {
                    fleet_id,
                    caller_char_id: Some(char_id),
                    require_boss_only: true,
                    motd: None,
                    is_free_move: Some(is_free_move),
                    is_registered,
                    is_voice_enabled,
                    is_loot_logging,
                },
            ) {
                Ok(out) => {
                    notify::sync_fleet_options_to_db(ctx);
                    let old = notify::options_keyval(out.old_free_move, out.old_registered, out.auto_join_squad_id);
                    notify::emit_options_changed(ctx, fleet_id, old, notify::options_keyval(out.new_free_move, out.new_registered, out.auto_join_squad_id));
                    crate::services::notify_emitters_character_services::emit_fleet_respawn_points_if_any(ctx, fleet_id);
                    Some(Ok(PyRep::keyval([("isFreeMove", PyRep::Bool(out.new_free_move)), ("isRegistered", PyRep::Bool(out.new_registered))])))
                }
                Err(e) => Some(Err(crate::services::domain_err_to_rpc(e))),
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
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found"), client_error: None });
                };
                if fleet.boss_id != char_id {
                    return Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only Fleet Boss can set auto-join squad".into(), client_error: None });
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
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found"), client_error: None });
                };
                if fleet.boss_id != char_id {
                    return Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only Fleet Boss can change fleet max size".into(), client_error: None });
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
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into(), client_error: None }));
            };
            if !ctx.db.with_snapshot(|snap| snap.fleets.get(&fleet_id).is_some_and(|f| f.member_details.contains_key(&char_id))) {
                session.fleet_id = None;
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller is not a member of this fleet".into(), client_error: None }));
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
                        client_error: None,
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
                    client_error: None,
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
                            .unwrap_or_else(|| {
                                let s_id = if session.character_id == Some(m.character_id) {
                                    session.solar_system_id.unwrap_or(0)
                                } else {
                                    0
                                };
                                let ship = if session.character_id == Some(m.character_id) {
                                    session.ship_id.unwrap_or(0)
                                } else {
                                    0
                                };
                                (s_id, ship, 606)
                            });
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

        ("fleetMgr", "AddToWatchlist") => {
            super::handle_fleet_watchlist_ops(ctx, session, "AddToWatchlist", args).await
        }
        ("fleetMgr", "RemoveFromWatchlist") => {
            super::handle_fleet_watchlist_ops(ctx, session, "RemoveFromWatchlist", args).await
        }
        ("fleetMgr", "RegisterForDamageUpdates") => {
            super::handle_fleet_watchlist_ops(ctx, session, "RegisterForDamageUpdates", args).await
        }
        ("fleetObjectHandler", "AddToWatchlist") => {
            super::handle_fleet_watchlist_ops(ctx, session, "AddToWatchlist", args).await
        }
        ("fleetObjectHandler", "RemoveFromWatchlist") => {
            super::handle_fleet_watchlist_ops(ctx, session, "RemoveFromWatchlist", args).await
        }
        ("fleetObjectHandler", "RegisterForDamageUpdates") => {
            super::handle_fleet_watchlist_ops(ctx, session, "RegisterForDamageUpdates", args).await
        }

        // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
        ("fleetObjectHandler", "SetBroadcastFilter") | ("fleetMgr", "SetBroadcastFilter") => {
            Some(handle_set_broadcast_filter(ctx, session, args))
        }

        // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
        ("fleetObjectHandler", "GetBroadcastFilter") | ("fleetMgr", "GetBroadcastFilter") => {
            Some(handle_get_broadcast_filter(ctx, session))
        }

        ("fleetObjectHandler", "GetBroadcastHistory") | ("fleetMgr", "GetBroadcastHistory") => {
            Some(handle_get_broadcast_history(ctx, session))
        }

        ("fleetMgr", "Regroup") | ("fleetObjectHandler", "Regroup") => {
            // parity: feat:fleet.member-movement-optins
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(fleet_id) = session.fleet_id else {
                return Some(Err(BridgeRpcError { code: "NotInFleet".into(), message: "Character is not in a fleet".into(), client_error: None }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                    client_error: None,
                }));
            };
            let caller_ship_id = session.ship_id.unwrap_or(0);
            let res = ctx.db.with_snapshot(|snap| {
                let Some(fleet) = snap.fleets.get(&fleet_id) else {
                    return Err(BridgeRpcError { code: "FleetNotFound".into(), message: format!("Fleet {fleet_id} not found"), client_error: None });
                };
                let Some(caller_m) = fleet.member_details.get(&char_id) else {
                    return Err(BridgeRpcError { code: "NotInFleet".into(), message: "Caller not in fleet".into(), client_error: None });
                };
                if fleet.boss_id != char_id && role_to_i64(caller_m.role) > 3 {
                    return Err(BridgeRpcError { code: "FleetPermissionDenied".into(), message: "Only Fleet Commanders can issue Regroup".into(), client_error: None });
                }
                crate::services::structure_control_ops::reject_if_controlling_structure(ctx, caller_ship_id, "issue fleet regroup")?;
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
                        if ch.solar_system_id == sys_id
                            && ch.station_id.is_none()
                            && ch.structure_id.is_none()
                            && !snap.structures.contains_key(&ch.active_ship_id)
                        {
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
                client_error: None,
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
                client_error: None,
            });
        };
        let only = fleet.members.len() == 1;
        let Some(member) = fleet.member_details.get_mut(&char_id) else {
            return Err(BridgeRpcError {
                code: "MemberNotFound".into(),
                message: "Character not in fleet".into(),
                client_error: None,
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
            let opt_outs = notify::member_opt_outs(&after);
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("takesFleetWarp", PyRep::Bool(after.takes_fleet_warp)),
                ("acceptsFleetWarp", PyRep::Bool(after.takes_fleet_warp)),
                ("acceptsRegroup", PyRep::Bool(after.accepts_regroup)),
                ("acceptsFleetRegroups", PyRep::Bool(after.accepts_regroup)),
                ("acceptsConduitJumps", PyRep::Bool(after.accepts_conduit_jumps)),
                ("memberOptOuts", opt_outs),
            ])))
        }
        Err(e) => Some(Err(e)),
    }
}

pub(crate) fn compute_oriented_formation_offset(
    formation_id: i64,
    slot_idx: usize,
    spacing: f64,
    size: f64,
    warp_dir: DVec3,
    relative_delta: DVec3,
) -> DVec3 {
    if formation_id == 5 {
        return relative_delta.clamp_length_max(24_000.0);
    }
    if slot_idx == 0 || formation_id == 0 {
        return DVec3::ZERO;
    }
    let forward = if warp_dir.length_squared() > 1e-6 {
        warp_dir.normalize()
    } else {
        DVec3::X
    };
    let ref_up = if forward.y.abs() < 0.99 { DVec3::Y } else { DVec3::Z };
    let right = forward.cross(ref_up).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();
    let sp = spacing.clamp(2_500.0, 20_000.0);
    match formation_id {
        1 => {
            // Wall: 2D centered lattice in the (right, up) plane perpendicular to warp direction
            const WALL_SLOTS: [(i32, i32); 12] = [
                (1, 0), (-1, 0), (0, 1), (0, -1),
                (1, 1), (-1, 1), (1, -1), (-1, -1),
                (2, 0), (-2, 0), (0, 2), (0, -2),
            ];
            let (c, r) = if slot_idx <= WALL_SLOTS.len() {
                WALL_SLOTS[slot_idx - 1]
            } else {
                let k = (slot_idx - 1) as i32;
                ((k % 5) - 2, (k / 5) - 1)
            };
            (right * (c as f64 * sp) + up * (r as f64 * sp)).clamp_length_max(24_000.0)
        }
        2 => {
            // Sphere: 3D golden-angle Fibonacci shell around FC within command burst radius
            let radius = size.max(sp).clamp(3_000.0, 22_000.0);
            let k = slot_idx as f64;
            let golden = std::f64::consts::PI * (3.0 - 5.0_f64.sqrt());
            let y = 1.0 - ((2.0 * (k - 0.5)) / 12.0).clamp(0.1, 1.9);
            let r_xz = (1.0 - y * y).max(0.0).sqrt();
            let theta = golden * k;
            let (ux, uy, uz) = (r_xz * theta.cos(), y, r_xz * theta.sin());
            right * (ux * radius) + up * (uy * radius) + forward * (uz * radius)
        }
        3 => {
            // Arrow: V-wedge trailing behind FC along -forward (`idx=1` -> `sp * sqrt(2)`)
            let rank = slot_idx.div_ceil(2) as f64;
            let side = if slot_idx % 2 == 1 { 1.0 } else { -1.0 };
            (right * (side * rank * sp) - forward * (rank * sp)).clamp_length_max(24_000.0)
        }
        4 => {
            // Diamond: 3D octahedron vertices around FC
            let ring = ((slot_idx - 1) / 6 + 1) as f64;
            let axis = match (slot_idx - 1) % 6 {
                0 => right,
                1 => -right,
                2 => up,
                3 => -up,
                4 => forward,
                _ => -forward,
            };
            (axis * (sp * ring)).clamp_length_max(24_000.0)
        }
        _ => DVec3::ZERO,
    }
}

/// Commits fleet/member warp movement, repairs stale covert-cloak flags, applies optional
/// capacitor debits, atomically breaks non-covert protection, and fires warp hooks.
#[allow(clippy::too_many_arguments)]
async fn commit_fleet_member_warp(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    start_pos: DVec3,
    dest_pos: DVec3,
    warp_speed: f64,
    cs_opt: Option<&eve_destiny::CombatState>,
    is_covert_cloaked: bool,
    cap_debit_update: Option<(f64, f64, f64, bool)>,
) -> Result<(), String> {
    let repaired_covert_cloak = is_covert_cloaked
        && cs_opt.is_some_and(|cs| cs.is_cloaked && !cs.is_covert_cloak);
    if repaired_covert_cloak {
        let _ = sys
            .modify_combat_state(ship_id, |live_cs| {
                if live_cs.is_cloaked && !live_cs.is_covert_cloak {
                    live_cs.is_covert_cloak = true;
                }
            })
            .await;
    }
    let now_unix = ctx.now_unix();
    let warp_mode = MovementMode::new_warp(start_pos, dest_pos, warp_speed);
    match sys
        .set_warp_movement_mode_breaking_protection(
            ship_id,
            Some(char_id),
            warp_mode,
            crate::system_actor::WarpProvenance::PlayerOrFleetWarp,
            crate::system_actor::ProtectionBreakReason::WarpInitiation,
            now_unix,
        )
        .await
    {
        Ok((_tick, mut protection_events)) => {
            if let Some((new_cap, new_max_cap, old_cap, repaired_max_cap)) = cap_debit_update {
                if repaired_max_cap {
                    let _ = sys
                        .modify_combat_state(ship_id, move |live_cs| {
                            live_cs.max_capacitor = new_max_cap;
                            live_cs.capacitor = new_cap;
                        })
                        .await;
                } else {
                    let _ = sys.set_capacitor(ship_id, new_cap).await;
                }
                let _ = ctx.notify_ship_capacitor_change(
                    char_id,
                    ship_id,
                    new_cap,
                    new_max_cap,
                    Some(old_cap),
                );
            }
            if is_covert_cloaked {
                protection_events.gate_cloak_broken = false;
                protection_events.non_gate_cloak_broken = false;
            }
            crate::services::space_destiny::emit_protection_break_notifications(
                ctx,
                char_id,
                ship_id,
                &protection_events,
            );
            crate::services::space_destiny::track_player_or_fleet_warp(sys, ship_id, char_id);
            crate::services::nav_warp_interdiction::on_warp_initiated_hooks(
                ctx, sys, char_id, ship_id, sys_id,
            )
            .await;
            crate::services::pve_dungeon_runtime::on_warp_start_leave_dungeon_if_needed(
                ctx,
                char_id,
                sys_id,
                start_pos.to_array(),
                dest_pos.to_array(),
            );
            Ok(())
        }
        Err(e) => {
            if repaired_covert_cloak {
                let _ = sys
                    .modify_combat_state(ship_id, |live_cs| {
                        if live_cs.is_cloaked {
                            live_cs.is_covert_cloak = false;
                        }
                    })
                    .await;
            }
            Err(e)
        }
    }
}

pub(crate) async fn handle_fleet_warp(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
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
                client_error: None,
            }))
        }
    };
    let Some(sys_id) = session.solar_system_id else {
        return Some(Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solar system".into(),
            client_error: None,
        }));
    };

    let rpc = crate::router::RpcArgs::new(args, kwargs);
    let (dest_id, explicit_dest, min_range) = if args.len() >= 3
        && args.first().and_then(|a| a.as_f64()).is_some()
        && args.get(1).and_then(|a| a.as_f64()).is_some()
        && args.get(2).and_then(|a| a.as_f64()).is_some()
    {
        let pos = match rpc.req_finite_vec3(0) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let d = match rpc.opt_finite_f64(3, "minRange") {
            Ok(v) => v.unwrap_or(0.0).clamp(0.0, 100_000.0),
            Err(e) => return Some(Err(e)),
        };
        (0_i64, Some(pos), d)
    } else {
        let Some(did) = parse_dest_id(args, kwargs) else {
            return Some(Err(BridgeRpcError {
                code: "InvalidArgument".into(),
                message: "FleetWarp requires destination entity ID".into(),
                client_error: None,
            }));
        };
        let d = match rpc.opt_finite_f64(1, "minRange") {
            Ok(v) => v.unwrap_or(0.0).clamp(0.0, 100_000.0),
            Err(e) => return Some(Err(e)),
        };
        (did, None, d)
    };

    // parity: feat:fleet.takes-fleet-warp-toggle, feat:fleet.stored-formations-and-warp-shapes
    let (eligible_ship_ids, skipped_opt_out_members, resolved_caller_ship_id) =
        match ctx.db.with_snapshot(|snap| {
        let Some(fleet) = snap.fleets.get(&fleet_id) else {
            return Err(BridgeRpcError {
                code: "FleetNotFound".into(),
                message: format!("Fleet {fleet_id} not found"),
                client_error: None,
            });
        };
        let Some(caller_member) = fleet.member_details.get(&char_id) else {
            return Err(BridgeRpcError {
                code: "NotInFleet".into(),
                message: "Caller not in fleet".into(),
                client_error: None,
            });
        };
        let caller_role = role_to_i64(caller_member.role);
        if caller_role > 3 && fleet.boss_id != char_id {
            return Err(BridgeRpcError {
                code: "FleetPermissionDenied".into(),
                message: "Only Fleet, Wing, or Squad Commanders can initiate Fleet Warp".into(),
                client_error: None,
            });
        }
        let caller_rec = snap.characters.get(&char_id);
        let resolved_caller_ship_id = session
            .ship_id
            .or_else(|| caller_rec.map(|c| c.active_ship_id))
            .unwrap_or(0);
        if session.station_id.is_some()
            || session.structure_id.is_some()
            || caller_rec.is_some_and(|c| c.station_id.is_some() || c.structure_id.is_some())
        {
            return Err(BridgeRpcError::ship_not_in_space(
                resolved_caller_ship_id,
                Some(char_id),
                Some(sys_id),
                "FleetWarp",
                "Cannot initiate fleet warp while docked",
            ));
        }
        if resolved_caller_ship_id > 0 {
            crate::services::structure_control_ops::reject_if_controlling_structure(
                ctx,
                resolved_caller_ship_id,
                "initiate fleet warp",
            )?;
        }
        let mut ordered_members: Vec<&FleetMemberRecord> = fleet.member_details.values().collect();
        ordered_members.sort_by_key(|m| {
            (
                m.character_id != char_id,
                role_to_i64(m.role),
                m.wing_id.unwrap_or(0),
                m.squad_id.unwrap_or(0),
                m.character_id,
            )
        });
        let mut ships = Vec::new();
        let mut skipped = Vec::new();
        for m in ordered_members {
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
                    && !snap.structures.contains_key(&ch.active_ship_id)
                {
                    ships.push((m.character_id, ch.active_ship_id));
                }
            }
        }
        Ok((ships, skipped, resolved_caller_ship_id))
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
    let (form_idx, spacing_idx) = if explicit_dest.is_some() { (4, 5) } else { (2, 3) };
    let formation_id = args
        .get(form_idx)
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("formationID").and_then(|a| a.as_i64()))
        .or_else(|| stored_form.map(|(fid, _, _)| fid))
        .unwrap_or(0);
    let formation_spacing = args
        .get(spacing_idx)
        .and_then(|a| a.as_f64())
        .or_else(|| kwargs.get("formationSpacing").and_then(|a| a.as_f64()))
        .or_else(|| stored_form.map(|(_, sp, _)| sp))
        .unwrap_or(match formation_id {
            1 | 3 | 4 => 5_000.0,
            2 => 10_000.0,
            _ => 0.0,
        })
        .clamp(0.0, 100_000.0);

    let formation_size = kwargs
        .get("formationSize")
        .and_then(|a| a.as_f64())
        .or_else(|| stored_form.map(|(_, _, sz)| sz))
        .unwrap_or(formation_spacing.max(10_000.0))
        .clamp(0.0, 100_000.0);

    if (32_000_000..=32_999_999).contains(&sys_id) {
        return Some(Err(BridgeRpcError {
            code: "CannotWarpInAbyssalSpace".into(),
            message: "Bookmarks and warp navigation are disabled inside Abyssal Deadspace".into(),
            client_error: None,
        }));
    }
    if let Err(e) =
        crate::services::tourney_service::check_tourney_warp_restricted(ctx, sys_id, char_id)
    {
        return Some(Err(e));
    }
    let sys = ctx.systems.get_or_spawn(sys_id);
    let caller_ship_id = resolved_caller_ship_id;
    let caller_pos = sys
        .get_ball_snapshot(caller_ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position);
    let now_unix = ctx.now_unix();
    let mut dest_is_bookmark = false;
    let mut dest_is_mission = false;
    let dest_pos = if let Some(pos) = explicit_dest {
        pos
    } else if let Ok(Some((_, b))) = sys.get_ball_snapshot(dest_id).await {
        b.position
    } else if let Some(bm_res) = ctx.db.with_snapshot(|snap| {
        let bm = snap.bookmarks.get(&dest_id)?;
        if sys_id == 30_100_000 || bm.solar_system_id == 30_100_000 {
            return Some(Err(BridgeRpcError::from_user_key(eve_proto::UserErrorKey::DunBlacklistCannotWarp)));
        }
        if super::is_bookmark_expired_in_snap(snap, dest_id, now_unix)
            || bm.solar_system_id != sys_id
        {
            return Some(Err(BridgeRpcError {
                code: "BookmarkNotFound".into(),
                message: format!("Bookmark {dest_id} not found or inaccessible"),
                client_error: None,
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
                client_error: None,
            }));
        }
        Some(Ok(DVec3::new(bm.x, bm.y, bm.z)))
    }) {
        match bm_res {
            Ok(p) => {
                dest_is_bookmark = true;
                p
            }
            Err(e) => return Some(Err(e)),
        }
    } else if let Some(st) = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .and_then(|s| s.stations.iter().find(|st| st.station_id == dest_id))
    {
        DVec3::new(st.position.x, st.position.y, st.position.z)
    } else if let Some(m_pos) = match crate::services::agents_missions::agents_missions_warp::resolve_accepted_mission_warp_origin(
        ctx, session, char_id, sys_id, dest_id,
    ) {
        Ok(pos) => pos,
        Err(e) => return Some(Err(e)),
    } {
        dest_is_mission = true;
        m_pos
    } else {
        DVec3::new(150_000_000.0, 0.0, 0.0)
    };

    if let Err(e) = crate::services::pve_dungeon_runtime::check_deadspace_warp_allowed(
        ctx,
        sys_id,
        caller_pos.unwrap_or(dest_pos).to_array(),
        dest_pos.to_array(),
        dest_pos.to_array(),
    ) {
        return Some(Err(e));
    }
    if let Err(e) = crate::services::pve_ess_reserve_banks::check_ess_warp_destination_allowed(
        ctx, sys_id, dest_pos, dest_pos,
    )
    .await
    {
        return Some(Err(e));
    }
    if !dest_is_bookmark {
        if let Err(e) =
            crate::services::pve_dungeon_runtime_rpc::check_warp_target_blocked_by_gated_dungeon_room(
                ctx,
                sys_id,
                dest_pos,
                (dest_id > 0).then_some(dest_id),
            )
        {
            return Some(Err(e));
        }
    }

    let fleet_warp_dir = caller_pos
        .map(|cp| (dest_pos - cp).normalize_or_zero())
        .filter(|d| d.length_squared() > 1e-6)
        .unwrap_or(DVec3::X);
    let anchor_pos = dest_pos - fleet_warp_dir * min_range;
    let mut warped_members = Vec::new();
    let mut warped_landings = Vec::new();
    let mut landing_positions = Vec::new();
    let mut ess_bubble_blocked = false;
    let mut deadspace_blocked = false;
    let mut gated_dungeon_err: Option<BridgeRpcError> = None;
    for (member_char_id, ship_id) in eligible_ship_ids {
        if let Err(err) = crate::services::nav_warp_interdiction::validate_pre_warp_common(
            ctx,
            member_char_id,
            ship_id,
            sys_id,
            &sys,
        )
        .await
        {
            if err.code == "EssIntrusionBubbleWarpBlocked" {
                ess_bubble_blocked = true;
            }
            continue;
        }
        let validated = match ctx
            .validate_ship_movement(
                member_char_id,
                ship_id,
                crate::services::space_nav_movement_guard::MovementIntent::Warp { sys_id },
            )
            .await
        {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(ball) = validated.ball else {
            continue;
        };
        if ball.max_velocity <= 0.0 {
            continue;
        }
        if let Some(c_pos) = caller_pos {
            if member_char_id != char_id && ball.position.distance(c_pos) > 8_000_000.0 {
                continue;
            }
        }
        let cs_opt = validated.combat_state;
        let is_covert_cloaked = validated.is_covert_cloaked;
        let wdo_lvl = ctx.db.with_snapshot(|s| {
            s.skills
                .get(&member_char_id)
                .and_then(|m| m.get(&3455))
                .map(|sk| sk.level.min(5))
                .unwrap_or(0)
        });

        let dist = ball.position.distance(dest_pos);
        if dist < WARP_MIN_DISTANCE_M {
            continue;
        }
        let slot_idx = warped_members.len();
        let rel_delta = caller_pos.map(|cp| ball.position - cp).unwrap_or(DVec3::ZERO);
        let offset = compute_oriented_formation_offset(
            formation_id,
            slot_idx,
            formation_spacing,
            formation_size,
            fleet_warp_dir,
            rel_delta,
        );
        let mut landing = anchor_pos + offset;
        if ball.position.distance(landing) < WARP_MIN_DISTANCE_M {
            continue;
        }
        landing = match crate::services::nav_warp_interdiction::check_warp_interdiction_and_adjust_dest(
            &sys,
            ship_id,
            ball.position,
            landing,
        ) {
            Ok(adjusted) => adjusted,
            Err(_) => continue,
        };

        let (base_warp_cap_need, sde_max_cap) = ctx
            .sde
            .types
            .get(&ball.type_id)
            .map(|t| {
                let wcn = t.default_attributes.get(&153).copied().unwrap_or(1.24e-6);
                let cap = t
                    .default_attributes
                    .get(&eve_sde::attr_ids::CAPACITOR_CAPACITY)
                    .copied()
                    .unwrap_or(250.0);
                (wcn, cap)
            })
            .unwrap_or((1.24e-6, 250.0));
        let eff_warp_cap_need = base_warp_cap_need * (1.0 - 0.10 * (wdo_lvl as f64));
        let cap_per_m =
            (eff_warp_cap_need * ball.mass) / crate::services::space_destiny::AU_IN_METERS;
        let requested_dist_m = ball.position.distance(landing);
        let req_cap = cap_per_m * requested_dist_m;
        let mut cap_debit_update: Option<(f64, f64, f64, bool)> = None;
        if let Some(mut cs) = cs_opt.clone() {
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
                if affordable_dist_m < WARP_MIN_DISTANCE_M {
                    continue;
                }
                let warp_dir = (landing - ball.position).normalize_or_zero();
                landing = ball.position + warp_dir * affordable_dist_m;
                cs.capacitor = 0.0;
            } else {
                cs.capacitor = (cs.capacitor - req_cap).max(0.0);
            }
            cap_debit_update = Some((cs.capacitor, cs.max_capacitor, old_cap, repaired_max_cap));
        }

        if crate::services::pve_dungeon_runtime::check_deadspace_warp_allowed(
            ctx,
            sys_id,
            ball.position.to_array(),
            dest_pos.to_array(),
            landing.to_array(),
        )
        .is_err()
        {
            deadspace_blocked = true;
            continue;
        }
        if crate::services::pve_ess_reserve_banks::check_ess_warp_destination_allowed(
            ctx, sys_id, dest_pos, landing,
        )
        .await
        .is_err()
        {
            ess_bubble_blocked = true;
            continue;
        }
        if !dest_is_bookmark {
            if let Err(e) =
                crate::services::pve_dungeon_runtime_rpc::check_warp_target_blocked_by_gated_dungeon_room(
                    ctx, sys_id, landing, None,
                )
            {
                if gated_dungeon_err.is_none() {
                    gated_dungeon_err = Some(e);
                }
                continue;
            }
        }
        let warp_speed = crate::services::space_destiny::compute_pilot_warp_speed_au_s(
            ctx,
            member_char_id,
            sys_id,
            ball.type_id,
        );
        if commit_fleet_member_warp(
            ctx,
            &sys,
            member_char_id,
            ship_id,
            sys_id,
            ball.position,
            landing,
            warp_speed,
            cs_opt.as_ref(),
            is_covert_cloaked,
            cap_debit_update,
        )
        .await
        .is_ok()
        {
            warped_members.push(PyRep::Integer(member_char_id));
            warped_landings.push((member_char_id, landing));
            landing_positions.push(PyRep::Tuple(vec![
                PyRep::Integer(member_char_id),
                PyRep::Real(landing.x),
                PyRep::Real(landing.y),
                PyRep::Real(landing.z),
            ]));
        }
    }
    if warped_members.is_empty() && deadspace_blocked {
        return Some(Err(BridgeRpcError::with_client_error(
            "NaturalPhenomenaDisruptingWarp",
            "Natural phenomena are disrupting warp",
            eve_proto::ClientError::CustomInfo("Natural phenomena are disrupting warp".into()),
        )));
    }
    if warped_members.is_empty() && ess_bubble_blocked {
        return Some(Err(BridgeRpcError {
            code: "EssIntrusionBubbleWarpBlocked".into(),
            message: "Cannot fleet warp out from inside the 75 km ESS warp disruption bubble".into(),
            client_error: None,
        }));
    }
    if warped_members.is_empty() {
        if let Some(err) = gated_dungeon_err {
            return Some(Err(err));
        }
    }

    for &opt_out_cid in &skipped_opt_out_members {
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            OnFleetMoveFailed {
                char_id: opt_out_cid,
                is_kicked: false,
            },
        );
    }

    if (dest_is_bookmark || dest_is_mission) && !warped_landings.is_empty() {
        let stuff_type = if dest_is_bookmark { "bookmark" } else { "mission" };
        crate::services::agents_missions::agents_missions_warp::materialize_mission_pocket_on_fleet_warp(
            ctx,
            &sys,
            char_id,
            sys_id,
            stuff_type,
            dest_id,
            &warped_landings,
        )
        .await;
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

fn handle_set_broadcast_filter(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    // parity: feat:fleet.broadcast-scope-and-filter
    let (_, char_id) = ServerContext::require_character(session)?;
    let b_name = args.first().and_then(|a| a.as_str()).unwrap_or("Target");
    let enabled = args
        .get(1)
        .and_then(|a| match a {
            PyRep::Bool(b) => Some(*b),
            PyRep::Integer(i) => Some(*i != 0),
            _ => None,
        })
        .unwrap_or(true);
    let muted_list = {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        let set = ex.broadcast_filters.entry(char_id).or_default();
        if enabled {
            set.remove(b_name);
        } else {
            set.insert(b_name.to_string());
        }
        let mut v: Vec<String> = set.iter().cloned().collect();
        v.sort();
        v
    };
    Ok(PyRep::keyval([
        ("broadcastType", PyRep::String(b_name.to_string())),
        ("enabled", PyRep::Bool(enabled)),
        (
            "mutedBroadcasts",
            PyRep::List(muted_list.into_iter().map(PyRep::String).collect()),
        ),
    ]))
}

fn handle_get_broadcast_filter(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let muted_list = {
        let ex = ctx.fleet_extras.lock().unwrap();
        let mut v: Vec<String> = ex
            .broadcast_filters
            .get(&char_id)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        v.sort();
        v
    };
    Ok(PyRep::keyval([(
        "mutedBroadcasts",
        PyRep::List(muted_list.into_iter().map(PyRep::String).collect()),
    )]))
}

fn handle_get_broadcast_history(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(fleet_id) = session.fleet_id else {
        return Err(BridgeRpcError {
            code: "NotInFleet".into(),
            message: "Character is not in a fleet".into(),
            client_error: None,
        });
    };
    let in_fleet = ctx.db.with_snapshot(|snap| {
        snap.fleets
            .get(&fleet_id)
            .is_some_and(|f| f.members.contains(&char_id))
    });
    if !in_fleet {
        return Err(BridgeRpcError {
            code: "NotInFleet".into(),
            message: "Character is not a member of that fleet".into(),
            client_error: None,
        });
    }
    let history = ctx
        .fleet_extras
        .lock()
        .unwrap()
        .broadcast_history
        .get(&fleet_id)
        .cloned()
        .unwrap_or_default();
    let entries: Vec<PyRep> = history
        .into_iter()
        .map(|(ts, sender, name, scope, item_id, type_id)| {
            PyRep::keyval([
                ("timestamp", PyRep::Integer(ts as i64)),
                ("senderID", PyRep::Integer(sender)),
                ("broadcast", PyRep::String(name)),
                ("scope", PyRep::Integer(scope)),
                ("itemID", item_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("typeID", type_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
            ])
        })
        .collect();
    Ok(PyRep::List(entries))
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(handle_fleet_ops(ctx, session, service, method, args, kwargs))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("fleetObjectHandler", "SendBroadcast", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "BroadcastToBubble", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "BroadcastToSystem", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "SendBroadcast", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "BroadcastToBubble", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "BroadcastToSystem", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "WarpToMember", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "WarpToMember", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "FleetWarp", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "FleetWarp", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "WarpFleet", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "WarpFleet", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "SetMotd", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "SetMotd", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "SetMotdEx", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "GetMotd", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "SetOptions", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "SetAutoJoinSquadID", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "SetFleetMaxSize", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "GetFleetMaxSize", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "SetTakesFleetWarp", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "SetTakesFleetWarp", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "SetAcceptsRegroupValue", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "SetAcceptsRegroupValue", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "SetAcceptsConduitJumpsValue", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "SetAcceptsConduitJumpsValue", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "GetFleetComposition", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "GetInitState", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "GetInitState", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "GetWings", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "GetWings", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "GetFleet", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "GetFleet", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "AddToWatchlist", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "RemoveFromWatchlist", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "RegisterForDamageUpdates", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "AddToWatchlist", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "RemoveFromWatchlist", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "RegisterForDamageUpdates", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "SetBroadcastFilter", RpcDispatchHandler::Async(dispatch_rpc)), // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
    rpc_entry("fleetMgr", "SetBroadcastFilter", RpcDispatchHandler::Async(dispatch_rpc)), // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
    rpc_entry("fleetObjectHandler", "GetBroadcastFilter", RpcDispatchHandler::Async(dispatch_rpc)), // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
    rpc_entry("fleetMgr", "GetBroadcastFilter", RpcDispatchHandler::Async(dispatch_rpc)), // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
    rpc_entry("fleetObjectHandler", "GetBroadcastHistory", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "GetBroadcastHistory", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetMgr", "Regroup", RpcDispatchHandler::Async(dispatch_rpc)),
    rpc_entry("fleetObjectHandler", "Regroup", RpcDispatchHandler::Async(dispatch_rpc)),
];

#[cfg(test)]
#[path = "fleet_social_fleet_ops_tests.rs"]
pub(crate) mod fleet_social_fleet_ops_tests;

#[cfg(test)]
#[path = "fleet_social_fleet_ops_edges_tests.rs"]
mod fleet_social_fleet_ops_edges_tests;
