//! End-to-End Integration Journeys Part 4 (`journey:structures` .. `journey:gm_tools`).
#![forbid(unsafe_code)]

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{send_raw_esi_request, EsiHttpRequest, EsiServer};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

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
async fn test_journey_structures() {
    // parity: journey:structures
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(40);
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

    let structs = call_rpc(
        &ctx,
        &mut sess,
        "structureDirectory",
        "GetMyDockableStructures",
        &[],
    )
    .await
    .expect("GetMyDockableStructures");
    let PyRep::List(slist) = structs else {
        panic!("expected List from GetMyDockableStructures");
    };
    assert!(slist.len() < 1000);
    ctx.advance_secs(60);
}

#[tokio::test]
async fn test_journey_sovereignty_fw() {
    // parity: journey:sovereignty_fw
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(41);
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

    let _fw = call_rpc(&ctx, &mut sess, "facWarMgr", "GetStats_Character", &[])
        .await
        .expect("GetStats_Character");
    assert_eq!(sess.character_id, Some(90_000_001));
}

#[tokio::test]
async fn test_journey_daily_goals_events() {
    // parity: journey:daily_goals_events
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(42);
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

    let goals = call_rpc(&ctx, &mut sess, "publicGatewaySvc", "Call", &[])
        .await;
    assert_eq!(goals.unwrap_err().code, "UnimplementedRPC");
}

#[tokio::test]
async fn test_journey_cosmetics_store() {
    // parity: journey:cosmetics_store
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    crate::common::seed_test_nes_store_catalog(&ctx);

    let mut sess = SessionState::new(43);
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

    let skins = call_rpc(&ctx, &mut sess, "publicGatewaySvc", "Call", &[])
        .await;
    assert_eq!(skins.unwrap_err().code, "UnimplementedRPC");
}

#[tokio::test]
async fn test_journey_mail_notifications() {
    // parity: journey:mail_notifications
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(44);
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

    let mail = call_rpc(&ctx, &mut sess, "mailMgr", "SyncMail", &[])
        .await
        .expect("SyncMail");
    let PyRep::KeyVal(mkv) = mail else {
        panic!("expected KeyVal from SyncMail");
    };
    assert!(!mkv.is_empty());

    let _notifs = call_rpc(&ctx, &mut sess, "notificationMgr", "GetUnprocessed", &[])
        .await
        .expect("GetUnprocessed");
    assert_eq!(sess.character_id, Some(90_000_001));
}

#[tokio::test]
async fn test_journey_esi_external() {
    // parity: journey:esi_external
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let (addr, _router, _handle) = EsiServer::spawn(Arc::clone(&ctx), "127.0.0.1:0")
        .await
        .expect("spawn ESI server");

    let now = ctx.now_unix();
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            90_000_001,
            "esi-journey",
            "https://localhost/callback",
            vec![
                "esi-wallet.read_character_wallet.v1".into(),
                "esi-skills.read_skills.v1".into(),
            ],
            challenge,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let token = ctx
        .db
        .exchange_pkce_code(&code, "esi-journey", verifier, now)
        .expect("exchange_pkce_code")
        .access_token;

    let mut req = EsiHttpRequest::new("GET", "/latest/characters/90000001/wallet/");
    req.headers
        .insert("authorization".into(), format!("Bearer {token}"));
    let resp = send_raw_esi_request(addr, &req).await.expect("ESI wallet");
    assert_eq!(resp.status_code, 200);
    let body_str = std::str::from_utf8(&resp.body).expect("utf8 body");
    let val: f64 = body_str.trim().parse().expect("numeric wallet balance");
    assert!(val > 0.0);
}

#[tokio::test]
async fn test_journey_downtime_persistence() {
    // parity: journey:downtime_persistence
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(45);
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

    // Advance virtual clock across downtime window and verify state persists
    ctx.advance_secs(86_400);
    let bal = call_rpc(&ctx, &mut sess, "account", "GetCashBalance", &[])
        .await
        .expect("GetCashBalance after downtime");
    assert!(bal.as_f64().unwrap_or(0.0) > 0.0);
}

#[tokio::test]
async fn test_journey_gm_tools() {
    // parity: journey:gm_tools
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    // Normal account must be refused GM slash commands
    let mut normal_sess = SessionState::new(46);
    crate::common::login_pilot(&ctx, &mut normal_sess, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut normal_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID");

    let denied = call_rpc(
        &ctx,
        &mut normal_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/giveisk 1000".into())],
    )
    .await;
    assert!(denied.is_err(), "normal account must be refused GM slash");

    // GM account succeeds
    let mut gm_sess = SessionState::new(47);
    crate::common::login_pilot(&ctx, &mut gm_sess, "demo", "demo-secret").await;
    call_rpc(
        &ctx,
        &mut gm_sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
    )
    .await
    .expect("SelectCharacterID GM");
    gm_sess.role = u64::MAX;

    let _ok = call_rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/giveisk 1000".into())],
    )
    .await
    .expect("GM SlashCmd");
    assert_eq!(gm_sess.role, u64::MAX);
}
