//! Fleet notifications (`SO-2`) in the argument layout the V24 client unpacks.
//!
//! Every emitter cites the client handler in `eve/client/script/parklife/fleetSvc.pyj` (`FleetSvc`)
//! whose signature defines the payload. Emission happens from the `fleetMgr` / `fleetObjectHandler`
//! / `fleetProxy` code paths right after the fleet state change commits to `WorldRepository`.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::FleetMemberRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use super::fleet_social_fleets::{ensure_fleet_channel_member, role_to_i64};

/// Bounded grace window (seconds) a socket-disconnected member stays in their fleet (`SO-2`).
pub const FLEET_DISCONNECT_GRACE_SECS: u64 = 120;
/// Bounded lifetime (seconds) of a pending fleet invitation before automatic expiry (`SO-2`).
pub const FLEET_INVITE_EXPIRY_SECS: u64 = 120;

/// `evefleet/const.pyj`: `fleetJobCreator = 2` (fleet boss bit in `member.job`).
pub(super) const FLEET_JOB_CREATOR: u32 = 2;
/// `evefleet/const.pyj`: `rejectFleetInviteAlreadyInFleet = 2` (`OnFleetJoinReject` reason code).
pub(super) const REJECT_INVITE_ALREADY_IN_FLEET: i64 = 2;
/// `evefleet/const.pyj`: `BROADCAST_DOWN = 1`, `BROADCAST_UP = 2` (broadcast group scope).
pub(super) const BROADCAST_DOWN: i64 = 1;
pub(super) const BROADCAST_UP: i64 = 2;

/// `evefleet/members.pyj` `GetNewMemberOptOutsKeyval(acceptsFleetWarp)`: `KeyVal(acceptsFleetWarp,
/// acceptsConduitJumps=True, acceptsFleetRegroups=True)`. `FleetMemberRecord` stores one flag, so the
/// conduit/regroup opt-outs use that function's defaults.
pub(super) fn member_opt_outs(m: &FleetMemberRecord) -> PyRep {
    PyRep::keyval([
        ("acceptsFleetWarp", PyRep::Bool(m.takes_fleet_warp)),
        ("acceptsConduitJumps", PyRep::Bool(true)),
        ("acceptsFleetRegroups", PyRep::Bool(true)),
    ])
}

