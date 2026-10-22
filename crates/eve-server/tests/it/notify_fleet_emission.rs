//! Fleet notification emission (`SO-2`): every notification below is produced by the real
//! `fleetMgr` / `fleetObjectHandler` / `fleetProxy` RPC that commits the fleet change, and is
//! asserted on each recipient's queue with the argument layout of the V24 client handler in
//! `eve/client/script/parklife/fleetSvc.pyj` (`FleetSvc.On…`), plus absence for non-recipients.

use crate::common;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::{cache_notifications::PendingMachoNotification, ServerContext};
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    s
}

/// Two seeded pilots plus a freshly created third pilot (never in the fleet unless invited).
async fn three_pilots(ctx: &ServerContext) -> (SessionState, SessionState, SessionState, i64) {
    let s1 = pilot(ctx, 501, "demo", "demo-secret", P1).await;
    let s2 = pilot(ctx, 502, "other", "other-secret", P2).await;
    let uid = ctx.db.provision_player_account("fleetp3", "fleetp3-secret");
    let p3 = ctx.db.create_character(uid, "Fleet Pilot Three", 1, 1, 11, 17, &ctx.sde).expect("create p3").character_id;
    let s3 = pilot(ctx, 503, "fleetp3", "fleetp3-secret", p3).await;
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }
    (s1, s2, s3, p3)
}

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {} {}", e.code, e.message))
}

/// Args of every `event` notification queued for `char_id` (drains the queue).
fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n: &PendingMachoNotification| n.event == event)
        .map(|n| n.args)
        .collect()
}

fn kv(pairs: &[(&str, PyRep)]) -> PyRep {
    PyRep::keyval(pairs.iter().cloned())
}

fn opt_outs() -> PyRep {
    kv(&[
        ("acceptsFleetWarp", PyRep::Bool(true)),
        ("acceptsConduitJumps", PyRep::Bool(true)),
        ("acceptsFleetRegroups", PyRep::Bool(true)),
    ])
}

fn member(char_id: i64, wing: i64, squad: i64, role: i64, job: i64) -> PyRep {
    kv(&[
        ("charID", PyRep::Integer(char_id)),
        ("wingID", PyRep::Integer(wing)),
        ("squadID", PyRep::Integer(squad)),
        ("role", PyRep::Integer(role)),
        ("job", PyRep::Integer(job)),
        ("memberOptOuts", opt_outs()),
    ])
}

fn int(rep: &PyRep, key: &str) -> i64 {
    rep.dict_get(key).and_then(|v| v.as_i64()).unwrap_or_else(|| panic!("missing {key} in {rep:?}"))
}

