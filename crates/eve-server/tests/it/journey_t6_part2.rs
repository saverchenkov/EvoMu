//! In-process integration smoke tests for `plan:T-6` (part 2: journeys 14..=29).
//! Note: `journey:*` ledger rows remain open until real V24 client execution via `tools/client-harness/run_journeys`.
#![forbid(unsafe_code)]

use crate::common::{login_pilot, pin_clock, probe_scan, scan_row_field};
use crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite;
use eve_db::base64url_encode_no_pad;
use eve_machonet::SessionState;
use eve_proto::quasar::{sha256_bytes, EvePublicRequest, GoogleAny, GoogleTimestamp, QuasarMessage};
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use glam::DVec3;
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

fn bound_id(rep: &PyRep) -> u64 {
    match rep {
        PyRep::Tuple(t) => t[0].as_i64().unwrap_or(0) as u64,
        other => other.as_i64().unwrap_or(0) as u64,
    }
}

#[tokio::test]
async fn test_journey_14_exploration() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_017);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[PyRep::Integer(100_000_001)]).await.unwrap();
    let scan_bind = rpc(&ctx, &mut sess, "scanMgr", "MachoBindObject", &[PyRep::Integer(30_000_142), PyRep::None]).await.unwrap();
    let sid = bound_id(&scan_bind);
    let dscan = ctx
        .dispatch_rpc(&mut sess, "scanMgr", Some(sid), "DirectionalScan", &[PyRep::Real(14.3), PyRep::Real(360.0), PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)])], &HashMap::new(), None)
        .await
        .unwrap();
    assert!(!matches!(dscan, PyRep::None));
    let rows = probe_scan(&ctx, &mut sess, DVec3::ZERO, 4.0 * 149_597_870_700.0).await;
    assert_ne!(rows.len(), 0);
    let cert = scan_row_field(&rows, rows[0].dict_get("signatureID").unwrap().as_str().unwrap(), "certainty");
    assert!(!matches!(cert, PyRep::None), "certainty field present in scan row");
}

#[tokio::test]
async fn test_journey_15_wormholes_filaments() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_018);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
    rpc(&ctx, &mut sess, "stationSvc", "Undock", &[PyRep::Integer(100_000_001)]).await.unwrap();
    ctx.advance_secs(15);
    let wh = rpc(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(999_999)]).await;
    assert_eq!(wh.unwrap_err().code, "WormholeNotPresent");
}

#[tokio::test]
async fn test_journey_16_ship_loss() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_019);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let ins = rpc(&ctx, &mut sess, "insuranceSvc", "InsureShip", &[PyRep::Integer(100_000_001), PyRep::Real(100_000.0)])
        .await
        .expect("InsureShip");
    assert!(!matches!(ins, PyRep::None));
    let km = rpc(&ctx, &mut sess, "corpRegistry", "GetRecentKills", &[PyRep::Integer(90_000_001)]).await.unwrap();
    assert!(!matches!(km, PyRep::None));
}

#[tokio::test]
async fn test_journey_17_clones() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_020);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let state = rpc(&ctx, &mut sess, "jumpCloneSvc", "GetCloneState", &[]).await.unwrap();
    assert!(matches!(state, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::Object { .. }));
    rpc(&ctx, &mut sess, "charMgr", "SetHomeStation", &[PyRep::Integer(60_003_760)])
        .await
        .expect("SetHomeStation");
    assert_eq!(ctx.db.get_character(90_000_001).unwrap().home_station_id, 60_003_760);
}

#[tokio::test]
async fn test_journey_18_industry() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_021);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let jobs = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[]).await.unwrap();
    assert!(matches!(jobs, PyRep::List(_) | PyRep::Object { .. } | PyRep::Tuple(_)));
}

#[tokio::test]
async fn test_journey_19_planetary() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_022);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let planets = rpc(&ctx, &mut sess, "planetMgr", "GetPlanetsForChar", &[]).await.unwrap();
    assert!(!matches!(planets, PyRep::None));
}

#[tokio::test]
async fn test_journey_20_corporation() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_023);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();
    ctx.advance_secs(15);

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
    .await
    .expect("AddCorporation");
    let corp_id = corp
        .as_i64()
        .or_else(|| corp.dict_get("corporationID").and_then(|v| v.as_i64()))
        .unwrap_or(0);
    assert!(corp_id > 0, "AddCorporation must return positive corp_id, got {corp:?}");
    assert_eq!(ctx.db.get_character(90_000_001).unwrap().corporation_id, corp_id);
}

