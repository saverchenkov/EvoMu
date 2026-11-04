//! Modular integration tests for Fleet Object Handler (`fleetObjectHandler`) member operations,
//! broadcasts, MOTD, fleet options, warp settings, and composition telemetry RPCs.

use crate::common;
use eve_db::FleetRole;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;
const SERVICE: &str = "fleetObjectHandler";

async fn setup_pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, SERVICE, None, method, args, &HashMap::new(), None)
        .await
}

fn get_i64(rep: &PyRep, key: &str) -> Option<i64> {
    rep.dict_get(key).and_then(|v| v.as_i64())
}

fn get_bool(rep: &PyRep, key: &str) -> Option<bool> {
    rep.dict_get(key).and_then(|v| v.as_bool())
}

fn get_str<'a>(rep: &'a PyRep, key: &str) -> Option<&'a str> {
    rep.dict_get(key).and_then(|v| v.as_str())
}

/// 1. Member positioning (MoveMember, FinishMove, MassMoveMembers), session info, and leadership transfer.
#[tokio::test]
async fn test_fleet_object_member_movement_and_leadership() {
    // parity: rpc:fleetObjectHandler.MoveMember, rpc:fleetObjectHandler.FinishMove, rpc:fleetObjectHandler.MassMoveMembers, rpc:fleetObjectHandler.MakeLeader, rpc:fleetObjectHandler.UpdateMemberInfo, rpc:fleetObjectHandler.Reconnect
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = setup_pilot(&ctx, 1031, "demo", "demo-secret", PILOT1).await;
    let mut sess2 = setup_pilot(&ctx, 1032, "other", "other-secret", PILOT2).await;

    let create_rep = rpc(
        &ctx,
        &mut sess1,
        "CreateFleet",
        &[PyRep::String("Ops Fleet".into()), PyRep::String("Ops MOTD".into())],
    )
    .await
    .expect("CreateFleet");
    let fid = get_i64(&create_rep, "fleetID").expect("fleetID");

    // Create Wing 2 and Squad 2 in Wing 2
    let wing_rep = rpc(&ctx, &mut sess1, "CreateWing", &[PyRep::String("Wing 2".into())])
        .await
        .expect("CreateWing");
    let wid2 = get_i64(&wing_rep, "wingID").expect("wingID 2");

    let squad_rep = rpc(
        &ctx,
        &mut sess1,
        "CreateSquad",
        &[PyRep::Integer(wid2), PyRep::String("Squad 2".into())],
    )
    .await
    .expect("CreateSquad");
    let sid2 = get_i64(&squad_rep, "squadID").expect("squadID 2");

    // Invite and accept Pilot 2
    rpc(
        &ctx,
        &mut sess1,
        "Invite",
        &[
            PyRep::Integer(PILOT2),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(4),
        ],
    )
    .await
    .expect("Invite Pilot 2");

    ctx.advance_secs(12);
    rpc(&ctx, &mut sess2, "AcceptInvite", &[PyRep::Integer(fid)])
        .await
        .expect("AcceptInvite Pilot 2");

    // 1. MoveMember: boss moves Pilot 2 to Wing 2, Squad 2, role 3 (Squad Commander)
    let move_rep = rpc(
        &ctx,
        &mut sess1,
        "MoveMember",
        &[
            PyRep::Integer(PILOT2),
            PyRep::Integer(wid2),
            PyRep::Integer(sid2),
            PyRep::Integer(3),
        ],
    )
    .await
    .expect("MoveMember");

    assert_eq!(get_i64(&move_rep, "characterID"), Some(PILOT2));
    assert_eq!(get_i64(&move_rep, "wingID"), Some(wid2));
    assert_eq!(get_i64(&move_rep, "squadID"), Some(sid2));
    assert_eq!(get_i64(&move_rep, "role"), Some(3));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        let m = fleet.member_details.get(&PILOT2).expect("pilot 2 member details");
        assert_eq!(m.wing_id, Some(wid2));
        assert_eq!(m.squad_id, Some(sid2));
        assert_eq!(m.role, FleetRole::SquadCommander);
        let sq = fleet.squad_details.get(&sid2).expect("squad 2");
        assert_eq!(sq.commander_id, Some(PILOT2));
    });

    // 2. FinishMove: promote Pilot 2 to Wing Commander (role 2) within Wing 2
    let finish_move_rep = rpc(
        &ctx,
        &mut sess1,
        "FinishMove",
        &[
            PyRep::Integer(PILOT2),
            PyRep::Integer(wid2),
            PyRep::Integer(sid2),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("FinishMove");

    assert_eq!(get_i64(&finish_move_rep, "characterID"), Some(PILOT2));
    assert_eq!(get_i64(&finish_move_rep, "wingID"), Some(wid2));
    assert_eq!(get_i64(&finish_move_rep, "squadID"), Some(sid2));
    assert_eq!(get_i64(&finish_move_rep, "role"), Some(2));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        let m = fleet.member_details.get(&PILOT2).expect("pilot 2 member details");
        assert_eq!(m.role, FleetRole::WingCommander);
        let sq = fleet.squad_details.get(&sid2).expect("squad 2");
        assert_eq!(sq.commander_id, None);
        let w = fleet.wing_details.get(&wid2).expect("wing 2");
        assert_eq!(w.commander_id, Some(PILOT2));
    });

    // 2b. MassMoveMembers validation: reject invalid target wing and squad
    for bad_w in [999, 0, -1] {
        let err = rpc(
            &ctx,
            &mut sess1,
            "MassMoveMembers",
            &[PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::Integer(bad_w), PyRep::Integer(1)],
        )
        .await
        .expect_err("reject invalid wing");
        assert_eq!(err.code, "WingNotFound");
    }

    for bad_sq in [999, 0, -1] {
        let err = rpc(
            &ctx,
            &mut sess1,
            "MassMoveMembers",
            &[PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::Integer(1), PyRep::Integer(bad_sq)],
        )
        .await
        .expect_err("reject invalid squad");
        assert_eq!(err.code, "SquadNotFound");
    }

    let err_mismatch = rpc(
        &ctx,
        &mut sess1,
        "MassMoveMembers",
        &[PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::Integer(wid2), PyRep::Integer(1)],
    )
    .await
    .expect_err("reject squad not in wing");
    assert_eq!(err_mismatch.code, "SquadNotFound");

    let _ = ctx.drain_notifications_for_char(PILOT1);

    // 3. MassMoveMembers: bulk move list of pilots back to Wing 1, Squad 1
    // Verifies WingCommander slot is cleared and role set to SquadMember (4)
    let mass_rep = rpc(
        &ctx,
        &mut sess1,
        "MassMoveMembers",
        &[
            PyRep::List(vec![PyRep::Integer(PILOT2)]),
            PyRep::Integer(1),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("MassMoveMembers");

    assert_eq!(get_i64(&mass_rep, "movedCount"), Some(1));

    // Verify full V24 13-argument OnFleetMemberChanged notification payload
    let notifs = ctx.drain_notifications_for_char(PILOT1);
    let member_changed = notifs
        .iter()
        .find(|n| n.event == "OnFleetMemberChanged")
        .expect("OnFleetMemberChanged notification must be emitted");
    assert_eq!(member_changed.args.len(), 13, "OnFleetMemberChanged must have 13 arguments");
    assert_eq!(member_changed.args[0], PyRep::Integer(PILOT2)); // charID
    assert_eq!(member_changed.args[1], PyRep::Integer(fid)); // fleetID
    assert_eq!(member_changed.args[2], PyRep::Integer(wid2)); // oldWingID
    assert_eq!(member_changed.args[3], PyRep::Integer(sid2)); // oldSquadID
    assert_eq!(member_changed.args[7], PyRep::Integer(1)); // newWingID
    assert_eq!(member_changed.args[8], PyRep::Integer(1)); // newSquadID
    assert_eq!(member_changed.args[9], PyRep::Integer(4)); // newRole (SquadMember = 4)

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        let m = fleet.member_details.get(&PILOT2).expect("pilot 2 details");
        assert_eq!(m.wing_id, Some(1));
        assert_eq!(m.squad_id, Some(1));
        assert_eq!(m.role, FleetRole::SquadMember);
        let w = fleet.wing_details.get(&wid2).expect("wing 2");
        assert_eq!(w.commander_id, None, "Wing commander slot must be cleared");
    });

    // 3b. Verify SquadCommander slot clearing on MassMoveMembers
    rpc(&ctx, &mut sess1, "MoveMember", &[PyRep::Integer(PILOT2), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(3)]).await.expect("promote SC");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fleets[&fid].squad_details[&1].commander_id, Some(PILOT2));
    });
    rpc(&ctx, &mut sess1, "MassMoveMembers", &[PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::Integer(wid2), PyRep::Integer(sid2)]).await.expect("MassMove SC");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fleets[&fid].squad_details[&1].commander_id, None, "Squad commander slot must be cleared");
        assert_eq!(snap.fleets[&fid].member_details[&PILOT2].role, FleetRole::SquadMember);
    });
    rpc(&ctx, &mut sess1, "MassMoveMembers", &[PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::Integer(1), PyRep::Integer(1)]).await.expect("MassMove reset");

    // 4. UpdateMemberInfo: client synchronizes member/fleet association
    let umi_rep = rpc(&ctx, &mut sess2, "UpdateMemberInfo", &[])
        .await
        .expect("UpdateMemberInfo");

    assert_eq!(get_i64(&umi_rep, "characterID"), Some(PILOT2));
    assert_eq!(get_i64(&umi_rep, "fleetID"), Some(fid));

    // 5. Reconnect: reconnecting client confirms fleet membership
    let reconn_rep = rpc(&ctx, &mut sess2, "Reconnect", &[])
        .await
        .expect("Reconnect");

    assert_eq!(get_i64(&reconn_rep, "characterID"), Some(PILOT2));
    assert_eq!(get_i64(&reconn_rep, "fleetID"), Some(fid));

    // 6. MakeLeader: Fleet Boss transfers leadership to Pilot 2
    let make_leader_rep = rpc(
        &ctx,
        &mut sess1,
        "MakeLeader",
        &[PyRep::Integer(PILOT2)],
    )
    .await
    .expect("MakeLeader");

    assert_eq!(get_i64(&make_leader_rep, "fleetID"), Some(fid));
    assert_eq!(get_i64(&make_leader_rep, "newBossID"), Some(PILOT2));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        assert_eq!(fleet.boss_id, PILOT2);
    });

    // Former boss Pilot 1 cannot transfer leadership now (permission denied)
    let denied_rep = rpc(
        &ctx,
        &mut sess1,
        "MakeLeader",
        &[PyRep::Integer(PILOT1)],
    )
    .await
    .expect_err("MakeLeader from non-boss");
    assert_eq!(denied_rep.code, "FleetPermissionDenied");
}