#[tokio::test]
async fn test_fleet_structure_membership_notifications() {
    // parity: notify:OnFleetJoin, notify:OnFleetWingAdded, notify:OnFleetWingNameChanged, notify:OnFleetSquadAdded, notify:OnFleetSquadNameChanged, notify:OnFleetSquadDeleted, notify:OnFleetWingDeleted, notify:OnFleetMemberChanged, notify:OnFleetLeave, notify:OnFleetDisbanded
    eve_testkit::attest(&[
        "notify:OnFleetJoin",
        "notify:OnFleetWingAdded",
        "notify:OnFleetWingNameChanged",
        "notify:OnFleetSquadAdded",
        "notify:OnFleetSquadNameChanged",
        "notify:OnFleetSquadDeleted",
        "notify:OnFleetWingDeleted",
        "notify:OnFleetMemberChanged",
        "notify:OnFleetLeave",
        "notify:OnFleetDisbanded",
    ]);
    let ctx = ServerContext::new();
    let (mut s1, mut s2, _s3, p3) = three_pilots(&ctx).await;
    const FH: &str = "fleetObjectHandler";

    // CreateFleet: the creator's own OnFleetJoin(member) carries the boss job bit (fleetJobCreator = 2).
    call(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await;
    let fid = s1.fleet_id.expect("fleet id");
    assert_eq!(events(&ctx, P1, "OnFleetJoin"), vec![vec![member(P1, 1, 1, 1, 2)]]);
    assert!(events(&ctx, P2, "OnFleetJoin").is_empty(), "non-member must not get OnFleetJoin");

    // Wing / squad structure: OnFleetWingAdded(wingID), OnFleetWingNameChanged(wingID, name),
    // OnFleetSquadAdded(wingID, squadID), OnFleetSquadNameChanged(squadID, name).
    let wid = int(&call(&ctx, &mut s1, FH, "CreateWing", &[PyRep::String("Recon".into())]).await, "wingID");
    assert_eq!(events(&ctx, P1, "OnFleetWingAdded"), vec![vec![PyRep::Integer(wid)]]);
    call(&ctx, &mut s1, FH, "ChangeWingName", &[PyRep::Integer(wid), PyRep::String("Scouts".into())]).await;
    assert_eq!(events(&ctx, P1, "OnFleetWingNameChanged"), vec![vec![PyRep::Integer(wid), PyRep::String("Scouts".into())]]);
    let sid = int(&call(&ctx, &mut s1, FH, "CreateSquad", &[PyRep::Integer(wid), PyRep::String("Tackle".into())]).await, "squadID");
    assert_eq!(events(&ctx, P1, "OnFleetSquadAdded"), vec![vec![PyRep::Integer(wid), PyRep::Integer(sid)]]);
    call(&ctx, &mut s1, FH, "ChangeSquadName", &[PyRep::Integer(sid), PyRep::String("Tacklers".into())]).await;
    assert_eq!(events(&ctx, P1, "OnFleetSquadNameChanged"), vec![vec![PyRep::Integer(sid), PyRep::String("Tacklers".into())]]);

    // Invite + AcceptInvite: OnFleetJoin(member) reaches both members, not the outsider.
    call(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2)]).await;
    call(&ctx, &mut s2, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await;
    let joined = vec![vec![member(P2, 1, 1, 4, 0)]];
    assert_eq!(events(&ctx, P1, "OnFleetJoin"), joined);
    assert_eq!(events(&ctx, P2, "OnFleetJoin"), joined);
    assert!(events(&ctx, p3, "OnFleetJoin").is_empty());

    // MoveMember to squad commander: the 13-argument OnFleetMemberChanged to every member.
    call(&ctx, &mut s1, FH, "MoveMember", &[PyRep::Integer(P2), PyRep::Integer(wid), PyRep::Integer(sid), PyRep::Integer(3)]).await;
    let changed = vec![vec![
        PyRep::Integer(P2),
        PyRep::Integer(fid),
        PyRep::Integer(1),
        PyRep::Integer(1),
        PyRep::Integer(4),
        PyRep::Integer(0),
        opt_outs(),
        PyRep::Integer(wid),
        PyRep::Integer(sid),
        PyRep::Integer(3),
        PyRep::Integer(0),
        opt_outs(),
        PyRep::Bool(false),
    ]];
    assert_eq!(events(&ctx, P1, "OnFleetMemberChanged"), changed);
    assert_eq!(events(&ctx, P2, "OnFleetMemberChanged"), changed);
    assert!(events(&ctx, p3, "OnFleetMemberChanged").is_empty());

    // Deleting an empty squad and wing: OnFleetSquadDeleted(squadID), OnFleetWingDeleted(wingID) to both members.
    let tmp_w = int(&call(&ctx, &mut s1, FH, "CreateWing", &[PyRep::String("Temp".into())]).await, "wingID");
    let tmp_s = int(&call(&ctx, &mut s1, FH, "CreateSquad", &[PyRep::Integer(tmp_w), PyRep::String("Temp".into())]).await, "squadID");
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    call(&ctx, &mut s1, FH, "DeleteSquad", &[PyRep::Integer(tmp_s)]).await;
    call(&ctx, &mut s1, FH, "DeleteWing", &[PyRep::Integer(tmp_w)]).await;
    for c in [P1, P2] {
        let q = ctx.drain_notifications_for_char(c);
        let get = |e: &str| q.iter().filter(|n| n.event == e).map(|n| n.args.clone()).collect::<Vec<_>>();
        assert_eq!(get("OnFleetSquadDeleted"), vec![vec![PyRep::Integer(tmp_s)]]);
        assert_eq!(get("OnFleetWingDeleted"), vec![vec![PyRep::Integer(tmp_w)]]);
    }

    // LeaveFleet: OnFleetLeave(charID) to the leaver and to the remaining member.
    call(&ctx, &mut s2, FH, "LeaveFleet", &[]).await;
    assert_eq!(events(&ctx, P2, "OnFleetLeave"), vec![vec![PyRep::Integer(P2)]]);
    assert_eq!(events(&ctx, P1, "OnFleetLeave"), vec![vec![PyRep::Integer(P2)]]);
    assert!(events(&ctx, p3, "OnFleetLeave").is_empty());

    // DisbandFleet: OnFleetDisbanded(charIDs) with every member ID, to every member; then the fleet is gone.
    call(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2)]).await;
    call(&ctx, &mut s2, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await;
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    call(&ctx, &mut s1, FH, "DisbandFleet", &[]).await;
    let ids = vec![vec![PyRep::List(vec![PyRep::Integer(P1), PyRep::Integer(P2)])]];
    assert_eq!(events(&ctx, P1, "OnFleetDisbanded"), ids);
    assert_eq!(events(&ctx, P2, "OnFleetDisbanded"), ids);
    assert!(events(&ctx, p3, "OnFleetDisbanded").is_empty());
    assert!(ctx.db.with_snapshot(|snap| !snap.fleets.contains_key(&fid)));
}

