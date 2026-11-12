//! Chat (`SO-7`), Notifications, Contacts & Calendar (`P-5`/`CH-4`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{ChatChannelRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

#[path = "fleet_social_chat_auth_ops.rs"]
mod fleet_social_chat_auth_ops;

use fleet_social_chat_auth_ops::{
    build_initial_channel_record, check_channel_or_global_mute,
    handle_contacts_notifications_calendar, has_channel_operator_auth, has_channel_owner_auth,
    has_dynamic_org_operator_role, is_org_channel, is_system_channel, is_system_or_org_channel,
    validate_channel_join_auth,
};

pub(crate) use super::fleet_social_persistence::{
    CalendarEventState, SocialExtrasState,
};

pub(crate) fn get_delayed_local_speakers(ctx: &ServerContext, chan_id: &str) -> Vec<i64> {
    ctx.social_extras
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .delayed_local_speakers
        .get(chan_id)
        .map(|s| s.iter().copied().collect())
        .unwrap_or_default()
}

pub(crate) fn collect_wh_local_witness_speakers_to_drop_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    chan_id: &str,
    leaving_char_id: i64,
    remaining_members: &HashSet<i64>,
    candidate_speakers: &[i64],
) -> Vec<i64> {
    let mut dropped = Vec::new();
    for &spk in candidate_speakers {
        let wit_key = format!("{chan_id}:{spk}");
        let mut keep = remaining_members.contains(&spk);
        if let Some(wit) = snap.fleet_and_social_ops.wh_local_witnesses.get_mut(&wit_key) {
            wit.remove(&leaving_char_id);
            keep = keep || wit.iter().any(|m| remaining_members.contains(m));
        }
        if !keep {
            snap.fleet_and_social_ops.wh_local_witnesses.remove(&wit_key);
            dropped.push(spk);
        }
    }
    dropped
}

pub(crate) fn remove_delayed_local_speakers(
    ctx: &ServerContext,
    chan_id: &str,
    dropped_speakers: &[i64],
) {
    if dropped_speakers.is_empty() {
        return;
    }
    let mut s = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(speakers) = s.delayed_local_speakers.get_mut(chan_id) {
        for spk in dropped_speakers {
            speakers.remove(spk);
        }
    }
}

#[allow(dead_code)]
pub(crate) fn prune_delayed_local_speakers_on_leave(
    ctx: &ServerContext,
    snap: &mut eve_db::WorldStateSnapshot,
    chan_id: &str,
    leaving_char_id: i64,
    remaining_members: &HashSet<i64>,
) {
    let candidates = get_delayed_local_speakers(ctx, chan_id);
    let dropped = collect_wh_local_witness_speakers_to_drop_in_snap(
        snap,
        chan_id,
        leaving_char_id,
        remaining_members,
        &candidates,
    );
    remove_delayed_local_speakers(ctx, chan_id, &dropped);
}

pub(super) fn handle_chat_and_social(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let is_chat_service = matches!(
        service,
        "LSC" | "XmppChatMgr" | "chatAuthenticationService"
    ) || service.eq_ignore_ascii_case("lscservice")
        || service.eq_ignore_ascii_case("lsc")
        || service.eq_ignore_ascii_case("chat")
        || service.eq_ignore_ascii_case("chatmgr");

    if is_chat_service {
        return handle_chat_rpc(ctx, session, service, method, args);
    }

    if service == "charMgr" {
        return handle_contacts_notifications_calendar(ctx, session, service, method, args, kwargs);
    }

    None
}