#[tokio::test]
async fn test_journey_21_fleet() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_024);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let fleet = rpc(&ctx, &mut sess, "fleetObjectHandler", "CreateFleet", &[]).await.unwrap();
    assert!(!matches!(fleet, PyRep::None));
    let fleet_id = ctx.db.get_character(90_000_001).unwrap().fleet_id.expect("character fleet_id set");
    assert!(fleet_id > 0);
}

#[tokio::test]
async fn test_journey_22_structures() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_025);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let structs = rpc(&ctx, &mut sess, "structureDirectory", "GetStructures", &[]).await.unwrap();
    assert!(!matches!(structs, PyRep::None));
    let dockable = rpc(&ctx, &mut sess, "structureDirectory", "GetMyDockableStructures", &[]).await.unwrap();
    assert!(!matches!(dockable, PyRep::None));
}

#[tokio::test]
async fn test_journey_23_sovereignty_fw() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut sess = SessionState::new(73_026);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let sov = rpc(&ctx, &mut sess, "sovMgr", "GetSovStructuresInfoForSolarSystem", &[PyRep::Integer(30_000_142)]).await.unwrap();
    assert!(!matches!(sov, PyRep::None));
    let fw = rpc(&ctx, &mut sess, "facWarMgr", "GetStats_Character", &[]).await.unwrap();
    assert!(!matches!(fw, PyRep::None));
}

#[tokio::test]
async fn test_journey_24_daily_goals_events() {
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
    let rep = rpc(&ctx, &mut sess, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(req.encode_proto())])
        .await
        .expect("SendRequest");
    assert!(!matches!(rep, PyRep::None));
}

#[tokio::test]
async fn test_journey_25_cosmetics_store() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    crate::common::seed_test_nes_store_catalog(&ctx);
    let mut sess = SessionState::new(73_028);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.unwrap();

    let sub = rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[]).await.unwrap();
    assert_eq!(sub.dict_get("subscribed"), Some(&PyRep::Bool(true)));
    let cats = rpc(&ctx, &mut sess, "storeManager", "get_categories", &[]).await.expect("get_categories");
    assert!(matches!(cats, PyRep::List(ref l) if !l.is_empty()));
    let prods = rpc(&ctx, &mut sess, "storeManager", "get_products", &[]).await.expect("get_products");
    assert!(matches!(prods, PyRep::List(ref l) if !l.is_empty()));
}

#[tokio::test]
async fn test_journey_26_mail_notifications() {
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
    assert!(mail.as_i64().unwrap_or(0) > 0);
    let headers = rpc(&ctx, &mut s2, "mailMgr", "GetMailHeaders", &[]).await.unwrap();
    assert!(!matches!(headers, PyRep::None));
    let notifs = rpc(&ctx, &mut s2, "notificationMgr", "GetAllNotifications", &[]).await.unwrap();
    assert!(!matches!(notifs, PyRep::None));
    let evs = rpc(&ctx, &mut s2, "calendarMgr", "GetEventList", &[]).await.unwrap();
    assert!(!matches!(evs, PyRep::None));
}

#[tokio::test]
async fn test_journey_27_esi_external() {
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
    let body_str = std::str::from_utf8(&resp.body).expect("utf8 wallet body");
    let wallet_val: f64 = body_str.trim().parse().expect("numeric wallet balance");
    assert!(wallet_val > 0.0);

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
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let ctx2 = persist_and_reopen_sqlite(&ctx, "t6_downtime");
    let ch = ctx2.db.get_character(90_000_001).expect("character survives restart");
    assert!(ch.balance.0 > 0);
}

#[tokio::test]
async fn test_journey_29_gm_tools() {
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
    ctx.db.with_snapshot(|snap| {
        let has_trit = snap.items.values().any(|i| i.owner_id == 90_000_001 && i.type_id == 34 && i.quantity >= 100);
        assert!(has_trit, "/createitem 34 100 must create Tritanium");
        let skill_lvl = snap.skills.get(&90_000_001).and_then(|s| s.get(&3402)).map(|s| s.level).unwrap_or(0);
        assert_eq!(skill_lvl, 5, "/giveskill 3402 5 must set skill level 5");
    });
}
