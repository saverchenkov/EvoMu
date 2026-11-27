use crate::router::ServerContext;
use eve_db::{ChatChannelAclRecord, ChatChannelRecord};
use eve_machonet::SessionState;
use eve_proto::BridgeRpcError;
use std::collections::HashSet;

pub(crate) fn format_xmpp_channel_wire_id(internal: &str) -> String {
    if let Some(rest) = internal.strip_prefix("solarsystemid2:") {
        format!("local_{rest}")
    } else if let Some(rest) = internal.strip_prefix("corpid:") {
        format!("corp_{rest}")
    } else if let Some(rest) = internal.strip_prefix("allianceid:") {
        format!("alliance_{rest}")
    } else if let Some(rest) = internal.strip_prefix("fleetid:") {
        format!("fleet_{rest}")
    } else if let Some(rest) = internal.strip_prefix("channel:") {
        format!("player_{rest}")
    } else {
        internal.replace(':', "_")
    }
}

pub(crate) fn xmpp_alias_to_channel_key(localpart: &str) -> Option<String> {
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

pub(crate) fn find_existing_channel_key(
    snap: &eve_db::WorldStateSnapshot,
    chan_id: &str,
) -> Option<String> {
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
    if let Some(candidate) = localpart
        .strip_prefix("player_")
        .or_else(|| localpart.strip_prefix("PLAYER_"))
        .or_else(|| localpart.strip_prefix("channel_"))
        .or_else(|| localpart.strip_prefix("CHANNEL_"))
        .map(|slug| format!("channel:{slug}"))
        .or_else(|| xmpp_alias_to_channel_key(localpart))
    {
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

pub(crate) fn resolve_channel_key(snap: &eve_db::WorldStateSnapshot, chan_id: &str) -> String {
    if let Some(existing) = find_existing_channel_key(snap, chan_id) {
        return existing;
    }
    let localpart = chan_id.split('@').next().unwrap_or(chan_id);
    xmpp_alias_to_channel_key(localpart).unwrap_or_else(|| chan_id.to_string())
}

pub(crate) fn validate_system_channel_resync(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    method: &str,
    explicit_arg: Option<&str>,
) -> Result<(), BridgeRpcError> {
    let Some(arg) = explicit_arg else {
        return Ok(());
    };
    let actual_sys = session.solar_system_id.unwrap_or_else(|| {
        ctx.db
            .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
            .unwrap_or(0)
    });
    let sys_info = ctx.sde.solar_systems.get(&actual_sys);
    let allowed = if method == "EnsureResourceWarsChannelExists" {
        arg == format!("resourcewars:{actual_sys}")
    } else {
        arg == format!("solarsystemid2:{actual_sys}")
            || arg == format!("local_{actual_sys}")
            || arg == format!("resourcewars:{actual_sys}")
            || sys_info.is_some_and(|s| {
                arg == format!("constellationid:{}", s.constellation_id)
                    || arg == format!("constellation_{}", s.constellation_id)
                    || arg == format!("regionid:{}", s.region_id)
                    || arg == format!("region_{}", s.region_id)
            })
    };
    if !allowed {
        return Err(BridgeRpcError {
            code: "ChannelAccessDenied".into(),
            message: format!(
                "Cannot resync channel {arg} outside current solar system {actual_sys}"
            ),
            client_error: None,
        });
    }
    Ok(())
}

pub(crate) fn validate_and_apply_channel_invite(
    ctx: &ServerContext,
    char_id: i64,
    recipient_id: i64,
    explicit_chan: Option<&str>,
) -> Result<String, BridgeRpcError> {
    let chan_id = if let Some(cid) = explicit_chan.and_then(|s| {
        ctx.db
            .with_snapshot(|snap| find_existing_channel_key(snap, s))
            .or_else(|| {
                (s.starts_with("channel:") || s.starts_with("private:")).then(|| s.to_string())
            })
    }) {
        cid
    } else {
        let c1 = char_id.min(recipient_id);
        let c2 = char_id.max(recipient_id);
        format!("private:{c1}:{c2}")
    };

    let chan_blocked = ctx
        .social_extras
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .blocked_in_channel
        .get(&chan_id)
        .is_some_and(|b| b.contains(&char_id) || b.contains(&recipient_id));
    if chan_blocked {
        return Err(BridgeRpcError {
            code: "ChannelAccessDenied".into(),
            message: "Channel invite blocked by channel access list".into(),
            client_error: None,
        });
    }

    ctx.db.try_with_mut_snapshot(|snap| {
        let acl = snap
            .chat_channel_acl
            .get(&chan_id)
            .cloned()
            .unwrap_or_default();
        if acl.deny_list.contains(&char_id) || acl.deny_list.contains(&recipient_id) {
            return Err(BridgeRpcError {
                code: "ChannelAccessDenied".into(),
                message: "Channel invite rejected by channel deny list".into(),
                client_error: None,
            });
        }
        if let Some(chan) = snap.chat_channels.get_mut(&chan_id) {
            if !chan.members.contains(&char_id) {
                return Err(BridgeRpcError {
                    code: "ChannelAccessDenied".into(),
                    message: "Must be a member of channel to send invites".into(),
                    client_error: None,
                });
            }
            let has_invite_priv = chan.owner_id == Some(char_id)
                || chan.operators.contains(&char_id)
                || (!chan.is_private
                    && chan.password.as_ref().is_none_or(|p| p.is_empty())
                    && acl.allow_list.contains(&char_id));
            if !has_invite_priv {
                return Err(BridgeRpcError {
                    code: "ChannelAccessDenied".into(),
                    message: "Insufficient permissions to invite into channel".into(),
                    client_error: None,
                });
            }
            chan.members.insert(recipient_id);
        } else {
            snap.chat_channels.insert(
                chan_id.clone(),
                ChatChannelRecord {
                    channel_id: chan_id.clone(),
                    name: format!("Private Chat ({char_id}, {recipient_id})"),
                    motd: String::new(),
                    owner_id: Some(char_id),
                    members: HashSet::from([char_id, recipient_id]),
                    is_system_local: false,
                    solar_system_id: None,
                    is_private: true,
                    operators: HashSet::from([char_id]),
                    password: None,
                    is_delayed_local: false,
                },
            );
        }
        Ok(())
    })?;
    Ok(chan_id)
}

pub(crate) fn authorize_and_fetch_channel_members(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    chan_id: &str,
) -> Result<(ChatChannelRecord, ChatChannelAclRecord), BridgeRpcError> {
    let chan_key = ctx
        .db
        .with_snapshot(|snap| resolve_channel_key(snap, chan_id));
    let blocked_in_chan = {
        let extras = ctx.social_extras.lock().unwrap_or_else(|e| e.into_inner());
        extras
            .blocked_in_channel
            .get(chan_id)
            .is_some_and(|b| b.contains(&char_id))
            || extras
                .blocked_in_channel
                .get(&chan_key)
                .is_some_and(|b| b.contains(&char_id))
    };
    let (chan_opt, acl, authorized) = ctx.db.with_snapshot(|snap| {
        let chan = snap.chat_channels.get(&chan_key).cloned();
        let acl = snap
            .chat_channel_acl
            .get(&chan_key)
            .cloned()
            .unwrap_or_default();
        let auth = chan.as_ref().is_some_and(|c| {
            if blocked_in_chan || acl.deny_list.contains(&char_id) {
                return false;
            }
            let ch_rec = snap.characters.get(&char_id);
            let ch_corp = session
                .corporation_id
                .or_else(|| ch_rec.map(|r| r.corporation_id));
            let ch_ally = session
                .alliance_id
                .or_else(|| ch_rec.and_then(|r| r.alliance_id));
            let is_public_scope = chan_key.starts_with("solarsystemid2:")
                || chan_key.starts_with("constellationid:")
                || chan_key.starts_with("regionid:")
                || chan_key.starts_with("resourcewars:")
                || (!c.is_private
                    && !chan_key.starts_with("private:")
                    && !chan_key.starts_with("channel:")
                    && !chan_key.starts_with("corpid:")
                    && !chan_key.starts_with("allianceid:")
                    && !chan_key.starts_with("fleetid:")
                    && c.password.as_ref().is_none_or(|p| p.is_empty()));
            let is_org_scope = chan_key
                .strip_prefix("corpid:")
                .and_then(|s| s.parse::<i64>().ok())
                .is_some_and(|cid| ch_corp == Some(cid))
                || chan_key
                    .strip_prefix("allianceid:")
                    .and_then(|s| s.parse::<i64>().ok())
                    .is_some_and(|aid| ch_ally == Some(aid))
                || chan_key
                    .strip_prefix("fleetid:")
                    .and_then(|s| s.parse::<i64>().ok())
                    .is_some_and(|fid| {
                        session.fleet_id == Some(fid)
                            || snap
                                .fleets
                                .get(&fid)
                                .is_some_and(|f| f.members.contains(&char_id))
                    });
            let is_member = c.members.contains(&char_id)
                || c.owner_id == Some(char_id)
                || c.operators.contains(&char_id);
            is_public_scope || is_org_scope || is_member
        });
        (chan, acl, auth)
    });
    let Some(chan) = chan_opt else {
        return Err(BridgeRpcError {
            code: "ChannelNotFound".into(),
            message: format!("Channel {chan_id} does not exist"),
            client_error: None,
        });
    };
    if !authorized {
        return Err(BridgeRpcError {
            code: "ChannelAccessDenied".into(),
            message: format!("Not authorized to inspect channel {chan_id}"),
            client_error: None,
        });
    }
    Ok((chan, acl))
}

#[cfg(test)]
#[path = "fleet_social_chat_acl_helpers_cov_unit_tests.rs"]
mod fleet_social_chat_acl_helpers_cov_unit_tests;