/// Fleet member record as `FleetSvc.members[charID]`: `OnFleetJoin` reads `member.charID`,
/// `fleetbroadcastexports.GetRankName` reads `member.job`/`member.role`, and
/// `OnFleetMemberChanged` writes `charID`, `wingID`, `squadID`, `role`, `job`, `memberOptOuts`.
pub(super) fn member_keyval(m: &FleetMemberRecord) -> PyRep {
    PyRep::keyval([
        ("charID", PyRep::Integer(m.character_id)),
        ("wingID", m.wing_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("squadID", m.squad_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("role", PyRep::Integer(role_to_i64(m.role))),
        ("job", PyRep::Integer(m.job as i64)),
        ("memberOptOuts", member_opt_outs(m)),
    ])
}

/// Fleet options as `FleetSvc.Clear` initialises them: `KeyVal(isFreeMove, isRegistered, autoJoinSquadID)`.
pub(crate) fn options_keyval(free_move: bool, registered: bool, auto_join: Option<i64>) -> PyRep {
    PyRep::keyval([
        ("isFreeMove", PyRep::Bool(free_move)),
        ("isRegistered", PyRep::Bool(registered)),
        ("autoJoinSquadID", auto_join.map(PyRep::Integer).unwrap_or(PyRep::None)),
    ])
}

/// `FleetSvc.OnFleetJoin(member)` to every fleet member (including the joiner, whose client runs
/// `InitFleet` when `member.charID == session.charid`).
pub(super) fn emit_join(ctx: &ServerContext, fleet_id: i64, char_id: i64) {
    let member = ctx.db.with_snapshot(|snap| {
        snap.fleets.get(&fleet_id).and_then(|f| f.member_details.get(&char_id).cloned())
    });
    if let Some(m) = member {
        ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetJoin", "fleetid", vec![member_keyval(&m)]);
    }
}

/// `FleetSvc.OnFleetLeave(charID)` to the leaver (client `Clear()`s when `charID == session.charid`)
/// and to the remaining members (already removed from `WorldRepository` when this runs).
pub(super) fn emit_leave(ctx: &ServerContext, fleet_id: i64, char_id: i64) {
    let args = vec![PyRep::Integer(char_id)];
    ctx.notify(NotificationTarget::Character(char_id), "OnFleetLeave", "charid", args.clone());
    ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetLeave", "fleetid", args);
}

/// `FleetSvc.OnFleetMemberChanged(charID, fleetID, oldWingID, oldSquadID, oldRole, oldJob,
/// oldMemberOptOuts, newWingID, newSquadID, newRole, newJob, newMemberOptOuts, isOnlyMember)`.
/// The client never reads `isOnlyMember`; DERIVED: sent as "this member is the fleet's only member".
pub(super) fn emit_member_changed(
    ctx: &ServerContext,
    fleet_id: i64,
    old: &FleetMemberRecord,
    new: &FleetMemberRecord,
    is_only_member: bool,
) {
    let opt = |v: Option<i64>| v.map(PyRep::Integer).unwrap_or(PyRep::None);
    let args = vec![
        PyRep::Integer(new.character_id),
        PyRep::Integer(fleet_id),
        opt(old.wing_id),
        opt(old.squad_id),
        PyRep::Integer(role_to_i64(old.role)),
        PyRep::Integer(old.job as i64),
        member_opt_outs(old),
        opt(new.wing_id),
        opt(new.squad_id),
        PyRep::Integer(role_to_i64(new.role)),
        PyRep::Integer(new.job as i64),
        member_opt_outs(new),
        PyRep::Bool(is_only_member),
    ];
    ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetMemberChanged", "fleetid", args);
}

/// `FleetSvc.OnFleetOptionsChanged(oldOptions, options)` to the whole fleet.
pub(super) fn emit_options_changed(ctx: &ServerContext, fleet_id: i64, old: PyRep, new: PyRep) {
    ctx.notify(NotificationTarget::Fleet(fleet_id), "OnFleetOptionsChanged", "fleetid", vec![old, new]);
}

/// `FleetSvc.OnFleetMotdChanged(motd, reloadMotd)`; `reloadMotd` makes the client push the text into
/// the fleet chat window (`SetMotd`).
pub(super) fn emit_motd(ctx: &ServerContext, target: NotificationTarget, motd: &str, reload: bool) {
    let id_type = if matches!(target, NotificationTarget::Fleet(_)) { "fleetid" } else { "charid" };
    ctx.notify(target, "OnFleetMotdChanged", id_type, vec![PyRep::String(motd.to_string()), PyRep::Bool(reload)]);
}

/// Recipients of a broadcast sent with group `scope` by `sender` (`evefleet/const.pyj`
/// `BROADCAST_DOWN`/`BROADCAST_UP`/`BROADCAST_ALL`): DOWN reaches the sender's subordinates, UP the
/// sender's superiors, anything else the whole fleet. The sender always sees their own broadcast.
pub(super) fn broadcast_group_allows(scope: i64, sender: &FleetMemberRecord, member: &FleetMemberRecord) -> bool {
    if member.character_id == sender.character_id {
        return true;
    }
    let (sr, mr) = (role_to_i64(sender.role), role_to_i64(member.role));
    let in_subtree = |boss: &FleetMemberRecord, sub: &FleetMemberRecord| match role_to_i64(boss.role) {
        1 => true,
        2 => sub.wing_id == boss.wing_id,
        3 => sub.squad_id == boss.squad_id,
        _ => false,
    };
    match scope {
        BROADCAST_DOWN => mr > sr && in_subtree(sender, member),
        BROADCAST_UP => mr < sr && in_subtree(member, sender),
        _ => true,
    }
}

/// Returns `true` if `char_id` is currently online with an active character session.
pub fn is_character_online_for_fleet(ctx: &ServerContext, char_id: i64) -> bool {
    crate::services::character_contacts::is_character_online(ctx, char_id)
}

/// Returns the active `fleet_id` that `char_id` belongs to (after ticking lifecycle timers), if any.
pub fn character_active_fleet_id(ctx: &ServerContext, char_id: i64) -> Option<i64> {
    tick_fleet_lifecycle(ctx);
    ctx.db.with_snapshot(|snap| {
        snap.fleets
            .values()
            .find(|f| f.members.contains(&char_id) || f.member_details.contains_key(&char_id))
            .map(|f| f.fleet_id)
    })
}

/// Removes `char_id` from `fleet_id` (`members`, `member_details`, wing/squad commander slots,
/// and `fleetid:{fleet_id}` chat channel), promotes a successor boss if needed, or deletes the
/// fleet and its channel when empty. Emits `OnFleetLeave` and (on boss handover) `OnFleetMemberChanged`.
pub fn remove_character_from_fleet(
    ctx: &ServerContext,
    fleet_id: i64,
    char_id: i64,
) -> (bool, Option<i64>) {
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.disconnected_members.remove(&char_id);
        ex.invite_details.remove(&(fleet_id, char_id));
        ex.invite_timestamps.remove(&(fleet_id, char_id));
        ex.watchlist.remove(&char_id);
        for targets in ex.watchlist.values_mut() {
            targets.remove(&char_id);
        }
        if let Some(reqs) = ex.join_requests.get_mut(&fleet_id) {
            reqs.remove(&char_id);
        }
    }
    let (was_member, disbanded, hand_over) = ctx.db.with_mut_snapshot(|snap| {
        let mut was_in_fleet = false;
        let mut should_disband = false;
        let mut hand_over: Option<(FleetMemberRecord, FleetMemberRecord, bool)> = None;
        if let Some(fleet) = snap.fleets.get_mut(&fleet_id) {
            fleet.invites.remove(&char_id);
            let old_len = fleet.members.len();
            fleet.members.retain(|&m| m != char_id);
            if fleet.members.len() != old_len {
                was_in_fleet = true;
            }
            if fleet.member_details.remove(&char_id).is_some() {
                was_in_fleet = true;
            }
            for w in fleet.wing_details.values_mut() {
                if w.commander_id == Some(char_id) {
                    w.commander_id = None;
                }
            }
            for s in fleet.squad_details.values_mut() {
                if s.commander_id == Some(char_id) {
                    s.commander_id = None;
                }
            }
            if fleet.members.is_empty() && fleet.member_details.is_empty() {
                should_disband = true;
            } else if fleet.boss_id == char_id {
                let next_boss = fleet
                    .member_details
                    .values()
                    .min_by_key(|m| (role_to_i64(m.role), m.character_id))
                    .map(|m| m.character_id)
                    .or_else(|| fleet.members.first().copied())
                    .unwrap_or(0);
                if next_boss != 0 {
                    fleet.boss_id = next_boss;
                    let only = fleet.members.len() == 1;
                    if let Some(m) = fleet.member_details.get_mut(&next_boss) {
                        let old = m.clone();
                        m.job |= FLEET_JOB_CREATOR;
                        hand_over = Some((old, m.clone(), only));
                    }
                }
            }
        }
        let chan_key = format!("fleetid:{fleet_id}");
        if let Some(chan) = snap.chat_channels.get_mut(&chan_key) {
            chan.members.remove(&char_id);
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.fleet_id = None;
        }
        if should_disband {
            for ch in snap.characters.values_mut() {
                if ch.fleet_id == Some(fleet_id) {
                    ch.fleet_id = None;
                }
            }
            snap.fleets.remove(&fleet_id);
            snap.chat_channels.remove(&chan_key);
        }
        (was_in_fleet, should_disband, hand_over)
    });

    if disbanded {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.adverts.remove(&fleet_id);
        ex.fleet_names.remove(&fleet_id);
        ex.max_sizes.remove(&fleet_id);
        ex.auto_join_squads.remove(&fleet_id);
        ex.join_requests.remove(&fleet_id);
        ex.invite_details.retain(|&(fid, _), _| fid != fleet_id);
        ex.invite_timestamps.retain(|&(fid, _), _| fid != fleet_id);
    } else if let Some((_, ref new_m, _)) = hand_over {
        if let Some(ad) = ctx.fleet_extras.lock().unwrap().adverts.get_mut(&fleet_id) {
            if ad.leader_id == char_id {
                ad.leader_id = new_m.character_id;
            }
        }
    }
    sync_fleet_join_requests_to_db(ctx);
    sync_fleet_watchlist_to_db(ctx);

    if was_member {
        emit_leave(ctx, fleet_id, char_id);
        if let Some((old, new, only)) = &hand_over {
            emit_member_changed(ctx, fleet_id, old, new, *only);
        }
    }
    (disbanded, hand_over.map(|(_, n, _)| n.character_id))
}

/// Removes `char_id` from all fleets and pending fleet invites.
pub fn remove_character_from_all_fleets(
    ctx: &ServerContext,
    char_id: i64,
) -> Vec<(i64, bool, Option<i64>)> {
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.disconnected_members.remove(&char_id);
        ex.invite_details.retain(|&(_, cid), _| cid != char_id);
        ex.invite_timestamps.retain(|&(_, cid), _| cid != char_id);
    }
    let fleet_ids: Vec<i64> = ctx.db.with_mut_snapshot(|snap| {
        for f in snap.fleets.values_mut() {
            f.invites.remove(&char_id);
        }
        snap.fleets
            .values()
            .filter(|f| f.members.contains(&char_id) || f.member_details.contains_key(&char_id))
            .map(|f| f.fleet_id)
            .collect()
    });
    let mut out = Vec::new();
    for fid in fleet_ids {
        let (disbanded, new_boss) = remove_character_from_fleet(ctx, fid, char_id);
        out.push((fid, disbanded, new_boss));
    }
    out
}

