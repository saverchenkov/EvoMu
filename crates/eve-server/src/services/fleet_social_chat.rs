//! Chat (`SO-7`), Notifications, Contacts & Calendar (`P-5`/`CH-4`).

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::{ChatChannelRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Default)]
pub(super) struct MailingListState {
    pub(super) list_id: i64,
    pub(super) name: String,
    pub(super) owner_id: i64,
    pub(super) default_access: i64,
    pub(super) default_role: i64,
    pub(super) cost: i64,
    pub(super) members: HashMap<i64, i64>, // char_id -> role (0=normal, 1=muted, 2=operator, 3=owner)
    pub(super) entity_access: HashMap<i64, i64>, // entity_id -> access_level
    pub(super) welcome_mail: Option<(String, String)>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct CalendarEventState {
    pub(super) event_id: i64,
    pub(super) owner_id: i64,
    pub(super) creator_id: i64,
    pub(super) event_date_time: i64,
    pub(super) duration: i64,
    pub(super) importance: i64,
    pub(super) title: String,
    pub(super) description: String,
    pub(super) _scope: String, // "Personal" | "Corporation" | "Alliance"
    pub(super) responses: HashMap<i64, i64>, // char_id -> response (1=Accepted, 2=Declined, 3=Tentative, 0=Undecided)
}

#[derive(Clone, Debug, Default)]
pub(super) struct NotificationItem {
    pub(super) notification_id: i64,
    pub(super) type_id: i64,
    pub(super) sender_id: i64,
    pub(super) receiver_id: i64,
    pub(super) processed: bool,
    pub(super) created_ft: i64,
    pub(super) data: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SocialExtrasState {
    pub(super) next_id: i64,
    pub(super) cspa_charges: HashMap<i64, IskCents>, // char_id -> CSPA charge
    pub(super) trashed_mails: HashSet<(i64, i64)>,   // (char_id, mail_id)
    pub(super) mailing_lists: HashMap<i64, MailingListState>,
    pub(super) contacts: HashMap<i64, HashMap<i64, (f64, bool, u64)>>, // char_id -> (contact_id -> (standing, in_watchlist, label_mask))
    pub(super) blocked: HashMap<i64, HashSet<i64>>,                    // char_id -> blocked_ids
    pub(super) char_notes: HashMap<(i64, i64), String>,                // (char_id, item_id) -> note
    pub(super) notifications: HashMap<i64, Vec<NotificationItem>>,     // char_id -> notifications (cap 500)
    pub(super) calendar_events: HashMap<i64, CalendarEventState>,
    pub(super) muted_in_channel: HashMap<(String, i64), (u64, String)>, // (chan_id, char_id) -> (until_unix, reason)
    pub(super) blocked_in_channel: HashMap<String, HashSet<i64>>,       // chan_id -> blocked char_ids
    pub(super) delayed_local_speakers: HashMap<String, HashSet<i64>>,   // chan_id -> spoken char_ids
    pub(super) chat_rate_window: HashMap<i64, (u64, u32)>,              // char_id -> (unix_sec, count)
}

impl SocialExtrasState {
    /// Fresh per-`ServerContext` state; local IDs (mailing lists, calendar events) start at 700,000.
    pub(crate) fn new() -> Self {
        Self { next_id: 700_000, ..Default::default() }
    }
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
        "notificationMgr" | "addressbook" | "charMgr"
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
            if ctx.social_extras.lock().unwrap().blocked_in_channel.get(chan_id).is_some_and(|b| b.contains(&target_char)) {
                return Some(Err(BridgeRpcError { code: "ChannelAccessDenied".into(), message: "Blocked from channel".into() }));
            }
            let is_delayed = is_delayed_local_channel(chan_id);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
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
                if let Some(ref pw) = chan.password {
                    if !pw.is_empty() && chan.owner_id != Some(caller_id) && !chan.operators.contains(&caller_id) && password_arg != Some(pw.as_str()) {
                        return Err(BridgeRpcError { code: "InvalidChannelPassword".into(), message: "Incorrect password for channel".into() });
                    }
                }
                chan.members.insert(target_char);
                let visible_count = if chan.is_delayed_local {
                    let speakers = ctx.social_extras.lock().unwrap().delayed_local_speakers.get(chan_id).cloned().unwrap_or_default();
                    chan.members.iter().filter(|m| speakers.contains(m)).count()
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
                let mut target_chans = vec![local_id.clone(), corp_chan];
                if let Some(aid) = alliance_id {
                    target_chans.push(format!("allianceid:{aid}"));
                }
                if let Some(fid) = fleet_id {
                    target_chans.push(format!("fleetid:{fid}"));
                }
                for c in snap.chat_channels.values_mut() {
                    let is_sys_scoped = c.channel_id.starts_with("solarsystemid2:")
                        || c.channel_id.starts_with("corpid:")
                        || c.channel_id.starts_with("allianceid:")
                        || c.channel_id.starts_with("fleetid:");
                    if is_sys_scoped && !target_chans.contains(&c.channel_id) {
                        c.members.remove(&char_id);
                    }
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

        ("XmppChatMgr", "GetDeprecatedPrefsFallback") | ("XmppChatMgr", "Hostname") => {
            Some(Ok(PyRep::String("chat.evomu.local".into())))
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
            let password_arg = args.get(1).and_then(|a| a.as_str());
            let motd_arg = args.get(2).and_then(|a| a.as_str()).unwrap_or("");

            if ctx.social_extras
                .lock()
                .unwrap()
                .blocked_in_channel
                .get(chan_id)
                .is_some_and(|b| b.contains(&char_id))
            {
                return Some(Err(BridgeRpcError {
                    code: "ChannelAccessDenied".into(),
                    message: "You are blocked from joining this channel".into(),
                }));
            }

            let is_delayed = is_delayed_local_channel(chan_id);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
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
            let _ = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(chan) = snap.chat_channels.get_mut(chan_id) { chan.members.remove(&char_id); }
                Ok::<(), std::convert::Infallible>(())
            });
            ctx.social_extras.lock().unwrap().delayed_local_speakers.entry(chan_id.to_string()).or_default().remove(&char_id);
            ctx.notify(NotificationTarget::All, "OnChannelLeft", "channelid", vec![PyRep::String(chan_id.to_string()), PyRep::Integer(char_id)]);
            Some(Ok(PyRep::keyval([("channelID", PyRep::String(chan_id.to_string())), ("left", PyRep::Bool(true))])))
        }

