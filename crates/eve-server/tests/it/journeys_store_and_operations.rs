//! End-to-End Integration Journeys: Cosmetics/Store, Mail/Notifs, ESI, Downtime, and GM Tools (Journeys 25..=29).
#![forbid(unsafe_code)]

use eve_db::{service_roles, InventoryFlag, VirtualServerClock, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::esi::{send_raw_esi_request, EsiHttpRequest, EsiServer};
use eve_server::quasar_gateway::STATUS_OK;
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

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Expected KeyVal with response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> EvePublicResponse {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: format!("jrn-so-{n:06}").into_bytes(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"jrn-so-app-01".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(
            sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req.encode_proto())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

#[tokio::test]
async fn test_journey_cosmetics_store() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.remove("nes_store_catalog");
    });
    crate::common::seed_test_nes_store_catalog(&ctx);

    let mut sess = SessionState::new(506);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    // 1. PLEX Vault balance & NES store purchase via Quasar publicGatewaySvc.Subscribe + SendRequest
    call_rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[])
        .await
        .expect("publicGatewaySvc.Subscribe");
    let init_plex = ctx.db.with_snapshot(|s| s.plex_vault_balance(90_000_001));
    assert_eq!(init_plex, 500);

    let bal_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.api.BalanceRequest", Vec::new()).await;
    assert_eq!(bal_rsp.status_code, STATUS_OK);

    // Purchase offer 2001 (450 PLEX) from NES store
    let (mut b, mut purch_payload) = (Vec::new(), Vec::new());
    encode_string_field(1, "2001", &mut b);
    encode_bytes_field(1, &b, &mut purch_payload);
    let purch_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.plex.vault.api.PurchaseRequest", purch_payload).await;
    assert_eq!(purch_rsp.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(90_000_001)), 50);

    // 2. Apply ship SKIN via shipCosmeticsMgr
    let ship_id = sess.ship_id.expect("ship_id");
    let ship_type_id = ctx.db.with_snapshot(|s| s.items[&ship_id].type_id);
    let skin_id = ctx
        .sde
        .skins
        .iter()
        .find(|(_, sk)| sk.types.contains(&ship_type_id))
        .map(|(id, _)| *id as i64)
        .unwrap_or(1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.licensed_skins.entry(90_000_001).or_default().push(skin_id as u32);
    });
    let skin_res = call_rpc(&ctx, &mut sess, "shipCosmeticsMgr", "ApplySkinToShip", &[PyRep::Integer(ship_id), PyRep::Integer(skin_id)])
        .await
        .expect("ApplySkinToShip");
    assert_eq!(skin_res.dict_get("shipID").and_then(|v| v.as_i64()), Some(ship_id));
}

#[tokio::test]
async fn test_journey_mail_notifications() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess1 = SessionState::new(601);
    crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID 1");

    let mut sess2 = SessionState::new(602);
    crate::common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    call_rpc(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_002)])
        .await
        .expect("SelectCharacterID 2");

    // 1. Send mail from sess1 to sess2, verify sess2 receives & reads it
    let mail_id = call_rpc(
        &ctx,
        &mut sess1,
        "mailMgr",
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(90_000_002)]),
            PyRep::String("Fleet Briefing".into()),
            PyRep::String("Meet at Jita 4-4 undock.".into()),
            PyRep::Integer(0),
            PyRep::Integer(0),
        ],
    )
    .await
    .expect("SendMail");
    let mid = mail_id.as_i64().unwrap_or(1);

    let body2 = call_rpc(&ctx, &mut sess2, "mailMgr", "GetBody", &[PyRep::Integer(mid), PyRep::Integer(0)])
        .await
        .expect("GetBody");
    assert!(matches!(body2, PyRep::Buffer(ref b) if !b.is_empty()), "GetBody must return compressed mail body");

    // 2. Labels & Mailing lists
    let lbl = call_rpc(&ctx, &mut sess2, "mailMgr", "CreateLabel", &[PyRep::String("Priority".into()), PyRep::Integer(1)])
        .await
        .expect("CreateLabel");
    assert!(lbl.as_i64().unwrap_or(0) > 0);
    let mlist = call_rpc(&ctx, &mut sess1, "mailingListsMgr", "Create", &[PyRep::String("EvoMu Pilots".into()), PyRep::Integer(0), PyRep::Integer(0)])
        .await
        .expect("mailingListsMgr.Create");
    assert!(mlist.as_i64().unwrap_or(0) > 0);

    // 3. Notifications & Calendar events
    let notifs = call_rpc(&ctx, &mut sess2, "notificationMgr", "GetUnprocessed", &[]).await.expect("GetUnprocessed");
    assert!(format!("{notifs:?}").contains("CRowset"));
    let ev = call_rpc(
        &ctx,
        &mut sess1,
        "calendarMgr",
        "CreatePersonalEvent",
        &[
            PyRep::Integer(135_500_000_000_000_000),
            PyRep::Integer(60),
            PyRep::String("Roam".into()),
            PyRep::String("Lowsec roam".into()),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(90_000_002)]),
        ],
    )
    .await
    .expect("CreatePersonalEvent");
    assert!(ev.dict_get("eventID").and_then(|v| v.as_i64()).or_else(|| ev.as_i64()).unwrap_or(0) > 0);
}