fn handle_chat_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("chatAuthenticationService", "GetAuthenticationToken") => {
            handle_get_chat_authentication_token(ctx, session)
        }

        ("XmppChatMgr", "AddCharacterToChannel") => {
            let (_, caller_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "AddCharacterToChannel requires channelID".into() }));
            };
            let (target_char, password_arg) = match (args.get(1), args.get(2)) {
                (Some(PyRep::Integer(cid)), Some(PyRep::String(pw))) => (*cid, Some(pw.as_str())),
                (Some(PyRep::Integer(cid)), _) => (*cid, None),
                (Some(PyRep::String(pw)), _) => (caller_id, Some(pw.as_str())),
                _ => (caller_id, None),
            };
            if target_char != caller_id {
                let blocked_in_contacts = crate::services::character_skills::character_contacts::is_blocked_by(ctx, target_char, caller_id);
                let blocked_in_social = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner()).blocked.get(&target_char).is_some_and(|b| b.contains(&caller_id));
                if blocked_in_contacts || blocked_in_social {
                    return Some(Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Target character has blocked caller".into() }));
                }
            }
            let is_delayed = is_delayed_local_channel(chan_id);
            let sys_id_opt = parse_channel_system_id(chan_id);
            let blocked_in_social_chan = is_character_blocked_in_social_extras(ctx, chan_id, target_char);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let chan_key = resolve_channel_key(snap, chan_id);
                validate_channel_join_auth(ctx, snap, session, caller_id, target_char, &chan_key)?;
                let existing_owner = snap.chat_channels.get(&chan_key).and_then(|c| c.owner_id);
                if existing_owner != Some(target_char)
                    && (blocked_in_social_chan || is_character_blocked_in_acl(snap, &chan_key, target_char))
                {
                    return Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Blocked from channel".into() });
                }
                let chan_exists = snap.chat_channels.contains_key(&chan_key);
                if !chan_exists && target_char != caller_id && !session.has_gm_role() {
                    return Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Cannot add another character to non-existent channel".into() });
                }
                if target_char != caller_id {
                    let existing = snap.chat_channels.get(&chan_key);
                    if !has_channel_operator_auth(snap, session, caller_id, &chan_key, existing) {
                        return Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Only channel owner or operator may add other characters".into() });
                    }
                }
                let caller_is_op = {
                    let existing = snap.chat_channels.get(&chan_key);
                    has_channel_operator_auth(snap, session, caller_id, &chan_key, existing)
                };
                let chan = snap.chat_channels.entry(chan_key.clone()).or_insert_with(|| {
                    build_initial_channel_record(
                        &chan_key,
                        caller_id,
                        "",
                        None,
                        is_delayed,
                        sys_id_opt,
                    )
                });
                if is_system_or_org_channel(&chan_key) {
                    chan.owner_id = None;
                    chan.operators.clear();
                    chan.password = None;
                }
                if let Some(ref pw) = chan.password {
                    if !pw.is_empty() && !caller_is_op && chan.owner_id != Some(caller_id) && !chan.operators.contains(&caller_id) && password_arg != Some(pw.as_str()) {
                        return Err(BridgeRpcError { code: "InvalidChannelPassword".into(), message: "Incorrect password for channel".into() });
                    }
                }
                chan.members.insert(target_char);
                Ok((chan_key, chan.name.clone(), chan.motd.clone(), chan.owner_id, chan.members.len(), chan.is_delayed_local))
            });
            match res {
                Ok((chan_key, name, motd, owner_id, members_len, delayed)) => {
                    let count = if delayed {
                        ctx.social_extras
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .delayed_local_speakers
                            .get(&chan_key)
                            .map(|s| s.len())
                            .unwrap_or(0)
                    } else {
                        members_len
                    };
                    let jid = format!("{}@chat.evomu.local", chan_key);
                    if !delayed {
                        ctx.notify(NotificationTarget::All, "OnChannelJoined", "channelid", vec![PyRep::String(chan_key.clone()), PyRep::Integer(target_char)]);
                    }
                    Some(Ok(PyRep::keyval([
                        ("channelID", PyRep::String(chan_key)),
                        ("characterID", PyRep::Integer(target_char)),
                        ("jid", PyRep::String(jid)),
                        ("name", PyRep::String(name)),
                        ("motd", PyRep::String(motd)),
                        ("memberCount", PyRep::Integer(count as i64)),
                        ("isDelayedLocal", PyRep::Bool(delayed)),
                        ("ownerID", owner_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                        ("joined", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("XmppChatMgr", "EnsureResourceWarsChannelExists")
        | ("XmppChatMgr", "ResyncSystemChannelAccess") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let explicit_arg = args.first().and_then(|a| a.as_str());
            if let Err(e) = crate::services::fleet_social_chat_acl_helpers::validate_system_channel_resync(
                ctx, session, char_id, method, explicit_arg,
            ) {
                return Some(Err(e));
            }
            let (social_blocked_chans, delayed_speakers_snapshot): (
                HashSet<String>,
                HashMap<String, Vec<i64>>,
            ) = {
                let s = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
                let blocked = if session.has_gm_role() {
                    HashSet::new()
                } else {
                    s.blocked_in_channel
                        .iter()
                        .filter_map(|(k, b)| b.contains(&char_id).then(|| k.to_ascii_lowercase()))
                        .collect()
                };
                let speakers = s
                    .delayed_local_speakers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.iter().copied().collect()))
                    .collect();
                (blocked, speakers)
            };
            let (local_chan, synced_channels, is_delayed, dropped_by_channel) = ctx.db.with_mut_snapshot(|snap| {
                let ch = snap.characters.get(&char_id);
                let sys_id = explicit_arg
                    .and_then(parse_channel_system_id)
                    .or(session.solar_system_id)
                    .or_else(|| ch.map(|c| c.solar_system_id))
                    .unwrap_or(0);
                let corp_id = session.corporation_id.or_else(|| ch.map(|c| c.corporation_id)).unwrap_or(1_000_009);
                let alliance_id = session.alliance_id.or_else(|| ch.and_then(|c| c.alliance_id));
                let fleet_id = session.fleet_id.or_else(|| snap.fleets.values().find(|f| f.members.contains(&char_id)).map(|f| f.fleet_id));
                let local_id = if method == "EnsureResourceWarsChannelExists" {
                    explicit_arg.map(|s| s.to_string()).unwrap_or_else(|| format!("resourcewars:{sys_id}"))
                } else {
                    explicit_arg.filter(|s| !s.starts_with("solarsystemid2:")).map(|s| s.to_string()).unwrap_or_else(|| format!("solarsystemid2:{sys_id}"))
                };
                let corp_chan = format!("corpid:{corp_id}");
                let mut target_chans = if (32_000_000..=32_999_999).contains(&sys_id) && method != "EnsureResourceWarsChannelExists" {
                    vec![corp_chan]
                } else {
                    vec![local_id.clone(), corp_chan]
                };
                if let Some(aid) = alliance_id {
                    target_chans.push(format!("allianceid:{aid}"));
                }
                if let Some(fid) = fleet_id {
                    target_chans.push(format!("fleetid:{fid}"));
                }
                let mut left_delayed = Vec::new();
                for c in snap.chat_channels.values_mut() {
                    let is_sys_scoped = c.channel_id.starts_with("solarsystemid2:")
                        || c.channel_id.starts_with("corpid:")
                        || c.channel_id.starts_with("allianceid:")
                        || c.channel_id.starts_with("fleetid:");
                    if is_sys_scoped && !target_chans.contains(&c.channel_id) && c.members.remove(&char_id) && c.is_delayed_local {
                        left_delayed.push((c.channel_id.clone(), c.members.clone()));
                    }
                }
                let mut dropped_by_channel = Vec::new();
                for (lcid, rem_m) in left_delayed {
                    let candidates = delayed_speakers_snapshot
                        .get(&lcid)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    let dropped = collect_wh_local_witness_speakers_to_drop_in_snap(
                        snap, &lcid, char_id, &rem_m, candidates,
                    );
                    if !dropped.is_empty() {
                        dropped_by_channel.push((lcid, dropped));
                    }
                }
                for cid in &target_chans {
                    if !session.has_gm_role()
                        && (social_blocked_chans.contains(&cid.to_ascii_lowercase())
                            || is_character_blocked_in_acl(snap, cid, char_id))
                    {
                        continue;
                    }
                    let delayed = is_delayed_local_channel(cid);
                    let sys_opt = parse_channel_system_id(cid);
                    let chan = snap
                        .chat_channels
                        .entry(cid.clone())
                        .or_insert_with(|| {
                            build_initial_channel_record(cid, char_id, "", None, delayed, sys_opt)
                        });
                    if is_system_or_org_channel(cid) {
                        chan.owner_id = None;
                        chan.operators.clear();
                        chan.password = None;
                    }
                    chan.members.insert(char_id);
                }
                let is_del = is_delayed_local_channel(&local_id);
                (local_id, target_chans, is_del, dropped_by_channel)
            });
            for (lcid, dropped) in dropped_by_channel {
                remove_delayed_local_speakers(ctx, &lcid, &dropped);
            }
            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(local_chan)),
                ("exists", PyRep::Bool(true)),
                ("isDelayedLocal", PyRep::Bool(is_delayed)),
                ("channels", PyRep::List(synced_channels.into_iter().map(PyRep::String).collect())),
            ])))
        }

        ("XmppChatMgr", "CreatePlayerOwnedChannel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("PlayerChannel").trim();
            if name.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "CreatePlayerOwnedChannel requires non-empty channel name".into(),
                }));
            }
            let password = args.get(1).and_then(|a| a.as_str()).map(|s| s.to_string());
            let motd = args.get(2).and_then(|a| a.as_str()).unwrap_or("");
            let chan_id = format!("channel:{}", name.to_lowercase().replace(' ', "_"));
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if find_existing_channel_key(snap, &chan_id).is_some()
                    || snap
                        .chat_channels
                        .values()
                        .any(|c| c.name.eq_ignore_ascii_case(name) && c.channel_id.eq_ignore_ascii_case(&chan_id))
                {
                    return Err(BridgeRpcError {
                        code: "ChannelAlreadyExists".into(),
                        message: format!("Channel {chan_id} already exists"),
                    });
                }
                snap.chat_channels.insert(
                    chan_id.clone(),
                    ChatChannelRecord {
                        channel_id: chan_id.clone(),
                        name: name.to_string(),
                        motd: motd.to_string(),
                        owner_id: Some(char_id),
                        members: HashSet::from([char_id]),
                        is_system_local: false,
                        solar_system_id: None,
                        is_private: false,
                        operators: HashSet::from([char_id]),
                        password,
                        is_delayed_local: false,
                    },
                );
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("channelID", PyRep::String(chan_id)),
                    ("name", PyRep::String(name.to_string())),
                    ("ownerID", PyRep::Integer(char_id)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("XmppChatMgr", "CreatePrivateChannel") => {
            handle_chat_rpc(ctx, session, "LSC", "Invite", args)
        }

        ("XmppChatMgr", "GrantChannelOwnership") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GrantChannelOwnership requires channelID".into(),
                }));
            };
            let Some(new_owner_id) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GrantChannelOwnership requires newOwnerID".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(chan_key) = find_existing_channel_key(snap, chan_id) else {
                    return Err(BridgeRpcError {
                        code: "ChannelNotFound".into(),
                        message: format!("Channel {chan_id} not found"),
                    });
                };
                if is_system_or_org_channel(&chan_key) {
                    return Err(BridgeRpcError {
                        code: "NotChannelOwner".into(),
                        message: "System and organization channels do not allow ownership transfer".into(),
                    });
                }
                let Some(chan) = snap.chat_channels.get_mut(&chan_key) else {
                    return Err(BridgeRpcError {
                        code: "ChannelNotFound".into(),
                        message: format!("Channel {chan_id} not found"),
                    });
                };
                if !has_channel_owner_auth(session, char_id, &chan_key, chan) {
                    return Err(BridgeRpcError {
                        code: "NotChannelOwner".into(),
                        message: "Only the channel owner can transfer ownership".into(),
                    });
                }
                chan.owner_id = Some(new_owner_id);
                chan.operators.insert(new_owner_id);
                chan.members.insert(new_owner_id);
                clear_channel_acl_restrictions(&mut snap.chat_channel_acl, &chan_key, new_owner_id);
                Ok(chan_key)
            });
            match res {
                Ok(chan_key) => {
                    clear_social_extras_channel_restrictions(ctx, &chan_key, new_owner_id);
                    Some(Ok(PyRep::keyval([
                        ("channelID", PyRep::String(chan_id.to_string())),
                        ("ownerID", PyRep::Integer(new_owner_id)),
                        ("transferred", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("XmppChatMgr", "SetMotd") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let chan_id = args.first().and_then(|a| a.as_str()).unwrap_or("local");
            let motd = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let can_op = {
                    let chan_ref = snap.chat_channels.get(chan_id);
                    has_channel_operator_auth(snap, session, char_id, chan_id, chan_ref)
                };
                let Some(chan) = snap.chat_channels.get_mut(chan_id) else {
                    return Err(BridgeRpcError {
                        code: "ChannelNotFound".into(),
                        message: format!("Channel {chan_id} not found"),
                    });
                };
                if !can_op {
                    return Err(BridgeRpcError {
                        code: "NotChannelOperator".into(),
                        message: "Requires channel operator privileges".into(),
                    });
                }
                chan.motd = motd.to_string();
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("channelID", PyRep::String(chan_id.to_string())),
                    ("motd", PyRep::String(motd.to_string())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("XmppChatMgr", "GMMute") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let chan_id = args.get(1).and_then(|a| a.as_str()).unwrap_or("global");
            let auth_check = ctx.db.with_snapshot(|snap| {
                let chan_key = resolve_channel_key(snap, chan_id);
                let chan_opt = snap.chat_channels.get(&chan_key);
                let is_op = has_channel_operator_auth(snap, session, char_id, &chan_key, chan_opt);
                let targets_owner = chan_opt.and_then(|c| c.owner_id) == Some(target_id);
                (chan_key, is_op, targets_owner)
            });
            let (chan_key, is_authorized, targets_owner) = auth_check;
            if !is_authorized {
                return Some(Err(BridgeRpcError {
                    code: "ChatAccessDenied".into(),
                    message: "GMMute requires channel operator or GM role".into(),
                }));
            }
            if targets_owner && !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "NotChannelOwner".into(),
                    message: "Channel owner cannot be muted by a non-GM operator".into(),
                }));
            }
            let duration_sec = args.get(2).and_then(|a| a.as_i64()).unwrap_or(300).max(1) as u64;
            let reason = args
                .get(3)
                .and_then(|a| a.as_str())
                .unwrap_or("Channel moderation mute");
            let until = ctx.now_unix() + duration_sec;
            ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner()).muted_in_channel.insert((chan_key.clone(), target_id), (until, reason.to_string()));
            ctx.db.with_mut_snapshot(|snap| {
                snap.chat_channel_acl
                    .entry(chan_key.clone())
                    .or_default()
                    .muted_until
                    .insert(target_id, (until, reason.to_string()));
            });
            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(chan_key)),
                ("characterID", PyRep::Integer(target_id)),
                ("mutedUntil", PyRep::Integer(until as i64)),
            ])))
        }

        ("XmppChatMgr", "GMUnmute") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let chan_id = args.get(1).and_then(|a| a.as_str()).unwrap_or("global");
            let (chan_key, is_authorized) = ctx.db.with_snapshot(|snap| {
                let chan_key = resolve_channel_key(snap, chan_id);
                let ok = has_channel_operator_auth(snap, session, char_id, &chan_key, snap.chat_channels.get(&chan_key));
                (chan_key, ok)
            });
            if !is_authorized {
                return Some(Err(BridgeRpcError { code: "ChatAccessDenied".into(), message: "GMUnmute requires channel operator or GM role".into() }));
            }
            ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner()).muted_in_channel.remove(&(chan_key.clone(), target_id));
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(acl) = snap.chat_channel_acl.get_mut(&chan_key) {
                    acl.muted_until.remove(&target_id);
                }
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("XmppChatMgr", "GetHelpChannel") => handle_get_help_channel(ctx, session, args),

        ("XmppChatMgr", "GetRookieChannel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let now = ctx.now_unix();
            let is_rookie = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&char_id).is_some_and(|ch| {
                    let effective_created = if ch.create_date_unix == 1_704_067_200 { now } else { ch.create_date_unix };
                    now.saturating_sub(effective_created) <= 30 * 86_400
                })
            });
            Some(Ok(if is_rookie { PyRep::String("rookie_help".into()) } else { PyRep::None }))
        }

        ("XmppChatMgr", "Hostname") => handle_xmpp_hostname(ctx),

        // parity: rpc:XmppChatMgr.GetDeprecatedPrefsFallback
        ("XmppChatMgr", "GetDeprecatedPrefsFallback") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let filter_key = args.first().and_then(|a| a.as_str()).filter(|s| !s.is_empty());
            let mut entries: Vec<(PyRep, PyRep)> =
                crate::services::cluster_macho_misc_services::with_misc_state(ctx, |st| {
                    st.char_settings
                        .get(&char_id)
                        .map(|m| {
                            m.iter()
                                .filter(|(k, _)| filter_key.is_none_or(|fk| k.as_str() == fk))
                                .map(|(k, v)| (PyRep::String(k.clone()), v.clone()))
                                .collect()
                        })
                        .unwrap_or_default()
                });
            ctx.db.with_snapshot(|snap| {
                for c in snap.chat_channels.values().filter(|c| c.members.contains(&char_id)) {
                    if filter_key.is_some_and(|fk| c.channel_id != fk) {
                        continue;
                    }
                    if !entries.iter().any(|(k, _)| k.as_str() == Some(c.channel_id.as_str())) {
                        entries.push((
                            PyRep::String(c.channel_id.clone()),
                            PyRep::keyval([
                                ("channelID", PyRep::String(c.channel_id.clone())),
                                ("displayName", PyRep::String(c.name.clone())),
                                ("motd", PyRep::String(c.motd.clone())),
                                ("isDelayedLocal", PyRep::Bool(c.is_delayed_local)),
                                ("isOperator", PyRep::Bool(has_channel_operator_auth(snap, session, char_id, &c.channel_id, Some(c)))),
                                ("isMuted", PyRep::Bool(
                                    snap.chat_channel_acl.get(&c.channel_id).is_some_and(|a| a.muted_until.contains_key(&char_id))
                                    || snap.chat_channel_acl.get("global").is_some_and(|a| a.muted_until.contains_key(&char_id))
                                )),
                            ]),
                        ));
                    }
                }
            });
            Some(Ok(PyRep::Dict(entries)))
        }

        (_, "GetChannels") | (_, "GetMyMessages") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.chat_channels.values().filter(|c| c.members.contains(&char_id)).map(|c| vec![
                    PyRep::String(c.channel_id.clone()),
                    PyRep::String(c.name.clone()),
                    PyRep::String(c.motd.clone()),
                    PyRep::Integer(c.members.len() as i64),
                    PyRep::Bool(c.is_delayed_local),
                ]).collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["channelID", "displayName", "motd", "memberCount", "isDelayedLocal"],
                rows,
            )))
        }

        (_, "JoinChannel") | (_, "JoinChannels") | (_, "CreateOrJoinChannel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let chan_id_owned = match args.first() {
                Some(PyRep::String(s)) => s.clone(),
                Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) => list
                    .first()
                    .and_then(|x| x.as_str())
                    .unwrap_or("local")
                    .to_string(),
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "JoinChannel requires channelID".into(),
                    }))
                }
            };
            let chan_id = chan_id_owned.as_str();
            if crate::services::pve_special_sites_abyssal::is_abyssal_local_channel_disabled(chan_id, session.solar_system_id) {
                return Some(Err(BridgeRpcError { code: "LocalChannelDisabledInAbyssalSpace".into(), message: "Local chat channel is disabled inside Abyssal Deadspace".into() }));
            }
            let password_arg = args.get(1).and_then(|a| a.as_str());
            let motd_arg = args.get(2).and_then(|a| a.as_str()).unwrap_or("");

            let is_delayed = is_delayed_local_channel(chan_id);
            let sys_id_opt = parse_channel_system_id(chan_id);
            let blocked_in_social_chan = is_character_blocked_in_social_extras(ctx, chan_id, char_id);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let chan_key = resolve_channel_key(snap, chan_id);
                validate_channel_join_auth(ctx, snap, session, char_id, char_id, &chan_key)?;
                let existing_owner = snap.chat_channels.get(&chan_key).and_then(|c| c.owner_id);
                if existing_owner != Some(char_id)
                    && (blocked_in_social_chan || is_character_blocked_in_acl(snap, &chan_key, char_id))
                {
                    return Err(BridgeRpcError {
                        code: "ChannelAccessDenied".into(),
                        message: "You are blocked from joining this channel".into(),
                    });
                }
                let is_allowed_explicitly = snap
                    .chat_channel_acl
                    .get(&chan_key)
                    .is_some_and(|a| a.allow_list.contains(&char_id));
                let caller_is_op = {
                    let existing = snap.chat_channels.get(&chan_key);
                    has_channel_operator_auth(snap, session, char_id, &chan_key, existing)
                };
                let chan = snap
                    .chat_channels
                    .entry(chan_key.clone())
                    .or_insert_with(|| {
                        build_initial_channel_record(
                            &chan_key,
                            char_id,
                            motd_arg,
                            password_arg,
                            is_delayed,
                            sys_id_opt,
                        )
                    });
                if is_system_or_org_channel(&chan_key) {
                    chan.owner_id = None;
                    chan.operators.clear();
                    chan.password = None;
                }
                if let Some(ref pw) = chan.password {
                    if !pw.is_empty()
                        && !caller_is_op
                        && chan.owner_id != Some(char_id)
                        && !chan.operators.contains(&char_id)
                        && !is_allowed_explicitly
                        && password_arg != Some(pw.as_str())
                    {
                        return Err(BridgeRpcError {
                            code: "InvalidChannelPassword".into(),
                            message: "Incorrect password for channel".into(),
                        });
                    }
                }
                chan.members.insert(char_id);
                Ok((
                    chan_key,
                    chan.name.clone(),
                    chan.motd.clone(),
                    chan.members.len(),
                    chan.is_delayed_local,
                    chan.owner_id,
                ))
            });

            match res {
                Ok((chan_key, name, motd, members_len, delayed, owner_id)) => {
                    let count = if delayed {
                        ctx.social_extras
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .delayed_local_speakers
                            .get(&chan_key)
                            .map(|s| s.len())
                            .unwrap_or(0)
                    } else {
                        members_len
                    };
                    if !delayed {
                        ctx.notify(
                            NotificationTarget::All,
                            "OnChannelJoined",
                            "channelid",
                            vec![PyRep::String(chan_key.clone()), PyRep::Integer(char_id)],
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("channelID", PyRep::String(chan_key)),
                        ("name", PyRep::String(name)),
                        ("motd", PyRep::String(motd)),
                        ("memberCount", PyRep::Integer(count as i64)),
                        ("isDelayedLocal", PyRep::Bool(delayed)),
                        ("ownerID", owner_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        (_, "LeaveChannel") | (_, "LeaveChannels") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "LeaveChannel requires channelID string".into() }));
            };
            let is_delayed = is_delayed_local_channel(chan_id);
            let candidate_speakers = if is_delayed {
                get_delayed_local_speakers(ctx, chan_id)
            } else {
                Vec::new()
            };
            let dropped_speakers = ctx.db.with_mut_snapshot(|snap| {
                let rem_members = if let Some(chan) = snap.chat_channels.get_mut(chan_id) {
                    chan.members.remove(&char_id);
                    chan.members.clone()
                } else {
                    HashSet::new()
                };
                if is_delayed {
                    collect_wh_local_witness_speakers_to_drop_in_snap(
                        snap,
                        chan_id,
                        char_id,
                        &rem_members,
                        &candidate_speakers,
                    )
                } else {
                    Vec::new()
                }
            });
            remove_delayed_local_speakers(ctx, chan_id, &dropped_speakers);
            if !is_delayed {
                ctx.notify(NotificationTarget::All, "OnChannelLeft", "channelid", vec![PyRep::String(chan_id.to_string()), PyRep::Integer(char_id)]);
            }
            Some(Ok(PyRep::keyval([("channelID", PyRep::String(chan_id.to_string())), ("left", PyRep::Bool(true))])))
        }

        (_, "SendMessage") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "SendMessage requires channelID string".into() }));
            };
            if crate::services::pve_special_sites_abyssal::is_abyssal_local_channel_disabled(chan_id, session.solar_system_id) {
                return Some(Err(BridgeRpcError { code: "LocalChannelDisabledInAbyssalSpace".into(), message: "Local chat channel is disabled inside Abyssal Deadspace".into() }));
            }
            let text = args.get(1).and_then(|a| a.as_str()).unwrap_or("o7");
            if text.is_empty() || text.chars().count() > 2_000 {
                return Some(Err(BridgeRpcError { code: "InvalidChatMessageLength".into(), message: "Chat message must be 1..=2000 characters".into() }));
            }
            if !crate::services::lookup_svc::is_word_allowed(text) {
                return Some(Err(BridgeRpcError { code: "UserError".into(), message: "ChatContentInvalidBannedWords".into() }));
            }
            let chan_key = ctx.db.with_snapshot(|snap| resolve_channel_key(snap, chan_id));
            let chan_lower = chan_key.to_ascii_lowercase();
            let now_unix = ctx.now_unix();
            if let Ok(cid_u32) = u32::try_from(char_id) {
                if let Some((_, reason)) = crate::services::quasar_social_chat::check_active_chat_mute(
                    &ctx.quasar_gateway.chat_moderation,
                    &ctx.db,
                    cid_u32,
                    now_unix,
                ) {
                    return Some(Err(BridgeRpcError { code: "CharacterChatBanned".into(), message: format!("Character is banned from chat: {reason}") }));
                }
            }
            if let Some(reason) = check_channel_or_global_mute(ctx, &chan_key, char_id, now_unix) {
                return Some(Err(BridgeRpcError {
                    code: "MutedInChannel".into(),
                    message: format!("You are muted in {chan_key}: {reason}"),
                }));
            }
            let blocked_in_social_chan = is_character_blocked_in_social_extras(ctx, &chan_key, char_id);
            let auth_res: Result<(String, Vec<i64>), BridgeRpcError> = ctx.db.with_snapshot(|snap| {
                let sender_name = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "Pilot".into());
                if !session.has_gm_role() {
                    validate_scoped_channel_sender(ctx, session, &chan_lower)?;
                    let chan = snap.chat_channels.get(&chan_key).ok_or_else(|| BridgeRpcError {
                        code: "ChannelAccessDenied".into(),
                        message: format!("Must join channel {chan_key} before sending messages"),
                    })?;
                    if chan.owner_id != Some(char_id)
                        && (blocked_in_social_chan || is_character_blocked_in_acl(snap, &chan_key, char_id))
                    {
                        return Err(BridgeRpcError {
                            code: "ChannelAccessDenied".into(),
                            message: "Blocked from channel".into(),
                        });
                    }
                    if !chan.members.contains(&char_id) {
                        return Err(BridgeRpcError {
                            code: "ChannelAccessDenied".into(),
                            message: format!("Must be a member of {chan_key} to send messages"),
                        });
                    }
                }
                let members = snap
                    .chat_channels
                    .get(&chan_key)
                    .map(|c| c.members.iter().copied().collect())
                    .unwrap_or_else(|| vec![char_id]);
                Ok((sender_name, members))
            });
            let (sender_name, members) = match auth_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let is_delayed = is_delayed_local_channel(&chan_key);
            {
                let mut s = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
                let entry = s.chat_rate_window.entry(char_id).or_insert((now_unix, 0));
                if entry.0 == now_unix {
                    entry.1 += 1;
                    if entry.1 > 20 {
                        return Some(Err(BridgeRpcError { code: "ChatRateLimitExceeded".into(), message: "Chat message rate limit exceeded".into() }));
                    }
                } else {
                    *entry = (now_unix, 1);
                }
                if is_delayed {
                    s.delayed_local_speakers.entry(chan_key.clone()).or_default().insert(char_id);
                }
            }
            if is_delayed {
                ctx.db.with_mut_snapshot(|snap| {
                    let cur_members = snap.chat_channels.get(&chan_key).map(|c| c.members.clone()).unwrap_or_default();
                    let wit_key = format!("{chan_key}:{char_id}");
                    let wit = snap
                        .fleet_and_social_ops
                        .wh_local_witnesses
                        .entry(wit_key)
                        .or_default();
                    wit.extend(cur_members.into_iter().filter(|&m| m != char_id));
                });
            }

            let now_ft = ctx.now_blueos_filetime_ticks();

            let notif_args = vec![
                PyRep::String(chan_key.clone()),
                PyRep::Integer(char_id),
                PyRep::String(sender_name),
                PyRep::String(text.to_string()),
                PyRep::Integer(now_ft),
            ];

            if let Some(rest) = chan_lower.strip_prefix("solarsystemid2:") {
                let sys_id: u32 = rest.parse().unwrap_or(0);
                ctx.notify(NotificationTarget::SolarSystem(sys_id), "OnClientEvent_ChatMessageSent", "channelid", notif_args);
            } else if let Some(rest) = chan_lower.strip_prefix("corpid:") {
                let corp_id: i64 = rest.parse().unwrap_or(0);
                ctx.notify(NotificationTarget::Corporation(corp_id), "OnClientEvent_ChatMessageSent", "channelid", notif_args);
            } else if let Some(rest) = chan_lower.strip_prefix("allianceid:") {
                let alliance_id: i64 = rest.parse().unwrap_or(0);
                ctx.notify(NotificationTarget::Alliance(alliance_id), "OnClientEvent_ChatMessageSent", "channelid", notif_args);
            } else if let Some(rest) = chan_lower.strip_prefix("fleetid:") {
                let fleet_id: i64 = rest.parse().unwrap_or(0);
                ctx.notify(NotificationTarget::Fleet(fleet_id), "OnClientEvent_ChatMessageSent", "channelid", notif_args);
            } else {
                for mid in members {
                    ctx.notify(
                        NotificationTarget::Character(mid),
                        "OnClientEvent_ChatMessageSent",
                        "channelid",
                        notif_args.clone(),
                    );
                }
            }

            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(chan_key)),
                ("sent", PyRep::Bool(true)),
                ("text", PyRep::String(text.to_string())),
            ])))
        }

        (_, "Invite") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(recipient_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Invite requires recipient characterID".into(),
                }));
            };
            let snap_cspa_charge = ctx
                .db
                .with_snapshot(|snap| snap.character_cspa_charges.get(&recipient_id).copied());
            let (esi_blocked, esi_friendly) = ctx
                .esi_state
                .char_contacts
                .read()
                .ok()
                .and_then(|g| g.get(&recipient_id).and_then(|l| l.iter().find(|e| e.contact_id == char_id)).map(|e| (e.is_blocked, e.standing > 0.0)))
                .unwrap_or((false, false));
            let (is_blocked, cspa_charge) = {
                let s = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
                let friendly = esi_friendly
                    || s.contacts
                        .get(&recipient_id)
                        .and_then(|m| m.get(&char_id))
                        .is_some_and(|&(st, _, _)| st > 0.0);
                let blocked = (esi_blocked
                    || s.blocked
                        .get(&recipient_id)
                        .is_some_and(|b| b.contains(&char_id)))
                    && !friendly;
                let charge = if friendly || recipient_id == char_id {
                    IskCents(0)
                } else {
                    snap_cspa_charge
                        .or_else(|| s.cspa_charges.get(&recipient_id).copied())
                        .unwrap_or(IskCents(0))
                };
                (blocked, charge)
            };
            if is_blocked {
                return Some(Err(BridgeRpcError {
                    code: "SenderBlockedByRecipient".into(),
                    message: "Recipient has blocked invitations from you".into(),
                }));
            }
            if cspa_charge.0 > 0 {
                let paid = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(ch) = snap.characters.get_mut(&char_id) else {
                        return Err(BridgeRpcError {
                            code: "CharacterNotFound".into(),
                            message: "Caller character not found".into(),
                        });
                    };
                    ch.balance = ch.balance.checked_sub(cspa_charge).ok_or_else(|| BridgeRpcError {
                        code: "InsufficientFundsForCSPA".into(),
                        message: "Insufficient ISK to pay recipient CSPA charge for chat invite".into(),
                    })?;
                    Ok(())
                });
                if let Err(e) = paid {
                    return Some(Err(e));
                }
            }
            let explicit_chan = args.get(1).and_then(|a| a.as_str());
            let chan_id = match crate::services::fleet_social_chat_acl_helpers::validate_and_apply_channel_invite(
                ctx, char_id, recipient_id, explicit_chan,
            ) {
                Ok(cid) => cid,
                Err(e) => return Some(Err(e)),
            };

            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnChannelJoined",
                "channelid",
                vec![PyRep::String(chan_id.clone()), PyRep::Integer(char_id)],
            );
            ctx.notify(
                NotificationTarget::Character(recipient_id),
                "OnChannelJoined",
                "channelid",
                vec![PyRep::String(chan_id.clone()), PyRep::Integer(recipient_id)],
            );

            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(chan_id)),
                ("recipientID", PyRep::Integer(recipient_id)),
                ("invited", PyRep::Bool(true)),
            ])))
        }

        (_, "GetMembers") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetMembers requires channelID".into(),
                }));
            };
            let (chan, acl) = match crate::services::fleet_social_chat_acl_helpers::authorize_and_fetch_channel_members(
                ctx, session, char_id, chan_id,
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let visible_members: Vec<i64> = if chan.is_delayed_local {
                ctx.social_extras
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .delayed_local_speakers
                    .get(chan_id)
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default()
            } else {
                chan.members.iter().copied().collect()
            };
            let rows: Vec<Vec<PyRep>> = visible_members
                .into_iter()
                .map(|m| vec![PyRep::Integer(m), PyRep::String(chan_id.to_string())])
                .collect();
            let mut ops: Vec<i64> = chan.operators.iter().copied().collect();
            ops.sort_unstable();
            let mut allowed: Vec<i64> = acl.allow_list.iter().copied().collect();
            allowed.sort_unstable();
            let mut denied: Vec<i64> = acl.deny_list.iter().copied().collect();
            denied.sort_unstable();
            let mut muted: Vec<i64> = acl.muted_until.keys().copied().collect();
            muted.sort_unstable();
            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(chan.channel_id)),
                ("name", PyRep::String(chan.name.clone())),
                ("displayName", PyRep::String(chan.name)),
                ("motd", PyRep::String(chan.motd)),
                ("isDelayedLocal", PyRep::Bool(chan.is_delayed_local)),
                (
                    "passwordRequired",
                    PyRep::Bool(chan.password.as_ref().is_some_and(|p| !p.is_empty())),
                ),
                (
                    "ownerID",
                    chan.owner_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                (
                    "operators",
                    PyRep::List(ops.into_iter().map(PyRep::Integer).collect()),
                ),
                (
                    "allowList",
                    PyRep::List(allowed.into_iter().map(PyRep::Integer).collect()),
                ),
                (
                    "denyList",
                    PyRep::List(denied.into_iter().map(PyRep::Integer).collect()),
                ),
                (
                    "mutedList",
                    PyRep::List(muted.into_iter().map(PyRep::Integer).collect()),
                ),
                (
                    "members",
                    PyRep::rowset(vec!["characterID", "channelID"], rows),
                ),
            ])))
        }

        (_, "AccessControl")
        | (_, "RenameChannel")
        | (_, "SetChannelMOTD")
        | (_, "SetChannelPassword")
        | (_, "SetChannelLanguageRestriction")
        | (_, "DestroyChannel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Channel operation requires channelID".into(),
                }));
            };
            if method == "DestroyChannel" {
                if is_system_or_org_channel(chan_id) {
                    return Some(Err(BridgeRpcError {
                        code: "NotChannelOwner".into(),
                        message: "System and organization channels cannot be destroyed".into(),
                    }));
                }
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(chan_key) = find_existing_channel_key(snap, chan_id) else {
                        return Err(BridgeRpcError {
                            code: "ChannelNotFound".into(),
                            message: format!("Channel {chan_id} not found"),
                        });
                    };
                    let Some(chan) = snap.chat_channels.get(&chan_key) else {
                        return Err(BridgeRpcError {
                            code: "ChannelNotFound".into(),
                            message: format!("Channel {chan_id} not found"),
                        });
                    };
                    if !has_channel_owner_auth(session, char_id, &chan_key, chan) {
                        return Err(BridgeRpcError {
                            code: "NotChannelOwner".into(),
                            message: "Only channel owner can destroy the channel".into(),
                        });
                    }
                    snap.chat_channels.remove(&chan_key);
                    snap.chat_channel_acl.remove(&chan_key);
                    Ok(chan_key)
                });
                return match res {
                    Ok(chan_key) => {
                        ctx.notify(
                            NotificationTarget::All,
                            "OnChannelDeleted",
                            "channelid",
                            vec![PyRep::String(chan_key)],
                        );
                        Some(Ok(PyRep::Bool(true)))
                    }
                    Err(e) => Some(Err(e)),
                };
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let chan_key = find_existing_channel_key(snap, chan_id)
                    .unwrap_or_else(|| chan_id.to_lowercase());
                if is_system_or_org_channel(&chan_key)
                    && matches!(method, "RenameChannel" | "SetChannelPassword")
                {
                    return Err(BridgeRpcError {
                        code: "NotChannelOwner".into(),
                        message: "System and organization channels cannot be renamed or password-locked".into(),
                    });
                }
                let can_op = {
                    let chan_ref = snap.chat_channels.get(&chan_key);
                    has_channel_operator_auth(snap, session, char_id, &chan_key, chan_ref)
                };
                let Some(chan) = snap.chat_channels.get_mut(&chan_key) else {
                    return Err(BridgeRpcError {
                        code: "ChannelNotFound".into(),
                        message: format!("Channel {chan_id} not found"),
                    });
                };
                if !can_op {
                    return Err(BridgeRpcError {
                        code: "NotChannelOperator".into(),
                        message: "Requires channel owner or operator privileges".into(),
                    });
                }
                let mut social_op = PostAccessControlSocialOp::None;
                match method {
                    "RenameChannel" => {
                        if let Some(new_name) = args.get(1).and_then(|a| a.as_str()) {
                            chan.name = new_name.to_string();
                        }
                    }
                    "SetChannelMOTD" => {
                        if let Some(new_motd) = args.get(1).and_then(|a| a.as_str()) {
                            chan.motd = new_motd.to_string();
                        }
                    }
                    "SetChannelPassword" => {
                        chan.password = args
                            .get(1)
                            .and_then(|a| a.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string());
                    }
                    "AccessControl" => {
                        let target_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
                        let mode = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
                        let caller_is_owner_or_gm =
                            has_channel_owner_auth(session, char_id, &chan_key, chan);
                        let targets_owner = chan.owner_id == Some(target_id);
                        match mode {
                            3 => {
                                if !caller_is_owner_or_gm {
                                    return Err(BridgeRpcError {
                                        code: "NotChannelOwner".into(),
                                        message: "Only the channel owner can transfer ownership"
                                            .into(),
                                    });
                                }
                                chan.owner_id = Some(target_id);
                                chan.operators.insert(target_id);
                                chan.members.insert(target_id);
                                clear_channel_acl_restrictions(&mut snap.chat_channel_acl, &chan_key, target_id);
                                social_op = PostAccessControlSocialOp::ClearRestrictions(target_id);
                            }
                            2 => {
                                if is_system_or_org_channel(&chan_key) || !caller_is_owner_or_gm {
                                    return Err(BridgeRpcError {
                                        code: "NotChannelOwner".into(),
                                        message: "Only the channel owner or a GM can grant operator rights".into(),
                                    });
                                }
                                chan.operators.insert(target_id);
                                let acl = snap.chat_channel_acl.entry(chan_key.clone()).or_default();
                                acl.allow_list.insert(target_id);
                                acl.deny_list.remove(&target_id);
                            }
                            1 => {
                                let acl = snap.chat_channel_acl.entry(chan_key.clone()).or_default();
                                acl.allow_list.insert(target_id);
                                acl.deny_list.remove(&target_id);
                                acl.muted_until.remove(&target_id);
                                social_op = PostAccessControlSocialOp::AllowAndUnmute(target_id);
                            }
                            -1 => {
                                if targets_owner && !session.has_gm_role() {
                                    return Err(BridgeRpcError {
                                        code: "NotChannelOwner".into(),
                                        message: "Channel owner cannot be muted by a non-GM operator".into(),
                                    });
                                }
                                let until = ctx.now_unix() + 3600;
                                snap.chat_channel_acl
                                    .entry(chan_key.clone())
                                    .or_default()
                                    .muted_until
                                    .insert(target_id, (until, "Operator mute".into()));
                                social_op = PostAccessControlSocialOp::Mute(target_id, until);
                            }
                            -2 => {
                                if targets_owner {
                                    return Err(BridgeRpcError {
                                        code: "NotChannelOwner".into(),
                                        message: "Channel owner cannot be kicked or banned".into(),
                                    });
                                }
                                if chan.operators.contains(&target_id) && !caller_is_owner_or_gm {
                                    return Err(BridgeRpcError {
                                        code: "NotChannelOwner".into(),
                                        message: "Only the channel owner can demote or ban an operator".into(),
                                    });
                                }
                                chan.members.remove(&target_id);
                                chan.operators.remove(&target_id);
                                let acl = snap.chat_channel_acl.entry(chan_key.clone()).or_default();
                                acl.deny_list.insert(target_id);
                                acl.allow_list.remove(&target_id);
                                social_op = PostAccessControlSocialOp::Block(target_id);
                            }
                            _ => {
                                if targets_owner && !session.has_gm_role() {
                                    return Err(BridgeRpcError {
                                        code: "NotChannelOwner".into(),
                                        message: "Channel owner ACL cannot be modified by a non-GM operator".into(),
                                    });
                                }
                                if chan.operators.contains(&target_id) && !caller_is_owner_or_gm {
                                    return Err(BridgeRpcError {
                                        code: "NotChannelOwner".into(),
                                        message: "Only the channel owner can modify operator ACL".into(),
                                    });
                                }
                                if let Some(acl) = snap.chat_channel_acl.get_mut(&chan_key) {
                                    acl.allow_list.remove(&target_id);
                                    acl.deny_list.remove(&target_id);
                                    acl.muted_until.remove(&target_id);
                                }
                                social_op = PostAccessControlSocialOp::AllowAndUnmute(target_id);
                            }
                        }
                    }
                    _ => {}
                }
                Ok((chan_key, chan.name.clone(), chan.motd.clone(), chan.owner_id, social_op))
            });

            match res {
                Ok((chan_key, name, motd, owner_id, social_op)) => {
                    apply_post_access_control_social_op(ctx, &chan_key, social_op);
                    Some(Ok(PyRep::keyval([
                        ("channelID", PyRep::String(chan_key)),
                        ("name", PyRep::String(name)),
                        ("motd", PyRep::String(motd)),
                        (
                            "ownerID",
                            owner_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        ),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}

fn is_delayed_local_channel(chan_id: &str) -> bool {
    let Some(sys_id) = parse_channel_system_id(chan_id) else {
        return false;
    };
    sys_id >= 31_000_000
        || (30_000_140..=30_000_141).contains(&sys_id)
        || sys_id == 30_002_086
        || sys_id == 30_100_000
        || crate::services::wormhole_lifecycle_ops::system_wormhole_profile(sys_id).0 > 0
}

fn find_existing_channel_key(snap: &eve_db::WorldStateSnapshot, chan_id: &str) -> Option<String> {
    if snap.chat_channels.contains_key(chan_id) {
        return Some(chan_id.to_string());
    }
    if let Some(found) = snap
        .chat_channels
        .keys()
        .find(|k| k.eq_ignore_ascii_case(chan_id))
    {
        return Some(found.clone());
    }
    let localpart = chan_id.split('@').next().unwrap_or(chan_id);
    if let Some(slug) = localpart
        .strip_prefix("player_")
        .or_else(|| localpart.strip_prefix("PLAYER_"))
    {
        let candidate = format!("channel:{slug}");
        if let Some(found) = snap
            .chat_channels
            .keys()
            .find(|k| k.eq_ignore_ascii_case(&candidate))
        {
            return Some(found.clone());
        }
    }
    None
}

fn resolve_channel_key(snap: &eve_db::WorldStateSnapshot, chan_id: &str) -> String {
    find_existing_channel_key(snap, chan_id).unwrap_or_else(|| chan_id.to_string())
}

enum PostAccessControlSocialOp {
    None,
    ClearRestrictions(i64),
    AllowAndUnmute(i64),
    Mute(i64, u64),
    Block(i64),
}

fn apply_post_access_control_social_op(
    ctx: &ServerContext,
    chan_key: &str,
    social_op: PostAccessControlSocialOp,
) {
    match social_op {
        PostAccessControlSocialOp::None => {}
        PostAccessControlSocialOp::ClearRestrictions(target_id) => {
            clear_social_extras_channel_restrictions(ctx, chan_key, target_id);
        }
        PostAccessControlSocialOp::AllowAndUnmute(target_id) => {
            let mut s = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
            s.blocked_in_channel
                .entry(chan_key.to_string())
                .or_default()
                .remove(&target_id);
            s.muted_in_channel.remove(&(chan_key.to_string(), target_id));
        }
        PostAccessControlSocialOp::Mute(target_id, until) => {
            ctx.social_extras
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .muted_in_channel
                .insert((chan_key.to_string(), target_id), (until, "Operator mute".into()));
        }
        PostAccessControlSocialOp::Block(target_id) => {
            ctx.social_extras
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .blocked_in_channel
                .entry(chan_key.to_string())
                .or_default()
                .insert(target_id);
        }
    }
}

fn is_character_blocked_in_social_extras(
    ctx: &ServerContext,
    chan_key: &str,
    target_id: i64,
) -> bool {
    let s = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
    s.blocked_in_channel
        .iter()
        .any(|(k, blocked)| k.eq_ignore_ascii_case(chan_key) && blocked.contains(&target_id))
}

fn is_character_blocked_in_acl(
    snap: &eve_db::WorldStateSnapshot,
    chan_key: &str,
    target_id: i64,
) -> bool {
    snap.chat_channel_acl
        .iter()
        .any(|(k, acl)| k.eq_ignore_ascii_case(chan_key) && acl.deny_list.contains(&target_id))
}

fn clear_channel_acl_restrictions(
    chat_channel_acl: &mut HashMap<String, eve_db::ChatChannelAclRecord>,
    chan_key: &str,
    target_id: i64,
) {
    for (k, acl) in chat_channel_acl.iter_mut() {
        if k.eq_ignore_ascii_case(chan_key) {
            acl.deny_list.remove(&target_id);
            acl.muted_until.remove(&target_id);
        }
    }
}

fn clear_social_extras_channel_restrictions(
    ctx: &ServerContext,
    chan_key: &str,
    target_id: i64,
) {
    let mut s = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
    for (k, blocked) in s.blocked_in_channel.iter_mut() {
        if k.eq_ignore_ascii_case(chan_key) {
            blocked.remove(&target_id);
        }
    }
    s.muted_in_channel
        .retain(|(k, cid), _| !(*cid == target_id && k.eq_ignore_ascii_case(chan_key)));
}

fn validate_scoped_channel_sender(
    ctx: &ServerContext,
    session: &SessionState,
    chan_lower: &str,
) -> Result<(), BridgeRpcError> {
    if let Some(rest) = chan_lower.strip_prefix("solarsystemid2:") {
        let sys_id: u32 = rest.parse().unwrap_or(0);
        if sys_id == 0 || session.solar_system_id != Some(sys_id) {
            return Err(BridgeRpcError {
                code: "ChannelAccessDenied".into(),
                message: "Sender is not in the target solar system".into(),
            });
        }
    } else if let Some(rest) = chan_lower.strip_prefix("resourcewars:") {
        let sys_id: u32 = rest.parse().unwrap_or(0);
        if sys_id == 0 || session.solar_system_id != Some(sys_id) {
            return Err(BridgeRpcError {
                code: "ChannelAccessDenied".into(),
                message: "Sender is not in the target solar system".into(),
            });
        }
    } else if let Some(rest) = chan_lower.strip_prefix("constellationid:") {
        let cid: u32 = rest.parse().unwrap_or(0);
        let session_cid = session.constellation_id.or_else(|| {
            session
                .solar_system_id
                .and_then(|sid| ctx.sde.solar_systems.get(&sid).map(|s| s.constellation_id))
        });
        if cid == 0 || session_cid != Some(cid) {
            return Err(BridgeRpcError {
                code: "ChannelAccessDenied".into(),
                message: "Sender is not in the target constellation".into(),
            });
        }
    } else if let Some(rest) = chan_lower.strip_prefix("regionid:") {
        let rid: u32 = rest.parse().unwrap_or(0);
        let session_rid = session.region_id.or_else(|| {
            session
                .solar_system_id
                .and_then(|sid| ctx.sde.solar_systems.get(&sid).map(|s| s.region_id))
        });
        if rid == 0 || session_rid != Some(rid) {
            return Err(BridgeRpcError {
                code: "ChannelAccessDenied".into(),
                message: "Sender is not in the target region".into(),
            });
        }
    } else if let Some(rest) = chan_lower.strip_prefix("factionid:") {
        let fid: i64 = rest.parse().unwrap_or(0);
        if fid == 0 || session.war_faction_id != Some(fid) {
            return Err(BridgeRpcError {
                code: "ChannelAccessDenied".into(),
                message: "Sender is not enlisted in the target faction".into(),
            });
        }
    } else if let Some(rest) = chan_lower.strip_prefix("corpid:") {
        let corp_id: i64 = rest.parse().unwrap_or(0);
        if corp_id == 0 || session.corporation_id != Some(corp_id) {
            return Err(BridgeRpcError {
                code: "ChannelAccessDenied".into(),
                message: "Sender is not in the target corporation".into(),
            });
        }
    } else if let Some(rest) = chan_lower.strip_prefix("allianceid:") {
        let alliance_id: i64 = rest.parse().unwrap_or(0);
        if alliance_id == 0 || session.alliance_id != Some(alliance_id) {
            return Err(BridgeRpcError {
                code: "ChannelAccessDenied".into(),
                message: "Sender is not in the target alliance".into(),
            });
        }
    } else if let Some(rest) = chan_lower.strip_prefix("fleetid:") {
        let fleet_id: i64 = rest.parse().unwrap_or(0);
        if fleet_id == 0 || session.fleet_id != Some(fleet_id) {
            return Err(BridgeRpcError {
                code: "ChannelAccessDenied".into(),
                message: "Sender is not in the target fleet".into(),
            });
        }
    }
    Ok(())
}

fn parse_channel_system_id(chan_id: &str) -> Option<u32> {
    let lower = chan_id.to_ascii_lowercase();
    lower
        .strip_prefix("solarsystemid2:")
        .and_then(|s| s.parse().ok())
}

pub(super) fn extract_id_list(arg: Option<&PyRep>) -> Vec<i64> {
    let mut ids = Vec::new();
    match arg {
        Some(PyRep::List(list)) | Some(PyRep::Tuple(list)) => {
            for item in list {
                if let Some(id) = item.as_i64() {
                    ids.push(id);
                }
            }
        }
        Some(rep) => {
            if let Some(id) = rep.as_i64() {
                ids.push(id);
            }
        }
        None => {}
    }
    ids
}

fn generate_xmpp_auth_token() -> String {
    let mut bytes = [0u8; 32];
    if let Err(err) = getrandom::getrandom(&mut bytes) {
        panic!("OS CSPRNG unavailable for XMPP token generation: {err}");
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.len() != b_bytes.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a_bytes.iter().zip(b_bytes.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

pub fn mint_or_rotate_xmpp_auth_token(ctx: &ServerContext, char_id: i64) -> String {
    let token = generate_xmpp_auth_token();
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .remove(&format!("xmpp:token:{char_id}"));
        let _ = snap
            .kv_mut(eve_db::Ns::XmppAuthToken)
            .set_with_unix(char_id, "", &token, now_unix);
    });
    token
}

pub fn ensure_xmpp_auth_token(ctx: &ServerContext, char_id: i64) -> String {
    let key = format!("xmpp:token:{char_id}");
    if let Some(existing) = ctx.db.with_snapshot(|snap| {
        snap.kv(eve_db::Ns::XmppAuthToken)
            .get::<String>(char_id, "")
            .filter(|t| !t.is_empty())
            .or_else(|| {
                snap.service_state_blobs
                    .get(&key)
                    .filter(|t| !t.is_empty())
                    .cloned()
            })
    }) {
        return existing;
    }
    let fresh = generate_xmpp_auth_token();
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(existing) = snap
            .kv(eve_db::Ns::XmppAuthToken)
            .get::<String>(char_id, "")
            .filter(|t| !t.is_empty())
            .or_else(|| {
                snap.service_state_blobs
                    .get(&key)
                    .filter(|t| !t.is_empty())
                    .cloned()
            })
        {
            return existing;
        }
        let _ = snap
            .kv_mut(eve_db::Ns::XmppAuthToken)
            .set_with_unix(char_id, "", &fresh, now_unix);
        fresh.clone()
    })
}

pub fn revoke_xmpp_auth_token(ctx: &ServerContext, char_id: i64) {
    let key = format!("xmpp:token:{char_id}");
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.remove(&key);
        snap.kv_mut(eve_db::Ns::XmppAuthToken)
            .remove(char_id, "");
    });
}

fn handle_get_chat_authentication_token(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let token = ensure_xmpp_auth_token(ctx, char_id);
    Some(Ok(PyRep::String(token)))
}

fn handle_get_help_channel(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let _ = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let lang = args
        .first()
        .and_then(|a| a.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(&session.language_id)
        .trim()
        .to_ascii_lowercase();
    let default_chan = match lang.as_str() {
        "de" => "help_de",
        "fr" => "help_fr",
        "ru" => "help_ru",
        "ja" => "help_ja",
        "zh" => "help_zh",
        "ko" => "help_ko",
        "es" => "help_es",
        _ => "help_en",
    };
    let chan = ctx.db.with_snapshot(|snap| {
        snap.kv(eve_db::Ns::XmppHelpChannel)
            .get::<String>(0, &lang)
            .or_else(|| {
                snap.service_state_blobs
                    .get(&format!("xmpp:help_channel:{lang}"))
                    .cloned()
            })
            .unwrap_or_else(|| default_chan.to_string())
    });
    Some(Ok(PyRep::String(chan)))
}

fn handle_xmpp_hostname(ctx: &ServerContext) -> Option<Result<PyRep, BridgeRpcError>> {
    let host = ctx.db.with_snapshot(|snap| {
        snap.kv(eve_db::Ns::XmppHostname)
            .get::<String>(0, "")
            .or_else(|| snap.service_state_blobs.get("xmpp:hostname").cloned())
            .unwrap_or_else(|| "chat.evomu.local".to_string())
    });
    Some(Ok(PyRep::String(host)))
}

fn extract_xml_attr<'a>(xml: &'a str, attr: &str) -> Option<&'a str> {
    let single_pat = format!("{attr}='");
    if let Some(pos) = xml.find(&single_pat) {
        let rest = &xml[pos + single_pat.len()..];
        if let Some(end) = rest.find('\'') {
            return Some(&rest[..end]);
        }
    }
    let double_pat = format!("{attr}=\"");
    if let Some(pos) = xml.find(&double_pat) {
        let rest = &xml[pos + double_pat.len()..];
        if let Some(end) = rest.find('"') {
            return Some(&rest[..end]);
        }
    }
    None
}

fn escape_xml_attr(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn unescape_xml_attr(raw: &str) -> String {
    raw.replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Per-connection XMPP authentication & resource-bind identity tracked by the XMPP TCP loop (`#940`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XmppStreamAuth {
    pub user_id: Option<u64>,
    pub character_id: Option<i64>,
    pub bound_jid: Option<String>,
    pub sasl_authenticated: bool,
}

fn xmpp_not_authorized_iq(stanza_id: &str) -> String {
    let esc_id = escape_xml_attr(stanza_id);
    format!(
        "<iq type='error' id='{esc_id}'><error type='auth'><not-authorized xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></error></iq>"
    )
}

fn xmpp_item_not_found_iq(stanza_id: &str) -> String {
    let esc_id = escape_xml_attr(stanza_id);
    format!(
        "<iq type='error' id='{esc_id}'><error type='cancel'><item-not-found xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></error></iq>"
    )
}

fn decode_xmpp_base64(input: &str) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if clean.is_empty() || clean.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity((clean.len() / 4) * 3);
    let chunks = clean.chunks_exact(4);
    let total_chunks = chunks.len();
    for (idx, chunk) in chunks.enumerate() {
        let v0 = val(chunk[0])?;
        let v1 = val(chunk[1])?;
        let is_last = idx + 1 == total_chunks;
        if chunk[2] == b'=' {
            if !is_last || chunk[3] != b'=' {
                return None;
            }
            out.push((v0 << 2) | (v1 >> 4));
        } else if chunk[3] == b'=' {
            if !is_last {
                return None;
            }
            let v2 = val(chunk[2])?;
            out.push((v0 << 2) | (v1 >> 4));
            out.push(((v1 & 0x0f) << 4) | (v2 >> 2));
        } else {
            let v2 = val(chunk[2])?;
            let v3 = val(chunk[3])?;
            out.push((v0 << 2) | (v1 >> 4));
            out.push(((v1 & 0x0f) << 4) | (v2 >> 2));
            out.push(((v2 & 0x03) << 6) | v3);
        }
    }
    Some(out)
}

fn extract_xml_elem_text(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let mut offset = 0usize;
    while let Some(rel) = xml[offset..].find(&open) {
        let start = offset + rel;
        let after = start + open.len();
        let next = xml.as_bytes().get(after).copied();
        if !matches!(
            next,
            Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n')
        ) {
            offset = after;
            continue;
        }
        let mut in_quote: Option<u8> = None;
        let bytes = xml.as_bytes();
        let mut gt = None;
        let mut i = after;
        while i < bytes.len() {
            let b = bytes[i];
            if let Some(q) = in_quote {
                if b == q {
                    in_quote = None;
                }
            } else if b == b'\'' || b == b'"' {
                in_quote = Some(b);
            } else if b == b'>' {
                gt = Some(i);
                break;
            }
            i += 1;
        }
        let gt_idx = gt?;
        if gt_idx > after && bytes[gt_idx - 1] == b'/' {
            return None;
        }
        let close = format!("</{tag}>");
        let end_rel = xml[gt_idx + 1..].find(&close)?;
        return Some(unescape_xml_attr(xml[gt_idx + 1..gt_idx + 1 + end_rel].trim()));
    }
    None
}

fn parse_xmpp_char_id(raw: &str) -> Option<i64> {
    let local = raw.split('@').next().unwrap_or(raw).trim();
    let stripped = local
        .strip_prefix("char_")
        .or_else(|| local.strip_prefix("CHAR_"))
        .unwrap_or(local);
    stripped.parse::<i64>().ok().filter(|&cid| cid > 0)
}

/// Verifies SASL PLAIN `<auth>` credentials against server-issued CSPRNG character XMPP tokens
/// (`xmpp:token:{cid}`) or `AccountRecord` credentials (`ctx.db.authenticate_at`) and records the
/// authenticated `(user_id, character_id)` on `auth` (`#940`). Fails closed on invalid/banned/revoked auth.
pub fn xmpp_authenticate_sasl_plain(
    ctx: &ServerContext,
    stanza: &str,
    auth: &mut XmppStreamAuth,
) -> Result<(), &'static str> {
    auth.user_id = None;
    auth.character_id = None;
    auth.bound_jid = None;
    auth.sasl_authenticated = false;

    if let Some(mech) = extract_xml_attr(stanza, "mechanism") {
        if !mech.eq_ignore_ascii_case("PLAIN") {
            return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><invalid-mechanism/></failure>");
        }
    }
    let Some(b64_text) = extract_xml_elem_text(stanza, "auth") else {
        return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
    };
    let Some(raw) = decode_xmpp_base64(&b64_text) else {
        return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
    };
    let parts: Vec<&[u8]> = raw.split(|&b| b == b'\0').collect();
    let (authzid_b, authcid_b, passwd_b) = match parts.as_slice() {
        [z, c, p] => (*z, *c, *p),
        [c, p] => (&[] as &[u8], *c, *p),
        _ => return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>"),
    };
    let Ok(authzid) = std::str::from_utf8(authzid_b) else {
        return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
    };
    let Ok(authcid) = std::str::from_utf8(authcid_b) else {
        return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
    };
    let Ok(passwd) = std::str::from_utf8(passwd_b) else {
        return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
    };
    if authcid.trim().is_empty() || passwd.is_empty() {
        return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
    }

    let now_unix = ctx.now_unix();
    if let Some(cid) = parse_xmpp_char_id(authcid) {
        if !authzid.trim().is_empty() && parse_xmpp_char_id(authzid) != Some(cid) {
            return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
        }
        let lookup = ctx.db.with_snapshot(|snap| {
            let ch = snap.characters.get(&cid)?;
            let acc = snap.accounts.values().find(|a| a.user_id == ch.user_id);
            let restricted = acc.is_some_and(|a| a.check_active_restriction(now_unix).is_err());
            let custom_token = snap
                .kv(eve_db::Ns::XmppAuthToken)
                .get::<String>(cid, "")
                .filter(|t| !t.is_empty())
                .or_else(|| {
                    snap.service_state_blobs
                        .get(&format!("xmpp:token:{cid}"))
                        .cloned()
                        .filter(|t| !t.is_empty())
                });
            Some((ch.user_id, restricted, custom_token))
        });
        let Some((uid, restricted, custom_token)) = lookup else {
            return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
        };
        if restricted {
            return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
        }
        let token_ok = custom_token
            .as_deref()
            .is_some_and(|expected_tok| constant_time_eq(passwd, expected_tok));
        if token_ok {
            auth.user_id = Some(uid);
            auth.character_id = Some(cid);
            auth.sasl_authenticated = true;
            return Ok(());
        }
        return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
    }

    let uname = authcid.split('@').next().unwrap_or(authcid).trim();
    let Ok(acc) = ctx.db.authenticate_at(uname, passwd, now_unix) else {
        return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
    };
    let explicit_cid = parse_xmpp_char_id(authzid);
    let resolved_cid = ctx.db.with_snapshot(|snap| {
        if let Some(cid) = explicit_cid {
            if snap
                .characters
                .get(&cid)
                .is_some_and(|c| c.user_id == acc.user_id)
            {
                return Some(cid);
            }
            return None;
        }
        snap.sessions
            .get(&acc.user_id)
            .and_then(|s| s.character_id)
            .filter(|cid| snap.characters.get(cid).is_some_and(|c| c.user_id == acc.user_id))
            .or_else(|| {
                snap.characters
                    .values()
                    .filter(|c| c.user_id == acc.user_id)
                    .map(|c| c.character_id)
                    .min()
            })
    });
    let Some(cid) = resolved_cid else {
        return Err("<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>");
    };
    auth.user_id = Some(acc.user_id);
    auth.character_id = Some(cid);
    auth.sasl_authenticated = true;
    Ok(())
}

fn xmpp_social_block_snapshot(
    ctx: &ServerContext,
    caller_cid: i64,
) -> (HashSet<String>, HashSet<i64>) {
    let (blocked_in_channels, mut owners_blocking_caller) = ctx
        .social_extras
        .lock()
        .map(|se| {
            let chans: HashSet<String> = se
                .blocked_in_channel
                .iter()
                .filter_map(|(chan_key, blocked_set)| {
                    blocked_set.contains(&caller_cid).then(|| chan_key.clone())
                })
                .collect();
            let owners: HashSet<i64> = se
                .blocked
                .iter()
                .filter_map(|(&owner_cid, blocked_set)| {
                    blocked_set.contains(&caller_cid).then_some(owner_cid)
                })
                .collect();
            (chans, owners)
        })
        .unwrap_or_default();
    if let Ok(esi_contacts) = ctx.esi_state.char_contacts.read() {
        for (&owner_cid, list) in esi_contacts.iter() {
            if list
                .iter()
                .any(|c| c.contact_id == caller_cid && c.is_blocked)
            {
                owners_blocking_caller.insert(owner_cid);
            }
        }
    }
    (blocked_in_channels, owners_blocking_caller)
}

fn xmpp_channel_visibility_for_caller(
    snap: &eve_db::WorldStateSnapshot,
    chan_key: &str,
    chan: &ChatChannelRecord,
    caller_cid: i64,
    blocked_in_channels: &HashSet<String>,
    owners_blocking_caller: &HashSet<i64>,
) -> (bool, bool, bool) {
    let is_owner = chan.owner_id == Some(caller_cid);
    let is_op = chan.operators.contains(&caller_cid);
    let is_member = chan.members.contains(&caller_cid);
    let acl_opt = snap.chat_channel_acl.get(chan_key);
    let is_allowed = acl_opt.is_some_and(|a| a.allow_list.contains(&caller_cid));
    let is_denied_acl = !is_owner && acl_opt.is_some_and(|a| a.deny_list.contains(&caller_cid));
    let is_blocked_in_chan = !is_owner && blocked_in_channels.contains(chan_key);
    let is_blocked_owner = !is_owner
        && chan.owner_id.is_some_and(|oid| {
            owners_blocking_caller.contains(&oid)
                || snap
                    .char_contacts
                    .get(&oid)
                    .is_some_and(|list| list.iter().any(|c| c.contact_id == caller_cid && c.is_blocked))
        });
    let is_denied = is_denied_acl || is_blocked_in_chan || is_blocked_owner;
    let has_membership = is_owner || is_op || is_member || is_allowed;
    let has_password = chan.password.as_deref().is_some_and(|p| !p.is_empty());
    let can_see_forme = !is_denied && has_membership;
    let can_see_public_or_search =
        !is_denied && (has_membership || (!chan.is_private && !has_password));
    (can_see_forme, can_see_public_or_search, is_owner)
}

fn xmpp_authenticated_active_caller(
    ctx: &ServerContext,
    auth: Option<&XmppStreamAuth>,
) -> Option<(Option<u64>, i64)> {
    let a = auth?;
    if !a.sasl_authenticated || a.bound_jid.is_none() {
        return None;
    }
    let cid = a.character_id?;
    let now_unix = ctx.now_unix();
    let allowed = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&cid);
        let target_uid = a.user_id.or_else(|| ch.map(|c| c.user_id));
        let restricted = target_uid.is_some_and(|uid| {
            snap.accounts
                .values()
                .find(|acc| acc.user_id == uid)
                .is_some_and(|acc| acc.check_active_restriction(now_unix).is_err())
        });
        !restricted
    });
    allowed.then_some((a.user_id, cid))
}

