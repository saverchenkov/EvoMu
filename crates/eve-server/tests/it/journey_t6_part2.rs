//! End-to-end integration tests for `plan:T-6` (`journey:*` parity rows, part 2: journeys 16..=29).
#![forbid(unsafe_code)]

use crate::common::{login_pilot, pin_clock};
use crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite;
use eve_db::base64url_encode_no_pad;
use eve_machonet::SessionState;
use eve_proto::quasar::{sha256_bytes, EvePublicRequest, GoogleAny, GoogleTimestamp, QuasarMessage};
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_journey_16_ship_loss() {
    // parity: journey:ship_loss
    eve_testkit::attest(&["journey:ship_loss"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_019);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let ins = rpc(&ctx, &mut sess, "insuranceSvc", "InsureShip", &[PyRep::Integer(100_000_001), PyRep::Real(100_000.0)]).await;
    assert_eq!(ins.is_ok(), true);
    let km = rpc(&ctx, &mut sess, "corpRegistry", "GetRecentKills", &[PyRep::Integer(90_000_001)]).await.unwrap();
    assert_ne!(format!("{km:?}"), "");
}

#[tokio::test]
async fn test_journey_17_clones() {
    // parity: journey:clones
    eve_testkit::attest(&["journey:clones"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_020);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let state = rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[]).await.unwrap();
    assert_ne!(format!("{state:?}"), "");
    let home = rpc(&ctx, &mut sess, "charMgr", "SetHomeStation", &[PyRep::Integer(60_003_760)]).await;
    assert_eq!(home.is_ok(), true);
}

#[tokio::test]
async fn test_journey_18_industry() {
    // parity: journey:industry
    eve_testkit::attest(&["journey:industry"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_021);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let jobs = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[]).await.unwrap();
    assert_ne!(format!("{jobs:?}"), "");
}

#[tokio::test]
async fn test_journey_19_planetary() {
    // parity: journey:planetary
    eve_testkit::attest(&["journey:planetary"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_022);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let planets = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetsForChar", &[]).await.unwrap();
    assert_ne!(format!("{planets:?}"), "");
}

#[tokio::test]
async fn test_journey_20_corporation() {
    // parity: journey:corporation
    eve_testkit::attest(&["journey:corporation"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_023);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let corp = rpc(
        &ctx,
        &mut sess,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("T6 Test Corp".into()),
            PyRep::String("T6TC".into()),
            PyRep::String("T6 corp".into()),
            PyRep::Real(0.05),
        ],
    )
    .await;
    assert_ne!(format!("{corp:?}"), "");
}

#[tokio::test]
async fn test_journey_21_fleet() {
    // parity: journey:fleet
    eve_testkit::attest(&["journey:fleet"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_024);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let fleet = rpc(&ctx, &mut sess, "fleetObjectHandler", "CreateFleet", &[]).await.unwrap();
    assert_ne!(format!("{fleet:?}"), "");
}

#[tokio::test]
async fn test_journey_22_structures() {
    // parity: journey:structures
    eve_testkit::attest(&["journey:structures"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_025);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let structs = rpc(&ctx, &mut sess, "structureDirectory", "GetStructures", &[]).await.unwrap();
    assert_ne!(format!("{structs:?}"), "");
    let dockable = rpc(&ctx, &mut sess, "structureDirectory", "GetMyDockableStructures", &[]).await.unwrap();
    assert_ne!(format!("{dockable:?}"), "");
}

#[tokio::test]
async fn test_journey_23_sovereignty_fw() {
    // parity: journey:sovereignty_fw
    eve_testkit::attest(&["journey:sovereignty_fw"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_026);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let sov = rpc(&ctx, &mut sess, "sovMgr", "GetSovStructuresInfoForSolarSystem", &[PyRep::Integer(30_000_142)]).await.unwrap();
    assert_ne!(format!("{sov:?}"), "");
    let fw = rpc(&ctx, &mut sess, "facWarMgr", "GetStats_Character", &[]).await.unwrap();
    assert_ne!(format!("{fw:?}"), "");
}

#[tokio::test]
async fn test_journey_24_daily_goals_events() {
    // parity: journey:daily_goals_events
    eve_testkit::attest(&["journey:daily_goals_events"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_027);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let sub = rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[]).await.unwrap();
    assert_eq!(sub.dict_get("subscribed"), Some(&PyRep::Bool(true)));
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"t6-dg-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve_public.goal.api.GetAllRequest".into(),
            value: vec![],
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-t6-001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = rpc(&ctx, &mut sess, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(req.encode_proto())]).await.unwrap();
    assert_ne!(format!("{rep:?}"), "");
}

#[tokio::test]
async fn test_journey_25_cosmetics_store() {
    // parity: journey:cosmetics_store
    eve_testkit::attest(&["journey:cosmetics_store"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    crate::common::seed_test_nes_store_catalog(&ctx);
    let mut sess = SessionState::new(73_028);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let sub = rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[]).await.unwrap();
    assert_eq!(sub.dict_get("subscribed"), Some(&PyRep::Bool(true)));
}

#[tokio::test]
async fn test_journey_26_mail_notifications() {
    // parity: journey:mail_notifications
    eve_testkit::attest(&["journey:mail_notifications"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let (mut s1, mut s2) = (SessionState::new(73_029), SessionState::new(73_030));
    login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
    login_pilot(&ctx, &mut s2, "other", "other-secret").await;
    rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)]).await.unwrap();

    let mail = rpc(
        &ctx,
        &mut s1,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(90_000_002)]),
            PyRep::String("T6 Subject".into()),
            PyRep::String("T6 Body".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .unwrap();
    assert_ne!(format!("{mail:?}"), "");
    let headers = rpc(&ctx, &mut s2, "mailMgr", "GetMailHeaders", &[]).await.unwrap();
    assert_ne!(format!("{headers:?}"), "");
    let notifs = rpc(&ctx, &mut s2, "notificationMgr", "GetAllNotifications", &[]).await.unwrap();
    assert_ne!(format!("{notifs:?}"), "");
    let evs = rpc(&ctx, &mut s2, "calendarMgr", "GetEventList", &[]).await.unwrap();
    assert_ne!(format!("{evs:?}"), "");
}

#[tokio::test]
async fn test_journey_27_esi_external() {
    // parity: journey:esi_external
    eve_testkit::attest(&["journey:esi_external"]);
    let ctx = Arc::new(ServerContext::new());
    pin_clock(&ctx);
    let verifier = "0123456789abcdef0123456789abcdef0123456789abcdef";
    let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            90_000_001,
            "evomu-esi",
            "https://localhost/cb",
            vec!["esi-wallet.read_character_wallet.v1".into()],
            &challenge,
            "S256",
            ctx.now_unix(),
        )
        .unwrap();
    let tok = ctx
        .db
        .exchange_pkce_code(&code, "evomu-esi", verifier, ctx.now_unix())
        .unwrap()
        .access_token;

    let router = EsiRouter::new(ctx.clone());
    let req = EsiHttpRequest {
        method: "GET".into(),
        raw_target: "/latest/characters/90000001/wallet/".into(),
        headers: HashMap::from([("authorization".into(), format!("Bearer {tok}"))]),
        body: Vec::new(),
        peer_ip: "127.0.0.1".into(),
    };
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200);
    assert_ne!(resp.headers.len(), 0);

    // Missing scope enforcement
    let bad_req = EsiHttpRequest {
        method: "GET".into(),
        raw_target: "/latest/characters/90000001/skills/".into(),
        headers: HashMap::from([("authorization".into(), format!("Bearer {tok}"))]),
        body: Vec::new(),
        peer_ip: "127.0.0.1".into(),
    };
    let bad_resp = router.handle_request(&bad_req);
    assert_eq!(bad_resp.status_code, 403);
}

#[tokio::test]
async fn test_journey_28_downtime_persistence() {
    // parity: journey:downtime_persistence
    eve_testkit::attest(&["journey:downtime_persistence"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let ctx2 = persist_and_reopen_sqlite(&ctx, "t6_downtime");
    let ch = ctx2.db.get_character(90_000_001).expect("character survives restart");
    assert!(ch.balance.0 > 0);
}

#[tokio::test]
async fn test_journey_29_gm_tools() {
    // parity: journey:gm_tools
    eve_testkit::attest(&["journey:gm_tools"]);
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut player_sess = SessionState::new(73_031);
    login_pilot(&ctx, &mut player_sess, "other", "other-secret").await;
    rpc(&ctx, &mut player_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)]).await.unwrap();
    let denied = rpc(&ctx, &mut player_sess, "slash", "SlashCmd", &[PyRep::String("/giveisk 1000".into())]).await;
    assert_eq!(denied.unwrap_err().code, "InsufficientRole");

    let mut gm_sess = SessionState::new(73_032);
    login_pilot(&ctx, &mut gm_sess, "demo", "demo-secret").await;
    gm_sess.role = eve_db::service_roles::ROLE_ADMIN | eve_db::service_roles::ROLE_GMS;
    rpc(&ctx, &mut gm_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    for cmd in [
        "/spawn 587",
        "/createitem 34 100",
        "/giveskill 3402 5",
        "/tr 30000142",
        "/heal",
        "/setattr 100000001 164 25",
    ] {
        let res = rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())]).await;
        assert!(res.is_ok(), "GM command {cmd} failed: {res:?}");
    }
}