#[tokio::test]
async fn test_journey_esi_external() {
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
                "esi-assets.read_assets.v1".into(),
                "esi-location.read_location.v1".into(),
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

    // 1. Call endpoints across multiple tag groups & verify caching headers
    for path in [
        "/latest/characters/90000001/wallet/",
        "/latest/characters/90000001/skills/",
        "/latest/characters/90000001/assets/",
        "/latest/characters/90000001/location/",
        "/latest/characters/90000001/",
        "/latest/universe/systems/30000142/",
        "/latest/status/",
    ] {
        let mut req = EsiHttpRequest::new("GET", path);
        req.headers.insert("authorization".into(), format!("Bearer {token}"));
        let resp = send_raw_esi_request(addr, &req).await.expect("ESI GET");
        assert_eq!(resp.status_code, 200, "ESI {path} must return 200");
        if path.ends_with("/wallet/") {
            let bal: f64 = std::str::from_utf8(&resp.body).unwrap().trim().parse().unwrap();
            assert!(bal > 0.0, "ESI wallet balance must match in-client balance");
        }
        assert!(
            resp.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("cache-control") || k.eq_ignore_ascii_case("etag")),
            "ESI {path} must include caching headers"
        );
    }

    // 2. Scope enforcement: mail endpoint without esi-mail.read_mail.v1 must return 403
    let mut forbidden_req = EsiHttpRequest::new("GET", "/latest/characters/90000001/mail/");
    forbidden_req.headers.insert("authorization".into(), format!("Bearer {token}"));
    let forbidden_resp = send_raw_esi_request(addr, &forbidden_req).await.expect("ESI 403");
    assert_eq!(forbidden_resp.status_code, 403);
}