/// Removes `char_id` from joined chat channels (`solarsystemid2:*` Local and other channels;
/// includes `fleetid:*` iff `include_fleet` is true) and emits `OnChannelLeft` and `OnLSC`
/// `"LeaveChannel"` to all remaining members of each channel.
pub fn remove_character_from_chat_channels(
    ctx: &ServerContext,
    char_id: i64,
    include_fleet: bool,
) {
    let left_channels: Vec<(String, Vec<i64>)> = ctx.db.with_mut_snapshot(|snap| {
        let mut removed = Vec::new();
        for (chan_id, chan) in snap.chat_channels.iter_mut() {
            if !include_fleet && chan_id.starts_with("fleetid:") {
                continue;
            }
            if chan.members.remove(&char_id) {
                let remaining: Vec<i64> = chan.members.iter().copied().collect();
                removed.push((chan_id.clone(), remaining));
            }
        }
        removed
    });

    if !left_channels.is_empty() {
        let mut s = ctx.social_extras.lock().unwrap();
        for (chan_id, _) in &left_channels {
            if let Some(speakers) = s.delayed_local_speakers.get_mut(chan_id) {
                speakers.remove(&char_id);
            }
        }
    }

    for (chan_id, remaining_members) in left_channels {
        let left_args = vec![PyRep::String(chan_id), PyRep::Integer(char_id)];
        for mid in remaining_members {
            ctx.notify(
                NotificationTarget::Character(mid),
                "OnChannelLeft",
                "channelid",
                left_args.clone(),
            );
        }
    }
}

/// Full character logout cleanup (`authentication.Logout` / `userSvc.UserLogOffCharacter`):
/// immediately removes `char_id` from all fleets and chat channels (including Local and Fleet).
pub fn on_character_logout(ctx: &ServerContext, char_id: i64) {
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.character_login_stats.entry(char_id).or_default();
        st.last_logout_unix = now_unix;
    });
    remove_character_from_all_fleets(ctx, char_id);
    remove_character_from_chat_channels(ctx, char_id, true);
}

/// Socket disconnect cleanup for a character: removes `char_id` from Local (`solarsystemid2:*`)
/// and non-fleet chat channels immediately, and starts the 120-second fleet disconnect grace window.
pub fn handle_character_disconnect(ctx: &ServerContext, char_id: i64) {
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.character_login_stats.entry(char_id).or_default();
        st.last_logout_unix = now_unix;
    });
    remove_character_from_chat_channels(ctx, char_id, false);
    let in_fleet = ctx.db.with_snapshot(|snap| {
        snap.fleets
            .values()
            .any(|f| f.members.contains(&char_id) || f.member_details.contains_key(&char_id))
    });
    if in_fleet {
        ctx.fleet_extras
            .lock()
            .unwrap()
            .disconnected_members
            .insert(char_id, ctx.now_unix());
    }
}

/// Handles an abrupt socket disconnect on `session`, releasing the online lock, notifying
/// contact watchers, removing the character from Local chat, and entering fleet disconnect grace.
pub fn handle_fleet_member_disconnect(ctx: &ServerContext, session: &mut SessionState) {
    if let Some(uid) = session.user_id {
        ctx.db.release_online_lock(uid, session.session_id);
    }
    if let Some(cid) = session.character_id {
        crate::services::character_skills::notify_watchlist_presence(ctx, cid, false);
        handle_character_disconnect(ctx, cid);
    }
    session.character_id = None;
    session.fleet_id = None;
}

/// Expires pending fleet invitations (`>= 120s`) and disconnected fleet members (`>= 120s`).
pub fn tick_fleet_lifecycle(ctx: &ServerContext) {
    let now = ctx.now_unix();
    let (expired_invites, expired_members) = {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        let exp_inv: Vec<(i64, i64)> = ex
            .invite_timestamps
            .iter()
            .filter(|(_, &ts)| now.saturating_sub(ts) >= FLEET_INVITE_EXPIRY_SECS)
            .map(|(&k, _)| k)
            .collect();
        for k in &exp_inv {
            ex.invite_timestamps.remove(k);
            ex.invite_details.remove(k);
        }
        let exp_mem: Vec<i64> = ex
            .disconnected_members
            .iter()
            .filter(|(_, &ts)| now.saturating_sub(ts) >= FLEET_DISCONNECT_GRACE_SECS)
            .map(|(&cid, _)| cid)
            .collect();
        for cid in &exp_mem {
            ex.disconnected_members.remove(cid);
        }
        (exp_inv, exp_mem)
    };

    if !expired_invites.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            for (fid, cid) in expired_invites {
                if let Some(fleet) = snap.fleets.get_mut(&fid) {
                    fleet.invites.remove(&cid);
                }
            }
        });
    }

    for cid in expired_members {
        remove_character_from_all_fleets(ctx, cid);
    }
}