/// 2. Broadcasts, MOTD updates, options (free move / registration), auto-join squad, and setup loading.
#[tokio::test]
async fn test_fleet_object_broadcasts_motd_and_options() {
    // parity: rpc:fleetObjectHandler.SendBroadcast, rpc:fleetObjectHandler.GetMotd, rpc:fleetObjectHandler.SetMotdEx, rpc:fleetObjectHandler.SetOptions, rpc:fleetObjectHandler.SetAutoJoinSquadID, rpc:fleetObjectHandler.LoadFleetSetup
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = setup_pilot(&ctx, 1041, "demo", "demo-secret", PILOT1).await;
    let mut sess2 = setup_pilot(&ctx, 1042, "other", "other-secret", PILOT2).await;

    // Both pilots must be in a solar system to receive broadcasts
    sess1.solar_system_id = Some(30_000_142);
    sess2.solar_system_id = Some(30_000_142);

    let create_rep = rpc(
        &ctx,
        &mut sess1,
        "CreateFleet",
        &[PyRep::String("Command Fleet".into()), PyRep::String("Initial Squadron MOTD".into())],
    )
    .await
    .expect("CreateFleet");
    let fid = get_i64(&create_rep, "fleetID").expect("fleetID");

    // Invite and accept Pilot 2
    rpc(
        &ctx,
        &mut sess1,
        "Invite",
        &[
            PyRep::Integer(PILOT2),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(4),
        ],
    )
    .await
    .expect("Invite Pilot 2");

    ctx.advance_secs(12);
    rpc(&ctx, &mut sess2, "AcceptInvite", &[PyRep::Integer(fid)])
        .await
        .expect("AcceptInvite Pilot 2");

    // 1. GetMotd: inspect current fleet MOTD
    let motd_rep = rpc(&ctx, &mut sess1, "GetMotd", &[])
        .await
        .expect("GetMotd");
    assert_eq!(motd_rep.as_str(), Some("Initial Squadron MOTD"));

    // 2. SetMotdEx: commander updates MOTD
    let new_motd = "Operation Ironclad: Target broadcasts on primary";
    let set_motd_rep = rpc(
        &ctx,
        &mut sess1,
        "SetMotdEx",
        &[PyRep::String(new_motd.into())],
    )
    .await
    .expect("SetMotdEx");

    assert_eq!(get_i64(&set_motd_rep, "fleetID"), Some(fid));
    assert_eq!(get_str(&set_motd_rep, "motd"), Some(new_motd));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        assert_eq!(fleet.motd, new_motd);
    });

    let motd_updated = rpc(&ctx, &mut sess1, "GetMotd", &[]).await.expect("GetMotd updated");
    assert_eq!(motd_updated.as_str(), Some(new_motd));

    // 3. SetOptions: toggle free-move and registration flags
    let opt_rep = rpc(
        &ctx,
        &mut sess1,
        "SetOptions",
        &[PyRep::Bool(true), PyRep::Bool(false)],
    )
    .await
    .expect("SetOptions");

    assert_eq!(get_bool(&opt_rep, "isFreeMove"), Some(true));
    assert_eq!(get_bool(&opt_rep, "isRegistered"), Some(false));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        assert!(fleet.free_move);
    });

    // 4. SetAutoJoinSquadID: set designated default squad for incoming members
    let autojoin_rep = rpc(
        &ctx,
        &mut sess1,
        "SetAutoJoinSquadID",
        &[PyRep::Integer(1)],
    )
    .await
    .expect("SetAutoJoinSquadID");

    assert_eq!(get_i64(&autojoin_rep, "autoJoinSquadID"), Some(1));

    // 5. SendBroadcast: transmit fleet combat broadcast across the system
    let bcast_rep = rpc(
        &ctx,
        &mut sess1,
        "SendBroadcast",
        &[
            PyRep::String("Target".into()),
            PyRep::Integer(0), // All
            PyRep::Integer(999_001), // itemID
            PyRep::Integer(606), // typeID
        ],
    )
    .await
    .expect("SendBroadcast");

    assert_eq!(get_i64(&bcast_rep, "fleetID"), Some(fid));
    assert_eq!(get_str(&bcast_rep, "broadcast"), Some("Target"));
    assert_eq!(get_str(&bcast_rep, "broadcastType"), Some("Target"));
    assert_eq!(get_i64(&bcast_rep, "senderID"), Some(PILOT1));
    assert_eq!(get_i64(&bcast_rep, "targetID"), Some(999_001));
    assert_eq!(get_i64(&bcast_rep, "scope"), Some(0));
    assert_eq!(get_i64(&bcast_rep, "itemID"), Some(999_001));

    let PyRep::List(recipients) = bcast_rep.dict_get("recipientIDs").expect("recipientIDs list") else {
        panic!("expected recipientIDs list");
    };
    assert!(recipients.contains(&PyRep::Integer(PILOT1)));
    assert!(recipients.contains(&PyRep::Integer(PILOT2)));

    // 6. LoadFleetSetup: boss loads a saved doctrine setup atomically
    let setup_rep = rpc(
        &ctx,
        &mut sess1,
        "LoadFleetSetup",
        &[
            PyRep::String("Nanocruiser Doctrine".into()),
            PyRep::String("Heat sinks active — align stargate".into()),
            PyRep::Bool(false),
            PyRep::Integer(128),
        ],
    )
    .await
    .expect("LoadFleetSetup");

    assert_eq!(get_i64(&setup_rep, "fleetID"), Some(fid));
    assert_eq!(get_str(&setup_rep, "setupName"), Some("Nanocruiser Doctrine"));
    assert_eq!(get_str(&setup_rep, "motd"), Some("Heat sinks active — align stargate"));
    assert_eq!(get_bool(&setup_rep, "isFreeMove"), Some(false));
    assert_eq!(get_i64(&setup_rep, "maxSize"), Some(128));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        assert_eq!(fleet.motd, "Heat sinks active — align stargate");
        assert!(!fleet.free_move);
    });

    let state_check = rpc(&ctx, &mut sess1, "GetInitState", &[])
        .await
        .expect("GetInitState after LoadFleetSetup");
    assert_eq!(get_str(&state_check, "fleetName"), Some("Nanocruiser Doctrine"));
    assert_eq!(get_i64(&state_check, "maxSize"), Some(128));
}

