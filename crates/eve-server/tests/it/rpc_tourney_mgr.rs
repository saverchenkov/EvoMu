//! Comprehensive integration tests for Tournament Manager (`tourneyMgr`).
//!
//! Validates match lifecycle, team compositions, ship bans, pre-game checks,
//! referee overrides, countdown transitions, and teleportation telemetry.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn select_pilot(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_tourney_match_lifecycle_and_teams() {
    // parity: rpc:tourneyMgr.CreateMatch, rpc:tourneyMgr.AddPlayer
    // parity: rpc:tourneyMgr.RemovePlayer, rpc:tourneyMgr.GetTeamMembers
    // parity: rpc:tourneyMgr.LockMemberList, rpc:tourneyMgr.UnlockMemberList
    // parity: rpc:tourneyMgr.StartCountdown, rpc:tourneyMgr.AbortCountdown
    // parity: rpc:tourneyMgr.MatchCompleted, rpc:tourneyMgr.ResetMatch
    // parity: rpc:tourneyMgr.GetActiveMatchForCamera, rpc:tourneyMgr.GetMatchSystem
    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 80_001, "demo", "demo-secret", P1).await;

    // 1. Create match
    let create_res = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1),   // tourneyID
            PyRep::Integer(1001), // matchID
            PyRep::Integer(100),  // maxPoints
            PyRep::Integer(600),  // matchTime
            PyRep::Integer(0),    // overtime
            PyRep::Integer(0),    // rulesetID
        ],
    )
    .await
    .expect("CreateMatch");

    let PyRep::Tuple(items) = create_res else {
        panic!("expected 3-tuple from CreateMatch, got: {:?}", create_res);
    };
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].as_i64(), Some(1001));

    let PyRep::List(beacons) = &items[1] else {
        panic!("expected beacon list in CreateMatch return");
    };
    assert_eq!(beacons.len(), 2);

    let PyRep::List(captains) = &items[2] else {
        panic!("expected captain list in CreateMatch return");
    };
    assert_eq!(captains.len(), 2);

    // 2. Query match system
    let sys_rep = rpc(&ctx, &mut sess, "tourneyMgr", "GetMatchSystem", &[])
        .await
        .expect("GetMatchSystem");
    assert!(sys_rep.as_i64().is_some());

    // 3. Query active match for camera
    let cam_rep = rpc(&ctx, &mut sess, "tourneyMgr", "GetActiveMatchForCamera", &[])
        .await
        .expect("GetActiveMatchForCamera");
    let PyRep::Dict(cam_dict) = cam_rep else {
        panic!("expected Dict from GetActiveMatchForCamera");
    };
    let match_id_val = cam_dict
        .iter()
        .find(|(k, _)| k.as_str() == Some("matchID"))
        .and_then(|(_, v)| v.as_i64());
    assert_eq!(match_id_val, Some(1001));

    let team_one = cam_dict
        .iter()
        .find(|(k, _)| k.as_str() == Some("teamOneName"))
        .and_then(|(_, v)| v.as_str());
    assert_eq!(team_one, Some("Red Alliance"));

    // 4. Add players to teams
    let add_p1 = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "AddPlayer",
        &[PyRep::Integer(1001), PyRep::Integer(0), PyRep::Integer(P1)],
    )
    .await
    .expect("AddPlayer Red");
    assert_eq!(add_p1, PyRep::Bool(true));

    let add_p2 = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "AddPlayer",
        &[PyRep::Integer(1001), PyRep::Integer(1), PyRep::Integer(P2)],
    )
    .await
    .expect("AddPlayer Blue");
    assert_eq!(add_p2, PyRep::Bool(true));

    // 5. Query team members
    let members_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "GetTeamMembers",
        &[PyRep::Integer(1001)],
    )
    .await
    .expect("GetTeamMembers");
    let PyRep::List(teams) = members_rep else {
        panic!("expected 2-element list from GetTeamMembers");
    };
    assert_eq!(teams.len(), 2);
    let PyRep::List(red_roster) = &teams[0] else { panic!("expected red roster list"); };
    let PyRep::List(blue_roster) = &teams[1] else { panic!("expected blue roster list"); };
    assert_eq!(red_roster.len(), 1);
    assert_eq!(blue_roster.len(), 1);

    // 6. Remove player from blue team
    let rem_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "RemovePlayer",
        &[PyRep::Integer(1001), PyRep::Integer(1), PyRep::Integer(P2)],
    )
    .await
    .expect("RemovePlayer");
    assert_eq!(rem_rep, PyRep::Bool(true));

    let members_rep2 = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "GetTeamMembers",
        &[PyRep::Integer(1001)],
    )
    .await
    .expect("GetTeamMembers after remove");
    let PyRep::List(teams2) = members_rep2 else { panic!("expected teams"); };
    let PyRep::List(blue_roster2) = &teams2[1] else { panic!("expected blue roster"); };
    assert_eq!(blue_roster2.len(), 0);

    // 7. Lock and Unlock member list
    let lock_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "LockMemberList",
        &[PyRep::Integer(1001), PyRep::Integer(0), PyRep::List(vec![])],
    )
    .await
    .expect("LockMemberList");
    assert_eq!(lock_rep, PyRep::Bool(true));

    let unlock_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "UnlockMemberList",
        &[PyRep::Integer(1001), PyRep::Integer(0)],
    )
    .await
    .expect("UnlockMemberList");
    assert_eq!(unlock_rep, PyRep::Bool(true));

    // 8. Countdown management
    let cd_start = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "StartCountdown",
        &[PyRep::Integer(1001)],
    )
    .await
    .expect("StartCountdown");
    assert_eq!(cd_start, PyRep::Bool(true));

    let cd_abort = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "AbortCountdown",
        &[PyRep::Integer(1001)],
    )
    .await
    .expect("AbortCountdown");
    assert_eq!(cd_abort, PyRep::Bool(true));

    // 9. Match Completed
    let comp_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "MatchCompleted",
        &[PyRep::Integer(1001)],
    )
    .await
    .expect("MatchCompleted");
    assert_eq!(comp_rep, PyRep::Bool(true));

    // 10. Reset Match
    let reset_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "ResetMatch",
        &[PyRep::Integer(1001)],
    )
    .await
    .expect("ResetMatch");
    assert_eq!(reset_rep, PyRep::Bool(true));
}