fn xmpp_opening_tag_end(xml: &str) -> Option<usize> {
    let bytes = xml.as_bytes();
    let mut in_quote: Option<u8> = None;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if let Some(q) = in_quote {
            if b == q {
                in_quote = None;
            }
        } else if b == b'\'' || b == b'"' {
            in_quote = Some(b);
        } else if b == b'>' {
            return Some(i);
        }
        i += 1;
    }
    None
}

fn xmpp_is_service_jid(target_ref: &str) -> bool {
    let trimmed = target_ref.trim();
    !trimmed.contains('@')
        && (trimmed.is_empty()
            || trimmed.eq_ignore_ascii_case("conference.chat.evomu.local")
            || trimmed.eq_ignore_ascii_case("chat.evomu.local"))
}

fn xmpp_alias_to_channel_key(localpart: &str) -> Option<String> {
    let lower = localpart.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("local_") {
        if let Ok(id) = rest.parse::<u32>() {
            return Some(format!("solarsystemid2:{id}"));
        }
    } else if let Some(rest) = lower.strip_prefix("corp_") {
        if let Ok(id) = rest.parse::<i64>() {
            return Some(format!("corpid:{id}"));
        }
    } else if let Some(rest) = lower.strip_prefix("alliance_") {
        if let Ok(id) = rest.parse::<i64>() {
            return Some(format!("allianceid:{id}"));
        }
    } else if let Some(rest) = lower.strip_prefix("fleet_") {
        if let Ok(id) = rest.parse::<i64>() {
            return Some(format!("fleetid:{id}"));
        }
    } else if let Some(rest) = lower.strip_prefix("faction_") {
        if let Ok(id) = rest.parse::<i64>() {
            return Some(format!("factionid:{id}"));
        }
    } else if let Some(rest) = lower.strip_prefix("constellation_") {
        if let Ok(id) = rest.parse::<u32>() {
            return Some(format!("constellationid:{id}"));
        }
    } else if let Some(rest) = lower.strip_prefix("region_") {
        if let Ok(id) = rest.parse::<u32>() {
            return Some(format!("regionid:{id}"));
        }
    }
    None
}

