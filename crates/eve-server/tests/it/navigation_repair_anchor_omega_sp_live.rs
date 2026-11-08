use crate::common;
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_888;
const JITA_4_4: i64 = 60_003_760;

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, m: &str, a: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(s, svc, None, m, a, &HashMap::new(), None).await
}

#[tokio::test]
async fn test_warp_vector_item() {
    let ctx = ServerContext::new();
    let mut p = common::gm_pilot_in_space(&ctx, 1, P1, 606, 30_000_142).await;
    let ship_id = p.ship_id;
    let sys_id = 30_000_142;

    let vec_item_id = common::stock_ship_bay(&ctx, &mut p, 77825, 1, InventoryFlag::Cargo as i64).await;

    let res = call(&ctx, &mut p.sess, "warpVectorMgr", "UseVector", &[PyRep::Integer(vec_item_id)])
        .await
        .unwrap();
    let PyRep::KeyVal(dict) = res else { panic!("Expected KeyVal"); };
    assert_eq!(dict.get("consumed").unwrap().as_bool(), Some(true));

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (_, ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(ball.mode, eve_destiny::MovementMode::Warp { .. }));
}

#[tokio::test]
async fn test_pochven_wormhole_scan_and_jump() {
    let ctx = ServerContext::new();

    let sigs = eve_server::services::space_destiny::space_pochven_wormholes::pochven_static_wormholes(&ctx.sde, 30_000_142, 1, 1234567);
    assert_eq!(sigs.len(), 2);
    assert_eq!(sigs[0].lifetime_seconds, 43200);
    assert_eq!(sigs[0].max_jump_mass_kg, 410_000_000.0);
}

#[tokio::test]
async fn test_repairshop_quotes_and_restoration() {
    let ctx = ServerContext::new();
    let mut p = common::gm_pilot_in_space(&ctx, 2, P1, 606, 30_000_142).await;

    call(&ctx, &mut p.sess, "beyonce", "CmdDock", &[PyRep::Integer(JITA_4_4)])
        .await
        .expect("CmdDock Jita 4-4");
    assert_eq!(p.sess.station_id, Some(JITA_4_4));

    let res = call(&ctx, &mut p.sess, "repairSvc", "GetRepairQuotes", &[PyRep::List(vec![])])
        .await
        .unwrap();
    let PyRep::Dict(entries) = res else { panic!("not dict") };
    assert!(!entries.is_empty());
}

#[tokio::test]
async fn test_anchor_restrictions() {
    let ctx = ServerContext::new();
    let mut p = common::gm_pilot_in_space(&ctx, 3, P1, 20185, 30_000_142).await;

    call(&ctx, &mut p.sess, "beyonce", "CmdDock", &[PyRep::Integer(JITA_4_4)])
        .await
        .expect("CmdDock Jita 4-4");
    ctx.advance_secs(11);
    common::stock_ship_bay(&ctx, &mut p, 35832, 1, InventoryFlag::Cargo as i64).await;
    common::slash_cmd(&ctx, &mut p.sess, "/giveskill 3363 1").await;
    common::slash_cmd(&ctx, &mut p.sess, "/giveisk 5000000").await;
    let created = call(
        &ctx,
        &mut p.sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Nav Anchor Corp".into()),
            PyRep::String("NAVA".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let cid = created.dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(p.sess.corporation_id, Some(cid));
    ctx.advance_secs(7 * 86_400 + 60);
    call(&ctx, &mut p.sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");

    let res = call(&ctx, &mut p.sess, "structureDeployment", "Anchor", &[PyRep::Integer(35832), PyRep::String("Test Astrahus".into())]).await;
    let err = res.unwrap_err();
    assert_eq!(err.code, "AnchoringDisallowedInTradeHub");
}

#[tokio::test]
async fn test_omega_weekly_free_sp() {
    let ctx = ServerContext::new();
    let mut p = common::gm_pilot_in_space(&ctx, 4, P1, 606, 30_000_142).await;
    let uid = p.sess.user_id.unwrap();
    let exp = ctx.now_unix() + 30 * 86_400;
    common::slash_cmd(&ctx, &mut p.sess, &format!("/setomega {uid} {exp}")).await;

    let res = call(&ctx, &mut p.sess, "storeManager", "buy_offer", &[PyRep::Integer(1005)]).await.unwrap();
    let PyRep::KeyVal(dict) = res else { panic!("Expected KeyVal"); };
    assert_eq!(dict.get("freeSkillPointsGranted").unwrap().as_i64(), Some(15000));

    let res2 = call(&ctx, &mut p.sess, "storeManager", "buy_offer", &[PyRep::Integer(1005)]).await;
    assert_eq!(res2.unwrap_err().code.to_ascii_lowercase(), "weeklyofferalreadyclaimed");
}