#[tokio::test]
async fn test_tourney_bans_and_restrictions() {
    // parity: rpc:tourneyMgr.QueryShipList, rpc:tourneyMgr.StartAutobanning
    // parity: rpc:tourneyMgr.BanShip, rpc:tourneyMgr.BanShipType
    // parity: rpc:tourneyMgr.UnBanShipType, rpc:tourneyMgr.FinalizeBans
    // parity: rpc:tourneyMgr.OverrideDroneRestrict, rpc:tourneyMgr.OverrideLockRestrict
    // parity: rpc:tourneyMgr.OverrideMoveRestrict, rpc:tourneyMgr.OverrideWarpRestrict
    // parity: rpc:tourneyMgr.IsOtherMatchInProgress
    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 80_002, "demo", "demo-secret", P1).await;

    // 1. Query ship list
    let ships_rep = rpc(&ctx, &mut sess, "tourneyMgr", "QueryShipList", &[PyRep::Integer(1)])
        .await
        .expect("QueryShipList");
    let PyRep::Dict(ships_dict) = ships_rep else {
        panic!("expected Dict from QueryShipList");
    };
    let allowed_ships = ships_dict
        .iter()
        .find(|(k, _)| k.as_str() == Some("allowedShipList"))
        .and_then(|(_, v)| match v {
            PyRep::List(list) => Some(list),
            _ => None,
        });
    assert!(allowed_ships.is_some());
    assert!(!allowed_ships.unwrap().is_empty());

    // 2. Create match 2001
    rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1),
            PyRep::Integer(2001),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("CreateMatch 2001");

    // 3. Start Autobanning
    let auto_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "StartAutobanning",
        &[
            PyRep::Integer(2001),
            PyRep::Integer(1),
            PyRep::Integer(P1),
            PyRep::Integer(P2),
            PyRep::Integer(4),
        ],
    )
    .await
    .expect("StartAutobanning");
    assert_eq!(auto_rep, PyRep::Bool(true));

    // 4. BanShip (captain submit)
    let ban_ship_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "BanShip",
        &[
            PyRep::Integer(10),
            PyRep::List(vec![PyRep::Integer(620), PyRep::Integer(624)]),
        ],
    )
    .await
    .expect("BanShip");
    assert_eq!(ban_ship_rep, PyRep::Bool(true));

    // 5. BanShipType for Red and Blue
    let ban_red = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "BanShipType",
        &[PyRep::Integer(2001), PyRep::Integer(0), PyRep::Integer(620)],
    )
    .await
    .expect("BanShipType Red");
    assert_eq!(ban_red, PyRep::Bool(true));

    let ban_blue = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "BanShipType",
        &[PyRep::Integer(2001), PyRep::Integer(1), PyRep::Integer(624)],
    )
    .await
    .expect("BanShipType Blue");
    assert_eq!(ban_blue, PyRep::Bool(true));

    // 6. UnBanShipType
    let unban_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "UnBanShipType",
        &[PyRep::Integer(2001), PyRep::Integer(0), PyRep::Integer(620)],
    )
    .await
    .expect("UnBanShipType");
    assert_eq!(unban_rep, PyRep::Bool(true));

    // 7. FinalizeBans
    let fin_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "FinalizeBans",
        &[PyRep::Integer(2001)],
    )
    .await
    .expect("FinalizeBans");
    assert_eq!(fin_rep, PyRep::Bool(true));

    // 8. Drone, Lock, Move, Warp restrictions
    let drone_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "OverrideDroneRestrict",
        &[PyRep::Integer(2001), PyRep::Bool(true)],
    )
    .await
    .expect("OverrideDroneRestrict");
    assert_eq!(drone_rep, PyRep::Bool(true));

    let lock_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "OverrideLockRestrict",
        &[PyRep::Integer(2001), PyRep::Bool(true)],
    )
    .await
    .expect("OverrideLockRestrict");
    assert_eq!(lock_rep, PyRep::Bool(true));

    let move_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "OverrideMoveRestrict",
        &[PyRep::Integer(2001), PyRep::Bool(true)],
    )
    .await
    .expect("OverrideMoveRestrict");
    assert_eq!(move_rep, PyRep::Bool(true));

    let warp_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "OverrideWarpRestrict",
        &[PyRep::Integer(2001), PyRep::Bool(false)],
    )
    .await
    .expect("OverrideWarpRestrict");
    assert_eq!(warp_rep, PyRep::Bool(true));

    // 9. IsOtherMatchInProgress check
    let other_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "IsOtherMatchInProgress",
        &[PyRep::Integer(2001)],
    )
    .await
    .expect("IsOtherMatchInProgress");
    assert_eq!(other_rep, PyRep::Bool(false));
}