#[tokio::test]
async fn test_fleet_motd_options_requests_broadcast_notifications() {
    // parity: notify:OnFleetMotdChanged, notify:OnFleetOptionsChanged, notify:OnFleetJoinReject, notify:OnFleetJoinRequest, notify:OnFleetJoinRejected, notify:OnFleetBroadcast, rpc:fleetObjectHandler.RejectInvite
    eve_testkit::attest(&[
        "notify:OnFleetMotdChanged",
        "notify:OnFleetOptionsChanged",
        "notify:OnFleetJoinReject",
        "notify:OnFleetJoinRequest",
        "notify:OnFleetJoinRejected",
        "notify:OnFleetBroadcast",
    ]);
    let ctx = ServerContext::new();
    let (mut s1, mut s2, mut s3, p3) = three_pilots(&ctx).await;
    const FH: &str = "fleetObjectHandler";
    call(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await;
    let fid = s1.fleet_id.expect("fleet id");
    call(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2)]).await;
    call(&ctx, &mut s2, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await;
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // SetMotdEx: OnFleetMotdChanged(motd, reloadMotd=True) to both members only; a non-boss is refused.
    call(&ctx, &mut s1, FH, "SetMotdEx", &[PyRep::String("Align to Jita".into())]).await;
    let motd = vec![vec![PyRep::String("Align to Jita".into()), PyRep::Bool(true)]];
    assert_eq!(events(&ctx, P1, "OnFleetMotdChanged"), motd);
    assert_eq!(events(&ctx, P2, "OnFleetMotdChanged"), motd);
    assert!(events(&ctx, p3, "OnFleetMotdChanged").is_empty());
    let denied = ctx.dispatch_rpc(&mut s2, FH, None, "SetMotdEx", &[PyRep::String("x".into())], &HashMap::new(), None).await;
    assert_eq!(denied.expect_err("member cannot set MOTD").code, "FleetPermissionDenied");
    assert!(events(&ctx, P1, "OnFleetMotdChanged").is_empty());

    // SetOptions(options): OnFleetOptionsChanged(oldOptions, options) with the client's options KeyVal.
    let new_opts = kv(&[("isFreeMove", PyRep::Bool(true)), ("isRegistered", PyRep::Bool(false)), ("autoJoinSquadID", PyRep::None)]);
    call(&ctx, &mut s1, FH, "SetOptions", &[new_opts.clone()]).await;
    // CreateFleet set the fleet's first squad (1) as the auto-join squad; SetOptions only changes isFreeMove.
    let opts = |free: bool| kv(&[("isFreeMove", PyRep::Bool(free)), ("isRegistered", PyRep::Bool(false)), ("autoJoinSquadID", PyRep::Integer(1))]);
    let expected = vec![vec![opts(false), opts(true)]];
    assert_eq!(events(&ctx, P1, "OnFleetOptionsChanged"), expected);
    assert_eq!(events(&ctx, P2, "OnFleetOptionsChanged"), expected);
    assert!(ctx.db.with_snapshot(|snap| snap.fleets[&fid].free_move));
    assert!(events(&ctx, p3, "OnFleetOptionsChanged").is_empty());

    // RejectInvite(isAlreadyInFleet): OnFleetJoinReject(memberID, reasonCode) to the inviter only;
    // reasonCode is None for a plain decline and rejectFleetInviteAlreadyInFleet (2) otherwise.
    call(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(p3)]).await;
    call(&ctx, &mut s3, FH, "RejectInvite", &[PyRep::Bool(false)]).await;
    assert_eq!(events(&ctx, P1, "OnFleetJoinReject"), vec![vec![PyRep::Integer(p3), PyRep::None]]);
    assert!(events(&ctx, P2, "OnFleetJoinReject").is_empty());
    call(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(p3)]).await;
    call(&ctx, &mut s3, FH, "RejectInvite", &[PyRep::Bool(true)]).await;
    assert_eq!(events(&ctx, P1, "OnFleetJoinReject"), vec![vec![PyRep::Integer(p3), PyRep::Integer(2)]]);
    assert!(ctx.db.with_snapshot(|snap| !snap.fleets[&fid].invites.contains(&p3)));

    // ApplyToJoinFleet on a non-auto-join advert: OnFleetJoinRequest(info{charID, corpID}) to the boss.
    call(&ctx, &mut s1, "fleetProxy", "AddFleetFinderAdvert", &[PyRep::String("Ops".into()), PyRep::String("Ask".into()), PyRep::Integer(3), PyRep::Bool(false)]).await;
    let p3_corp = ctx.db.with_snapshot(|snap| snap.characters[&p3].corporation_id);
    let applied = call(&ctx, &mut s3, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(fid)]).await;
    assert_eq!(applied.dict_get("joined"), Some(&PyRep::Bool(false)));
    let info = kv(&[("charID", PyRep::Integer(p3)), ("corpID", PyRep::Integer(p3_corp))]);
    assert_eq!(events(&ctx, P1, "OnFleetJoinRequest"), vec![vec![info]]);
    assert!(events(&ctx, P2, "OnFleetJoinRequest").is_empty());

    // RejectJoinRequest: only the boss may reject; the applicant gets OnFleetJoinRejected(rejecterID).
    let not_boss = ctx.dispatch_rpc(&mut s2, FH, None, "RejectJoinRequest", &[PyRep::Integer(p3)], &HashMap::new(), None).await;
    assert_eq!(not_boss.expect_err("member cannot reject").code, "FleetPermissionDenied");
    assert!(events(&ctx, p3, "OnFleetJoinRejected").is_empty());
    call(&ctx, &mut s1, FH, "RejectJoinRequest", &[PyRep::Integer(p3)]).await;
    assert_eq!(events(&ctx, p3, "OnFleetJoinRejected"), vec![vec![PyRep::Integer(P1)]]);

    // SendBroadcast(name, broadcastScope, itemID, typeID): OnFleetBroadcast(name, scope, charID,
    // solarSystemID, itemID, typeID). BROADCAST_ALL (3) from the boss reaches both members and no outsider.
    let sys = s1.solar_system_id.expect("p1 solar system") as i64;
    call(&ctx, &mut s1, FH, "SendBroadcast", &[PyRep::String("EnemySpotted".into()), PyRep::Integer(3), PyRep::Integer(60_003_760), PyRep::Integer(1531)]).await;
    let bc = vec![vec![
        PyRep::String("EnemySpotted".into()),
        PyRep::Integer(3),
        PyRep::Integer(P1),
        PyRep::Integer(sys),
        PyRep::Integer(60_003_760),
        PyRep::Integer(1531),
    ]];
    assert_eq!(events(&ctx, P1, "OnFleetBroadcast"), bc);
    assert_eq!(events(&ctx, P2, "OnFleetBroadcast"), bc);
    assert!(events(&ctx, p3, "OnFleetBroadcast").is_empty());
    // BROADCAST_UP (2) from the squad member reaches its superior (the fleet commander) and itself;
    // BROADCAST_DOWN (1) from the same member has no subordinates, so only the sender sees it.
    let sys2 = s2.solar_system_id.expect("p2 solar system") as i64;
    call(&ctx, &mut s2, FH, "SendBroadcast", &[PyRep::String("NeedBackup".into()), PyRep::Integer(2), PyRep::None, PyRep::None]).await;
    let up = vec![vec![PyRep::String("NeedBackup".into()), PyRep::Integer(2), PyRep::Integer(P2), PyRep::Integer(sys2), PyRep::None, PyRep::None]];
    assert_eq!(events(&ctx, P1, "OnFleetBroadcast"), up);
    assert_eq!(events(&ctx, P2, "OnFleetBroadcast"), up);
    call(&ctx, &mut s2, FH, "SendBroadcast", &[PyRep::String("InPosition".into()), PyRep::Integer(1), PyRep::None, PyRep::None]).await;
    assert!(events(&ctx, P1, "OnFleetBroadcast").is_empty(), "BROADCAST_DOWN must not reach a superior");
    assert_eq!(events(&ctx, P2, "OnFleetBroadcast").len(), 1);

    // Boss hand-over on LeaveFleet: the successor's OnFleetMemberChanged gains the fleetJobCreator bit.
    call(&ctx, &mut s1, FH, "LeaveFleet", &[]).await;
    let q = events(&ctx, P2, "OnFleetMemberChanged");
    assert_eq!(q.len(), 1);
    assert_eq!((q[0][0].clone(), q[0][5].clone(), q[0][10].clone()), (PyRep::Integer(P2), PyRep::Integer(0), PyRep::Integer(2)));
}