/// Reconciles fleet state on `SelectCharacterID`:
/// - Runs `tick_fleet_lifecycle`.
/// - If `char_id` was within disconnect grace (`disconnected_members`) or already online in a fleet,
///   restores `session.fleet_id` and fleet chat channel membership.
/// - Otherwise removes any stale offline fleet membership so `GetFleetID`, `session.fleet_id`,
///   and `snap.fleets` always agree.
pub fn on_character_select(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    was_already_online: bool,
) {
    let now_unix = ctx.now_unix();
    let persisted_acct_key = ctx.db.with_mut_snapshot(|snap| {
        let st = snap.character_login_stats.entry(char_id).or_default();
        if st.last_login_unix == 0 || now_unix >= st.last_login_unix {
            st.last_login_unix = now_unix;
            st.logins_count = st.logins_count.saturating_add(1);
        }
        let cid = session.corporation_id.or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id)).unwrap_or(0);
        snap.corp_governance.get(&cid).and_then(|g| g.member_account_keys.get(&char_id)).copied()
    });
    if let Some(k) = persisted_acct_key {
        session.corp_account_key = k as i32;
    }
    tick_fleet_lifecycle(ctx);
    let within_grace = ctx
        .fleet_extras
        .lock()
        .unwrap()
        .disconnected_members
        .remove(&char_id)
        .is_some();
    let existing_fid = ctx.db.with_snapshot(|snap| {
        snap.fleets
            .values()
            .find(|f| f.members.contains(&char_id) || f.member_details.contains_key(&char_id))
            .map(|f| f.fleet_id)
    });
    if let Some(fid) = existing_fid {
        if within_grace || was_already_online {
            session.fleet_id = Some(fid);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(f) = snap.fleets.get(&fid) {
                    let boss_id = f.boss_id;
                    ensure_fleet_channel_member(&mut snap.chat_channels, fid, boss_id, char_id);
                }
            });
        } else {
            remove_character_from_all_fleets(ctx, char_id);
            session.fleet_id = None;
        }
    } else {
        session.fleet_id = None;
    }
}

/// Synchronizes `session.fleet_id` before each RPC dispatch (`dispatch.rs`): ticks lifecycle timers
/// and clears `session.fleet_id` if the character was kicked or the fleet no longer contains them.
pub fn sync_session_fleet(ctx: &ServerContext, session: &mut SessionState, char_id: i64) {
    tick_fleet_lifecycle(ctx);
    if let Some(fid) = session.fleet_id {
        let kicked_from_existing = ctx.db.with_snapshot(|snap| {
            snap.fleets
                .get(&fid)
                .is_some_and(|f| !f.member_details.contains_key(&char_id) && !f.members.contains(&char_id))
        });
        if kicked_from_existing {
            session.fleet_id = None;
        }
    }
}

/// Prunes all offline fleet members, pending invites, empty fleets, and offline Local chat members
/// on server startup/restart (`hydrate_social_state_from_db`) and daily downtime.
pub fn prune_offline_fleets_and_channels(ctx: &ServerContext) {
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        ex.disconnected_members.clear();
        ex.invite_timestamps.clear();
        ex.invite_details.clear();
    }
    let disbanded_fids: Vec<i64> = ctx.db.with_mut_snapshot(|snap| {
        let online_chars: std::collections::HashSet<i64> = snap
            .characters
            .values()
            .filter(|c| ctx.is_character_online(c.character_id, c.user_id))
            .map(|c| c.character_id)
            .collect();

        let mut to_remove = Vec::new();
        for (&fid, fleet) in snap.fleets.iter_mut() {
            fleet.invites.clear();
            fleet.members.retain(|cid| online_chars.contains(cid));
            fleet.member_details.retain(|cid, _| online_chars.contains(cid));
            for w in fleet.wing_details.values_mut() {
                if w.commander_id.is_some_and(|cid| !online_chars.contains(&cid)) {
                    w.commander_id = None;
                }
            }
            for s in fleet.squad_details.values_mut() {
                if s.commander_id.is_some_and(|cid| !online_chars.contains(&cid)) {
                    s.commander_id = None;
                }
            }
            if fleet.members.is_empty() && fleet.member_details.is_empty() {
                to_remove.push(fid);
            } else if !online_chars.contains(&fleet.boss_id) {
                if let Some(next_boss) = fleet
                    .member_details
                    .values()
                    .min_by_key(|m| (role_to_i64(m.role), m.character_id))
                    .map(|m| m.character_id)
                    .or_else(|| fleet.members.first().copied())
                {
                    fleet.boss_id = next_boss;
                    if let Some(m) = fleet.member_details.get_mut(&next_boss) {
                        m.job |= FLEET_JOB_CREATOR;
                    }
                }
            }
        }
        for &fid in &to_remove {
            snap.fleets.remove(&fid);
            snap.chat_channels.remove(&format!("fleetid:{fid}"));
        }
        for chan in snap.chat_channels.values_mut() {
            if chan.channel_id.starts_with("solarsystemid2:") {
                chan.members.retain(|cid| online_chars.contains(cid));
            } else if chan.channel_id.starts_with("fleetid:") {
                chan.members.retain(|cid| online_chars.contains(cid));
            }
        }
        to_remove
    });
    if !disbanded_fids.is_empty() {
        {
            let mut ex = ctx.fleet_extras.lock().unwrap();
            for fid in disbanded_fids {
                ex.adverts.remove(&fid);
                ex.fleet_names.remove(&fid);
                ex.max_sizes.remove(&fid);
                ex.auto_join_squads.remove(&fid);
                ex.join_requests.remove(&fid);
                ex.voice_enabled.remove(&fid);
                ex.loot_logging.remove(&fid);
            }
        }
        sync_fleet_join_requests_to_db(ctx);
        sync_fleet_options_to_db(ctx);
    }
}

pub fn sync_fleet_join_requests_to_db(ctx: &ServerContext) {
    let map = ctx.fleet_extras.lock().unwrap().join_requests.clone();
    if let Ok(json) = serde_json::to_string(&map) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert("fleet_join_requests".into(), json);
        });
    }
}

pub fn sync_fleet_watchlist_to_db(ctx: &ServerContext) {
    let map = ctx.fleet_extras.lock().unwrap().watchlist.clone();
    if let Ok(json) = serde_json::to_string(&map) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert("fleet_watchlist".into(), json);
        });
    }
}

