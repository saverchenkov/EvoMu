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
    is_system_or_org_channel, validate_channel_join_auth,
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
        Some(chan_id.to_string())
    } else {
        snap.chat_channels
            .keys()
            .find(|k| k.eq_ignore_ascii_case(chan_id))
            .cloned()
    }
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

fn handle_get_chat_authentication_token(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let token = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get(&format!("xmpp:token:{char_id}"))
            .cloned()
            .unwrap_or_else(|| format!("evomu-xmpp-token-{char_id}"))
    });
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
        snap.service_state_blobs
            .get(&format!("xmpp:help_channel:{lang}"))
            .cloned()
            .unwrap_or_else(|| default_chan.to_string())
    });
    Some(Ok(PyRep::String(chan)))
}

fn handle_xmpp_hostname(ctx: &ServerContext) -> Option<Result<PyRep, BridgeRpcError>> {
    let host = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("xmpp:hostname")
            .cloned()
            .unwrap_or_else(|| "chat.evomu.local".to_string())
    });
    Some(Ok(PyRep::String(host)))
}

/// Build an XMPP `<iq type='result'>` stanza matching the client's IQ query namespace.
/// V24.01 `xmppchatsvc.GetChannels` reads `response.children[0].children` on `disco#items`,
/// raising `IndexError` if the server returns an empty `<iq type='result' id='...'/>`.
pub fn xmpp_iq_reply(chunk: &str, stanza_id: &str) -> String {
    if chunk.contains("<bind") {
        format!(
            "<iq type='result' id='{stanza_id}'><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'><jid>user@chat.evomu.local/eve</jid></bind></iq>"
        )
    } else if chunk.contains("http://jabber.org/protocol/disco#items") {
        if chunk.contains("node='forme'") || chunk.contains("node=\"forme\"") {
            format!(
                "<iq type='result' id='{stanza_id}'><query xmlns='http://jabber.org/protocol/disco#items' node='forme'/></iq>"
            )
        } else {
            format!(
                "<iq type='result' id='{stanza_id}'><query xmlns='http://jabber.org/protocol/disco#items'/></iq>"
            )
        }
    } else if chunk.contains("http://jabber.org/protocol/disco#info") {
        format!(
            "<iq type='result' id='{stanza_id}'><query xmlns='http://jabber.org/protocol/disco#info'><identity category='conference' type='text' name='Conference'/><feature var='http://jabber.org/protocol/muc'/></query></iq>"
        )
    } else if chunk.contains("http://jabber.org/protocol/muc#admin") {
        format!(
            "<iq type='result' id='{stanza_id}'><query xmlns='http://jabber.org/protocol/muc#admin'/></iq>"
        )
    } else if chunk.contains("http://jabber.org/protocol/muc#owner") {
        format!(
            "<iq type='result' id='{stanza_id}'><query xmlns='http://jabber.org/protocol/muc#owner'/></iq>"
        )
    } else if chunk.contains("urn:xmpp:expiring_record#search") {
        format!(
            "<iq type='result' id='{stanza_id}'><query xmlns='urn:xmpp:expiring_record#search'/></iq>"
        )
    } else {
        format!("<iq type='result' id='{stanza_id}'/>")
    }
}

#[cfg(test)]
#[path = "fleet_social_chat_tests.rs"]
pub(crate) mod fleet_social_chat_tests;

#[cfg(test)]
#[path = "fleet_social_chat_edges_tests.rs"]
mod fleet_social_chat_edges_tests;