#[tokio::test]
async fn test_fleet_accept_retry_broadcast_location_guards() {
    // Regression for review 20261021-161834 P2 notes 1 and 3 (no ledger rows claimed here).
    let ctx = ServerContext::new();
    let (mut s1, mut s2, _s3, _p3) = three_pilots(&ctx).await;
    const FH: &str = "fleetObjectHandler";
    call(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await;
    let wid = int(&call(&ctx, &mut s1, FH, "CreateWing", &[PyRep::String("W".into())]).await, "wingID");
    let sid = int(&call(&ctx, &mut s1, FH, "CreateSquad", &[PyRep::Integer(wid), PyRep::String("S".into())]).await, "squadID");

    // 1. A failed AcceptInvite (FleetFull) keeps the inviter's wing/squad/role choice for the retry.
    call(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2), PyRep::Integer(wid), PyRep::Integer(sid), PyRep::Integer(3)]).await;
    call(&ctx, &mut s1, FH, "SetFleetMaxSize", &[PyRep::Integer(1)]).await;
    let full = ctx.dispatch_rpc(&mut s2, FH, None, "AcceptInvite", &[], &HashMap::new(), None).await;
    assert_eq!(full.expect_err("fleet is full").code, "FleetFull");
    call(&ctx, &mut s1, FH, "SetFleetMaxSize", &[PyRep::Integer(10)]).await;
    ctx.drain_notifications_for_char(P1);
    call(&ctx, &mut s2, FH, "AcceptInvite", &[]).await;
    assert_eq!(events(&ctx, P1, "OnFleetJoin"), vec![vec![member(P2, wid, sid, 3, 0)]]);

    // 2. BroadcastToBubble from a sender without a ball (docked) reaches only the sender, not the system.
    ctx.drain_notifications_for_char(P1);
    call(&ctx, &mut s1, FH, "BroadcastToBubble", &[PyRep::String("HealArmor".into()), PyRep::Integer(3), PyRep::None, PyRep::None]).await;
    assert_eq!(events(&ctx, P1, "OnFleetBroadcast").len(), 1);
    assert!(events(&ctx, P2, "OnFleetBroadcast").is_empty(), "bubble broadcast must not leak system-wide");
}
