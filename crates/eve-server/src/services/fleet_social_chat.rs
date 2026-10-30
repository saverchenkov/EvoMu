//! Chat (`SO-7`), Notifications, Contacts & Calendar (`P-5`/`CH-4`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{ChatChannelRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

pub(crate) use super::fleet_social_persistence::{
    CalendarEventState, SocialExtrasState,
};

fn prune_delayed_local_speakers_on_leave(
    ctx: &ServerContext,
    snap: &mut eve_db::WorldStateSnapshot,
    chan_id: &str,
    leaving_char_id: i64,
    remaining_members: &HashSet<i64>,
) {
    let mut s = ctx.social_extras.lock().unwrap();
    let Some(speakers) = s.delayed_local_speakers.get_mut(chan_id) else {
        return;
    };
    speakers.retain(|&spk| {
        let wit_key = format!("wh_local_witnesses:{chan_id}:{spk}");
        let mut wit: HashSet<i64> = snap
            .service_state_blobs
            .get(&wit_key)
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default();
        wit.remove(&leaving_char_id);
        let keep = remaining_members.contains(&spk)
            || wit.iter().any(|m| remaining_members.contains(m));
        if keep {
            if let Ok(j) = serde_json::to_string(&wit) {
                snap.service_state_blobs.insert(wit_key, j);
            }
        } else {
            snap.service_state_blobs.remove(&wit_key);
        }
        keep
    });
}