/// 3. Fleet sizing limits, warp/regroup/conduit opt-in settings, and composition telemetry.
#[tokio::test]
async fn test_fleet_object_sizing_warp_and_composition() {
    // parity: rpc:fleetObjectHandler.SetFleetMaxSize, rpc:fleetObjectHandler.GetFleetMaxSize, rpc:fleetObjectHandler.SetTakesFleetWarp, rpc:fleetObjectHandler.SetAcceptsRegroupValue, rpc:fleetObjectHandler.SetAcceptsConduitJumpsValue, rpc:fleetObjectHandler.GetFleetComposition
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess1 = setup_pilot(&ctx, 1051, "demo", "demo-secret", PILOT1).await;
    let mut sess2 = setup_pilot(&ctx, 1052, "other", "other-secret", PILOT2).await;

    sess1.solar_system_id = Some(30_000_142);
    sess2.solar_system_id = Some(30_000_142);

    let create_rep = rpc(
        &ctx,
        &mut sess1,
        "CreateFleet",
        &[PyRep::String("Skirmish Fleet".into()), PyRep::String("Combat Ready".into())],
    )
    .await
    .expect("CreateFleet");
    let fid = get_i64(&create_rep, "fleetID").expect("fleetID");

    // Invite and accept Pilot 2
    rpc(
        &ctx,
        &mut sess1,
        "Invite",
        &[
            PyRep::Integer(PILOT2),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(4),
        ],
    )
    .await
    .expect("Invite Pilot 2");

    ctx.advance_secs(12);
    rpc(&ctx, &mut sess2, "AcceptInvite", &[PyRep::Integer(fid)])
        .await
        .expect("AcceptInvite Pilot 2");

    // 1. SetFleetMaxSize: boss restricts fleet capacity
    let set_size_rep = rpc(
        &ctx,
        &mut sess1,
        "SetFleetMaxSize",
        &[PyRep::Integer(64)],
    )
    .await
    .expect("SetFleetMaxSize");

    assert_eq!(get_i64(&set_size_rep, "maxSize"), Some(64));

    // 2. GetFleetMaxSize: query the configured maximum fleet capacity
    let get_size_rep = rpc(&ctx, &mut sess1, "GetFleetMaxSize", &[])
        .await
        .expect("GetFleetMaxSize");
    assert_eq!(get_size_rep.as_i64(), Some(64));

    // 3. SetTakesFleetWarp: Pilot 2 disables takesFleetWarp (regroup and conduit stay true)
    let warp_rep = rpc(
        &ctx,
        &mut sess2,
        "SetTakesFleetWarp",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("SetTakesFleetWarp false");

    assert_eq!(get_i64(&warp_rep, "characterID"), Some(PILOT2));
    assert_eq!(get_bool(&warp_rep, "takesFleetWarp"), Some(false));
    assert_eq!(get_bool(&warp_rep, "acceptsRegroup"), Some(true));
    assert_eq!(get_bool(&warp_rep, "acceptsConduitJumps"), Some(true));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        let m = &fleet.member_details[&PILOT2];
        assert!(!m.takes_fleet_warp);
        assert!(m.accepts_regroup);
        assert!(m.accepts_conduit_jumps);
    });

    // 4. SetAcceptsRegroupValue: Pilot 2 disables regroup independently (warp remains false, conduit stays true)
    let regroup_rep = rpc(
        &ctx,
        &mut sess2,
        "SetAcceptsRegroupValue",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("SetAcceptsRegroupValue false");

    assert_eq!(get_i64(&regroup_rep, "characterID"), Some(PILOT2));
    assert_eq!(get_bool(&regroup_rep, "takesFleetWarp"), Some(false));
    assert_eq!(get_bool(&regroup_rep, "acceptsRegroup"), Some(false));
    assert_eq!(get_bool(&regroup_rep, "acceptsConduitJumps"), Some(true));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        let m = &fleet.member_details[&PILOT2];
        assert!(!m.takes_fleet_warp);
        assert!(!m.accepts_regroup);
        assert!(m.accepts_conduit_jumps);
    });

    // 5. SetAcceptsConduitJumpsValue: Pilot 2 disables conduit jumps independently
    let conduit_rep = rpc(
        &ctx,
        &mut sess2,
        "SetAcceptsConduitJumpsValue",
        &[PyRep::Bool(false)],
    )
    .await
    .expect("SetAcceptsConduitJumpsValue false");

    assert_eq!(get_i64(&conduit_rep, "characterID"), Some(PILOT2));
    assert_eq!(get_bool(&conduit_rep, "takesFleetWarp"), Some(false));
    assert_eq!(get_bool(&conduit_rep, "acceptsRegroup"), Some(false));
    assert_eq!(get_bool(&conduit_rep, "acceptsConduitJumps"), Some(false));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        let m = &fleet.member_details[&PILOT2];
        assert!(!m.takes_fleet_warp);
        assert!(!m.accepts_regroup);
        assert!(!m.accepts_conduit_jumps);
    });

    // Verify independent toggle: re-enabling regroup does NOT alter warp or conduit jump settings
    let regroup_re_enable = rpc(
        &ctx,
        &mut sess2,
        "SetAcceptsRegroupValue",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("SetAcceptsRegroupValue true");

    assert_eq!(get_bool(&regroup_re_enable, "takesFleetWarp"), Some(false));
    assert_eq!(get_bool(&regroup_re_enable, "acceptsRegroup"), Some(true));
    assert_eq!(get_bool(&regroup_re_enable, "acceptsConduitJumps"), Some(false));

    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fid).expect("fleet");
        let m = &fleet.member_details[&PILOT2];
        assert!(!m.takes_fleet_warp);
        assert!(m.accepts_regroup);
        assert!(!m.accepts_conduit_jumps);
    });

    // 6. GetFleetComposition: Fleet Commander inspects real-time ship & spatial telemetry
    let comp_rep = rpc(&ctx, &mut sess1, "GetFleetComposition", &[])
        .await
        .expect("GetFleetComposition by Fleet Commander");

    let PyRep::Rowset { columns, rows } = comp_rep else {
        panic!("expected rowset from GetFleetComposition");
    };
    assert_eq!(
        columns,
        vec![
            "characterID",
            "shipTypeID",
            "solarSystemID",
            "wingID",
            "squadID",
            "role",
            "takesFleetWarp",
            "x",
            "y",
            "z",
        ]
    );
    assert_eq!(rows.len(), 2);

    let p1_entry = rows.iter().find(|r| r[0] == PyRep::Integer(PILOT1)).expect("pilot 1 in comp");
    assert_eq!(p1_entry[5], PyRep::Integer(1)); // Fleet Commander
    assert_eq!(p1_entry[6], PyRep::Bool(true));

    let p2_entry = rows.iter().find(|r| r[0] == PyRep::Integer(PILOT2)).expect("pilot 2 in comp");
    assert_eq!(p2_entry[5], PyRep::Integer(4)); // Squad Member
    assert_eq!(p2_entry[6], PyRep::Bool(false)); // Disabled via SetAcceptsConduitJumpsValue

    // Non-commander Pilot 2 is denied access to fleet composition
    let denied = rpc(&ctx, &mut sess2, "GetFleetComposition", &[])
        .await
        .expect_err("GetFleetComposition by SquadMember");
    assert_eq!(denied.code, "FleetRoleDenied");
}