pub fn sync_fleet_options_to_db(ctx: &ServerContext) {
    let pair = {
        let ex = ctx.fleet_extras.lock().unwrap();
        (ex.voice_enabled.clone(), ex.loot_logging.clone())
    };
    if let Ok(json) = serde_json::to_string(&pair) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.service_state_blobs
                .insert("fleet_options".into(), json);
        });
    }
}

pub fn get_fleet_options(ctx: &ServerContext, fleet_id: i64) -> (bool, bool) {
    {
        let ex = ctx.fleet_extras.lock().unwrap();
        if let (Some(&v), Some(&l)) = (ex.voice_enabled.get(&fleet_id), ex.loot_logging.get(&fleet_id)) {
            return (v, l);
        }
    }
    let persisted = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs.get("fleet_options").and_then(|j| {
            serde_json::from_str::<(
                std::collections::HashMap<i64, bool>,
                std::collections::HashMap<i64, bool>,
            )>(j)
            .ok()
        })
    });
    if let Some((v_map, l_map)) = persisted {
        let v_out = v_map.get(&fleet_id).copied().unwrap_or(false);
        let l_out = l_map.get(&fleet_id).copied().unwrap_or(false);
        let mut ex = ctx.fleet_extras.lock().unwrap();
        for (fid, v) in v_map {
            ex.voice_enabled.entry(fid).or_insert(v);
        }
        for (fid, l) in l_map {
            ex.loot_logging.entry(fid).or_insert(l);
        }
        return (v_out, l_out);
    }
    let ex = ctx.fleet_extras.lock().unwrap();
    (
        ex.voice_enabled.get(&fleet_id).copied().unwrap_or(false),
        ex.loot_logging.get(&fleet_id).copied().unwrap_or(false),
    )
}

pub fn ensure_fleet_watchlist_loaded(ctx: &ServerContext) {
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        if ex.watchlist_loaded {
            return;
        }
        ex.watchlist_loaded = true;
    }
    let persisted = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("fleet_watchlist")
            .and_then(|j| {
                serde_json::from_str::<
                    std::collections::HashMap<i64, std::collections::HashSet<i64>>,
                >(j)
                .ok()
            })
    });
    if let Some(map) = persisted {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        for (wid, set) in map {
            ex.watchlist.entry(wid).or_insert(set);
        }
    }
}

async fn resolve_ship_hp_ratios(
    ctx: &ServerContext,
    target_char_id: i64,
) -> (Option<i64>, f64, f64, f64) {
    let info = ctx.db.with_snapshot(|snap| {
        let c = snap.characters.get(&target_char_id)?;
        let ship_id = c.active_ship_id;
        let item_info = snap.items.get(&ship_id).map(|it| {
            (
                it.type_id,
                it.dynamic_attributes.get(&264).copied(),
                it.dynamic_attributes.get(&266).copied(),
                it.dynamic_attributes.get(&3).copied(),
            )
        });
        Some((c.solar_system_id, ship_id, item_info))
    });
    let Some((sys_id, ship_id, item_info)) = info else {
        return (None, 1.0, 1.0, 1.0);
    };
    if ship_id > 0 {
        if let Some((_, sys)) = ctx
            .systems
            .active_system_handles()
            .into_iter()
            .find(|(id, _)| *id == sys_id)
        {
            if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                let sr = (cs.shield_hp / cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0);
                let ar = (cs.armor_hp / cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0);
                let hr = (cs.structure_hp / cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0);
                return (Some(ship_id), sr, ar, hr);
            }
        }
        if let Some((type_id, s_chg, a_dmg, h_dmg)) = item_info {
            let mut cs = ctx.build_ship_combat_state(target_char_id, ship_id, type_id);
            if let Some(sc) = s_chg {
                cs.shield_hp = sc.clamp(0.0, cs.max_shield_hp);
            }
            if let Some(ad) = a_dmg {
                cs.armor_hp = (cs.max_armor_hp - ad).clamp(0.0, cs.max_armor_hp);
            }
            if let Some(hd) = h_dmg {
                cs.structure_hp = (cs.max_structure_hp - hd).clamp(0.0, cs.max_structure_hp);
            }
            crate::sp6_lifecycle::apply_persisted_hp_to_combat_state(ctx, ship_id, &mut cs);
            let sr = (cs.shield_hp / cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0);
            let ar = (cs.armor_hp / cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0);
            let hr = (cs.structure_hp / cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0);
            return (Some(ship_id), sr, ar, hr);
        }
    }
    (None, 1.0, 1.0, 1.0)
}

pub fn get_fleet_join_requests(ctx: &ServerContext, fleet_id: i64) -> std::collections::HashSet<i64> {
    {
        let ex = ctx.fleet_extras.lock().unwrap();
        if let Some(set) = ex.join_requests.get(&fleet_id) {
            return set.clone();
        }
    }
    let persisted = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("fleet_join_requests")
            .and_then(|j| {
                serde_json::from_str::<
                    std::collections::HashMap<i64, std::collections::HashSet<i64>>,
                >(j)
                .ok()
            })
    });
    if let Some(map) = persisted {
        let out = map.get(&fleet_id).cloned().unwrap_or_default();
        let mut ex = ctx.fleet_extras.lock().unwrap();
        for (fid, set) in map {
            ex.join_requests.entry(fid).or_insert(set);
        }
        return out;
    }
    std::collections::HashSet::new()
}

