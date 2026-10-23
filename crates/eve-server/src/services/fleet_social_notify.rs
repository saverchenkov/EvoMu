//! Fleet notifications (`SO-2`) in the argument layout the V24 client unpacks.
//!
//! Every emitter cites the client handler in `eve/client/script/parklife/fleetSvc.pyj` (`FleetSvc`)
//! whose signature defines the payload. Emission happens from the `fleetMgr` / `fleetObjectHandler`
//! / `fleetProxy` code paths right after the fleet state change commits to `WorldRepository`.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::FleetMemberRecord;
use eve_machonet::SessionState;
use eve_proto::PyRep;

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
        ex.invite_details.retain(|&(fid, _), _| fid != fleet_id);
        ex.invite_timestamps.retain(|&(fid, _), _| fid != fleet_id);
    } else if let Some((_, ref new_m, _)) = hand_over {
        if let Some(ad) = ctx.fleet_extras.lock().unwrap().adverts.get_mut(&fleet_id) {
            if ad.leader_id == char_id {
                ad.leader_id = new_m.character_id;
            }
        }
    }

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
    remove_character_from_all_fleets(ctx, char_id);
    remove_character_from_chat_channels(ctx, char_id, true);
}

/// Socket disconnect cleanup for a character: removes `char_id` from Local (`solarsystemid2:*`)
/// and non-fleet chat channels immediately, and starts the 120-second fleet disconnect grace window.
pub fn handle_character_disconnect(ctx: &ServerContext, char_id: i64) {
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
        let mut ex = ctx.fleet_extras.lock().unwrap();
        for fid in disbanded_fids {
            ex.adverts.remove(&fid);
            ex.fleet_names.remove(&fid);
            ex.max_sizes.remove(&fid);
            ex.auto_join_squads.remove(&fid);
        }
    }
}