fn xmpp_scoped_channel_allowed(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    caller_cid: i64,
    target_ref: &str,
    blocked_in_channels: &HashSet<String>,
    owners_blocking_caller: &HashSet<i64>,
    allow_service_jid: bool,
) -> Result<Option<String>, ()> {
    let trimmed = target_ref.trim();
    if allow_service_jid && xmpp_is_service_jid(trimmed) {
        return Ok(None);
    }
    let localpart = trimmed.split('@').next().unwrap_or(trimmed).trim();
    if localpart.is_empty() {
        return Err(());
    }
    let lower = localpart.to_ascii_lowercase();
    let canonical_opt = xmpp_alias_to_channel_key(localpart);
    let scope_key = canonical_opt.as_deref().unwrap_or(lower.as_str());
    let ch_opt = snap.characters.get(&caller_cid);

    let mut is_scoped = false;
    if let Some(rest) = scope_key
        .strip_prefix("solarsystemid2:")
        .or_else(|| scope_key.strip_prefix("resourcewars:"))
    {
        is_scoped = true;
        let req_sys = rest.parse::<u32>().map_err(|_| ())?;
        if !ch_opt.is_some_and(|c| c.solar_system_id == req_sys) {
            return Err(());
        }
    } else if let Some(rest) = scope_key.strip_prefix("constellationid:") {
        is_scoped = true;
        let req_cid = rest.parse::<u32>().map_err(|_| ())?;
        if !ch_opt
            .and_then(|c| ctx.sde.solar_systems.get(&c.solar_system_id))
            .is_some_and(|s| s.constellation_id == req_cid)
        {
            return Err(());
        }
    } else if let Some(rest) = scope_key.strip_prefix("regionid:") {
        is_scoped = true;
        let req_rid = rest.parse::<u32>().map_err(|_| ())?;
        if !ch_opt
            .and_then(|c| ctx.sde.solar_systems.get(&c.solar_system_id))
            .is_some_and(|s| s.region_id == req_rid)
        {
            return Err(());
        }
    } else if let Some(rest) = scope_key.strip_prefix("corpid:") {
        is_scoped = true;
        let req_corp = rest.parse::<i64>().map_err(|_| ())?;
        if !ch_opt.is_some_and(|c| c.corporation_id == req_corp) {
            return Err(());
        }
    } else if let Some(rest) = scope_key.strip_prefix("allianceid:") {
        is_scoped = true;
        let req_ally = rest.parse::<i64>().map_err(|_| ())?;
        if ch_opt.and_then(|c| c.alliance_id) != Some(req_ally) {
            return Err(());
        }
    } else if let Some(rest) = scope_key.strip_prefix("fleetid:") {
        is_scoped = true;
        let req_fleet = rest.parse::<i64>().map_err(|_| ())?;
        if !snap
            .fleets
            .get(&req_fleet)
            .is_some_and(|f| f.members.contains(&caller_cid))
        {
            return Err(());
        }
    } else if let Some(rest) = scope_key.strip_prefix("factionid:") {
        is_scoped = true;
        let req_fac = rest.parse::<i64>().map_err(|_| ())?;
        if ch_opt.and_then(|c| c.war_faction_id) != Some(req_fac) {
            return Err(());
        }
    }

    let existing_key = find_existing_channel_key(snap, localpart).or_else(|| {
        canonical_opt
            .as_deref()
            .and_then(|k| find_existing_channel_key(snap, k))
    });
    if let Some(key) = existing_key {
        let Some(chan) = snap.chat_channels.get(&key) else {
            return Err(());
        };
        let (can_see_forme, can_see_public_or_search, _) = xmpp_channel_visibility_for_caller(
            snap,
            &key,
            chan,
            caller_cid,
            blocked_in_channels,
            owners_blocking_caller,
        );
        let is_denied_only = snap
            .chat_channel_acl
            .get(&key)
            .is_some_and(|a| a.deny_list.contains(&caller_cid))
            || blocked_in_channels.contains(&key);
        let allowed = if is_scoped {
            !is_denied_only
        } else if lower.starts_with("private:") || chan.is_private {
            can_see_forme
        } else {
            can_see_public_or_search
        };
        return if allowed {
            Ok(Some(chan.name.clone()))
        } else {
            Err(())
        };
    }

    if lower.starts_with("private:")
        || lower.starts_with("player_")
        || lower.starts_with("channel:")
    {
        return Err(());
    }
    if is_scoped {
        return Ok(None);
    }
    if lower.starts_with("help_")
        || matches!(
            lower.as_str(),
            "rookie_help" | "global" | "local" | "corp_recruitment"
        )
    {
        return Ok(None);
    }
    Err(())
}

