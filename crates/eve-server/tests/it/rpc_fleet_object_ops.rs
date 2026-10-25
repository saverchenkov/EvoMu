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
