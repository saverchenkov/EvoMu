use crate::router::ServerContext;
use eve_db::{ChatChannelAclRecord, ChatChannelRecord};
use eve_machonet::SessionState;
use eve_proto::BridgeRpcError;
use std::collections::HashSet;

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
            || arg == format!("resourcewars:{actual_sys}")
            || sys_info.is_some_and(|s| {
                arg == format!("constellationid:{}", s.constellation_id)
                    || arg == format!("regionid:{}", s.region_id)
            })
    };
    if !allowed {
        return Err(BridgeRpcError {
            code: "ChannelAccessDenied".into(),
            message: format!(
                "Cannot resync channel {arg} outside current solar system {actual_sys}"
            ),
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
    let chan_id = if let Some(cid) = explicit_chan.filter(|s| {
        s.starts_with("channel:")
            || s.starts_with("private:")
            || ctx.db.with_snapshot(|snap| snap.chat_channels.contains_key(*s))
    }) {
        cid.to_string()
    } else {
        let c1 = char_id.min(recipient_id);
        let c2 = char_id.max(recipient_id);
        format!("private:{c1}:{c2}")
    };

    let chan_blocked = ctx
        .social_extras
        .lock()
        .unwrap()
        .blocked_in_channel
        .get(&chan_id)
        .is_some_and(|b| b.contains(&char_id) || b.contains(&recipient_id));
    if chan_blocked {
        return Err(BridgeRpcError {
            code: "ChannelAccessDenied".into(),
            message: "Channel invite blocked by channel access list".into(),
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
            });
        }
        if let Some(chan) = snap.chat_channels.get_mut(&chan_id) {
            if !chan.members.contains(&char_id) {
                return Err(BridgeRpcError {
                    code: "ChannelAccessDenied".into(),
                    message: "Must be a member of channel to send invites".into(),
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
    let blocked_in_chan = ctx
        .social_extras
        .lock()
        .unwrap()
        .blocked_in_channel
        .get(chan_id)
        .is_some_and(|b| b.contains(&char_id));
    let (chan_opt, acl, authorized) = ctx.db.with_snapshot(|snap| {
        let chan = snap.chat_channels.get(chan_id).cloned();
        let acl = snap
            .chat_channel_acl
            .get(chan_id)
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
            let is_public_scope = chan_id.starts_with("solarsystemid2:")
                || chan_id.starts_with("constellationid:")
                || chan_id.starts_with("regionid:")
                || chan_id.starts_with("resourcewars:")
                || (!c.is_private
                    && !chan_id.starts_with("private:")
                    && !chan_id.starts_with("channel:")
                    && !chan_id.starts_with("corpid:")
                    && !chan_id.starts_with("allianceid:")
                    && !chan_id.starts_with("fleetid:")
                    && c.password.as_ref().is_none_or(|p| p.is_empty()));
            let is_org_scope = chan_id
                .strip_prefix("corpid:")
                .and_then(|s| s.parse::<i64>().ok())
                .is_some_and(|cid| ch_corp == Some(cid))
                || chan_id
                    .strip_prefix("allianceid:")
                    .and_then(|s| s.parse::<i64>().ok())
                    .is_some_and(|aid| ch_ally == Some(aid))
                || chan_id
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
        });
    };
    if !authorized {
        return Err(BridgeRpcError {
            code: "ChannelAccessDenied".into(),
            message: format!("Not authorized to inspect channel {chan_id}"),
        });
    }
    Ok((chan, acl))
}