/// Verifies whether an authenticated, bound XMPP caller is allowed to join/presence-sync `room_jid` (`#940`).
pub fn xmpp_can_join_room(
    ctx: &ServerContext,
    auth: Option<&XmppStreamAuth>,
    room_jid: &str,
) -> bool {
    let Some((_, caller_cid)) = xmpp_authenticated_active_caller(ctx, auth) else {
        return false;
    };
    let (blocked_in_channels, owners_blocking_caller) =
        xmpp_social_block_snapshot(ctx, caller_cid);
    ctx.db.with_snapshot(|snap| {
        xmpp_scoped_channel_allowed(
            ctx,
            snap,
            caller_cid,
            room_jid,
            &blocked_in_channels,
            &owners_blocking_caller,
            false,
        )
        .is_ok()
    })
}

/// Build an XMPP `<iq type='result'>` stanza matching the client's IQ query namespace.
/// V24.01 `xmppchatsvc.GetChannels` reads `response.children[0].children` on `disco#items`,
/// raising `IndexError` if the server returns an empty `<iq type='result' id='...'/>`.
pub fn xmpp_iq_reply(chunk: &str, stanza_id: &str) -> String {
    xmpp_iq_reply_with_auth(None, None, chunk, stanza_id)
}

/// Context-aware XMPP `<iq>` builder without an authenticated stream identity.
/// When `ctx.is_some()`, unauthenticated callers fail closed (`not-authorized`) so channel
/// inventories and `node='owner'` flags are never leaked across the cluster (`#940`).
pub fn xmpp_iq_reply_with_ctx(
    ctx: Option<&ServerContext>,
    chunk: &str,
    stanza_id: &str,
) -> String {
    xmpp_iq_reply_with_auth(ctx, None, chunk, stanza_id)
}