#[tokio::test]
async fn test_tourney_referee_overrides_and_teleport() {
    // parity: rpc:tourneyMgr.CompletePregameChecks, rpc:tourneyMgr.ExecuteMemberChange
    // parity: rpc:tourneyMgr.GetFancyDetails, rpc:tourneyMgr.OverrideError
    // parity: rpc:tourneyMgr.OverrideFleetBossID, rpc:tourneyMgr.RemoveSelfFromReferees
    // parity: rpc:tourneyMgr.ReturnAllPlayers, rpc:tourneyMgr.ReturnTeamPlayers
    // parity: rpc:tourneyMgr.TeleportPlayers, rpc:tourneyMgr.TeleportRefHome
    // parity: rpc:tourneyMgr.UpdateFleetDetails, rpc:tourneyMgr.CleanupGrid
    let ctx = ServerContext::new();
    let mut sess = select_pilot(&ctx, 80_003, "demo", "demo-secret", P1).await;

    // 1. Create match 3001
    rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1),
            PyRep::Integer(3001),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("CreateMatch 3001");

    // 2. Complete Pregame Checks
    let pregame = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "CompletePregameChecks",
        &[PyRep::Integer(3001)],
    )
    .await
    .expect("CompletePregameChecks");
    assert_eq!(pregame, PyRep::Bool(true));

    // 3. Execute Member Change
    let exec_res = rpc(&ctx, &mut sess, "tourneyMgr", "ExecuteMemberChange", &[])
        .await
        .expect("ExecuteMemberChange");
    assert_eq!(exec_res, PyRep::None);

    // 4. Get Fancy Details
    let fancy_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "GetFancyDetails",
        &[PyRep::Integer(3001)],
    )
    .await
    .expect("GetFancyDetails");
    let PyRep::Dict(fancy_dict) = fancy_rep else {
        panic!("expected Dict from GetFancyDetails");
    };

    // Verify index access for team 0 (Red) and team 1 (Blue)
    let red_bar = fancy_dict.iter().find(|(k, _)| k == &PyRep::Integer(0)).map(|(_, v)| v);
    assert!(red_bar.is_some());
    let blue_bar = fancy_dict.iter().find(|(k, _)| k == &PyRep::Integer(1)).map(|(_, v)| v);
    assert!(blue_bar.is_some());

    let match_id_val = fancy_dict
        .iter()
        .find(|(k, _)| k.as_str() == Some("matchID"))
        .and_then(|(_, v)| v.as_i64());
    assert_eq!(match_id_val, Some(3001));

    // 5. Override Fleet Boss IDs
    let fb_red = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "OverrideFleetBossID",
        &[PyRep::Integer(3001), PyRep::Integer(0), PyRep::Integer(90_000_100)],
    )
    .await
    .expect("OverrideFleetBossID Red");
    assert_eq!(fb_red, PyRep::Bool(true));

    let fb_blue = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "OverrideFleetBossID",
        &[PyRep::Integer(3001), PyRep::Integer(1), PyRep::Integer(90_000_200)],
    )
    .await
    .expect("OverrideFleetBossID Blue");
    assert_eq!(fb_blue, PyRep::Bool(true));

    // 6. Override Error
    let err_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "OverrideError",
        &[PyRep::Integer(3001), PyRep::Integer(P1), PyRep::String("PilotNotReady".into())],
    )
    .await
    .expect("OverrideError");
    assert_eq!(err_rep, PyRep::Bool(true));

    // 7. Teleport and Return players
    let tp_rep = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "TeleportPlayers",
        &[PyRep::Integer(3001), PyRep::Integer(0), PyRep::Integer(1)],
    )
    .await
    .expect("TeleportPlayers");
    assert_eq!(tp_rep, PyRep::Bool(true));

    let ret_team = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "ReturnTeamPlayers",
        &[PyRep::Integer(3001), PyRep::Integer(0)],
    )
    .await
    .expect("ReturnTeamPlayers");
    assert_eq!(ret_team, PyRep::Bool(true));

    let ret_all = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "ReturnAllPlayers",
        &[PyRep::Integer(3001)],
    )
    .await
    .expect("ReturnAllPlayers");
    assert_eq!(ret_all, PyRep::Bool(true));

    // 8. Teleport Ref Home
    let ref_home = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "TeleportRefHome",
        &[PyRep::Integer(3001)],
    )
    .await
    .expect("TeleportRefHome");
    assert_eq!(ref_home, PyRep::Bool(true));

    // 9. Update Fleet Details
    let update_fleet = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "UpdateFleetDetails",
        &[PyRep::Integer(3001), PyRep::Integer(0)],
    )
    .await
    .expect("UpdateFleetDetails");
    assert_eq!(update_fleet, PyRep::Bool(true));

    // 10. Cleanup Grid
    let cleanup = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "CleanupGrid",
        &[PyRep::Integer(3001)],
    )
    .await
    .expect("CleanupGrid");
    assert_eq!(cleanup, PyRep::Bool(true));

    // 11. Remove Self From Referees
    let rem_ref = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "RemoveSelfFromReferees",
        &[PyRep::Integer(3001)],
    )
    .await
    .expect("RemoveSelfFromReferees");
    assert_eq!(rem_ref, PyRep::Bool(true));
}