pub(crate) mod fleet_object_manager_lifecycle {
    // behavioral verification integration tests for 10 auditor-flagged rows across fleetObjectHandler and fleetMgr.

    use std::collections::HashMap;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use crate::common;

    const PILOT1: i64 = 90_000_001;
    const PILOT2: i64 = 90_000_002;

    async fn setup_pilot(
        ctx: &ServerContext,
        sess_id: u64,
        user_name: &str,
        secret: &str,
        char_id: i64,
    ) -> SessionState {
        let mut sess = SessionState::new(sess_id);
        common::login_pilot(ctx, &mut sess, user_name, secret).await;
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SelectCharacterID");
        sess
    }

    fn get_i64(rep: &PyRep, key: &str) -> Option<i64> {
        rep.dict_get(key).and_then(|v| v.as_i64())
    }

    fn get_bool(rep: &PyRep, key: &str) -> Option<bool> {
        rep.dict_get(key).and_then(|v| v.as_bool())
    }

    #[tokio::test]
    async fn test_fleet_object_and_manager_hierarchy_lifecycle() {
        // parity: rpc:fleetObjectHandler.CreateFleet, rpc:fleetObjectHandler.FinishMove, rpc:fleetObjectHandler.GetInitState, rpc:fleetObjectHandler.GetJoinRequests, rpc:fleetObjectHandler.GetWings, rpc:fleetObjectHandler.Init, rpc:fleetObjectHandler.MakeLeader, rpc:fleetMgr.AddToWatchlist, rpc:fleetMgr.RegisterForDamageUpdates, rpc:fleetMgr.RemoveFromWatchlist
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);

        let uid3 = ctx.db.provision_player_account("pilot3", "p3-sec");
        let pilot3 = ctx.db.create_character(uid3, "Pilot Three", 1, 1, 11, 17, &ctx.sde).expect("create pilot3").character_id;
        let uid4 = ctx.db.provision_player_account("pilot4", "p4-sec");
        let pilot4 = ctx.db.create_character(uid4, "Pilot Four", 1, 1, 11, 17, &ctx.sde).expect("create pilot4").character_id;

        let mut sess1 = setup_pilot(&ctx, 2011, "demo", "demo-secret", PILOT1).await;
        let mut sess2 = setup_pilot(&ctx, 2012, "other", "other-secret", PILOT2).await;
        let mut sess3 = setup_pilot(&ctx, 2013, "pilot3", "p3-sec", pilot3).await;
        let mut sess4 = setup_pilot(&ctx, 2014, "pilot4", "p4-sec", pilot4).await;
        let kw = HashMap::new();

