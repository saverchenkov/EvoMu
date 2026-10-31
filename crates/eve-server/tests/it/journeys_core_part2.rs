//! End-to-End Integration Journeys Part 2 (`journey:skills_training` .. `journey:exploration`).
#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord};
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

async fn call_bound_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bound_id: u64,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(
        sess,
        service,
        Some(bound_id),
        method,
        args,
        &HashMap::new(),
        None,
    )
    .await
}

fn bound_id_from_tuple(rep: &PyRep) -> u64 {
    match rep {
        PyRep::Tuple(t) if !t.is_empty() => t[0].as_i64().unwrap_or(1) as u64,
        other => other.as_i64().unwrap_or(1) as u64,
    }
}

#[tokio::test]
async fn test_journey_skills_training() {
    // parity: journey:skills_training
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_000_601,
            InventoryItemRecord {
                item_id: 100_000_601,
                type_id: 40520,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let mut sess = SessionState::new(20);
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

    let skill_bind = call_rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        "MachoBindObject",
        &[PyRep::Integer(90_000_001), PyRep::None],
    )
    .await
    .expect("skillMgr2 bind");
    let skill_bid = bound_id_from_tuple(&skill_bind);

    call_bound_rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        skill_bid,
        "SaveSkillQueue",
        &[PyRep::Integer(3327), PyRep::Integer(5)],
    )
    .await
    .expect("SaveSkillQueue");

    ctx.advance_secs(3600);

    let queue = call_bound_rpc(&ctx, &mut sess, "skillMgr2", skill_bid, "GetSkillQueue", &[])
        .await
        .expect("GetSkillQueue");
    assert!(!matches!(queue, PyRep::None));

    let injected = call_bound_rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        skill_bid,
        "InjectSkillpoints",
        &[PyRep::Integer(100_000_601), PyRep::Integer(1)],
    )
    .await
    .expect("InjectSkillpoints");
    assert!(injected.as_f64().unwrap_or(0.0) > 0.0);

    let sp = call_bound_rpc(
        &ctx,
        &mut sess,
        "skillMgr2",
        skill_bid,
        "ApplyFreeSkillPoints",
        &[],
    )
    .await
    .expect("ApplyFreeSkillPoints");
    assert!(sp.as_f64().unwrap_or(0.0) > 15_000_000.0);
}

#[tokio::test]
async fn test_journey_market_trade() {
    // parity: journey:market_trade
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(21);
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

    let orders = call_rpc(
        &ctx,
        &mut sess,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(34)],
    )
    .await
    .expect("GetOrders");
    let PyRep::Tuple(order_lists) = orders else {
        panic!("expected tuple of buy/sell orders");
    };
    assert_eq!(order_lists.len(), 2);

    let _tx = call_rpc(&ctx, &mut sess, "marketProxy", "CharGetTransactions", &[])
        .await
        .expect("CharGetTransactions");

    let _journal = call_rpc(&ctx, &mut sess, "account", "GetTransactions", &[])
        .await
        .expect("GetTransactions");
    assert_eq!(sess.character_id, Some(90_000_001));
}

#[tokio::test]
async fn test_journey_contracts() {
    // parity: journey:contracts
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(22);
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

    let my_contracts = call_rpc(&ctx, &mut sess, "contractProxy", "CollectMyPageInfo", &[])
        .await
        .expect("CollectMyPageInfo");
    let PyRep::KeyVal(kv) = my_contracts else {
        panic!("expected KeyVal from CollectMyPageInfo");
    };
    assert_eq!(kv.get("numOutstandingContracts").and_then(|v| v.as_i64()), Some(0));
}

#[tokio::test]
async fn test_journey_mining_loop() {
    // parity: journey:mining_loop
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(23);
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

    let _quotes = call_rpc(
        &ctx,
        &mut sess,
        "reprocessingSvc",
        "GetQuotes",
        &[PyRep::List(vec![]), PyRep::Integer(60_003_760)],
    )
    .await
    .expect("GetQuotes");
    assert_eq!(sess.station_id, Some(60_003_760));
}

#[tokio::test]
async fn test_journey_pve_combat() {
    // parity: journey:pve_combat
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(24);
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

    call_rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    ctx.advance_secs(5);
    assert_eq!(sess.station_id, None);
    assert_eq!(sess.solar_system_id, Some(30_000_142));
}

#[tokio::test]
async fn test_journey_missions() {
    // parity: journey:missions
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    crate::common::seed_test_lp_store_offers(&ctx);

    let mut sess = SessionState::new(25);
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

    let lp = call_rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(1_000_035)],
    )
    .await
    .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offers) = lp else {
        panic!("expected List of LP offers");
    };
    assert_eq!(offers.len(), 4);
}

#[tokio::test]
async fn test_journey_exploration() {
    // parity: journey:exploration
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(26);
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

    call_rpc(&ctx, &mut sess, "stationSvc", "Undock", &[])
        .await
        .expect("Undock");
    let _scan = call_rpc(&ctx, &mut sess, "scanMgr", "GetScanResults", &[])
        .await
        .expect("GetScanResults");
    assert_eq!(sess.solar_system_id, Some(30_000_142));
}