pub fn build_fleet_state_keyval(
    ctx: &ServerContext,
    fleet_id: Option<i64>,
    target_char_id: i64,
    shield_ratio: f64,
    armor_ratio: f64,
    hull_ratio: f64,
) -> PyRep {
    let (tags_dict, auto_join) = {
        let ex = ctx.fleet_extras.lock().unwrap();
        let tags = fleet_id
            .and_then(|fid| ex.target_tags.get(&fid))
            .map(|m| {
                m.iter()
                    .map(|(&tid, tag)| (PyRep::Integer(tid), PyRep::String(tag.clone())))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let aj = fleet_id
            .and_then(|fid| ex.auto_join_squads.get(&fid))
            .copied()
            .flatten();
        (tags, aj)
    };
    let (free_move, registered) = fleet_id
        .and_then(|fid| {
            ctx.db
                .with_snapshot(|snap| snap.fleets.get(&fid).map(|f| (f.free_move, f.is_registered)))
        })
        .unwrap_or((false, false));
    PyRep::keyval([
        ("targetTags", PyRep::Dict(tags_dict)),
        ("options", options_keyval(free_move, registered, auto_join)),
        ("characterID", PyRep::Integer(target_char_id)),
        ("shieldRatio", PyRep::Real(shield_ratio)),
        ("armorRatio", PyRep::Real(armor_ratio)),
        ("hullRatio", PyRep::Real(hull_ratio)),
        (
            "damageState",
            PyRep::Dict(vec![(
                PyRep::Integer(target_char_id),
                PyRep::Tuple(vec![
                    PyRep::Real(shield_ratio),
                    PyRep::Real(armor_ratio),
                    PyRep::Real(hull_ratio),
                ]),
            )]),
        ),
    ])
}

/// Streams damage updates (`OnDamageStateChange` and `OnFleetStateChange`) to characters watching `target_char_id`.
pub fn stream_fleet_watchlist_damage(
    ctx: &ServerContext,
    target_char_id: i64,
    shield_ratio: f64,
    armor_ratio: f64,
    hull_ratio: f64,
) {
    ensure_fleet_watchlist_loaded(ctx);
    let watchers: Vec<i64> = {
        let ex = ctx.fleet_extras.lock().unwrap();
        ex.watchlist
            .iter()
            .filter_map(|(&watcher_id, targets)| {
                if targets.contains(&target_char_id) {
                    Some(watcher_id)
                } else {
                    None
                }
            })
            .collect()
    };
    if watchers.is_empty() {
        return;
    }
    let (fleet_id, target_ship_id, valid_watchers) = ctx.db.with_snapshot(|snap| {
        let ship_id = snap
            .characters
            .get(&target_char_id)
            .map(|c| c.active_ship_id)
            .filter(|&id| id > 0);
        let fid_opt = snap
            .fleets
            .values()
            .find(|f| f.members.contains(&target_char_id) || f.member_details.contains_key(&target_char_id))
            .map(|f| f.fleet_id);
        let valid: Vec<i64> = if let Some(fid) = fid_opt {
            if let Some(f) = snap.fleets.get(&fid) {
                watchers
                    .into_iter()
                    .filter(|wid| f.members.contains(wid) || f.member_details.contains_key(wid))
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        (fid_opt, ship_id, valid)
    });
    if valid_watchers.is_empty() {
        return;
    }
    let fleet_state = build_fleet_state_keyval(
        ctx,
        fleet_id,
        target_char_id,
        shield_ratio,
        armor_ratio,
        hull_ratio,
    );
    for watcher_id in valid_watchers {
        if let Some(ship_id) = target_ship_id {
            let _ = ctx.notify_damage_state_change(
                NotificationTarget::Character(watcher_id),
                ship_id,
                shield_ratio,
                armor_ratio,
                hull_ratio,
            );
        }
        ctx.notify(
            NotificationTarget::Character(watcher_id),
            "OnFleetStateChange",
            "charid",
            vec![fleet_state.clone()],
        );
    }
}

pub fn build_wings_dict(fleet: &eve_db::FleetRecord) -> Vec<(PyRep, PyRep)> {
    let mut wings_dict = Vec::new();
    for &wid in &fleet.wings {
        if let Some(w) = fleet.wing_details.get(&wid) {
            let mut squad_dict = Vec::new();
            for &sid in &w.squad_ids {
                if let Some(s) = fleet.squad_details.get(&sid) {
                    squad_dict.push((
                        PyRep::Integer(s.squad_id),
                        PyRep::keyval([
                            ("squadID", PyRep::Integer(s.squad_id)),
                            ("wingID", PyRep::Integer(s.wing_id)),
                            ("name", PyRep::String(s.name.clone())),
                            ("commanderID", s.commander_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                        ]),
                    ));
                }
            }
            wings_dict.push((
                PyRep::Integer(w.wing_id),
                PyRep::keyval([
                    ("wingID", PyRep::Integer(w.wing_id)),
                    ("name", PyRep::String(w.name.clone())),
                    ("commanderID", w.commander_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                    ("squads", PyRep::Dict(squad_dict)),
                ]),
            ));
        }
    }
    wings_dict
}

pub async fn handle_fleet_watchlist_ops(
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
            }))
        }
    };
    ensure_fleet_watchlist_loaded(ctx);
    if method == "RegisterForDamageUpdates" {
        let mut target_ids = Vec::new();
        if let Some(first) = args.first() {
            match first {
                PyRep::List(list) | PyRep::Tuple(list) => {
                    for item in list {
                        if let Some(tid) = item.as_i64() {
                            target_ids.push(tid);
                        }
                    }
                }
                _ => {
                    if let Some(tid) = first.as_i64() {
                        target_ids.push(tid);
                    }
                }
            }
        }
        let valid = ctx.db.with_snapshot(|snap| {
            if let Some(f) = snap.fleets.get(&fleet_id) {
                target_ids.iter().all(|tid| f.members.contains(tid))
            } else {
                false
            }
        });
        if !valid {
            return Some(Err(BridgeRpcError {
                code: "MemberNotFound".into(),
                message: "One or more target characters are not in your fleet".into(),
            }));
        }
        {
            let mut ex = ctx.fleet_extras.lock().unwrap();
            let set = ex.watchlist.entry(char_id).or_default();
            set.clear();
            for &tid in &target_ids {
                set.insert(tid);
            }
        }
        sync_fleet_watchlist_to_db(ctx);
        for &tid in &target_ids {
            let (ship_opt, sr, ar, hr) = resolve_ship_hp_ratios(ctx, tid).await;
            if let Some(ship_id) = ship_opt {
                let _ = ctx.notify_damage_state_change(
                    NotificationTarget::Character(char_id),
                    ship_id,
                    sr,
                    ar,
                    hr,
                );
            }
            let fleet_state = build_fleet_state_keyval(ctx, Some(fleet_id), tid, sr, ar, hr);
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnFleetStateChange",
                "charid",
                vec![fleet_state],
            );
        }
        return Some(Ok(PyRep::keyval([
            ("characterID", PyRep::Integer(char_id)),
            ("watched", PyRep::Bool(!target_ids.is_empty())),
        ])));
    }
    let mut target_ids = Vec::new();
    match args.first() {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            for item in list {
                if let Some(tid) = item.as_i64() {
                    target_ids.push(tid);
                }
            }
        }
        Some(first) => {
            if let Some(tid) = first.as_i64() {
                target_ids.push(tid);
            }
        }
        None => target_ids.push(char_id),
    }
    let mut fav_ids = Vec::new();
    if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.get(1) {
        for item in list {
            if let Some(fid) = item.as_i64() {
                fav_ids.push(fid);
            }
        }
    }
    let in_fleet = !target_ids.is_empty()
        && ctx.db.with_snapshot(|snap| {
            snap.fleets
                .get(&fleet_id)
                .map(|f| {
                    target_ids.iter().all(|tid| f.members.contains(tid))
                        && fav_ids.iter().all(|fid| f.members.contains(fid))
                })
                .unwrap_or(false)
        });
    if !in_fleet {
        return Some(Err(BridgeRpcError {
            code: "MemberNotFound".into(),
            message: "Target character is not in your fleet".into(),
        }));
    }
    let watched = method != "RemoveFromWatchlist";
    {
        let mut ex = ctx.fleet_extras.lock().unwrap();
        let set = ex.watchlist.entry(char_id).or_default();
        for &fid in &fav_ids {
            if watched || !target_ids.contains(&fid) {
                set.insert(fid);
            }
        }
        for &tid in &target_ids {
            if watched {
                set.insert(tid);
            } else {
                set.remove(&tid);
            }
        }
    }
    sync_fleet_watchlist_to_db(ctx);
    let mut first_ratios = (1.0, 1.0, 1.0);
    for (idx, &tid) in target_ids.iter().enumerate() {
        let (ship_opt, s_ratio, a_ratio, h_ratio) = resolve_ship_hp_ratios(ctx, tid).await;
        if idx == 0 {
            first_ratios = (s_ratio, a_ratio, h_ratio);
        }
        if watched {
            if let Some(ship_id) = ship_opt {
                let _ = ctx.notify_damage_state_change(
                    NotificationTarget::Character(char_id),
                    ship_id,
                    s_ratio,
                    a_ratio,
                    h_ratio,
                );
            }
            let fleet_state = build_fleet_state_keyval(
                ctx,
                Some(fleet_id),
                tid,
                s_ratio,
                a_ratio,
                h_ratio,
            );
            ctx.notify(
                NotificationTarget::Character(char_id),
                "OnFleetStateChange",
                "charid",
                vec![fleet_state],
            );
        }
    }
    let primary_id = target_ids.first().copied().unwrap_or(char_id);
    Some(Ok(PyRep::keyval([
        ("characterID", PyRep::Integer(primary_id)),
        ("watched", PyRep::Bool(watched)),
        ("shieldRatio", PyRep::Real(first_ratios.0)),
        ("armorRatio", PyRep::Real(first_ratios.1)),
        ("hullRatio", PyRep::Real(first_ratios.2)),
    ])))
}