        // 1. CreateFleet (0-arg client arity returns fleetID integer) & Init
        let _ = ctx.drain_notifications_for_char(PILOT1);
        let create_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateFleet", &[], &kw, None).await.expect("CreateFleet 0-arg");
        let fid1 = create_rep.as_i64().expect("0-arg fleetObjectHandler.CreateFleet must return fleetID integer");
        assert_eq!(sess1.fleet_id, Some(fid1));
        assert_eq!(
            ctx.drain_notifications_for_char(PILOT1).iter().filter(|n| n.event == "OnFleetJoin").count(),
            1,
            "CreateFleet must emit OnFleetJoin once to creator"
        );

        // Set Pilot 1's active ship in DB to a Rifter (587) and train Leadership (3348=4), Wing Command (11574=3), Fleet Command (24764=2)
        let ship1_id = ctx.db.with_mut_snapshot(|snap| {
            let sid = snap.characters.get(&PILOT1).map(|c| c.active_ship_id).unwrap_or(100_000_001);
            if let Some(sh) = snap.items.get_mut(&sid) {
                sh.type_id = 587;
            }
            let sk = snap.skills.entry(PILOT1).or_default();
            sk.insert(3348, eve_db::TrainedSkillRecord { type_id: 3348, level: 4, skill_points: 45_255.0 });
            sk.insert(11_574, eve_db::TrainedSkillRecord { type_id: 11_574, level: 3, skill_points: 16_000.0 });
            sk.insert(24_764, eve_db::TrainedSkillRecord { type_id: 24_764, level: 2, skill_points: 4_243.0 });
            sid
        });
        let fleet_cnt_before = ctx.db.with_snapshot(|s| s.fleets.len());
        let init_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Init", &[PyRep::Integer(606)], &kw, None).await.expect("Init without adInfoData");
        assert_eq!(init_rep, PyRep::None, "Init must return None");
        assert_eq!(ctx.db.with_snapshot(|s| s.fleets.len()), fleet_cnt_before, "Init must not create a second fleet");
        assert!(
            ctx.drain_notifications_for_char(PILOT1).iter().any(|n| n.event == "OnFleetJoin"),
            "Init on newly created fleet must emit OnFleetJoin"
        );
        assert_eq!(sess1.fleet_id, Some(fid1));

        let ad_kwargs = HashMap::from([(
            "adInfoData".to_string(),
            PyRep::Dict(vec![
                (PyRep::String("fleetName".into()), PyRep::String("Init Advert".into())),
                (PyRep::String("description".into()), PyRep::String("Via Init".into())),
            ]),
        )]);
        let init_ad_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Init", &[PyRep::Integer(606)], &ad_kwargs, None).await.expect("Init with adInfoData");
        assert_eq!(init_ad_rep, PyRep::None);
        let my_ad = ctx.dispatch_rpc(&mut sess1, "fleetProxy", None, "GetMyFleetFinderAdvert", &[], &kw, None).await.expect("GetMyFleetFinderAdvert");
        let PyRep::Rowset { rows: ad_rows, .. } = &my_ad else { panic!("expected Rowset from GetMyFleetFinderAdvert") };
        assert_eq!(ad_rows.len(), 1);
        assert_eq!(ad_rows[0][0], PyRep::Integer(fid1));
        assert_eq!(ad_rows[0][1], PyRep::Integer(PILOT1));
        assert_eq!(ad_rows[0][2], PyRep::String("Init Advert".into()));
        assert_eq!(ad_rows[0][4], PyRep::Integer(sess1.solar_system_id.unwrap() as i64));

        // Extract initial unique wingID and squadID from GetWings
        let initial_wings_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetWings", &[], &kw, None).await.expect("GetWings initial");
        let PyRep::Dict(initial_wings) = &initial_wings_rep else { panic!("expected wings Dict") };
        assert_eq!(initial_wings.len(), 1);
        let wid1 = initial_wings[0].0.as_i64().expect("initial wingID");
        assert!(wid1 > 1, "0-arg CreateFleet must allocate a unique wingID > 1, got {wid1}");
        assert_eq!(initial_wings[0].1.dict_get("name").and_then(|v| v.as_str()), Some("Wing 1"));
        let PyRep::Dict(initial_squads) = initial_wings[0].1.dict_get("squads").expect("squads Dict") else { panic!("expected squads Dict") };
        assert_eq!(initial_squads.len(), 1);
        let sid1 = initial_squads[0].0.as_i64().expect("initial squadID");
        assert!(sid1 > 1 && sid1 != wid1, "0-arg CreateFleet must allocate a unique squadID > 1, got {sid1}");
        assert_eq!(initial_squads[0].1.dict_get("name").and_then(|v| v.as_str()), Some("Squad 1"));

        // Configure fleet options (isVoiceEnabled=true, isLootLogging=true) and MoveMember
        ctx.advance_secs(12);
        ctx.dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            None,
            "SetOptions",
            &[PyRep::keyval([
                ("isFreeMove", PyRep::Bool(false)),
                ("isRegistered", PyRep::Bool(false)),
                ("isVoiceEnabled", PyRep::Bool(true)),
                ("isLootLogging", PyRep::Bool(true)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("SetOptions");
        ctx.advance_secs(12);
        ctx.dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            None,
            "MoveMember",
            &[PyRep::Integer(PILOT1), PyRep::Integer(wid1), PyRep::Integer(sid1), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("MoveMember");

        // 2. GetInitState: returns options KeyVal, wings Dict, and members Dict with DB-backed skills/ship
        let init_st = ctx
            .dispatch_rpc(
                &mut sess1,
                "fleetObjectHandler",
                None,
                "GetInitState",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetInitState");

        let options_kv = init_st.dict_get("options").expect("options KeyVal");
        assert_eq!(get_bool(options_kv, "isFreeMove"), Some(false));
        assert_eq!(get_bool(options_kv, "isRegistered"), Some(false));
        assert_eq!(get_bool(options_kv, "isVoiceEnabled"), Some(true));
        assert_eq!(get_bool(&init_st, "isLootLogging"), Some(true));

        let PyRep::Dict(wings_dict) = init_st.dict_get("wings").expect("wings Dict") else {
            panic!("expected wings Dict");
        };
        assert!(!wings_dict.is_empty());

        let PyRep::Dict(members_dict) = init_st.dict_get("members").expect("members Dict") else {
            panic!("expected members Dict");
        };
        let boss_m = members_dict
            .iter()
            .find_map(|(k, v)| (k == &PyRep::Integer(PILOT1)).then_some(v))
            .expect("boss in members Dict");
        assert_eq!(get_i64(boss_m, "job"), Some(2)); // boss bit
        assert_eq!(get_i64(boss_m, "role"), Some(1)); // FleetCommander
        assert_eq!(get_i64(boss_m, "roleBooster"), Some(0)); // V24 uses command bursts
        assert_eq!(get_i64(boss_m, "shipTypeID"), Some(587)); // Active ship in DB, ignoring client Init(606)
        assert_eq!(
            boss_m.dict_get("skills"),
            Some(&PyRep::List(vec![PyRep::Integer(4), PyRep::Integer(3), PyRep::Integer(2)])),
            "GetInitState must return trained [Leadership, WingCommand, FleetCommand] skill levels"
        );

        // When character has no active ship item in DB, shipTypeID is None (never fabricated 606)
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT1) {
                c.active_ship_id = 0;
            }
        });
        let init_st_no_ship = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetInitState", &[], &kw, None).await.expect("GetInitState no ship");
        let PyRep::Dict(members_ns) = init_st_no_ship.dict_get("members").unwrap() else { panic!() };
        let boss_ns = members_ns.iter().find_map(|(k, v)| (k == &PyRep::Integer(PILOT1)).then_some(v)).unwrap();
        assert_eq!(boss_ns.dict_get("shipTypeID"), Some(&PyRep::None));
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&PILOT1) {
                c.active_ship_id = ship1_id;
            }
        });

