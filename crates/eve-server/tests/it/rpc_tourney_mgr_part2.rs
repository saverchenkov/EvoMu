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

    let red_bar = fancy_dict.iter().find(|(k, _)| k == &PyRep::Integer(0)).map(|(_, v)| v).expect("red bar");
    let blue_bar = fancy_dict.iter().find(|(k, _)| k == &PyRep::Integer(1)).map(|(_, v)| v).expect("blue bar");

    let PyRep::Tuple(red_stats) = red_bar else { panic!("expected red bar tuple"); };
    assert!(red_stats[0].as_i64().unwrap_or(0) > 0, "dynamic totalEHP must be > 0");
    assert!(red_stats[1].as_i64().unwrap_or(0) > 0, "dynamic totalDPS must be > 0");
    let PyRep::Tuple(blue_stats) = blue_bar else { panic!("expected blue bar tuple"); };
    assert_eq!(blue_stats[0].as_i64(), Some(0), "empty blue team has 0 EHP");

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

    ctx.db.with_mut_snapshot(|snap| {
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
    let jita_sys = ctx.systems.get_or_spawn(30000142);
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