pub(super) fn handle_chat_and_social(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
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

    if matches!(
        service,
        "addressbook" | "charMgr"
    ) {
        return handle_contacts_notifications_calendar(ctx, session, service, method, args);
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
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("token", PyRep::String(format!("evomu-xmpp-token-{char_id}"))),
                ("jid", PyRep::String(format!("{char_id}@chat.evomu.local"))),
            ])))
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
            let blocked_in_chan = ctx
                .social_extras
                .lock()
                .unwrap()
                .blocked_in_channel
                .get(chan_id)
                .is_some_and(|b| b.contains(&target_char))
                || ctx.db.with_snapshot(|snap| {
                    snap.chat_channel_acl
                        .get(chan_id)
                        .is_some_and(|a| a.deny_list.contains(&target_char))
                });
            if blocked_in_chan {
                return Some(Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Blocked from channel".into() }));
            }
            if target_char != caller_id {
                let blocked_in_contacts = crate::services::character_skills::character_contacts::is_blocked_by(ctx, target_char, caller_id);
                let blocked_in_social = ctx.social_extras.lock().unwrap().blocked.get(&target_char).is_some_and(|b| b.contains(&caller_id));
                if blocked_in_contacts || blocked_in_social {
                    return Some(Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Target character has blocked caller".into() }));
                }
            }
            let is_delayed = is_delayed_local_channel(chan_id);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let chan_exists = snap.chat_channels.contains_key(chan_id);
                if !chan_exists && target_char != caller_id {
                    return Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Cannot add another character to non-existent channel".into() });
                }
                let chan = snap.chat_channels.entry(chan_id.to_string()).or_insert_with(|| ChatChannelRecord {
                    channel_id: chan_id.to_string(),
                    name: chan_id.to_string(),
                    motd: String::new(),
                    owner_id: Some(caller_id),
                    members: HashSet::new(),
                    is_system_local: chan_id.starts_with("solarsystemid2:"),
                    solar_system_id: parse_channel_system_id(chan_id),
                    is_private: chan_id.starts_with("private:"),
                    operators: HashSet::from([caller_id]),
                    password: None,
                    is_delayed_local: is_delayed,
                });
                if target_char != caller_id && chan.owner_id != Some(caller_id) && !chan.operators.contains(&caller_id) {
                    return Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Only channel owner or operator may add other characters".into() });
                }
                if let Some(ref pw) = chan.password {
                    if !pw.is_empty() && chan.owner_id != Some(caller_id) && !chan.operators.contains(&caller_id) && password_arg != Some(pw.as_str()) {
                        return Err(BridgeRpcError { code: "InvalidChannelPassword".into(), message: "Incorrect password for channel".into() });
                    }
                }
                chan.members.insert(target_char);
                let visible_count = if chan.is_delayed_local {
                    ctx.social_extras.lock().unwrap().delayed_local_speakers.get(chan_id).map(|s| s.len()).unwrap_or(0)
                } else {
                    chan.members.len()
                };
                Ok((chan.name.clone(), chan.motd.clone(), chan.owner_id, visible_count, chan.is_delayed_local))
            });
            match res {
                Ok((name, motd, owner_id, count, delayed)) => {
                    let jid = format!("{}@chat.evomu.local", chan_id);
                    if !delayed {
                        ctx.notify(NotificationTarget::All, "OnChannelJoined", "channelid", vec![PyRep::String(chan_id.to_string()), PyRep::Integer(target_char)]);
                    }
                    Some(Ok(PyRep::keyval([
                        ("channelID", PyRep::String(chan_id.to_string())),
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
            let (local_chan, synced_channels, is_delayed) = ctx.db.with_mut_snapshot(|snap| {
                let ch = snap.characters.get(&char_id);
                let sys_id = explicit_arg
                    .and_then(parse_channel_system_id)
                    .or(session.solar_system_id)
                    .or_else(|| ch.map(|c| c.solar_system_id))
                    .unwrap_or(30_000_142);
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
                for (lcid, rem_m) in left_delayed {
                    prune_delayed_local_speakers_on_leave(ctx, snap, &lcid, char_id, &rem_m);
                }
                for cid in &target_chans {
                    let delayed = is_delayed_local_channel(cid);
                    let chan = snap
                        .chat_channels
                        .entry(cid.clone())
                        .or_insert_with(|| ChatChannelRecord {
                            channel_id: cid.clone(),
                            name: cid.clone(),
                            motd: String::new(),
                            owner_id: Some(char_id),
                            members: HashSet::from([char_id]),
                            is_system_local: cid.starts_with("solarsystemid2:"),
                            solar_system_id: parse_channel_system_id(cid),
                            is_private: false,
                            operators: HashSet::from([char_id]),
                            password: None,
                            is_delayed_local: delayed,
                        });
                    chan.members.insert(char_id);
                }
                let is_del = is_delayed_local_channel(&local_id);
                (local_id, target_chans, is_del)
            });
            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(local_chan)),
                ("exists", PyRep::Bool(true)),
                ("isDelayedLocal", PyRep::Bool(is_delayed)),
                ("channels", PyRep::List(synced_channels.into_iter().map(PyRep::String).collect())),
            ])))
        }

        ("XmppChatMgr", "CreatePlayerOwnedChannel") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("PlayerChannel");
            let password = args.get(1).and_then(|a| a.as_str()).map(|s| s.to_string());
            let motd = args.get(2).and_then(|a| a.as_str()).unwrap_or("");
            let chan_id = format!("channel:{}", name.to_lowercase().replace(' ', "_"));
            ctx.db.with_mut_snapshot(|snap| {
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
            });
            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(chan_id)),
                ("name", PyRep::String(name.to_string())),
                ("ownerID", PyRep::Integer(char_id)),
            ])))
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
                let chan_key = if snap.chat_channels.contains_key(chan_id) {
                    chan_id.to_string()
                } else {
                    chan_id.to_lowercase()
                };
                let Some(chan) = snap.chat_channels.get_mut(&chan_key) else {
                    return Err(BridgeRpcError {
                        code: "ChannelNotFound".into(),
                        message: format!("Channel {chan_id} not found"),
                    });
                };
                if chan.owner_id != Some(char_id) {
                    return Err(BridgeRpcError {
                        code: "NotChannelOwner".into(),
                        message: "Only the channel owner can transfer ownership".into(),
                    });
                }
                chan.owner_id = Some(new_owner_id);
                chan.operators.insert(new_owner_id);
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("channelID", PyRep::String(chan_id.to_string())),
                    ("ownerID", PyRep::Integer(new_owner_id)),
                    ("transferred", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("XmppChatMgr", "SetMotd") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let chan_id = args.first().and_then(|a| a.as_str()).unwrap_or("local");
            let motd = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(chan) = snap.chat_channels.get_mut(chan_id) else {
                    return Err(BridgeRpcError {
                        code: "ChannelNotFound".into(),
                        message: format!("Channel {chan_id} not found"),
                    });
                };
                if chan.owner_id != Some(char_id) && !chan.operators.contains(&char_id) {
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
            let is_authorized = session.has_gm_role()
                || ctx.db.with_snapshot(|snap| {
                    snap.chat_channels
                        .get(chan_id)
                        .is_some_and(|c| c.owner_id == Some(char_id) || c.operators.contains(&char_id))
                });
            if !is_authorized {
                return Some(Err(BridgeRpcError {
                    code: "ChatAccessDenied".into(),
                    message: "GMMute requires channel operator or GM role".into(),
                }));
            }
            let duration_sec = args.get(2).and_then(|a| a.as_i64()).unwrap_or(300).max(1) as u64;
            let reason = args
                .get(3)
                .and_then(|a| a.as_str())
                .unwrap_or("Channel moderation mute");
            let until = ctx.now_unix() + duration_sec;
            ctx.social_extras.lock().unwrap().muted_in_channel.insert((chan_id.to_string(), target_id), (until, reason.to_string()));
            ctx.db.with_mut_snapshot(|snap| {
                snap.chat_channel_acl
                    .entry(chan_id.to_string())
                    .or_default()
                    .muted_until
                    .insert(target_id, (until, reason.to_string()));
            });
            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(chan_id.to_string())),
                ("characterID", PyRep::Integer(target_id)),
                ("mutedUntil", PyRep::Integer(until as i64)),
            ])))
        }

        ("XmppChatMgr", "GMUnmute") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let target_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let chan_id = args.get(1).and_then(|a| a.as_str()).unwrap_or("global");
            let is_authorized = session.has_gm_role() || ctx.db.with_snapshot(|snap| {
                snap.chat_channels.get(chan_id).is_some_and(|c| c.owner_id == Some(char_id) || c.operators.contains(&char_id))
            });
            if !is_authorized {
                return Some(Err(BridgeRpcError { code: "ChatAccessDenied".into(), message: "GMUnmute requires channel operator or GM role".into() }));
            }
            ctx.social_extras.lock().unwrap().muted_in_channel.remove(&(chan_id.to_string(), target_id));
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(acl) = snap.chat_channel_acl.get_mut(chan_id) {
                    acl.muted_until.remove(&target_id);
                }
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("XmppChatMgr", "GetHelpChannel") => {
            let _ = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let lang = args.first().and_then(|a| a.as_str()).filter(|s| !s.is_empty()).unwrap_or(&session.language_id).trim().to_ascii_lowercase();
            let chan = match lang.as_str() {
                "de" => "help_de", "fr" => "help_fr", "ru" => "help_ru", "ja" => "help_ja",
                "zh" => "help_zh", "ko" => "help_ko", "es" => "help_es", _ => "help_en",
            };
            Some(Ok(PyRep::String(chan.into())))
        }

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

        ("XmppChatMgr", "Hostname") => {
            Some(Ok(PyRep::String("chat.evomu.local".into())))
        }

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
                                ("isOperator", PyRep::Bool(c.operators.contains(&char_id))),
                                ("isMuted", PyRep::Bool(snap.chat_channel_acl.get(&c.channel_id).is_some_and(|a| a.muted_until.contains_key(&char_id)))),
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

            let blocked_in_chan = ctx
                .social_extras
                .lock()
                .unwrap()
                .blocked_in_channel
                .get(chan_id)
                .is_some_and(|b| b.contains(&char_id))
                || ctx.db.with_snapshot(|snap| {
                    snap.chat_channel_acl
                        .get(chan_id)
                        .is_some_and(|a| a.deny_list.contains(&char_id))
                });
            if blocked_in_chan {
                return Some(Err(BridgeRpcError {
                    code: "ChannelAccessDenied".into(),
                    message: "You are blocked from joining this channel".into(),
                }));
            }

            let is_delayed = is_delayed_local_channel(chan_id);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let is_allowed_explicitly = snap
                    .chat_channel_acl
                    .get(chan_id)
                    .is_some_and(|a| a.allow_list.contains(&char_id));
                let chan = snap
                    .chat_channels
                    .entry(chan_id.to_string())
                    .or_insert_with(|| ChatChannelRecord {
                        channel_id: chan_id.to_string(),
                        name: chan_id.to_string(),
                        motd: motd_arg.to_string(),
                        owner_id: Some(char_id),
                        members: HashSet::new(),
                        is_system_local: chan_id.starts_with("solarsystemid2:"),
                        solar_system_id: parse_channel_system_id(chan_id),
                        is_private: chan_id.starts_with("private:"),
                        operators: HashSet::from([char_id]),
                        password: password_arg.map(|s| s.to_string()),
                        is_delayed_local: is_delayed,
                    });
                if let Some(ref pw) = chan.password {
                    if !pw.is_empty()
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
                let visible_count = if chan.is_delayed_local {
                    ctx.social_extras
                        .lock()
                        .unwrap()
                        .delayed_local_speakers
                        .get(chan_id)
                        .map(|s| s.len())
                        .unwrap_or(0)
                } else {
                    chan.members.len()
                };
                Ok((
                    chan.name.clone(),
                    chan.motd.clone(),
                    visible_count,
                    chan.is_delayed_local,
                    chan.owner_id,
                ))
            });

            match res {
                Ok((name, motd, count, delayed, owner_id)) => {
                    if !delayed {
                        ctx.notify(
                            NotificationTarget::All,
                            "OnChannelJoined",
                            "channelid",
                            vec![PyRep::String(chan_id.to_string()), PyRep::Integer(char_id)],
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("channelID", PyRep::String(chan_id.to_string())),
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
            ctx.db.with_mut_snapshot(|snap| {
                let rem_members = if let Some(chan) = snap.chat_channels.get_mut(chan_id) {
                    chan.members.remove(&char_id);
                    chan.members.clone()
                } else {
                    HashSet::new()
                };
                if is_delayed_local_channel(chan_id) {
                    prune_delayed_local_speakers_on_leave(ctx, snap, chan_id, char_id, &rem_members);
                }
            });
            if !is_delayed_local_channel(chan_id) {
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
            let now_unix = ctx.now_unix();
            let db_mute = ctx.db.with_snapshot(|snap| snap.chat_channel_acl.get(chan_id).and_then(|a| a.muted_until.get(&char_id).cloned()));
            if let Some((until, reason)) = db_mute {
                if now_unix < until {
                    return Some(Err(BridgeRpcError { code: "MutedInChannel".into(), message: format!("You are muted in {chan_id}: {reason}") }));
                }
            }
            {
                let mut s = ctx.social_extras.lock().unwrap();
                if let Some(&(until, ref reason)) = s.muted_in_channel.get(&(chan_id.to_string(), char_id)) {
                    if now_unix < until {
                        return Some(Err(BridgeRpcError { code: "MutedInChannel".into(), message: format!("You are muted in {chan_id}: {reason}") }));
                    }
                }
                let entry = s.chat_rate_window.entry(char_id).or_insert((now_unix, 0));
                if entry.0 == now_unix {
                    entry.1 += 1;
                    if entry.1 > 20 {
                        return Some(Err(BridgeRpcError { code: "ChatRateLimitExceeded".into(), message: "Chat message rate limit exceeded".into() }));
                    }
                } else {
                    *entry = (now_unix, 1);
                }
                if is_delayed_local_channel(chan_id) {
                    s.delayed_local_speakers.entry(chan_id.to_string()).or_default().insert(char_id);
                    ctx.db.with_mut_snapshot(|snap| {
                        let cur_members = snap.chat_channels.get(chan_id).map(|c| c.members.clone()).unwrap_or_default();
                        let wit_key = format!("wh_local_witnesses:{chan_id}:{char_id}");
                        let mut wit: HashSet<i64> = snap.service_state_blobs.get(&wit_key).and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
                        wit.extend(cur_members.into_iter().filter(|&m| m != char_id));
                        if let Ok(j) = serde_json::to_string(&wit) {
                            snap.service_state_blobs.insert(wit_key, j);
                        }
                    });
                }
            }

            let now_ft = ctx.now_blueos_filetime_ticks();
            let sender_name = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.name.clone()).unwrap_or_else(|| "Pilot".into()));

            let notif_args = vec![
                PyRep::String(chan_id.to_string()),
                PyRep::Integer(char_id),
                PyRep::String(sender_name),
                PyRep::String(text.to_string()),
                PyRep::Integer(now_ft),
            ];

            if chan_id.starts_with("solarsystemid2:") {
                let sys_id: u32 = chan_id.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                if session.solar_system_id != Some(sys_id) && !session.has_gm_role() {
                    return Some(Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Sender is not in the target solar system".into() }));
                }
                ctx.notify(NotificationTarget::SolarSystem(sys_id), "OnClientEvent_ChatMessageSent", "channelid", notif_args);
            } else if chan_id.starts_with("corpid:") {
                let corp_id: i64 = chan_id.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                if session.corporation_id != Some(corp_id) && !session.has_gm_role() {
                    return Some(Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Sender is not in the target corporation".into() }));
                }
                ctx.notify(NotificationTarget::Corporation(corp_id), "OnClientEvent_ChatMessageSent", "channelid", notif_args);
            } else if chan_id.starts_with("allianceid:") {
                let alliance_id: i64 = chan_id.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                if session.alliance_id != Some(alliance_id) && !session.has_gm_role() {
                    return Some(Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Sender is not in the target alliance".into() }));
                }
                ctx.notify(NotificationTarget::Alliance(alliance_id), "OnClientEvent_ChatMessageSent", "channelid", notif_args);
            } else if chan_id.starts_with("fleetid:") {
                let fleet_id: i64 = chan_id.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                if session.fleet_id != Some(fleet_id) && !session.has_gm_role() {
                    return Some(Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Sender is not in the target fleet".into() }));
                }
                ctx.notify(NotificationTarget::Fleet(fleet_id), "OnClientEvent_ChatMessageSent", "channelid", notif_args);
            } else {
                let members: Vec<i64> = ctx.db.with_snapshot(|snap| {
                    snap.chat_channels
                        .get(chan_id)
                        .map(|c| c.members.iter().copied().collect())
                        .unwrap_or_else(|| vec![char_id])
                });
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
                ("channelID", PyRep::String(chan_id.to_string())),
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
            let (is_blocked, cspa_charge) = {
                let s = ctx.social_extras.lock().unwrap();
                let (esi_blocked, esi_friendly) = ctx
                    .esi_state
                    .char_contacts
                    .read()
                    .ok()
                    .and_then(|g| g.get(&recipient_id).and_then(|l| l.iter().find(|e| e.contact_id == char_id)).map(|e| (e.is_blocked, e.standing > 0.0)))
                    .unwrap_or((false, false));
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
                    s.cspa_charges
                        .get(&recipient_id)
                        .copied()
                        .or_else(|| ctx.db.with_snapshot(|snap| snap.character_cspa_charges.get(&recipient_id).copied()))
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
            let chan_id = if let Some(cid) = explicit_chan
                .filter(|s| s.starts_with("channel:") || s.starts_with("private:"))
            {
                cid.to_string()
            } else {
                let c1 = char_id.min(recipient_id);
                let c2 = char_id.max(recipient_id);
                format!("private:{c1}:{c2}")
            };

            ctx.db.with_mut_snapshot(|snap| {
                let chan = snap
                    .chat_channels
                    .entry(chan_id.clone())
                    .or_insert_with(|| ChatChannelRecord {
                        channel_id: chan_id.clone(),
                        name: format!("Private Chat ({char_id}, {recipient_id})"),
                        motd: String::new(),
                        owner_id: Some(char_id),
                        members: HashSet::new(),
                        is_system_local: false,
                        solar_system_id: None,
                        is_private: true,
                        operators: HashSet::from([char_id]),
                        password: None,
                        is_delayed_local: false,
                    });
                chan.members.insert(char_id);
                chan.members.insert(recipient_id);
            });

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
            let (_, _) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetMembers requires channelID".into(),
                }));
            };
            let (chan_opt, acl) = ctx.db.with_snapshot(|snap| {
                (
                    snap.chat_channels.get(chan_id).cloned(),
                    snap.chat_channel_acl
                        .get(chan_id)
                        .cloned()
                        .unwrap_or_default(),
                )
            });
            let Some(chan) = chan_opt else {
                return Some(Err(BridgeRpcError {
                    code: "ChannelNotFound".into(),
                    message: format!("Channel {chan_id} does not exist"),
                }));
            };
            let visible_members: Vec<i64> = if chan.is_delayed_local {
                ctx.social_extras
                    .lock()
                    .unwrap()
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
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(chan) = snap.chat_channels.get(chan_id) else {
                        return Err(BridgeRpcError {
                            code: "ChannelNotFound".into(),
                            message: format!("Channel {chan_id} not found"),
                        });
                    };
                    if chan.owner_id != Some(char_id) {
                        return Err(BridgeRpcError {
                            code: "NotChannelOwner".into(),
                            message: "Only channel owner can destroy the channel".into(),
                        });
                    }
                    snap.chat_channels.remove(chan_id);
                    snap.chat_channel_acl.remove(chan_id);
                    Ok(())
                });
                return match res {
                    Ok(()) => {
                        ctx.notify(
                            NotificationTarget::All,
                            "OnChannelDeleted",
                            "channelid",
                            vec![PyRep::String(chan_id.to_string())],
                        );
                        Some(Ok(PyRep::Bool(true)))
                    }
                    Err(e) => Some(Err(e)),
                };
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let chan_key = if snap.chat_channels.contains_key(chan_id) {
                    chan_id.to_string()
                } else {
                    chan_id.to_lowercase()
                };
                let Some(chan) = snap.chat_channels.get_mut(&chan_key) else {
                    return Err(BridgeRpcError {
                        code: "ChannelNotFound".into(),
                        message: format!("Channel {chan_id} not found"),
                    });
                };
                if chan.owner_id != Some(char_id) && !chan.operators.contains(&char_id) {
                    return Err(BridgeRpcError {
                        code: "NotChannelOperator".into(),
                        message: "Requires channel owner or operator privileges".into(),
                    });
                }
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
                        match mode {
                            3 => {
                                if chan.owner_id != Some(char_id) {
                                    return Err(BridgeRpcError {
                                        code: "NotChannelOwner".into(),
                                        message: "Only the channel owner can transfer ownership"
                                            .into(),
                                    });
                                }
                                chan.owner_id = Some(target_id);
                                chan.operators.insert(target_id);
                            }
                            2 => {
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
                                ctx.social_extras
                                    .lock()
                                    .unwrap()
                                    .blocked_in_channel
                                    .entry(chan_key.clone())
                                    .or_default()
                                    .remove(&target_id);
                                ctx.social_extras
                                    .lock()
                                    .unwrap()
                                    .muted_in_channel
                                    .remove(&(chan_key.clone(), target_id));
                            }
                            -1 => {
                                let until = ctx.now_unix() + 3600;
                                snap.chat_channel_acl
                                    .entry(chan_key.clone())
                                    .or_default()
                                    .muted_until
                                    .insert(target_id, (until, "Operator mute".into()));
                                ctx.social_extras.lock().unwrap().muted_in_channel.insert(
                                    (chan_key.clone(), target_id),
                                    (until, "Operator mute".into()),
                                );
                            }
                            -2 => {
                                chan.members.remove(&target_id);
                                chan.operators.remove(&target_id);
                                let acl = snap.chat_channel_acl.entry(chan_key.clone()).or_default();
                                acl.deny_list.insert(target_id);
                                acl.allow_list.remove(&target_id);
                                ctx.social_extras
                                    .lock()
                                    .unwrap()
                                    .blocked_in_channel
                                    .entry(chan_key.clone())
                                    .or_default()
                                    .insert(target_id);
                            }
                            _ => {
                                if let Some(acl) = snap.chat_channel_acl.get_mut(&chan_key) {
                                    acl.allow_list.remove(&target_id);
                                    acl.deny_list.remove(&target_id);
                                    acl.muted_until.remove(&target_id);
                                }
                                ctx.social_extras
                                    .lock()
                                    .unwrap()
                                    .blocked_in_channel
                                    .entry(chan_key.clone())
                                    .or_default()
                                    .remove(&target_id);
                                ctx.social_extras
                                    .lock()
                                    .unwrap()
                                    .muted_in_channel
                                    .remove(&(chan_key.clone(), target_id));
                            }
                        }
                    }
                    _ => {}
                }
                Ok((chan.name.clone(), chan.motd.clone(), chan.owner_id))
            });

            match res {
                Ok((name, motd, owner_id)) => Some(Ok(PyRep::keyval([
                    ("channelID", PyRep::String(chan_id.to_string())),
                    ("name", PyRep::String(name)),
                    ("motd", PyRep::String(motd)),
                    (
                        "ownerID",
                        owner_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    ),
                ]))),
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
}