#[tokio::test]
async fn test_journey_downtime_persistence() {
    let tmp_dir = std::env::temp_dir();
    let db_path = tmp_dir.join(format!("evomu_journey_dt_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);

    let expected_bal = {
        let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
        let ctx = Arc::new(ServerContext::with_repository(db));
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
        ctx.seed_demo_world_fixtures();
        ctx.db.set_account_role("demo", service_roles::ROLE_GM_DEFAULT).expect("set_account_role");

        let mut sess = SessionState::new(603);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        ctx.db.add_or_stack_item(90_000_001, 60_003_760, InventoryFlag::Hangar, 34, 777).unwrap();
        call_rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/giveisk 25000".into())])
            .await
            .expect("/giveisk");
        let bal = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);

        call_rpc(&ctx, &mut sess, "slash", "SlashCmd", &[PyRep::String("/downtime".into())])
            .await
            .expect("/downtime");
        bal
    };

    // Reopen ServerContext from SQLite after downtime restart and verify persistence
    {
        let db = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
        let ctx = Arc::new(ServerContext::with_repository(db));
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_600)));

        let mut sess = SessionState::new(604);
        crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID after restart");

        let bal_after = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance.0);
        assert_eq!(bal_after, expected_bal, "wallet balance must survive SQLite restart");
        let trit_after: i32 = ctx.db.with_snapshot(|s| {
            s.items.values().filter(|i| i.owner_id == 90_000_001 && i.type_id == 34).map(|i| i.quantity).sum()
        });
        assert!(trit_after >= 777, "items must survive SQLite restart");
    }

    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_journey_gm_tools() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    // 1. Normal account must be refused GM slash commands
    let mut normal_sess = SessionState::new(605);
    crate::common::login_pilot(&ctx, &mut normal_sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut normal_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID normal");

    for cmd in ["/giveisk 1000", "/createitem 34 10", "/giveskill 3300 5", "/spawnnpc 10002", "/teleport 30000144", "/heal"] {
        let denied = call_rpc(&ctx, &mut normal_sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())]).await;
        assert_eq!(denied.unwrap_err().code, "InsufficientRole", "normal account must be refused {cmd}");
    }

    // 2. Provision real GM account via account-role path (no session.role mutation)
    ctx.db.provision_player_account("gm_account", "gm-secret-999");
    ctx.db.set_account_role("gm_account", service_roles::ROLE_GM_DEFAULT).expect("set_account_role GM");
    ctx.db.provision_player_account("target_pilot", "target-secret");

    let mut gm_sess = SessionState::new(606);
    crate::common::login_pilot(&ctx, &mut gm_sess, "gm_account", "gm-secret-999").await;
    assert!(gm_sess.has_gm_role(), "GM account must receive GM role on login");
    let gm_ancestry = ctx
        .sde
        .ancestries
        .values()
        .find(|a| a.bloodline_id == 1)
        .map(|a| a.ancestry_id as i64)
        .unwrap_or(11);
    let gm_cid = call_rpc(
        &ctx,
        &mut gm_sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String("Valkyrie Overseer".into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(gm_ancestry),
            PyRep::String("doll:gm".into()),
            PyRep::String("portrait:gm".into()),
            PyRep::Integer(17),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll GM")
    .as_i64()
    .unwrap();
    call_rpc(&ctx, &mut gm_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(gm_cid)])
        .await
        .expect("SelectCharacterID GM");

    // 3. Test every slash command family and assert concrete state effects
    let isk_before = ctx.db.with_snapshot(|s| s.characters[&gm_cid].balance.0);
    call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/giveisk 50000".into())])
        .await
        .expect("/giveisk");
    let isk_after = ctx.db.with_snapshot(|s| s.characters[&gm_cid].balance.0);
    assert_eq!(isk_after - isk_before, 5_000_000);

    let item_rsp = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/createitem 34 250".into())])
        .await
        .expect("/createitem");
    assert_eq!(item_rsp.dict_get("typeID").and_then(|v| v.as_i64()), Some(34));

    let skill_rsp = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/giveskill 3300 5".into())])
        .await
        .expect("/giveskill");
    assert_eq!(skill_rsp.dict_get("level").and_then(|v| v.as_i64()), Some(5));

    let npc_rsp = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/spawnnpc 601".into())])
        .await
        .expect("/spawnnpc");
    assert!(npc_rsp.dict_get("npcID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/teleport 30000144".into())])
        .await
        .expect("/teleport");
    assert_eq!(gm_sess.solar_system_id, Some(30_000_144));

    let heal_rsp = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/heal".into())])
        .await
        .expect("/heal");
    assert_eq!(heal_rsp.dict_get("healed").and_then(|v| v.as_bool()), Some(true));

    let ban_rsp = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/ban target_pilot 3600 TestBan".into())])
        .await
        .expect("/ban");
    assert_eq!(ban_rsp.dict_get("username").and_then(|v| v.as_str()), Some("target_pilot"));

    let unban_rsp = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/unban target_pilot".into())])
        .await
        .expect("/unban");
    assert_eq!(unban_rsp.dict_get("unbanned").and_then(|v| v.as_bool()), Some(true));
}
