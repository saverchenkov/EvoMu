//! Fleet notifications (`SO-2`) in the argument layout the V24 client unpacks.
//!
//! Every emitter cites the client handler in `eve/client/script/parklife/fleetSvc.pyj` (`FleetSvc`)
//! whose signature defines the payload. Emission happens from the `fleetMgr` / `fleetObjectHandler`
//! / `fleetProxy` code paths right after the fleet state change commits to `WorldRepository`.

use crate::router::{cache_notifications::NotificationTarget, ServerContext};
use eve_db::FleetMemberRecord;
use eve_proto::PyRep;

use super::fleet_social_fleets::role_to_i64;

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
pub(super) fn options_keyval(free_move: bool, registered: bool, auto_join: Option<i64>) -> PyRep {
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