fn parse_channel_system_id(chan_id: &str) -> Option<u32> {
    chan_id
        .strip_prefix("solarsystemid2:")
        .and_then(|s| s.parse().ok())
}

fn handle_contacts_notifications_calendar(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("addressbook", "GetContactList")
        | ("charMgr", "GetContactList")
        | ("addressbook", "AddContact")
        | ("addressbook", "EditContact")
        | ("charMgr", "AddContact")
        | ("charMgr", "EditContact")
        | ("charMgr", "EditContactsRelationshipID")
        | ("addressbook", "DeleteContacts")
        | ("charMgr", "DeleteContacts")
        | ("addressbook", "BlockOwners")
        | ("charMgr", "BlockOwners")
        | ("addressbook", "UnblockOwners")
        | ("charMgr", "UnblockOwners") => {
            let empty_kw = HashMap::new();
            crate::services::character_skills::character_contacts::handle_contacts(
                ctx, session, "charMgr", method, args, &empty_kw,
            )
        }

        ("charMgr", "GetSettingsInfo") => Some(
            ServerContext::require_character(session).map(|_| crate::services::client_login::settings_info_blob()),
        ),

        ("charMgr", "LogSettings") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let isk = args
                .first()
                .and_then(|a| a.as_f64().or_else(|| a.as_i64().map(|v| v as f64)))
                .unwrap_or(2_950.0)
                .clamp(0.0, 1_000_000.0);
            let charge = IskCents::from_f64_isk(isk);
            ctx.social_extras
                .lock()
                .unwrap()
                .cspa_charges
                .insert(char_id, charge);
            ctx.db.with_mut_snapshot(|snap| {
                snap.character_cspa_charges.insert(char_id, charge);
            });
            Some(Ok(PyRep::Real(isk)))
        }

        ("charMgr", "CSPAChargedAction") | ("charMgr", "CSPAChargedActionForMany") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let target_ids: Vec<i64> = if method == "CSPAChargedActionForMany" {
                let list_arg = args
                    .iter()
                    .find(|a| matches!(a, PyRep::List(_) | PyRep::Tuple(_)))
                    .or_else(|| args.first());
                extract_id_list(list_arg)
            } else {
                args.iter()
                    .rev()
                    .find_map(|a| a.as_i64())
                    .into_iter()
                    .collect()
            };
            let mut total_cspa = IskCents(0);
            {
                let s = ctx.social_extras.lock().unwrap();
                for &rec_id in &target_ids {
                    if rec_id == char_id || rec_id <= 0 {
                        continue;
                    }
                    let friendly = s
                        .contacts
                        .get(&rec_id)
                        .and_then(|m| m.get(&char_id))
                        .is_some_and(|&(st, _, _)| st > 0.0)
                        || ctx.db.with_snapshot(|snap| {
                            if snap.char_contacts.get(&rec_id).is_some_and(|list| {
                                list.iter()
                                    .any(|c| c.contact_id == char_id && !c.is_blocked && c.standing > 0.0)
                            }) {
                                return true;
                            }
                            let c1 = snap.characters.get(&char_id);
                            let c2 = snap.characters.get(&rec_id);
                            match (c1, c2) {
                                (Some(a), Some(b)) => {
                                    a.corporation_id == b.corporation_id
                                        || (a.alliance_id.is_some() && a.alliance_id == b.alliance_id)
                                }
                                _ => false,
                            }
                        });
                    if !friendly {
                        let charge = s
                            .cspa_charges
                            .get(&rec_id)
                            .copied()
                            .or_else(|| {
                                ctx.db
                                    .with_snapshot(|snap| snap.character_cspa_charges.get(&rec_id).copied())
                            })
                            .unwrap_or_else(|| IskCents::from_f64_isk(2_950.0));
                        if charge.0 > 0 {
                            total_cspa = total_cspa.checked_add(charge).unwrap_or(total_cspa);
                        }
                    }
                }
            }
            if total_cspa.0 > 0 {
                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(ch) = snap.characters.get_mut(&char_id) else {
                        return Err(BridgeRpcError {
                            code: "CharacterNotFound".into(),
                            message: format!("Character {char_id} not found"),
                        });
                    };
                    ch.balance = ch.balance.checked_sub(total_cspa).ok_or_else(|| BridgeRpcError {
                        code: "InsufficientFundsForCSPA".into(),
                        message: "Insufficient ISK to pay recipient CSPA charge".into(),
                    })?;
                    Ok(())
                });
                if let Err(e) = res {
                    return Some(Err(e));
                }
            }
            Some(Ok(PyRep::Real(total_cspa.to_wire_f64())))
        }

        ("charMgr", "SetNote") | ("charMgr", "GetNote") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            if method == "SetNote" {
                let note = args.get(1).and_then(|a| a.as_str()).unwrap_or("");
                ctx.social_extras
                    .lock()
                    .unwrap()
                    .char_notes
                    .insert((char_id, item_id), note.to_string());
                ctx.db.with_mut_snapshot(|snap| {
                    snap.char_notes.insert((char_id, item_id), note.to_string());
                });
                Some(Ok(PyRep::Bool(true)))
            } else {
                let note = ctx
                    .db
                    .with_snapshot(|snap| snap.char_notes.get(&(char_id, item_id)).cloned())
                    .or_else(|| {
                        ctx.social_extras
                            .lock()
                            .unwrap()
                            .char_notes
                            .get(&(char_id, item_id))
                            .cloned()
                    })
                    .unwrap_or_default();
                Some(Ok(PyRep::String(note)))
            }
        }

        _ => None,
    }
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