pub(super) fn handle_corp_contacts_and_labels(
    ctx: &ServerContext,
    corp_id: i64,
    has_diplomat: bool,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, eve_proto::BridgeRpcError>> {
    use super::fleet_social_persistence::{
        label_ids_from_mask, label_mask_from_ids, sync_corp_contacts_esi,
    };
    use eve_proto::BridgeRpcError;
    if !has_diplomat {
        return Some(Err(BridgeRpcError {
            code: "CorpRoleDenied".into(),
            message: "Managing corporate contacts and labels requires Diplomat or Director role"
                .into(),
        }));
    }
    match method {
        "AddCorporateContact" | "EditCorporateContact" | "EditContactsRelationshipID" => {
            let standing = args
                .get(1)
                .and_then(|a| a.as_f64())
                .unwrap_or(5.0)
                .clamp(-10.0, 10.0);
            let res: Result<Vec<(i64, f64, i64)>, BridgeRpcError> =
                ctx.db.try_with_mut_snapshot(|snap| {
                    if method == "AddCorporateContact" {
                        let contact_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
                        let valid_owner = contact_id > 0
                            && (snap.characters.contains_key(&contact_id)
                                || snap.corporations.contains_key(&contact_id)
                                || snap.alliances.contains_key(&contact_id)
                                || ctx.sde.npc_corporations.contains_key(&contact_id));
                        if !valid_owner {
                            return Err(BridgeRpcError {
                                code: "OwnerNotFound".into(),
                                message: format!("Contact owner {contact_id} does not exist"),
                            });
                        }
                        let ctype = if snap.alliances.contains_key(&contact_id) {
                            "alliance"
                        } else if snap.corporations.contains_key(&contact_id)
                            || ctx.sde.npc_corporations.contains_key(&contact_id)
                        {
                            "corporation"
                        } else {
                            "character"
                        };
                        let list = snap.corp_contacts.entry(corp_id).or_default();
                        if list.iter().any(|c| c.contact_id == contact_id) {
                            return Err(BridgeRpcError {
                                code: "ContactAlreadyExists".into(),
                                message: format!("Contact {contact_id} already exists"),
                            });
                        }
                        let label_mask = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0) as u64;
                        list.push(eve_db::SocialContactRecord {
                            contact_id,
                            contact_type: ctype.into(),
                            standing,
                            is_watched: false,
                            is_blocked: false,
                            label_ids: label_ids_from_mask(label_mask),
                        });
                        list.sort_by_key(|c| c.contact_id);
                        Ok(vec![(contact_id, standing, label_mask as i64)])
                    } else if method == "EditCorporateContact" {
                        let contact_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
                        let list = snap.corp_contacts.entry(corp_id).or_default();
                        let mask_opt = args.get(2).and_then(|a| a.as_i64());
                        if let Some(entry) = list.iter_mut().find(|c| c.contact_id == contact_id) {
                            entry.standing = standing;
                            if let Some(m) = mask_opt {
                                entry.label_ids = label_ids_from_mask(m as u64);
                            }
                            let mask = label_mask_from_ids(&entry.label_ids);
                            Ok(vec![(contact_id, standing, mask)])
                        } else if list.is_empty() && contact_id > 0 && contact_id < 99_000_000 {
                            let m = mask_opt.unwrap_or(0) as u64;
                            list.push(eve_db::SocialContactRecord {
                                contact_id,
                                contact_type: "character".into(),
                                standing,
                                is_watched: false,
                                is_blocked: false,
                                label_ids: label_ids_from_mask(m),
                            });
                            Ok(vec![(contact_id, standing, m as i64)])
                        } else {
                            Err(BridgeRpcError {
                                code: "ContactNotFound".into(),
                                message: format!("Contact {contact_id} not found"),
                            })
                        }
                    } else {
                        let contact_ids: Vec<i64> = match args.first() {
                            Some(PyRep::List(items) | PyRep::Tuple(items)) => {
                                items.iter().filter_map(|v| v.as_i64()).collect()
                            }
                            Some(PyRep::Integer(id)) => vec![*id],
                            _ => Vec::new(),
                        };
                        if contact_ids.is_empty() {
                            return Err(BridgeRpcError {
                                code: "ContactNotFound".into(),
                                message: "No contacts specified".into(),
                            });
                        }
                        let list = snap.corp_contacts.entry(corp_id).or_default();
                        for &cid in &contact_ids {
                            if !list.iter().any(|c| c.contact_id == cid) {
                                return Err(BridgeRpcError {
                                    code: "ContactNotFound".into(),
                                    message: format!("Contact {cid} not found"),
                                });
                            }
                        }
                        let mut updated = Vec::new();
                        for cid in contact_ids {
                            if let Some(entry) = list.iter_mut().find(|c| c.contact_id == cid) {
                                entry.standing = standing;
                                updated.push((cid, standing, label_mask_from_ids(&entry.label_ids)));
                            }
                        }
                        Ok(updated)
                    }
                });
            let updated = match res {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            sync_corp_contacts_esi(ctx, corp_id);
            let mut updates_rep = Vec::new();
            for &(cid, st, mask) in &updated {
                ctx.notify(
                    NotificationTarget::Corporation(corp_id),
                    "OnSetCorpStanding",
                    "corpid",
                    vec![PyRep::Integer(cid), PyRep::Real(st)],
                );
                updates_rep.push(PyRep::keyval([
                    ("contactID", PyRep::Integer(cid)),
                    ("standing", PyRep::Real(st)),
                    ("labelMask", PyRep::Integer(mask)),
                ]));
            }
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnOrganizationContactsUpdated",
                "corpid",
                vec![PyRep::Integer(corp_id), PyRep::List(updates_rep)],
            );
            let (first_cid, first_st, first_mask) = updated[0];
            Some(Ok(PyRep::keyval([
                ("contactID", PyRep::Integer(first_cid)),
                ("relationshipID", PyRep::Real(first_st)),
                ("labelMask", PyRep::Integer(first_mask)),
            ])))
        }
        "DeleteLabel" => {
            let lid = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let labels = snap.corp_contact_labels.entry(corp_id).or_default();
                let before = labels.len();
                labels.retain(|(id, _, _)| *id != lid);
                if labels.len() == before {
                    return Err(BridgeRpcError {
                        code: "LabelNotFound".into(),
                        message: format!("Contact label {lid} not found"),
                    });
                }
                if let Some(contacts) = snap.corp_contacts.get_mut(&corp_id) {
                    for c in contacts {
                        let mask = (label_mask_from_ids(&c.label_ids) as u64) & !(lid as u64);
                        c.label_ids = label_ids_from_mask(mask);
                    }
                }
                Ok(())
            });
            if let Err(e) = res {
                return Some(Err(e));
            }
            sync_corp_contacts_esi(ctx, corp_id);
            Some(Ok(PyRep::keyval([
                ("labelID", PyRep::Integer(lid)),
                ("deleted", PyRep::Bool(true)),
            ])))
        }
        _ => {
            let (lid, contact_ids): (i64, Vec<i64>) = match (args.first(), args.get(1)) {
                (Some(PyRep::List(items) | PyRep::Tuple(items)), _) => (
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(1),
                    items.iter().filter_map(|v| v.as_i64()).collect(),
                ),
                (Some(a0), Some(PyRep::List(items) | PyRep::Tuple(items))) => (
                    a0.as_i64().unwrap_or(1),
                    items.iter().filter_map(|v| v.as_i64()).collect(),
                ),
                _ => {
                    let a0 = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
                    let a1 = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
                    if a0 >= 1_000_000 && a1 > 0 && a1 < 1_000_000 {
                        (a1, vec![a0])
                    } else {
                        (a0, vec![a1])
                    }
                }
            };
            let res: Result<Vec<(i64, f64, i64)>, BridgeRpcError> =
                ctx.db.try_with_mut_snapshot(|snap| {
                    if contact_ids.is_empty() {
                        return Err(BridgeRpcError {
                            code: "ContactNotFound".into(),
                            message: "No contacts specified".into(),
                        });
                    }
                    let contacts = snap.corp_contacts.entry(corp_id).or_default();
                    for &cid in &contact_ids {
                        if !contacts.iter().any(|c| c.contact_id == cid) {
                            return Err(BridgeRpcError {
                                code: "ContactNotFound".into(),
                                message: format!("Contact {cid} not found"),
                            });
                        }
                    }
                    let mut updated = Vec::new();
                    for cid in contact_ids {
                        if let Some(entry) = contacts.iter_mut().find(|c| c.contact_id == cid) {
                            let mut mask = label_mask_from_ids(&entry.label_ids) as u64;
                            if method == "AssignLabels" {
                                mask |= lid as u64;
                            } else {
                                mask &= !(lid as u64);
                            }
                            entry.label_ids = label_ids_from_mask(mask);
                            updated.push((
                                cid,
                                entry.standing,
                                label_mask_from_ids(&entry.label_ids),
                            ));
                        }
                    }
                    contacts.sort_by_key(|c| c.contact_id);
                    Ok(updated)
                });
            let updated = match res {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            sync_corp_contacts_esi(ctx, corp_id);
            let updates_rep: Vec<PyRep> = updated
                .into_iter()
                .map(|(cid, st, mask)| {
                    PyRep::keyval([
                        ("contactID", PyRep::Integer(cid)),
                        ("standing", PyRep::Real(st)),
                        ("labelMask", PyRep::Integer(mask)),
                    ])
                })
                .collect();
            ctx.notify(
                NotificationTarget::Corporation(corp_id),
                "OnOrganizationContactsUpdated",
                "corpid",
                vec![PyRep::Integer(corp_id), PyRep::List(updates_rep)],
            );
            Some(Ok(PyRep::keyval([("labelID", PyRep::Integer(lid))])))
        }
    }
}
