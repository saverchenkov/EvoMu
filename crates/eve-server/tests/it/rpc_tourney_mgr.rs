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

    // Verify non-GM pilot is denied from creating tournament match
    sess.role = 0;
    let denied = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1),
            PyRep::Integer(1001),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await;
    assert_eq!(denied.unwrap_err().code, "AdminAccessDenied");

    // Restore GM role for match creation and administration
    sess.role = eve_machonet::service_roles::ROLE_GMH;

    // 1. Create match with explicit team/captain tuple
    let create_res = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1), // tourneyID
            PyRep::Tuple(vec![
                PyRep::Integer(1001),       // matchID
                PyRep::Integer(98_000_001), // red team corp ID
                PyRep::Integer(98_000_002), // blue team corp ID
                PyRep::Integer(P1),         // red captain
                PyRep::Integer(P2),         // blue captain
            ]),
            PyRep::Integer(100), // maxPoints
            PyRep::Integer(600), // matchTime
            PyRep::Integer(0),   // overtime
            PyRep::Integer(0),   // rulesetID
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

    // Verify duplicate active match ID is refused
    let dup_err = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1),
            PyRep::Integer(1001),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await;
    assert_eq!(dup_err.unwrap_err().code, "DuplicateMatchID");

    // Verify beacon balls spawned in match system ballpark
    let sys_handle = ctx.systems.get_or_spawn(30_000_142);
    let b1 = beacons[0].as_i64().expect("beacon 1 id");
    let b2 = beacons[1].as_i64().expect("beacon 2 id");
    assert!(sys_handle.get_ball_snapshot(b1).await.unwrap().is_some(), "Beacon ball 1 must exist in ballpark");
    assert!(sys_handle.get_ball_snapshot(b2).await.unwrap().is_some(), "Beacon ball 2 must exist in ballpark");

    // Verify AddPlayer on non-existent match fails with MatchNotFound and does not auto-create
    let missing_add = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "AddPlayer",
        &[PyRep::Integer(99_999), PyRep::Integer(0), PyRep::Integer(P1)],
    )
    .await;
    assert_eq!(missing_add.unwrap_err().code, "MatchNotFound");

    // Verify non-GM / non-referee pilot is denied from calling AddPlayer
    let mut sess_p2 = select_pilot(&ctx, 80_011, "other", "other-secret", P2).await;
    let non_ref_add = rpc(
        &ctx,
        &mut sess_p2,
        "tourneyMgr",
        "AddPlayer",
        &[PyRep::Integer(1001), PyRep::Integer(0), PyRep::Integer(P2)],
    )
    .await;
    assert_eq!(non_ref_add.unwrap_err().code, "AccessDenied");

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
    assert!(team_one.is_some() && team_one != Some("Red Alliance"));
    let nebula_id = cam_dict
        .iter()
        .find(|(k, _)| k.as_str() == Some("nebulaID"))
        .and_then(|(_, v)| v.as_i64());
    assert_eq!(nebula_id, Some(10_000_002));

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

    // BF-02: verify active ship query from snap (Rifter type 587, active ship 100_000_001)
    let PyRep::Tuple(p1_data) = &red_roster[0] else { panic!("expected pilot tuple"); };
    assert_eq!(p1_data[0].as_i64(), Some(P1));
    assert_eq!(p1_data[1].as_i64(), Some(587));
    assert_eq!(p1_data[2].as_i64(), Some(100_000_001));
    assert_eq!(p1_data[3].as_i64(), Some(2));

    // BF-01: unauthenticated call must fail
    let mut unauth_sess = SessionState::new(80_999);
    let unauth_res = rpc(
        &ctx,
        &mut unauth_sess,
        "tourneyMgr",
        "AddPlayer",
        &[PyRep::Integer(1001), PyRep::Integer(0), PyRep::Integer(P1)],
    )
    .await;
    assert!(unauth_res.is_err(), "unauthenticated AddPlayer must be rejected");

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
    let lock_unknown = rpc(&ctx, &mut sess, "tourneyMgr", "LockMemberList", &[PyRep::Integer(99_999), PyRep::Integer(0)]).await;
    assert_eq!(lock_unknown.unwrap_err().code, "MatchNotFound");

    let lock_rep = rpc(&ctx, &mut sess, "tourneyMgr", "LockMemberList", &[PyRep::Integer(1001), PyRep::Integer(0)]).await.expect("LockMemberList");
    assert_eq!(lock_rep, PyRep::Bool(true));

    let locked_add = rpc(&ctx, &mut sess, "tourneyMgr", "AddPlayer", &[PyRep::Integer(1001), PyRep::Integer(0), PyRep::Integer(P2)]).await;
    assert_eq!(locked_add.unwrap_err().code, "MemberListLocked");

    let unlock_rep = rpc(&ctx, &mut sess, "tourneyMgr", "UnlockMemberList", &[PyRep::Integer(1001), PyRep::Integer(0)]).await.expect("UnlockMemberList");
    assert_eq!(unlock_rep, PyRep::Bool(true));

    let unlock_add = rpc(&ctx, &mut sess, "tourneyMgr", "AddPlayer", &[PyRep::Integer(1001), PyRep::Integer(0), PyRep::Integer(P2)]).await;
    assert!(unlock_add.is_ok());

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

    // 9. Match Completed removes beacon balls from ballpark
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "MatchCompleted", &[PyRep::Integer(1001)]).await.expect("MatchCompleted"), PyRep::Bool(true));
    assert!(sys_handle.get_ball_snapshot(b1).await.unwrap().is_none(), "Beacon 1 must be despawned on MatchCompleted");
    assert!(sys_handle.get_ball_snapshot(b2).await.unwrap().is_none(), "Beacon 2 must be despawned on MatchCompleted");

    // 10. Reset Match clears bans, member list locks, and restores state to Setup
    assert!(rpc(&ctx, &mut sess, "tourneyMgr", "BanShipType", &[PyRep::Integer(1001), PyRep::Integer(0), PyRep::Integer(620)]).await.is_err(), "BanShipType must be rejected in Completed state");
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "ResetMatch", &[PyRep::Integer(1001)]).await.expect("ResetMatch to Setup"), PyRep::Bool(true));
    rpc(&ctx, &mut sess, "tourneyMgr", "LockMemberList", &[PyRep::Integer(1001), PyRep::Integer(0)]).await.expect("Lock before reset");
    rpc(&ctx, &mut sess, "tourneyMgr", "BanShipType", &[PyRep::Integer(1001), PyRep::Integer(0), PyRep::Integer(620)]).await.expect("Ban before reset");
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "ResetMatch", &[PyRep::Integer(1001)]).await.expect("ResetMatch"), PyRep::Bool(true));
    let after_reset = rpc(&ctx, &mut sess, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(1001)]).await.expect("GetFancyDetails after ResetMatch");
    assert_eq!(after_reset.dict_get("details").and_then(|d| d.dict_get("state")).and_then(|s| s.as_str()), Some("Setup"));
    assert_eq!(after_reset.dict_get("bans").and_then(|b| b.dict_get("red")), Some(&PyRep::List(Vec::new())));
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "AddPlayer", &[PyRep::Integer(1001), PyRep::Integer(0), PyRep::Integer(P2)]).await.expect("AddPlayer after ResetMatch unlocked"), PyRep::Bool(true));
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

    // 1. Query ship list and verify canonical subcapital tournament hulls (Rifter 587, Osprey 620, Moa 624)
    let ships_rep = rpc(&ctx, &mut sess, "tourneyMgr", "QueryShipList", &[PyRep::Integer(1)]).await.expect("QueryShipList");
    let Some(PyRep::List(allowed_ships)) = ships_rep.dict_get("allowedShipList") else { panic!("expected allowedShipList"); };
    assert!(allowed_ships.contains(&PyRep::Integer(587)) && allowed_ships.contains(&PyRep::Integer(620)) && allowed_ships.contains(&PyRep::Integer(624)));

    // Elevate to GM role for match creation
    sess.role = eve_machonet::service_roles::ROLE_GMH;

    // 2. Create match 2001
    rpc(&ctx, &mut sess, "tourneyMgr", "CreateMatch", &[
        PyRep::Integer(1), PyRep::Integer(2001), PyRep::Integer(100), PyRep::Integer(600), PyRep::Integer(0), PyRep::Integer(0),
    ]).await.expect("CreateMatch 2001");

    // 3. BanShip (captain submit) records 620 and 624 in match bans
    let ban_ship_rep = rpc(&ctx, &mut sess, "tourneyMgr", "BanShip", &[
        PyRep::Integer(2001), PyRep::List(vec![PyRep::Integer(620), PyRep::Integer(624)]),
    ]).await.expect("BanShip");
    assert_eq!(ban_ship_rep, PyRep::Bool(true));
    let fancy_after_banship = rpc(&ctx, &mut sess, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(2001)]).await.expect("GetFancyDetails after BanShip");
    assert_eq!(fancy_after_banship.dict_get("bans").and_then(|b| b.dict_get("red")), Some(&PyRep::List(vec![PyRep::Integer(620), PyRep::Integer(624)])));

    // 4. Start Autobanning with V24 kwargs (totalNumBans, redCharID, blueCharID)
    let _ = ctx.drain_notifications_for_char(P1);
    let mut autoban_kw = HashMap::new();
    autoban_kw.insert("totalNumBans".to_string(), PyRep::Integer(4));
    autoban_kw.insert("redCharID".to_string(), PyRep::Integer(P1));
    autoban_kw.insert("blueCharID".to_string(), PyRep::Integer(P2));
    let auto_rep = ctx.dispatch_rpc(&mut sess, "tourneyMgr", None, "StartAutobanning", &[PyRep::Integer(2001)], &autoban_kw, None).await.expect("StartAutobanning");
    assert_eq!(auto_rep, PyRep::Bool(true));
    assert!(ctx.drain_notifications_for_char(P1).iter().any(|n| n.event == "OnTournamentPerformBan"), "StartAutobanning must notify captain with OnTournamentPerformBan");

    // Verify autoban transitioned state to Banning and populated 4 ship-type bans per team
    let fancy_bans = rpc(&ctx, &mut sess, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(2001)]).await.expect("GetFancyDetails");
    assert_eq!(fancy_bans.dict_get("details").and_then(|d| d.dict_get("state")).and_then(|s| s.as_str()), Some("Banning"));
    let Some(PyRep::List(rb_list)) = fancy_bans.dict_get("bans").and_then(|b| b.dict_get("red")) else { panic!("expected red bans list"); };
    let Some(PyRep::List(bb_list)) = fancy_bans.dict_get("bans").and_then(|b| b.dict_get("blue")) else { panic!("expected blue bans list"); };
    assert_eq!(rb_list.len(), 4, "Autoban must have recorded 4 bans for red");
    assert_eq!(bb_list.len(), 4, "Autoban must have recorded 4 bans for blue");
    let blue_autoban_0 = bb_list[0].as_i64().expect("blue autoban type id");

    // 5. Verify unauthorized pilot (non-GM, non-referee, non-captain) is denied from BanShipType / UnBanShipType
    let mut sess_unauth = sess.clone();
    sess_unauth.session_id = 80_099;
    sess_unauth.user_id = Some(80_099);
    sess_unauth.character_id = Some(90_000_099);
    sess_unauth.role = 0;
    let unauth_ban = rpc(
        &ctx,
        &mut sess_unauth,
        "tourneyMgr",
        "BanShipType",
        &[PyRep::Integer(2001), PyRep::Integer(0), PyRep::Integer(620)],
    )
    .await;
    assert_eq!(unauth_ban.unwrap_err().code, "AccessDenied");

    let unauth_unban = rpc(
        &ctx,
        &mut sess_unauth,
        "tourneyMgr",
        "UnBanShipType",
        &[PyRep::Integer(2001), PyRep::Integer(0), PyRep::Integer(620)],
    )
    .await;
    assert_eq!(unauth_unban.unwrap_err().code, "AccessDenied");

    // 5. BanShipType for Red and Blue (referee/GM authorized)
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

    let free_blue = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "UnBanShipType",
        &[PyRep::Integer(2001), PyRep::Integer(1), PyRep::Integer(blue_autoban_0)],
    )
    .await
    .expect("UnBanShipType Blue autoban slot");
    assert_eq!(free_blue, PyRep::Bool(true));

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

    // 7. Drone, Lock, Move, Warp restrictions
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideDroneRestrict", &[PyRep::Integer(2001), PyRep::Bool(true)]).await.expect("OverrideDroneRestrict"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideLockRestrict", &[PyRep::Integer(2001), PyRep::Bool(true)]).await.expect("OverrideLockRestrict"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideMoveRestrict", &[PyRep::Integer(2001), PyRep::Bool(true)]).await.expect("OverrideMoveRestrict"), PyRep::Bool(true));

    // Banning P1's active ship type (Rifter 587) rejects AddPlayer and persists pilot_errors until cleared via OverrideError
    rpc(&ctx, &mut sess, "tourneyMgr", "BanShipType", &[PyRep::Integer(2001), PyRep::Integer(0), PyRep::Integer(587)]).await.expect("BanShipType 587");
    let banned_add = rpc(&ctx, &mut sess, "tourneyMgr", "AddPlayer", &[PyRep::Integer(2001), PyRep::Integer(0), PyRep::Integer(P1)]).await;
    assert_eq!(banned_add.unwrap_err().code, "TourneyShipTypeBanned");
    rpc(&ctx, &mut sess, "tourneyMgr", "UnBanShipType", &[PyRep::Integer(2001), PyRep::Integer(0), PyRep::Integer(587)]).await.expect("UnBanShipType 587");
    rpc(&ctx, &mut sess, "tourneyMgr", "AddPlayer", &[PyRep::Integer(2001), PyRep::Integer(0), PyRep::Integer(P1)]).await.expect("AddPlayer P1 after unban");

    // 8. FinalizeBans excludes banned hulls (e.g. 624 Moa) from QueryShipList(2001) and locks further bans
    let fin_rep = rpc(&ctx, &mut sess, "tourneyMgr", "FinalizeBans", &[PyRep::Integer(2001)]).await.expect("FinalizeBans");
    assert_eq!(fin_rep, PyRep::Bool(true));
    let post_fin_ships = rpc(&ctx, &mut sess, "tourneyMgr", "QueryShipList", &[PyRep::Integer(2001)]).await.expect("QueryShipList after FinalizeBans");
    let Some(PyRep::List(post_allowed)) = post_fin_ships.dict_get("allowedShipList") else { panic!("expected allowedShipList"); };
    assert!(!post_allowed.contains(&PyRep::Integer(624)), "Finalized banned hull 624 must be excluded from QueryShipList");
    assert!(post_allowed.contains(&PyRep::Integer(587)), "Unbanned Rifter 587 must remain in QueryShipList");
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "CompletePregameChecks", &[PyRep::Integer(2001)]).await.unwrap_err().code, "TourneyShipTypeBanned", "Persisted pilot_errors must block CompletePregameChecks until overridden");
    rpc(&ctx, &mut sess, "tourneyMgr", "OverrideError", &[PyRep::Integer(2001), PyRep::Integer(P1), PyRep::String("TourneyShipTypeBanned".into())]).await.expect("OverrideError");
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "CompletePregameChecks", &[PyRep::Integer(2001)]).await.expect("CompletePregameChecks after OverrideError"), PyRep::Bool(true));
    assert!(
        eve_server::services::tourney_service::check_tourney_warp_restricted(&ctx, 30_000_142, P1).is_ok(),
        "Warp must not be restricted while match is in Setup"
    );

    // Transition match 2001 into Countdown; default restrictions (true) block warp, move, lock, and drone launch
    rpc(&ctx, &mut sess, "tourneyMgr", "StartCountdown", &[PyRep::Integer(2001)]).await.expect("StartCountdown 2001");
    assert_eq!(eve_server::services::tourney_service::check_tourney_warp_restricted(&ctx, 30_000_142, P1).unwrap_err().code, "WarpRestrictedInTourney");
    assert_eq!(eve_server::services::tourney_service::check_tourney_drone_restricted(&ctx, 30_000_142, P1).unwrap_err().code, "DroneRestrictedInTourney");
    assert_eq!(eve_server::services::tourney_service::check_tourney_lock_restricted(&ctx, 30_000_142, P1).unwrap_err().code, "LockRestrictedInTourney");
    assert_eq!(eve_server::services::tourney_service::check_tourney_move_restricted(&ctx, 30_000_142, P1).unwrap_err().code, "MoveRestrictedInTourney");

    // Verify space/combat/dogma RPCs enforce the restrictions when pilot is in match system
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.station_id = None;
            ch.solar_system_id = 30_000_142;
        }
    });
    sess.station_id = None;
    sess.solar_system_id = Some(30_000_142);
    sess.ship_id = Some(100_000_001);
    let beyonce_oid = ctx.bind_object(&sess, "beyonce", 30_000_142).unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(60_003_760)], &HashMap::new(), None).await.unwrap_err().code, "WarpRestrictedInTourney");
    assert_eq!(rpc(&ctx, &mut sess, "beyonce", "CmdSetSpeedFraction", &[PyRep::Real(0.5)]).await.unwrap_err().code, "MoveRestrictedInTourney");
    assert_eq!(rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(100_000_002)]).await.unwrap_err().code, "LockRestrictedInTourney");
    assert_eq!(rpc(&ctx, &mut sess, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(999_001)])]).await.unwrap_err().code, "DroneRestrictedInTourney");

    // When countdown completes and match transitions to InProgress, warp/move/lock/drones are allowed by default without override
    ctx.advance_virtual_clock(65);
    assert!(eve_server::services::tourney_service::check_tourney_warp_restricted(&ctx, 30_000_142, P1).is_ok());
    assert!(eve_server::services::tourney_service::check_tourney_drone_restricted(&ctx, 30_000_142, P1).is_ok());
    assert!(eve_server::services::tourney_service::check_tourney_lock_restricted(&ctx, 30_000_142, P1).is_ok());
    assert!(eve_server::services::tourney_service::check_tourney_move_restricted(&ctx, 30_000_142, P1).is_ok());

    // Override*Restrict(2001, true) restricts warp, drone, lock, and movement
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideWarpRestrict", &[PyRep::Integer(2001), PyRep::Bool(true)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideDroneRestrict", &[PyRep::Integer(2001), PyRep::Bool(true)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideLockRestrict", &[PyRep::Integer(2001), PyRep::Bool(true)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideMoveRestrict", &[PyRep::Integer(2001), PyRep::Bool(true)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(eve_server::services::tourney_service::check_tourney_warp_restricted(&ctx, 30_000_142, P1).unwrap_err().code, "WarpRestrictedInTourney");
    assert_eq!(eve_server::services::tourney_service::check_tourney_drone_restricted(&ctx, 30_000_142, P1).unwrap_err().code, "DroneRestrictedInTourney");
    assert_eq!(eve_server::services::tourney_service::check_tourney_lock_restricted(&ctx, 30_000_142, P1).unwrap_err().code, "LockRestrictedInTourney");
    assert_eq!(eve_server::services::tourney_service::check_tourney_move_restricted(&ctx, 30_000_142, P1).unwrap_err().code, "MoveRestrictedInTourney");

    // Override*Restrict(2001, false) unlocks warp, drone, lock, and movement
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideWarpRestrict", &[PyRep::Integer(2001), PyRep::Bool(false)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideDroneRestrict", &[PyRep::Integer(2001), PyRep::Bool(false)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideLockRestrict", &[PyRep::Integer(2001), PyRep::Bool(false)]).await.unwrap(), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideMoveRestrict", &[PyRep::Integer(2001), PyRep::Bool(false)]).await.unwrap(), PyRep::Bool(true));
    assert!(eve_server::services::tourney_service::check_tourney_warp_restricted(&ctx, 30_000_142, P1).is_ok());
    assert!(eve_server::services::tourney_service::check_tourney_drone_restricted(&ctx, 30_000_142, P1).is_ok());
    assert!(eve_server::services::tourney_service::check_tourney_lock_restricted(&ctx, 30_000_142, P1).is_ok());
    assert!(eve_server::services::tourney_service::check_tourney_move_restricted(&ctx, 30_000_142, P1).is_ok());

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


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod rpc_tourney_mgr_ext_b {
    use super::*;

// Part 2 of integration tests covering tourneyMgr referee overrides and teleports.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn select_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
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
    sess.role = eve_machonet::service_roles::ROLE_GMH;

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

    let exec_add = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "ExecuteMemberChange",
        &[PyRep::Integer(3001), PyRep::Integer(0), PyRep::Integer(P1), PyRep::Bool(true)],
    )
    .await
    .expect("ExecuteMemberChange Add");
    assert_eq!(exec_add, PyRep::None);

    // Banning P1's ship (587) fails CompletePregameChecks and TeleportPlayers until unbanned and overridden
    rpc(&ctx, &mut sess, "tourneyMgr", "BanShipType", &[PyRep::Integer(3001), PyRep::Integer(0), PyRep::Integer(587)]).await.expect("Ban 587");
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "CompletePregameChecks", &[PyRep::Integer(3001)]).await.unwrap_err().code, "TourneyShipTypeBanned");
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "TeleportPlayers", &[PyRep::Integer(3001), PyRep::Integer(0), PyRep::Integer(1)]).await.unwrap_err().code, "TourneyShipTypeBanned");
    rpc(&ctx, &mut sess, "tourneyMgr", "UnBanShipType", &[PyRep::Integer(3001), PyRep::Integer(0), PyRep::Integer(587)]).await.expect("UnBan 587");
    rpc(&ctx, &mut sess, "tourneyMgr", "OverrideError", &[PyRep::Integer(3001), PyRep::Integer(P1), PyRep::String(String::new())]).await.expect("Clear P1 pilot_errors");
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "CompletePregameChecks", &[PyRep::Integer(3001)]).await.unwrap(), PyRep::Bool(true));

    // Match 3002 with maxPoints=1 rejects P1 (Rifter = 2 points) on AddPlayer and ExecuteMemberChange
    rpc(&ctx, &mut sess, "tourneyMgr", "CreateMatch", &[PyRep::Integer(1), PyRep::Integer(3002), PyRep::Integer(1), PyRep::Integer(600), PyRep::Integer(0), PyRep::Integer(0)]).await.expect("CreateMatch 3002");
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "AddPlayer", &[PyRep::Integer(3002), PyRep::Integer(0), PyRep::Integer(P1)]).await.unwrap_err().code, "TourneyMaxPointsExceeded");
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "ExecuteMemberChange", &[PyRep::Integer(3002), PyRep::Integer(0), PyRep::Integer(P1), PyRep::Bool(true)]).await.unwrap_err().code, "TourneyMaxPointsExceeded");

    // 4. Get Fancy Details (and verify MatchNotFound on unknown match ID)
    let missing_fancy = rpc(&ctx, &mut sess, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(99_999)]).await;
    assert_eq!(missing_fancy.unwrap_err().code, "MatchNotFound");
    let missing_members = rpc(&ctx, &mut sess, "tourneyMgr", "GetTeamMembers", &[PyRep::Integer(99_999)]).await;
    assert_eq!(missing_members.unwrap_err().code, "MatchNotFound");

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

    let red_bar = fancy_dict.iter().find(|(k, _)| k == &PyRep::Integer(0)).map(|(_, v)| v).expect("red bar");
    let blue_bar = fancy_dict.iter().find(|(k, _)| k == &PyRep::Integer(1)).map(|(_, v)| v).expect("blue bar");
    let details_tup = fancy_dict.iter().find(|(k, _)| k == &PyRep::Integer(2)).map(|(_, v)| v).expect("details tuple");
    assert_eq!(
        details_tup,
        &PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(600), PyRep::Integer(0), PyRep::None, PyRep::Integer(100)]),
        "GetFancyDetails[2] must return V24 6-tuple (stateInt, startTime, matchTime, overtime, winner, maxPoints)"
    );

    let PyRep::Tuple(red_stats) = red_bar else { panic!("expected red bar tuple"); };
    let base_ehp = red_stats[0].as_i64().unwrap_or(0);
    let base_dps = red_stats[1].as_i64().unwrap_or(0);
    assert!(base_ehp > 1000, "SDE dogma hull EHP for Rifter must be > 1000, got {base_ehp}");
    assert_eq!(base_dps, 148, "Rifter SDE hardpoints + drone bandwidth base DPS must equal 148");
    assert_eq!(red_stats[4].as_i64(), Some(0), "initial match score must be 0");
    let PyRep::Tuple(blue_stats) = blue_bar else { panic!("expected blue bar tuple"); };
    assert_eq!(blue_stats[0].as_i64(), Some(0), "empty blue team has 0 EHP");

    // Fitting a high-slot weapon module to P1's active ship (100_000_001) increases totalDPS by (3.0 * 25.0 / 2.0) = 38
    ctx.db.with_mut_snapshot(|snap| {
        let mut attrs = HashMap::new();
        attrs.insert(64, 3.0); // damageMultiplier
        attrs.insert(51, 2000.0); // rate of fire (ms)
        snap.items.insert(
            999_777_111,
            eve_db::InventoryItemRecord {
                item_id: 999_777_111,
                type_id: 488, // 200mm AutoCannon I
                owner_id: P1,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: attrs,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let fancy_with_gun = rpc(&ctx, &mut sess, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(3001)]).await.expect("GetFancyDetails with gun");
    let PyRep::Dict(fancy_gun_dict) = fancy_with_gun else { panic!("expected Dict"); };
    let PyRep::Tuple(red_stats_gun) = fancy_gun_dict.iter().find(|(k, _)| k == &PyRep::Integer(0)).map(|(_, v)| v).unwrap() else { panic!("expected tuple"); };
    assert_eq!(red_stats_gun[1].as_i64(), Some(186), "Fitted weapon module must increase team totalDPS from 148 to 186");

    let match_id_val = fancy_dict
        .iter()
        .find(|(k, _)| k.as_str() == Some("matchID"))
        .and_then(|(_, v)| v.as_i64());
    assert_eq!(match_id_val, Some(3001));

    // 5. Override Fleet Boss IDs and verify persistence via GetFancyDetails
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideFleetBossID", &[PyRep::Integer(3001), PyRep::Integer(0), PyRep::Integer(P1)]).await.expect("OverrideFleetBossID Red"), PyRep::Bool(true));
    assert_eq!(rpc(&ctx, &mut sess, "tourneyMgr", "OverrideFleetBossID", &[PyRep::Integer(3001), PyRep::Integer(1), PyRep::Integer(90_000_200)]).await.expect("OverrideFleetBossID Blue"), PyRep::Bool(true));
    let fancy_bosses = rpc(&ctx, &mut sess, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(3001)]).await.expect("GetFancyDetails after OverrideFleetBossID");
    assert_eq!(
        fancy_bosses.dict_get("details").and_then(|d| d.dict_get("fleetBossIDs")),
        Some(&PyRep::List(vec![PyRep::Integer(P1), PyRep::Integer(90_000_200)])),
        "OverrideFleetBossID must update match fleetBossIDs in GetFancyDetails"
    );

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

    // 7. Teleport and Return players update ship location, ballpark, and cross-session state
    rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "ExecuteMemberChange",
        &[PyRep::Integer(3001), PyRep::Integer(1), PyRep::Integer(P2), PyRep::Bool(true)],
    )
    .await
    .expect("Add P2 to Blue Team");
    let mut sess_p2 = select_pilot(&ctx, 80_019, "other", "other-secret", P2).await;
    assert_eq!(sess_p2.station_id, Some(60_003_760));

    let jita_sys = ctx.systems.get_or_spawn(30000142);
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
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&100_000_001).map(|i| i.location_id)),
        Some(30_000_142),
        "TeleportPlayers must move active ship into match solar system"
    );
    assert!(jita_sys.get_ball_snapshot(100_000_001).await.unwrap().is_some(), "Ship ball must spawn in match ballpark");

    rpc(&ctx, &mut sess, "tourneyMgr", "TeleportPlayers", &[PyRep::Integer(3001), PyRep::Integer(1), PyRep::Integer(0)])
        .await
        .expect("TeleportPlayers Blue Team");
    rpc(&ctx, &mut sess_p2, "tourneyMgr", "GetMatchSystem", &[PyRep::Integer(3001)])
        .await
        .expect("GetMatchSystem as P2");
    assert_eq!(sess_p2.station_id, None, "Teleported non-referee pilot session must sync station_id=None");
    assert_eq!(sess_p2.solar_system_id, Some(30_000_142));

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
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items.get(&100_000_001).map(|i| i.location_id)),
        Some(60_003_760),
        "ReturnTeamPlayers must dock ship back at home station"
    );
    assert!(jita_sys.get_ball_snapshot(100_000_001).await.unwrap().is_none(), "Ship ball must be removed from ballpark on ReturnTeamPlayers");

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
    rpc(&ctx, &mut sess_p2, "tourneyMgr", "GetMatchSystem", &[PyRep::Integer(3001)])
        .await
        .expect("GetMatchSystem as P2 after return");
    assert_eq!(sess_p2.station_id, Some(60_003_760), "Returned non-referee pilot session must sync back to home station");

    // 8. Teleport Ref Home updates referee ship location and session
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
    assert_eq!(sess.station_id, Some(60_003_760));

    // 9. Update Fleet Details links active fleet in snap.fleets
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            77_001,
            eve_db::FleetRecord {
                fleet_id: 77_001,
                boss_id: P1,
                motd: String::new(),
                free_move: true,
                is_registered: false,
                wings: Vec::new(),
                squads: Vec::new(),
                members: vec![P1],
                wing_details: HashMap::new(),
                squad_details: HashMap::new(),
                member_details: HashMap::new(),
                invites: std::collections::HashSet::new(),
            },
        );
        snap.items.insert(
            999_888_777,
            eve_db::InventoryItemRecord {
                item_id: 999_888_777,
                type_id: 26557,
                owner_id: P1,
                location_id: 30000142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
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
    let fancy_fleets = rpc(&ctx, &mut sess, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(3001)]).await.expect("GetFancyDetails fleets");
    let PyRep::Dict(ff_dict) = fancy_fleets else { panic!("expected Dict"); };
    let PyRep::Dict(details_dict) = ff_dict.iter().find(|(k, _)| k.as_str() == Some("details")).map(|(_, v)| v).unwrap() else { panic!("expected details dict"); };
    let PyRep::List(fleet_ids) = details_dict.iter().find(|(k, _)| k.as_str() == Some("fleetIDs")).map(|(_, v)| v).unwrap() else { panic!("expected fleetIDs list"); };
    assert_eq!(
        fleet_ids[0].as_i64(),
        Some(77_001),
        "UpdateFleetDetails must link active fleet from snap.fleets"
    );
    let _ = jita_sys
        .spawn_ball(eve_destiny::Ball {
            id: 999_888_777,
            type_id: 26557,
            position: glam::DVec3::new(100.0, 200.0, 300.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

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
    assert!(!ctx.db.with_snapshot(|snap| snap.items.contains_key(&999_888_777)));
    assert!(jita_sys.get_ball_position_sync(999_888_777).is_none());

    // 11. Remove Self From Referees: drop GM role so referee status comes solely from m.referees
    sess.role = 0;
    let lock_as_ref = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "LockMemberList",
        &[PyRep::Integer(3001), PyRep::Integer(0)],
    )
    .await
    .expect("referee without GM role should still pass check_referee");
    assert_eq!(lock_as_ref, PyRep::Bool(true));

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

    // Repeat RemoveSelfFromReferees must fail now that caller is no longer in m.referees
    let rem_again = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "RemoveSelfFromReferees",
        &[PyRep::Integer(3001)],
    )
    .await;
    assert_eq!(
        rem_again.expect_err("second RemoveSelfFromReferees must fail").code,
        "TourneyNotReferee"
    );

    // Former referee (without GM role) is now denied referee-only operations
    let denied_former_ref = rpc(
        &ctx,
        &mut sess,
        "tourneyMgr",
        "ResetMatch",
        &[PyRep::Integer(3001)],
    )
    .await;
    assert_eq!(
        denied_former_ref.expect_err("former referee ResetMatch must be rejected").code,
        "AccessDenied"
    );

    // 12. Referee authorization check for another non-referee pilot
    let mut sess_other = select_pilot(&ctx, 80_004, "other", "other-secret", P2).await;
    let denied_err = rpc(&ctx, &mut sess_other, "tourneyMgr", "ResetMatch", &[PyRep::Integer(3001)]).await;
    assert!(denied_err.is_err(), "non-referee ResetMatch must be rejected");
    assert_eq!(denied_err.unwrap_err().code, "AccessDenied");
}

}
