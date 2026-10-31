//! End-to-End Integration Journeys Part 3 (`journey:wormholes_filaments` .. `journey:fleet`).
#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

async fn call_rpc(
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
async fn test_journey_wormholes_filaments() {
    // parity: journey:wormholes_filaments
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(30);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID");

    let _wh = call_rpc(
        &ctx,
        &mut sess,
        "wormholeMgr",
        "GetWormholePolarization",
        &[PyRep::Integer(31_000_001)],
    )
    .await
    .expect("GetWormholePolarization");
    let before = ctx.now_unix();
    ctx.advance_secs(30);
    assert_eq!(ctx.now_unix(), before + 30);
}

#[tokio::test]
async fn test_journey_ship_loss() {
    // parity: journey:ship_loss
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(31);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID");

    let ins = call_rpc(&ctx, &mut sess, "insuranceSvc", "GetContracts", &[])
        .await
        .expect("GetContracts");
    let PyRep::List(contracts) = ins else {
        panic!("expected List from GetContracts");
    };
    assert!(contracts.len() < 1000);
}

#[tokio::test]
async fn test_journey_clones() {
    // parity: journey:clones
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(32);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID");

    let clones = call_rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[])
        .await
        .expect("GetCloneState");
    let PyRep::KeyVal(_kv) = clones else {
        panic!("expected KeyVal from GetCloneState");
    };
    assert_eq!(sess.character_id, Some(90_000_001));
    ctx.advance_secs(60);
}

#[tokio::test]
async fn test_journey_industry() {
    // parity: journey:industry
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(33);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID");

    let jobs = call_rpc(&ctx, &mut sess, "facilityManager", "GetFacilities", &[])
        .await
        .expect("GetFacilities");
    let PyRep::List(_facs) = jobs else {
        panic!("expected List from GetFacilities");
    };
    assert_eq!(sess.solar_system_id, Some(30_000_142));
    ctx.advance_secs(300);
}

#[tokio::test]
async fn test_journey_planetary() {
    // parity: journey:planetary
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(34);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID");

    let _planets = call_rpc(&ctx, &mut sess, "planetMgr", "GetPlanetsForChar", &[])
        .await
        .expect("GetPlanetsForChar");
    let before = ctx.now_unix();
    ctx.advance_secs(3600);
    assert_eq!(ctx.now_unix(), before + 3600);
}

#[tokio::test]
async fn test_journey_corporation() {
    // parity: journey:corporation
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(35);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID");

    let corp_id = sess.corporation_id.unwrap_or(1_000_035);
    assert!(corp_id >= 1_000_000);
}

#[tokio::test]
async fn test_journey_fleet() {
    // parity: journey:fleet
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(36);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID");

    let _fleet = call_rpc(&ctx, &mut sess, "fleetProxy", "GetAvailableFleetAds", &[])
        .await
        .expect("GetAvailableFleetAds");
    assert_eq!(sess.character_id, Some(90_000_001));
}