/// Authenticated XMPP `<iq>` builder that enforces SASL + resource bind (`auth`) against
/// `snap.chat_channels`, `snap.chat_channel_acl`, passwords, privacy flags, and `ctx.social_extras`
/// blocklists (`#940`).
pub fn xmpp_iq_reply_with_auth(
    ctx: Option<&ServerContext>,
    auth: Option<&XmppStreamAuth>,
    chunk: &str,
    stanza_id: &str,
) -> String {
    let mut cloned = auth.cloned();
    xmpp_iq_reply_with_auth_mut(ctx, cloned.as_mut(), chunk, stanza_id)
}

/// Mutable-auth variant of [`xmpp_iq_reply_with_auth`] used by the XMPP stream loop so `<bind>`
/// updates `auth.bound_jid` after SASL authentication succeeds (`#940`).
pub fn xmpp_iq_reply_with_auth_mut(
    ctx: Option<&ServerContext>,
    auth: Option<&mut XmppStreamAuth>,
    chunk: &str,
    stanza_id: &str,
) -> String {
    let esc_stanza_id = escape_xml_attr(stanza_id);
    let mut trimmed = chunk.trim_start();
    while trimmed.starts_with("<?") {
        let Some(pi_end) = trimmed.find("?>") else {
            break;
        };
        trimmed = trimmed[pi_end + 2..].trim_start();
    }
    let (iq_open_tag, iq_body) = if trimmed.starts_with("<iq") {
        if let Some(end) = xmpp_opening_tag_end(trimmed) {
            (&trimmed[..=end], trimmed[end + 1..].trim_start())
        } else {
            (trimmed, "")
        }
    } else {
        ("", trimmed)
    };
    let is_bind = iq_body.starts_with("<bind")
        && matches!(
            iq_body.as_bytes().get(5),
            Some(b' ' | b'\t' | b'\r' | b'\n' | b'>' | b'/') | None
        );
    if is_bind {
        let Some(_) = ctx else {
            return format!(
                "<iq type='result' id='{esc_stanza_id}'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><jid>user@chat.evomu.local/eve</jid></bind></iq>"
            );
        };
        let Some(a) = auth else {
            return xmpp_not_authorized_iq(stanza_id);
        };
        let Some(cid) = a.character_id.filter(|_| a.sasl_authenticated) else {
            return xmpp_not_authorized_iq(stanza_id);
        };
        let res_raw = extract_xml_elem_text(iq_body, "resource").unwrap_or_else(|| "eve".to_string());
        let res_clean: String = res_raw
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            .take(64)
            .collect();
        let resource = if res_clean.is_empty() {
            "eve".to_string()
        } else {
            res_clean
        };
        let jid = format!("char_{cid}@chat.evomu.local/{resource}");
        a.bound_jid = Some(jid.clone());
        let esc_jid = escape_xml_attr(&jid);
        return format!(
            "<iq type='result' id='{esc_stanza_id}'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><jid>{esc_jid}</jid></bind></iq>"
        );
    }

    let caller_cid = if let Some(c) = ctx {
        let Some((_, cid)) = xmpp_authenticated_active_caller(c, auth.as_deref()) else {
            return xmpp_not_authorized_iq(stanza_id);
        };
        Some(cid)
    } else {
        None
    };

    let query_open_tag = if iq_body.starts_with("<query") {
        xmpp_opening_tag_end(iq_body)
            .map(|end| &iq_body[..=end])
            .unwrap_or(iq_body)
    } else {
        ""
    };
    let query_xmlns = extract_xml_attr(query_open_tag, "xmlns").unwrap_or("");

    if query_xmlns == "http://jabber.org/protocol/disco#items" {
        let node_opt = extract_xml_attr(query_open_tag, "node");
        if let Some(node) = node_opt.filter(|n| n.starts_with("byname/")) {
            let target = unescape_xml_attr(&node["byname/".len()..]);
            let target_trimmed = target.trim();
            let mut items: Vec<(String, String)> = Vec::new();
            if let (Some(c), Some(cid)) = (ctx, caller_cid) {
                let (blocked_in_channels, owners_blocking_caller) =
                    xmpp_social_block_snapshot(c, cid);
                c.db.with_snapshot(|snap| {
                    for (key, chan) in &snap.chat_channels {
                        let Some(slug) = key
                            .strip_prefix("channel:")
                            .or_else(|| key.strip_prefix("CHANNEL:"))
                        else {
                            continue;
                        };
                        let (_, can_see_public_or_search, _) = xmpp_channel_visibility_for_caller(
                            snap,
                            key,
                            chan,
                            cid,
                            &blocked_in_channels,
                            &owners_blocking_caller,
                        );
                        if !can_see_public_or_search {
                            continue;
                        }
                        let xmpp_room = format!("player_{slug}");
                        if chan.name.eq_ignore_ascii_case(target_trimmed)
                            || slug.eq_ignore_ascii_case(target_trimmed)
                            || xmpp_room.eq_ignore_ascii_case(target_trimmed)
                        {
                            items.push((
                                format!("{xmpp_room}@conference.chat.evomu.local"),
                                chan.name.clone(),
                            ));
                        }
                    }
                });
            }
            items.sort_by(|a, b| a.0.cmp(&b.0));
            let mut body = String::new();
            for (jid, display_name) in items {
                let esc_jid = escape_xml_attr(&jid);
                let esc_name = escape_xml_attr(&display_name);
                body.push_str(&format!("<item jid='{esc_jid}' name='{esc_name}'/>"));
            }
            let esc_node = escape_xml_attr(&unescape_xml_attr(node));
            format!(
                "<iq type='result' id='{esc_stanza_id}'><query xmlns='http://jabber.org/protocol/disco#items' node='{esc_node}'>{body}</query></iq>"
            )
        } else {
            let is_forme = node_opt == Some("forme");
            let mut items: Vec<(String, String, bool)> = Vec::new();
            if let (Some(c), Some(cid)) = (ctx, caller_cid) {
                let (blocked_in_channels, owners_blocking_caller) =
                    xmpp_social_block_snapshot(c, cid);
                c.db.with_snapshot(|snap| {
                    for (key, chan) in &snap.chat_channels {
                        let Some(slug) = key
                            .strip_prefix("channel:")
                            .or_else(|| key.strip_prefix("CHANNEL:"))
                        else {
                            continue;
                        };
                        let (can_see_forme, can_see_public_or_search, is_owner) =
                            xmpp_channel_visibility_for_caller(
                                snap,
                                key,
                                chan,
                                cid,
                                &blocked_in_channels,
                                &owners_blocking_caller,
                            );
                        let visible = if is_forme {
                            can_see_forme
                        } else {
                            can_see_public_or_search
                        };
                        if !visible {
                            continue;
                        }
                        items.push((
                            format!("player_{slug}@conference.chat.evomu.local"),
                            chan.name.clone(),
                            is_owner,
                        ));
                    }
                });
            }
            items.sort_by(|a, b| a.0.cmp(&b.0));
            let mut body = String::new();
            for (jid, display_name, is_owner) in items {
                let esc_jid = escape_xml_attr(&jid);
                let esc_name = escape_xml_attr(&display_name);
                if is_owner {
                    body.push_str(&format!(
                        "<item jid='{esc_jid}' name='{esc_name}' node='owner'/>"
                    ));
                } else {
                    body.push_str(&format!("<item jid='{esc_jid}' name='{esc_name}'/>"));
                }
            }
            if is_forme {
                format!(
                    "<iq type='result' id='{esc_stanza_id}'><query xmlns='http://jabber.org/protocol/disco#items' node='forme'>{body}</query></iq>"
                )
            } else {
                format!(
                    "<iq type='result' id='{esc_stanza_id}'><query xmlns='http://jabber.org/protocol/disco#items'>{body}</query></iq>"
                )
            }
        }
    } else if query_xmlns == "http://jabber.org/protocol/disco#info" {
        let mut display_name = "Conference".to_string();
        if let (Some(c), Some(cid)) = (ctx, caller_cid) {
            let to_attr = extract_xml_attr(iq_open_tag, "to")
                .or_else(|| extract_xml_attr(chunk, "to"))
                .unwrap_or("");
            let node_attr = extract_xml_attr(query_open_tag, "node");
            let target_ref = if !xmpp_is_service_jid(to_attr) {
                to_attr
            } else {
                node_attr.unwrap_or(to_attr)
            };
            let (blocked_in_channels, owners_blocking_caller) = xmpp_social_block_snapshot(c, cid);
            let info_res = c.db.with_snapshot(|snap| {
                xmpp_scoped_channel_allowed(
                    c,
                    snap,
                    cid,
                    target_ref,
                    &blocked_in_channels,
                    &owners_blocking_caller,
                    true,
                )
            });
            match info_res {
                Ok(Some(name)) => display_name = name,
                Ok(None) => {}
                Err(()) => return xmpp_item_not_found_iq(stanza_id),
            }
        }
        let esc_name = escape_xml_attr(&display_name);
        format!(
            "<iq type='result' id='{esc_stanza_id}'><query xmlns='http://jabber.org/protocol/disco#info'><identity category='conference' type='text' name='{esc_name}'/><feature var='http://jabber.org/protocol/muc'/></query></iq>"
        )
    } else if query_xmlns == "http://jabber.org/protocol/muc#admin" {
        if let (Some(c), Some(cid)) = (ctx, caller_cid) {
            let Some(target_ref) = extract_xml_attr(iq_open_tag, "to") else {
                return xmpp_not_authorized_iq(stanza_id);
            };
            let localpart = target_ref.split('@').next().unwrap_or(target_ref).trim();
            if localpart.is_empty() || xmpp_is_service_jid(target_ref) {
                return xmpp_not_authorized_iq(stanza_id);
            }
            let allowed_admin = c.db.with_snapshot(|snap| {
                let resolved_key = find_existing_channel_key(snap, localpart)
                    .or_else(|| {
                        xmpp_alias_to_channel_key(localpart)
                            .and_then(|k| find_existing_channel_key(snap, &k))
                    })
                    .or_else(|| xmpp_alias_to_channel_key(localpart));
                let Some(key) = resolved_key else {
                    return false;
                };
                if is_system_channel(&key) {
                    return false;
                }
                if is_org_channel(&key) {
                    let dummy_sess = SessionState::new(0);
                    return has_dynamic_org_operator_role(snap, &dummy_sess, cid, &key);
                }
                snap.chat_channels
                    .get(&key)
                    .is_some_and(|ch| ch.owner_id == Some(cid) || ch.operators.contains(&cid))
            });
            if !allowed_admin {
                return xmpp_not_authorized_iq(stanza_id);
            }
        }
        format!(
            "<iq type='result' id='{esc_stanza_id}'><query xmlns='http://jabber.org/protocol/muc#admin'/></iq>"
        )
    } else if query_xmlns == "http://jabber.org/protocol/muc#owner" {
        if let (Some(c), Some(cid)) = (ctx, caller_cid) {
            let Some(target_ref) = extract_xml_attr(iq_open_tag, "to") else {
                return xmpp_not_authorized_iq(stanza_id);
            };
            let localpart = target_ref.split('@').next().unwrap_or(target_ref).trim();
            if localpart.is_empty() || xmpp_is_service_jid(target_ref) {
                return xmpp_not_authorized_iq(stanza_id);
            }
            let allowed_owner = c.db.with_snapshot(|snap| {
                let resolved_key = find_existing_channel_key(snap, localpart).or_else(|| {
                    xmpp_alias_to_channel_key(localpart)
                        .and_then(|k| find_existing_channel_key(snap, &k))
                });
                resolved_key
                    .as_deref()
                    .and_then(|k| snap.chat_channels.get(k))
                    .is_some_and(|ch| {
                        !is_system_or_org_channel(&ch.channel_id) && ch.owner_id == Some(cid)
                    })
            });
            if !allowed_owner {
                return xmpp_not_authorized_iq(stanza_id);
            }
        }
        format!(
            "<iq type='result' id='{esc_stanza_id}'><query xmlns='http://jabber.org/protocol/muc#owner'/></iq>"
        )
    } else if query_xmlns == "urn:xmpp:expiring_record#search" {
        format!(
            "<iq type='result' id='{esc_stanza_id}'><query xmlns='urn:xmpp:expiring_record#search'/></iq>"
        )
    } else {
        format!("<iq type='result' id='{esc_stanza_id}'/>")
    }
}

#[cfg(test)]
#[path = "fleet_social_chat_tests.rs"]
pub(crate) mod fleet_social_chat_tests;

#[cfg(test)]
#[path = "fleet_social_chat_edges_tests.rs"]
mod fleet_social_chat_edges_tests;