        // 3. GetWings: returns dict of wingID -> KeyVal(wingID, name, squads Dict) and reflects wing/squad mutations
        let cw_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateWing", &[PyRep::String("Alpha Wing".into())], &kw, None).await.expect("CreateWing");
        let wid2 = get_i64(&cw_rep, "wingID").expect("wid2");
        let cs_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateSquad", &[PyRep::Integer(wid2), PyRep::String("Bravo Squad".into())], &kw, None).await.expect("CreateSquad");
        let sid2 = get_i64(&cs_rep, "squadID").expect("sid2");
        let wings_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetWings", &[], &kw, None).await.expect("GetWings after CreateWing/CreateSquad");
        let PyRep::Dict(wings_after_add) = &wings_rep else { panic!("expected wings Dict") };
        assert_eq!(wings_after_add.len(), 2);
        let w2_kv = wings_after_add.iter().find_map(|(k, v)| (k == &PyRep::Integer(wid2)).then_some(v)).expect("wid2 in GetWings");
        assert_eq!(get_i64(w2_kv, "wingID"), Some(wid2));
        assert_eq!(w2_kv.dict_get("name").and_then(|v| v.as_str()), Some("Alpha Wing"));
        let PyRep::Dict(w2_squads) = w2_kv.dict_get("squads").expect("w2 squads Dict") else { panic!("expected squads Dict") };
        let s2_kv = w2_squads.iter().find_map(|(k, v)| (k == &PyRep::Integer(sid2)).then_some(v)).expect("sid2 in w2 squads");
        assert_eq!(get_i64(s2_kv, "squadID"), Some(sid2));
        assert_eq!(s2_kv.dict_get("name").and_then(|v| v.as_str()), Some("Bravo Squad"));
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "DeleteSquad", &[PyRep::Integer(sid2)], &kw, None).await.expect("DeleteSquad");
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "DeleteWing", &[PyRep::Integer(wid2)], &kw, None).await.expect("DeleteWing");
        let wings_after_del = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetWings", &[], &kw, None).await.expect("GetWings after DeleteWing");
        let PyRep::Dict(wings_del_dict) = &wings_after_del else { panic!("expected wings Dict") };
        assert_eq!(wings_del_dict.len(), 1);
        assert!(!wings_del_dict.iter().any(|(k, _)| k == &PyRep::Integer(wid2)), "Deleted wing must be removed from GetWings");

        // 4. Advert and Join Requests vs Invites + SQLite persistence of join_requests
        ctx.dispatch_rpc(
            &mut sess1, "fleetProxy", None, "AddFleetFinderAdvert",
            &[PyRep::String("Join Advert".into()), PyRep::String("Apply now".into()), PyRep::Integer(3), PyRep::Bool(false)],
            &kw, None,
        ).await.expect("AddFleetFinderAdvert");

        // Pilot 3 and Pilot 4 apply to join
        ctx.dispatch_rpc(&mut sess3, "fleetProxy", None, "ApplyToJoinFleet", &[PyRep::Integer(fid1)], &kw, None).await.expect("ApplyToJoinFleet Pilot 3");
        ctx.dispatch_rpc(&mut sess4, "fleetProxy", None, "ApplyToJoinFleet", &[PyRep::Integer(fid1)], &kw, None).await.expect("ApplyToJoinFleet Pilot 4");

        // Pilot 2 is merely invited
        ctx.dispatch_rpc(
            &mut sess1, "fleetObjectHandler", None, "Invite",
            &[PyRep::Integer(PILOT2), PyRep::Integer(wid1), PyRep::Integer(sid1), PyRep::Integer(4)],
            &kw, None,
        ).await.expect("Invite Pilot 2");

        // GetJoinRequests returns ONLY applicants (Pilot 3 & Pilot 4), NOT invitees (Pilot 2)
        let reqs_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetJoinRequests", &[], &kw, None).await.expect("GetJoinRequests");
        let PyRep::Dict(reqs_dict) = reqs_rep else { panic!("expected dict") };
        assert!(reqs_dict.iter().any(|(k, _)| k == &PyRep::Integer(pilot3)));
        assert!(reqs_dict.iter().any(|(k, _)| k == &PyRep::Integer(pilot4)));
        assert!(!reqs_dict.iter().any(|(k, _)| k == &PyRep::Integer(PILOT2)));

        // RejectJoinRequest emits OnFleetJoinRejected(rejecterID), NOT OnFleetJoinReject, and persists removal in SQLite
        let _ = ctx.drain_notifications_for_char(pilot3);
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "RejectJoinRequest", &[PyRep::Integer(pilot3)], &kw, None).await.expect("RejectJoinRequest");
        let p3_reject_notifs = ctx.drain_notifications_for_char(pilot3);
        assert!(!p3_reject_notifs.iter().any(|n| n.event == "OnFleetJoinReject"));
        let rej_notif = p3_reject_notifs.iter().find(|n| n.event == "OnFleetJoinRejected").expect("OnFleetJoinRejected");
        assert_eq!(rej_notif.args, vec![PyRep::Integer(PILOT1)]);

        // Pilot 2 accepts invite and joins fleet -> emits OnFleetJoin to fleet members
        let _ = ctx.drain_notifications_for_char(PILOT1);
        ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "AcceptInvite", &[PyRep::Integer(fid1)], &kw, None).await.expect("AcceptInvite");
        assert!(ctx.drain_notifications_for_char(PILOT1).iter().any(|n| n.event == "OnFleetJoin"));

        // Persist to SQLite, reload blobs from SQLite, re-hydrate social state, and verify GetJoinRequests hydrates only Pilot 4
        let pid = std::process::id();
        let json_path = std::env::temp_dir().join(format!("evomu_fleet_reqs_{pid}.json"));
        let sqlite_path = std::env::temp_dir().join(format!("evomu_fleet_reqs_{pid}.sqlite"));
        let _ = std::fs::remove_file(&json_path);
        let _ = std::fs::remove_file(&sqlite_path);
        ctx.db.save_snapshot_to_file(&json_path).expect("save_snapshot_to_file");
        {
            let writer = eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("open_sqlite writer");
            writer.load_snapshot_from_file(&json_path).expect("load_snapshot_from_file");
        }
        let repo_reloaded = eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("reopen_sqlite");
        let snap_reloaded = repo_reloaded.with_snapshot(|s| s.clone());
        let _ = std::fs::remove_file(&json_path);
        let _ = std::fs::remove_file(&sqlite_path);
        ctx.db.with_mut_snapshot(|s| s.service_state_blobs = snap_reloaded.service_state_blobs);
        eve_server::services::fleet_social::hydrate_social_state_from_db(&ctx);
        let reqs_after_reload = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetJoinRequests", &[], &kw, None).await.expect("GetJoinRequests after SQLite reload");
        let PyRep::Dict(reqs_reloaded_dict) = reqs_after_reload else { panic!("expected dict") };
        assert!(!reqs_reloaded_dict.iter().any(|(k, _)| k == &PyRep::Integer(pilot3)), "Rejected applicant must remain absent after SQLite reload");
        assert!(reqs_reloaded_dict.iter().any(|(k, _)| k == &PyRep::Integer(pilot4)), "Remaining applicant must survive SQLite reload");

        // Non-commander Pilot 2 cannot call GetJoinRequests
        let unauth_reqs = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "GetJoinRequests", &[], &kw, None).await.unwrap_err();
        assert_eq!(unauth_reqs.code, "FleetPermissionDenied");

        // 5. FinishMove with 0 args leaves caller role untouched
        let fm_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "FinishMove", &[], &kw, None).await.expect("FinishMove");
        assert_eq!(fm_rep, PyRep::None);

        let init_after_fm = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetInitState", &[], &kw, None).await.expect("GetInitState after FinishMove");
        let PyRep::Dict(m_dict) = init_after_fm.dict_get("members").unwrap() else { panic!() };
        let boss_after = m_dict.iter().find_map(|(k, v)| (k == &PyRep::Integer(PILOT1)).then_some(v)).unwrap();
        assert_eq!(get_i64(boss_after, "role"), Some(1)); // Still FleetCommander

        // 6. MakeLeader: transfers boss, moves job bit 2, and emits OnFleetMemberChanged + OnChangeFleetBoss
        let unauth_lead = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "MakeLeader", &[PyRep::Integer(PILOT1)], &kw, None).await.unwrap_err();
        assert_eq!(unauth_lead.code, "FleetPermissionDenied");

        let _ = ctx.drain_notifications_for_char(PILOT2);
        let make_lead = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "MakeLeader", &[PyRep::Integer(PILOT2)], &kw, None).await.expect("MakeLeader");
        assert_eq!(get_i64(&make_lead, "newBossID"), Some(PILOT2));
        let lead_notifs = ctx.drain_notifications_for_char(PILOT2);
        assert!(lead_notifs.iter().any(|n| n.event == "OnFleetMemberChanged"));
        assert!(lead_notifs.iter().any(|n| n.event == "OnChangeFleetBoss" && n.args == vec![PyRep::Integer(PILOT1), PyRep::Integer(PILOT2)]));

        let init_after_lead = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "GetInitState", &[], &kw, None).await.expect("GetInitState after MakeLeader");
        let PyRep::Dict(m_dict2) = init_after_lead.dict_get("members").unwrap() else { panic!() };
        let new_boss_m = m_dict2.iter().find_map(|(k, v)| (k == &PyRep::Integer(PILOT2)).then_some(v)).unwrap();
        assert_eq!(get_i64(new_boss_m, "job"), Some(2));
        assert_eq!(get_i64(new_boss_m, "role"), Some(4), "MakeLeader preserves new boss hierarchy role");
        let old_boss_m = m_dict2.iter().find_map(|(k, v)| (k == &PyRep::Integer(PILOT1)).then_some(v)).unwrap();
        assert_eq!(get_i64(old_boss_m, "job"), Some(0));
        assert_eq!(get_i64(old_boss_m, "role"), Some(1), "MakeLeader preserves old boss hierarchy role");

        // 7. Watchlist: AddToWatchlist, RegisterForDamageUpdates, RemoveFromWatchlist + persistence + real weapon damage streaming
        let non_member_watch = ctx.dispatch_rpc(&mut sess2, "fleetMgr", None, "AddToWatchlist", &[PyRep::List(vec![PyRep::Integer(pilot3)])], &kw, None).await.unwrap_err();
        assert_eq!(non_member_watch.code, "MemberNotFound");

        // Pre-damage Pilot 1's docked ship via persisted SHIELD_CHARGE (attr 264) to verify initial watchlist HP ratio
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(sh) = snap.items.get_mut(&ship1_id) {
                sh.dynamic_attributes.insert(264, 100.0);
            }
        });
        let _ = ctx.drain_notifications_for_char(PILOT2);
        let add_watch = ctx.dispatch_rpc(
            &mut sess2, "fleetMgr", None, "AddToWatchlist",
            &[PyRep::List(vec![PyRep::Integer(PILOT1)]), PyRep::List(vec![PyRep::Integer(PILOT1)])],
            &kw, None,
        ).await.expect("AddToWatchlist");
        assert_eq!(get_bool(&add_watch, "watched"), Some(true));
        let init_sr = add_watch.dict_get("shieldRatio").and_then(|v| v.as_f64()).expect("initial shieldRatio");
        assert!(init_sr < 1.0 && init_sr > 0.0, "Docked ship with persisted SHIELD_CHARGE must report partial shieldRatio, got {init_sr}");
        assert!(ctx.drain_notifications_for_char(PILOT2).iter().any(|n| n.event == "OnFleetStateChange"));

        // RegisterForDamageUpdates with list persists fleet_watchlist in service_state_blobs; verify SQLite roundtrip & cache eviction hydration
        let reg_dam = ctx.dispatch_rpc(&mut sess2, "fleetMgr", None, "RegisterForDamageUpdates", &[PyRep::List(vec![PyRep::Integer(PILOT1)])], &kw, None).await.expect("RegisterForDamageUpdates");
        assert_eq!(get_bool(&reg_dam, "watched"), Some(true));
        ctx.db.save_snapshot_to_file(&json_path).expect("save_snapshot_to_file watchlist");
        {
            let writer = eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("open_sqlite writer watchlist");
            writer.load_snapshot_from_file(&json_path).expect("load_snapshot_from_file watchlist");
        }
        let repo_wl = eve_db::WorldRepository::open_sqlite(&sqlite_path).expect("reopen_sqlite watchlist");
        let snap_wl = repo_wl.with_snapshot(|s| s.clone());
        let _ = std::fs::remove_file(&json_path);
        let _ = std::fs::remove_file(&sqlite_path);
        ctx.db.with_mut_snapshot(|s| s.service_state_blobs = snap_wl.service_state_blobs);
        eve_server::services::fleet_social::hydrate_social_state_from_db(&ctx);
        let init_reloaded = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "GetInitState", &[], &kw, None).await.expect("GetInitState after SQLite reload");
        let opts_reloaded = init_reloaded.dict_get("options").expect("options after reload");
        assert_eq!(get_bool(opts_reloaded, "isVoiceEnabled"), Some(true));
        assert_eq!(get_bool(&init_reloaded, "isLootLogging"), Some(true));

        // Spawn watched Pilot 1 and attacker Pilot 3 in lowsec (30_002_537) and fire real weapon via dogmaIM.Activate
        let lowsec_sys = 30_002_537u32;
        let ship3_id = 400_000_303i64;
        let autocannon_id = 400_000_304i64;
        let ammo_id = 400_000_305i64;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c1) = snap.characters.get_mut(&PILOT1) {
                c1.solar_system_id = lowsec_sys;
                c1.station_id = None;
                c1.active_ship_id = ship1_id;
            }
            if let Some(s1) = snap.items.get_mut(&ship1_id) {
                s1.location_id = lowsec_sys as i64;
            }
            if let Some(c3) = snap.characters.get_mut(&pilot3) {
                c3.solar_system_id = lowsec_sys;
                c3.station_id = None;
                c3.active_ship_id = ship3_id;
            }
            snap.items.insert(ship3_id, eve_db::InventoryItemRecord {
                item_id: ship3_id, type_id: 587, owner_id: pilot3, location_id: lowsec_sys as i64,
                flag: eve_db::InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            snap.items.insert(autocannon_id, eve_db::InventoryItemRecord {
                item_id: autocannon_id, type_id: 486, owner_id: pilot3, location_id: ship3_id,
                flag: eve_db::InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            snap.items.insert(ammo_id, eve_db::InventoryItemRecord {
                item_id: ammo_id, type_id: 185, owner_id: pilot3, location_id: ship3_id,
                flag: eve_db::InventoryFlag::HiSlot0, quantity: 100, singleton: false, version: 1, ..Default::default()
            });
        });
        sess1.solar_system_id = Some(lowsec_sys);
        sess1.station_id = None;
        sess1.ship_id = Some(ship1_id);
        sess3.solar_system_id = Some(lowsec_sys);
        sess3.station_id = None;
        sess3.ship_id = Some(ship3_id);
        let sys_handle = ctx.systems.get_or_spawn(lowsec_sys);
        let _ = sys_handle.spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship1_id, type_id: 587, position: eve_destiny::DVec3::ZERO, velocity: eve_destiny::DVec3::ZERO,
                max_velocity: 350.0, inertia: 3.0, mass: 1_200_000.0, radius: 40.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        ).await;
        let _ = sys_handle.spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship3_id, type_id: 587, position: eve_destiny::DVec3::new(1_000.0, 0.0, 0.0), velocity: eve_destiny::DVec3::ZERO,
                max_velocity: 350.0, inertia: 3.0, mass: 1_200_000.0, radius: 40.0, speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        ).await;

        let _ = sys_handle.add_target_lock(ship3_id, eve_destiny::TargetLock { target_id: ship1_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await;
        let _ = ctx.drain_notifications_for_char(PILOT2);
        ctx.set_invention_roll_override(Some(0.50));
        eve_server::services::combat_crimewatch::dispatch_combat_rpc(
            &ctx,
            &mut sess3,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(autocannon_id), PyRep::Integer(ship1_id)],
            &kw,
        )
        .await
        .expect("dogmaIM.Activate real weapon damage against watched Pilot 1");

        let mut notifs = ctx.drain_notifications_for_char(PILOT2);
        assert!(
            notifs.iter().any(|n| n.event == "OnDamageStateChange" && n.args.first() == Some(&PyRep::Integer(ship1_id))),
            "Watcher Pilot 2 must receive OnDamageStateChange for watched ship"
        );
        let dam_notif = notifs.into_iter().find(|n| n.event == "OnFleetStateChange").expect("Pilot 2 must receive OnFleetStateChange on combat damage");
        assert_eq!(dam_notif.args.len(), 1, "OnFleetStateChange must carry 1 argument (fleetState KeyVal)");
        let fleet_state = &dam_notif.args[0];
        assert_eq!(get_i64(fleet_state, "characterID"), Some(PILOT1));
        assert!(matches!(fleet_state.dict_get("targetTags"), Some(PyRep::Dict(_))));
        let shield_ratio = fleet_state.dict_get("shieldRatio").and_then(|v| v.as_f64()).expect("shieldRatio");
        assert!(shield_ratio < 1.0, "Real weapon damage must reduce watched Pilot 1 shieldRatio below 1.0, got {shield_ratio}");

        // RemoveFromWatchlist
        let rem_watch = ctx
            .dispatch_rpc(
                &mut sess2,
                "fleetMgr",
                None,
                "RemoveFromWatchlist",
                &[PyRep::Integer(PILOT1)],
                &kw,
                None,
            )
            .await
            .expect("RemoveFromWatchlist");
        assert_eq!(get_bool(&rem_watch, "watched"), Some(false));

        // Fire weapon again -> Pilot 2 receives NO damage updates after RemoveFromWatchlist
        let _ = ctx.drain_notifications_for_char(PILOT2);
        eve_server::services::combat_crimewatch::dispatch_combat_rpc(
            &ctx,
            &mut sess3,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(autocannon_id), PyRep::Integer(ship1_id)],
            &kw,
        )
        .await
        .expect("dogmaIM.Activate second shot");
        notifs = ctx.drain_notifications_for_char(PILOT2);
        assert!(!notifs.into_iter().any(|n| n.event == "OnFleetStateChange"), "Removed watcher must not receive updates");

        // 8. CreateFleet when already in a fleet returns AlreadyInFleet
        let create2_err = ctx
            .dispatch_rpc(
                &mut sess1,
                "fleetObjectHandler",
                None,
                "CreateFleet",
                &[PyRep::String("Beta Fleet".into())],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(create2_err.code, "AlreadyInFleet");
    }
}
