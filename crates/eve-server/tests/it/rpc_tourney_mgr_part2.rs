//! Part 2 of integration tests covering tourneyMgr referee overrides and teleports.

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

    let PyRep::Tuple(red_stats) = red_bar else { panic!("expected red bar tuple"); };
    let base_ehp = red_stats[0].as_i64().unwrap_or(0);
    let base_dps = red_stats[1].as_i64().unwrap_or(0);
    assert!(base_ehp > 0, "dynamic totalEHP must be > 0");
    assert!(base_dps > 0, "dynamic totalDPS must be > 0");
    let PyRep::Tuple(blue_stats) = blue_bar else { panic!("expected blue bar tuple"); };
    assert_eq!(blue_stats[0].as_i64(), Some(0), "empty blue team has 0 EHP");

    // Fitting a high-slot weapon module to P1's active ship (100_000_001) increases totalDPS
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
    assert!(red_stats_gun[1].as_i64().unwrap_or(0) > base_dps, "Fitted weapon module must increase team totalDPS");

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
        &[PyRep::Integer(3001), PyRep::Integer(0), PyRep::Integer(P1)],
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

    // 12. Referee authorization check
    let mut sess_other = select_pilot(&ctx, 80_004, "other", "other-secret", P2).await;
    let denied_err = rpc(&ctx, &mut sess_other, "tourneyMgr", "ResetMatch", &[PyRep::Integer(3001)]).await;
    assert!(denied_err.is_err(), "non-referee ResetMatch must be rejected");
    assert_eq!(denied_err.unwrap_err().code, "AccessDenied");
}