        (_, "SendMessage") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "SendMessage requires channelID string".into(),
                }));
            };
            let text = args.get(1).and_then(|a| a.as_str()).unwrap_or("o7");
            if text.is_empty() || text.chars().count() > 2_000 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidChatMessageLength".into(),
                    message: "Chat message must be 1..=2000 characters".into(),
                }));
            }
            let now_unix = ctx.now_unix();
            {
                let mut s = ctx.social_extras.lock().unwrap();
                if let Some(&(until, ref reason)) =
                    s.muted_in_channel.get(&(chan_id.to_string(), char_id))
                {
                    if now_unix < until {
                        return Some(Err(BridgeRpcError {
                            code: "MutedInChannel".into(),
                            message: format!("You are muted in {chan_id}: {reason}"),
                        }));
                    }
                }
                let entry = s.chat_rate_window.entry(char_id).or_insert((now_unix, 0));
                if entry.0 == now_unix {
                    entry.1 += 1;
                    if entry.1 > 20 {
                        return Some(Err(BridgeRpcError {
                            code: "ChatRateLimitExceeded".into(),
                            message: "Chat message rate limit exceeded".into(),
                        }));
                    }
                } else {
                    *entry = (now_unix, 1);
                }
                if is_delayed_local_channel(chan_id) {
                    s.delayed_local_speakers
                        .entry(chan_id.to_string())
                        .or_default()
                        .insert(char_id);
                }
            }

            let now_ft = ctx.now_blueos_filetime_ticks();
            let sender_name = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "Pilot".into())
            });

            let notif_args = vec![
                PyRep::String(chan_id.to_string()),
                PyRep::Integer(char_id),
                PyRep::String(sender_name),
                PyRep::String(text.to_string()),
                PyRep::Integer(now_ft),
            ];

            if chan_id.starts_with("solarsystemid2:") {
                let sys_id: u32 = chan_id.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                ctx.notify(NotificationTarget::SolarSystem(sys_id), "OnClientEvent_ChatMessageSent", "channelid", notif_args);
            } else if chan_id.starts_with("corpid:") {
                let corp_id: i64 = chan_id.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                ctx.notify(NotificationTarget::Corporation(corp_id), "OnClientEvent_ChatMessageSent", "channelid", notif_args);
            } else if chan_id.starts_with("allianceid:") {
                let alliance_id: i64 = chan_id.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                ctx.notify(NotificationTarget::Alliance(alliance_id), "OnClientEvent_ChatMessageSent", "channelid", notif_args);
            } else if chan_id.starts_with("fleetid:") {
                let fleet_id: i64 = chan_id.split(':').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
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

        (_, "GetChannelInfo") | (_, "GetMembers") => {
            let (_, _) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let Some(chan_id) = args.first().and_then(|a| a.as_str()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetChannelInfo requires channelID".into(),
                }));
            };
            let chan_opt = ctx
                .db
                .with_snapshot(|snap| snap.chat_channels.get(chan_id).cloned());
            let Some(chan) = chan_opt else {
                return Some(Err(BridgeRpcError {
                    code: "ChannelNotFound".into(),
                    message: format!("Channel {chan_id} does not exist"),
                }));
            };
            let visible_members: Vec<i64> = if chan.is_delayed_local {
                let speakers = ctx.social_extras
                    .lock()
                    .unwrap()
                    .delayed_local_speakers
                    .get(chan_id)
                    .cloned()
                    .unwrap_or_default();
                chan.members
                    .iter()
                    .copied()
                    .filter(|m| speakers.contains(m))
                    .collect()
            } else {
                chan.members.iter().copied().collect()
            };
            let rows: Vec<Vec<PyRep>> = visible_members
                .into_iter()
                .map(|m| vec![PyRep::Integer(m), PyRep::String(chan_id.to_string())])
                .collect();
            Some(Ok(PyRep::keyval([
                ("channelID", PyRep::String(chan.channel_id)),
                ("name", PyRep::String(chan.name)),
                ("motd", PyRep::String(chan.motd)),
                ("isDelayedLocal", PyRep::Bool(chan.is_delayed_local)),
                (
                    "ownerID",
                    chan.owner_id.map(PyRep::Integer).unwrap_or(PyRep::None),
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
                            }
                            -1 => {
                                ctx.social_extras.lock().unwrap().muted_in_channel.insert(
                                    (chan_key.clone(), target_id),
                                    (ctx.now_unix() + 3600, "Operator mute".into()),
                                );
                            }
                            -2 => {
                                chan.members.remove(&target_id);
                                chan.operators.remove(&target_id);
                                ctx.social_extras
                                    .lock()
                                    .unwrap()
                                    .blocked_in_channel
                                    .entry(chan_key.clone())
                                    .or_default()
                                    .insert(target_id);
                            }
                            _ => {
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
    sys_id >= 31_000_000 || (30_000_140..=30_000_141).contains(&sys_id) || sys_id == 30_002_086
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
        ("notificationMgr", "GetAllNotifications")
        | ("notificationMgr", "GetFormattedNotifications")
        | ("notificationMgr", "GetUnprocessed")
        | ("notificationMgr", "GetByGroupID")
        | ("notificationMgr", "FormatNotifications") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let items = ctx.social_extras
                .lock()
                .unwrap()
                .notifications
                .get(&char_id)
                .cloned()
                .unwrap_or_default();
            let rows: Vec<Vec<PyRep>> = items
                .into_iter()
                .filter(|n| method != "GetUnprocessed" || !n.processed)
                .map(|n| {
                    vec![
                        PyRep::Integer(n.notification_id),
                        PyRep::Integer(n.type_id),
                        PyRep::Integer(n.sender_id),
                        PyRep::Integer(n.receiver_id),
                        PyRep::Bool(n.processed),
                        PyRep::Integer(n.created_ft),
                        PyRep::String(n.data),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec![
                    "notificationID",
                    "typeID",
                    "senderID",
                    "receiverID",
                    "processed",
                    "created",
                    "data",
                ],
                rows,
            )))
        }

        ("notificationMgr", "MarkAllAsProcessed")
        | ("notificationMgr", "MarkAsProcessed")
        | ("notificationMgr", "MarkGroupAsProcessed") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let ids = extract_id_list(args.first());
            if let Some(list) = ctx.social_extras.lock().unwrap().notifications.get_mut(&char_id) {
                for n in list.iter_mut() {
                    if ids.is_empty() || ids.contains(&n.notification_id) {
                        n.processed = true;
                    }
                }
            }
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnNotificationChange",
                "charid",
                vec![PyRep::Integer(char_id)],
            );
            Some(Ok(PyRep::Bool(true)))
        }

        ("notificationMgr", "DeleteAllNotifications")
        | ("notificationMgr", "DeleteGroupNotifications")
        | ("notificationMgr", "DeleteNotifications") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            ctx.social_extras
                .lock()
                .unwrap()
                .notifications
                .remove(&char_id);
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnNotificationDeleted",
                "charid",
                vec![PyRep::Integer(char_id)],
            );
            Some(Ok(PyRep::Bool(true)))
        }

        ("addressbook", "GetContactList") | ("charMgr", "GetContactList") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let s = ctx.social_extras.lock().unwrap();
            let contacts: Vec<Vec<PyRep>> = s
                .contacts
                .get(&char_id)
                .map(|m| {
                    m.iter()
                        .map(|(&cid, &(standing, watch, mask))| {
                            vec![
                                PyRep::Integer(cid),
                                PyRep::Real(standing),
                                PyRep::Bool(watch),
                                PyRep::Integer(mask as i64),
                            ]
                        })
                        .collect()
                })
                .unwrap_or_default();
            let blocked: Vec<Vec<PyRep>> = s
                .blocked
                .get(&char_id)
                .map(|b| b.iter().map(|&bid| vec![PyRep::Integer(bid)]).collect())
                .unwrap_or_default();
            Some(Ok(PyRep::keyval([
                (
                    "addresses",
                    PyRep::rowset(
                        vec!["contactID", "relationshipID", "inWatchlist", "labelMask"],
                        contacts,
                    ),
                ),
                ("blocked", PyRep::rowset(vec!["contactID"], blocked)),
            ])))
        }

        ("addressbook", "AddContact")
        | ("addressbook", "EditContact")
        | ("charMgr", "AddContact")
        | ("charMgr", "EditContact")
        | ("charMgr", "EditContactsRelationshipID") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let contact_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let standing = args
                .get(1)
                .and_then(|a| a.as_f64())
                .unwrap_or(5.0)
                .clamp(-10.0, 10.0);
            let watch = matches!(args.get(2), Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)));
            let mask = args.get(3).and_then(|a| a.as_i64()).unwrap_or(0) as u64;
            ctx.social_extras
                .lock()
                .unwrap()
                .contacts
                .entry(char_id)
                .or_default()
                .insert(contact_id, (standing, watch, mask));
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnContactChange",
                "charid",
                vec![
                    PyRep::List(vec![PyRep::Integer(contact_id)]),
                    PyRep::Real(standing),
                ],
            );
            Some(Ok(PyRep::keyval([
                ("contactID", PyRep::Integer(contact_id)),
                ("relationshipID", PyRep::Real(standing)),
                ("inWatchlist", PyRep::Bool(watch)),
            ])))
        }

        ("addressbook", "DeleteContacts") | ("charMgr", "DeleteContacts") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let ids = extract_id_list(args.first());
            if let Some(m) = ctx.social_extras.lock().unwrap().contacts.get_mut(&char_id) {
                for id in &ids {
                    m.remove(id);
                }
            }
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnContactNoLongerContact",
                "charid",
                vec![PyRep::List(ids.into_iter().map(PyRep::Integer).collect())],
            );
            Some(Ok(PyRep::Bool(true)))
        }

        ("addressbook", "BlockOwners") | ("charMgr", "BlockOwners") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let ids = extract_id_list(args.first());
            {
                let mut s = ctx.social_extras.lock().unwrap();
                let b = s.blocked.entry(char_id).or_default();
                for &id in &ids {
                    b.insert(id);
                }
            }
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnBlockContacts",
                "charid",
                vec![PyRep::List(ids.into_iter().map(PyRep::Integer).collect())],
            );
            Some(Ok(PyRep::Bool(true)))
        }

        ("addressbook", "UnblockOwners") | ("charMgr", "UnblockOwners") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let ids = extract_id_list(args.first());
            if let Some(b) = ctx.social_extras.lock().unwrap().blocked.get_mut(&char_id) {
                for id in &ids {
                    b.remove(id);
                }
            }
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnUnblockContacts",
                "charid",
                vec![PyRep::List(ids.into_iter().map(PyRep::Integer).collect())],
            );
            Some(Ok(PyRep::Bool(true)))
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
                Some(Ok(PyRep::Bool(true)))
            } else {
                let note = ctx.social_extras
                    .lock()
                    .unwrap()
                    .char_notes
                    .get(&(char_id, item_id))
                    .cloned()
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
