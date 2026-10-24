//! Authentic Live EVE Client Journey Test Drivers (`journey:*` rows)
//!
//! Integration tests for key real-client journeys:
//! - `journey:launch_login`: launcher login, SSO token exchange, gateway handshake, character select.
//! - `journey:station_ui`: station hangar view, fitting window, item drag-drop, repair shop.
//! - `journey:undock_navigate`: station undock sequence, space ball creation, navigation alignment, subwarp propulsion, stop command.
//! - `journey:market_trade`: station market orders browse, buy order execution, ISK balance update, item delivered to station hangar.
//! - `journey:pve_combat`: target locking, turret activation, damage application, shield/armor reduction, wreck spawning.
//! - `journey:daily_goals_events`: AIR daily goals display, progress contribution update, claim reward.
//! - `journey:cosmetics_store`: SKINR studio design preview, pattern application, sequencing initiation.

#![forbid(unsafe_code)]

use eve_db::{
    base64url_encode_no_pad, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_dogma::attr_ids;
use eve_machonet::{
    CryptoChallengePacket, CryptoHandshakeResult, CryptoRequestPacket, CryptoServerHandshake,
    HandshakeCommand, HandshakeIdentity, HandshakeStepOutput, MachoHandshakeStateMachine,
    SessionState, VersionExchange,
};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, sha256_bytes, EvePublicRequest, EvePublicResponse,
    GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::dispatch_combat_rpc;
use eve_server::services::quasar_daily_goals::default_daily_goal_templates;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(m) => m.get(key),
        PyRep::Dict(entries) => entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then_some(v)),
        PyRep::DBRow { columns, .. } => columns
            .iter()
            .find_map(|(k, v)| (k == key).then_some(v)),
        _ => None,
    }
}

fn bound_id_of(rep: &PyRep) -> u64 {
    rep.as_i64().expect("bound ID as integer") as u64
}

fn listed_item_ids(rep: &PyRep) -> Vec<i64> {
    if let PyRep::ObjectEx { list, .. } = rep {
        let mut ids: Vec<i64> = list
            .iter()
            .filter_map(|row| match row {
                PyRep::DBRow { columns, .. } => columns
                    .iter()
                    .find(|(n, _)| n == "itemID")
                    .and_then(|(_, v)| v.as_i64()),
                _ => None,
            })
            .collect();
        ids.sort_unstable();
        ids
    } else {
        Vec::new()
    }
}

async fn call_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
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
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_url: &str,
    payload_value: Vec<u8>,
) -> EvePublicResponse {
    let empty_kw = HashMap::new();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"jrn-test-corr-01".to_vec(),
        payload: Some(GoogleAny {
            type_url: if type_url.starts_with(TYPE_GOOGLEAPIS_PREFIX) {
                type_url.to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{type_url}")
            },
            value: payload_value,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-jrn-test-001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &empty_kw, None)
        .await
        .expect("SendRequest");
    extract_response_proto(&rep)
}

fn make_uuid_request(uuid: [u8; 16]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn make_admin_progress(uuid: [u8; 16], delta: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    encode_varint_field(3, delta, &mut out);
    out
}

// ============================================================================
// 1. journey:launch_login
// ============================================================================

#[tokio::test]
async fn test_journey_launch_login() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    // 1. Launcher Login & SSO PKCE token exchange
    let sso_user = "demo";
    let sso_pass = "demo-secret";
    let code_verifier = "0123456789abcdef0123456789abcdef0123456789abcdef";
    let verifier_digest = sha256_bytes(code_verifier.as_bytes());
    let code_challenge = base64url_encode_no_pad(&verifier_digest);
    let auth_code = ctx
        .db
        .authorize_pkce(
            sso_user, sso_pass, 90_000_001, "evomu-launcher",
            "http://localhost:8080/callback", vec!["esi-skills.read_skills.v1".into()],
            &code_challenge, "S256", ctx.now_unix(),
        )
        .expect("authorize_pkce must succeed");

    let bundle = ctx
        .db
        .exchange_pkce_code(&auth_code, "evomu-launcher", code_verifier, ctx.now_unix())
        .expect("exchange_pkce_code must succeed");
    assert!(!bundle.access_token.is_empty(), "SSO access token must be non-empty");
    assert!(bundle.expires_in > 0, "SSO token must have positive expiry");

    // SSO token exchange onto macho session
    let mut sess = SessionState::new(31_001);
    let sso_login = call_rpc(
        &ctx, &mut sess, "authentication", "Login",
        &[PyRep::String("__sso__".into()), PyRep::String(bundle.access_token.clone())],
    ).await.expect("SSO Login must succeed");
    assert!(matches!(sso_login, PyRep::KeyVal(_)), "Login must return KeyVal context");
    assert!(sess.is_authenticated(), "Session must be authenticated after SSO Login");
    assert_eq!(sess.user_id, Some(1), "Demo user ID 1 must be set in session");

    // Verify user token retrieval
    let user_token = call_rpc(&ctx, &mut sess, "userSvc", "GetUserToken", &[]).await.expect("GetUserToken");
    assert_eq!(user_token, PyRep::String("sso-tok-1".into()), "GetUserToken must return deterministic sso-tok-1");

    // 2. Gateway Handshake
    let mut hm = MachoHandshakeStateMachine::new(VersionExchange::default(), 1, 5001, 7001);
    assert!(!hm.is_authenticated(), "Handshake starts unauthenticated");
    let step1 = hm.step(&VersionExchange::default().to_pyrep()).expect("step version exchange");
    assert!(matches!(step1, HandshakeStepOutput::SendFramesAndContinue(_)));

    let crypto_req = HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo());
    let step2 = hm.step(&crypto_req.to_pyrep()).expect("step crypto request");
    assert!(matches!(step2, HandshakeStepOutput::SendFramesAndContinue(_)));

    let chal = CryptoChallengePacket::new_password_login("demo", "demo-secret");
    let step3 = hm.step(&chal.to_pyrep()).expect("step challenge packet");
    assert!(matches!(step3, HandshakeStepOutput::NeedsAuthentication(_)));

    let identity = HandshakeIdentity {
        user_id: 1, role: 1, language_id: "EN".into(), user_type: 30,
        address: "127.0.0.1".into(), access_token: Some(bundle.access_token), computer_hash: None,
    };
    let frames = hm.complete_challenge(&chal, Ok(identity), 1).expect("complete challenge");
    let srv_hs = CryptoServerHandshake::from_pyrep(&frames[1]).expect("parse server handshake");
    let res_pkt = CryptoHandshakeResult::for_server_challenge(&srv_hs.server_challenge);
    let final_step = hm.step(&res_pkt.to_pyrep()).expect("step final result");
    assert!(matches!(final_step, HandshakeStepOutput::Authenticated { .. }));
    assert!(hm.is_authenticated(), "Macho handshake machine is complete");

    // Quasar Gateway Handshake / Subscription
    let sub_rep = call_rpc(&ctx, &mut sess, "publicGatewaySvc", "Subscribe", &[]).await.expect("publicGatewaySvc.Subscribe");
    assert_eq!(dict_get(&sub_rep, "subscribed"), Some(&PyRep::Bool(true)), "Quasar subscription must be active");
    let app_uuid = dict_get(&sub_rep, "application_instance_uuid");
    assert!(matches!(app_uuid, Some(PyRep::Bytes(b)) if b.len() == 16), "Must provide 16-byte application uuid");

    // 3. Character Select
    let chars_rep = call_rpc(&ctx, &mut sess, "charUnboundMgr", "GetCharactersToSelect", &[]).await.expect("GetCharactersToSelect");
    assert!(chars_rep.is_crowset(), "GetCharactersToSelect must return CRowset");
    let char_rows = chars_rep.crowset_rows().expect("GetCharactersToSelect CRowset rows");
    assert!(!char_rows.is_empty(), "Demo account must have at least 1 character");
    let char_id = 90_000_001i64;
    let found = char_rows.iter().any(|r| r.dbrow_get("characterID") == Some(&PyRep::Integer(char_id)));
    assert!(found, "Character 90000001 must be present in GetCharactersToSelect");
    let sel_rep = call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    assert!(matches!(sel_rep, PyRep::KeyVal(_) | PyRep::Dict(_)), "SelectCharacterID must return character context");
    assert_eq!(sess.character_id, Some(char_id), "Character ID must be bound to session");
    assert_eq!(sess.station_id, Some(60_003_760), "Character must be located docked in Jita IV-4");
    assert_eq!(sess.solar_system_id, Some(30_000_142), "Session must be bound to Jita solar system");
    assert_eq!(sess.ship_id, Some(100_000_001), "Active ship must be 100000001 (Rifter)");
}

// ============================================================================
// 2. journey:station_ui
// ============================================================================

#[tokio::test]
async fn test_journey_station_ui() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(31_002);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID");

    let station_id = 60_003_760i64;
    let ship_id = 100_000_001i64;
    let kw = HashMap::new();

    // 1. Station hangar view: list station items via invbroker MachoBindObject
    let hangar_bind = ctx.dispatch_rpc(
        &mut sess, "invbroker", None, "MachoBindObject",
        &[PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::Integer(15)]), PyRep::None],
        &kw, None,
    ).await.expect("invbroker MachoBindObject station");
    let PyRep::Tuple(hangar_tuple) = hangar_bind else { panic!("MachoBindObject must return 2-tuple"); };
    let mgr_bid = bound_id_of(&hangar_tuple[0]);
    let hangar = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(mgr_bid), "GetInventory", &[PyRep::Integer(10_004), PyRep::None], &kw, None)
        .await
        .expect("GetInventory(containerHangar)");
    let hangar_bid = bound_id_of(&hangar);
    let hangar_list = ctx
        .dispatch_rpc(&mut sess, "invbroker", Some(hangar_bid), "List", &[], &kw, None)
        .await
        .expect("hangar List");
    let hangar_ids = listed_item_ids(&hangar_list);
    assert!(!hangar_ids.is_empty(), "Station hangar must contain seeded items");
    assert!(hangar_ids.contains(&ship_id), "Station hangar must contain active ship");
    assert!(hangar_ids.contains(&100_000_008), "Station hangar must contain Tritanium");

    // 2. Fitting window: dogma ship attributes & ship inventory list
    let ship_dogma_bind = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "MachoBindObject",
        &[PyRep::Integer(ship_id), PyRep::Tuple(vec![PyRep::String("GetShipAttributes".into()), PyRep::Tuple(vec![]), PyRep::Dict(vec![])])],
        &kw, None,
    ).await.expect("dogmaIM MachoBindObject ship");
    let PyRep::Tuple(dogma_tuple) = ship_dogma_bind else { panic!("dogmaIM MachoBindObject must return 2-tuple"); };
    let PyRep::Dict(attrs) = &dogma_tuple[1] else { panic!("GetShipAttributes must return Dict"); };
    assert!(!attrs.is_empty(), "Ship attributes dictionary must not be empty");
    let has_cpu = attrs.iter().any(|(k, _)| k.as_str() == Some("cpuOutput") || k.as_i64() == Some(attr_ids::CPU_OUTPUT as i64));
    let has_pg = attrs.iter().any(|(k, _)| k.as_str() == Some("powerOutput") || k.as_i64() == Some(attr_ids::POWER_OUTPUT as i64));
    assert!(has_cpu && has_pg, "Ship attributes must provide CPU and Powergrid outputs");

    // 3. Item drag-drop: fit 1MN Afterburner (100000003) into MedSlot0 (flag 19)
    let module_id = 100_000_003i64;
    call_rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(module_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::MedSlot0 as i64)])
        .await.expect("Fit 1MN Afterburner into MedSlot0");
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&module_id).expect("item 100000003 in DB");
        assert_eq!(it.location_id, ship_id, "Fitted module location must be active ship");
        assert_eq!(it.flag, InventoryFlag::MedSlot0, "Module flag must be MedSlot0");
    });

    // Drag module back out into station hangar
    call_rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(module_id), PyRep::Integer(station_id), PyRep::Integer(InventoryFlag::Hangar as i64)])
        .await.expect("Drag module back into station hangar");
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&module_id).expect("item 100000003 in DB");
        assert_eq!(it.location_id, station_id);
        assert_eq!(it.flag, InventoryFlag::Hangar);
    });

    // 4. Repair shop: damage ship hull/armor, quote cost, and execute RepairAll
    ctx.db.with_mut_snapshot(|snap| {
        let ship_rec = snap.items.get_mut(&ship_id).expect("ship item in DB");
        ship_rec.dynamic_attributes.insert(attr_ids::DAMAGE, 120.0);
        ship_rec.dynamic_attributes.insert(attr_ids::ARMOR_DAMAGE, 180.0);
    });

    let quote_rep = call_rpc(&ctx, &mut sess, "repairSvc", "GetDamagedModulesAndRepairCost", &[PyRep::Integer(ship_id)])
        .await.expect("GetDamagedModulesAndRepairCost");
    let total_dmg = dict_get(&quote_rep, "totalDamagedHP").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let cost_isk = dict_get(&quote_rep, "repairCostISK").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert_eq!(total_dmg, 300.0, "Total damaged HP must equal 120 + 180");
    assert_eq!(cost_isk, 150_000.0, "Repair cost must be 300 HP * 500 ISK/HP");

    let bal_before = ctx.db.get_character(90_000_001).unwrap().balance;
    let rep_res = call_rpc(&ctx, &mut sess, "repairSvc", "RepairAll", &[]).await.expect("RepairAll");
    assert_eq!(dict_get(&rep_res, "repaired"), Some(&PyRep::Bool(true)), "RepairAll must report repaired == true");

    ctx.db.with_snapshot(|snap| {
        let ship_rec = snap.items.get(&ship_id).expect("ship item in DB");
        let dmg = ship_rec.dynamic_attributes.get(&attr_ids::DAMAGE).copied().unwrap_or(0.0);
        let arm_dmg = ship_rec.dynamic_attributes.get(&attr_ids::ARMOR_DAMAGE).copied().unwrap_or(0.0);
        assert_eq!(dmg, 0.0, "Hull damage must be cleared to 0");
        assert_eq!(arm_dmg, 0.0, "Armor damage must be cleared to 0");
    });
    let bal_after = ctx.db.get_character(90_000_001).unwrap().balance;
    assert_eq!(bal_before.0 - bal_after.0, IskCents::from_isk(150_000).0, "Balance must be deducted by repair cost");
}

// ============================================================================
// 3. journey:undock_navigate
// ============================================================================

#[tokio::test]
async fn test_journey_undock_navigate() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(31_003);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID");

    let ship_id = 100_000_001i64;
    let sys_id = 30_000_142u32;
    let mut undock_kw = HashMap::new();
    undock_kw.insert("onlineModules".into(), PyRep::Bool(true));

    // 1. Station undock sequence
    let undock_res = ctx.dispatch_rpc(
        &mut sess, "ship", None, "Undock",
        &[PyRep::Integer(ship_id), PyRep::Bool(false)], &undock_kw, None,
    ).await.expect("ship.Undock");
    assert_eq!(dict_get(&undock_res, "shipID"), Some(&PyRep::Integer(ship_id)), "ship.Undock must return active shipID");
    assert_eq!(dict_get(&undock_res, "solarSystemID"), Some(&PyRep::Integer(sys_id as i64)), "ship.Undock must return destination solarSystemID");
    assert_eq!(sess.station_id, None, "Session station_id must be None in space");
    assert_eq!(sess.solar_system_id, Some(sys_id), "Session solar_system_id must be Jita");
    assert_eq!(sess.ship_id, Some(ship_id));

    // 2. Space ball creation in system ballpark
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ball_opt = sys_handle.get_ball_snapshot(ship_id).await.expect("get_ball_snapshot");
    assert!(ball_opt.is_some(), "Ship space ball must exist in system");
    let (_, ball) = ball_opt.unwrap();
    assert_eq!(ball.id, ship_id);
    assert_eq!(ball.type_id, 587, "Space ball type must match Rifter (587)");
    assert!(ball.radius > 0.0, "Ball radius must be positive");
    assert_eq!(ball.speed_fraction, 1.0);
    assert!(matches!(ball.mode, MovementMode::GotoDirection { .. }));

    let beyonce_id = dict_get(&undock_res, "beyonceBoundID")
        .and_then(|v| v.as_i64())
        .map(|v| v as u64)
        .unwrap_or_else(|| ctx.bind_object(&mut sess, "beyonce", sys_id as i64).unwrap());

    // 3. Navigation alignment: align to celestial planet Jita I (40009077)
    let celestial_id = 40_009_077i64;
    let align_res = ctx.dispatch_rpc(
        &mut sess, "beyonce", Some(beyonce_id), "CmdAlignTo",
        &[PyRep::Integer(celestial_id)], &undock_kw, None,
    ).await.expect("CmdAlignTo");
    assert!(matches!(align_res, PyRep::Integer(_) | PyRep::None | PyRep::Bool(_)), "CmdAlignTo must complete");
    let _ = sys_handle.tick().await;
    let (_, ball_align) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        matches!(ball_align.mode, MovementMode::GotoDirection { .. } | MovementMode::Follow { .. }),
        "Ship must be aligned towards target celestial vector"
    );

    // 4. Subwarp propulsion: accelerate to full speed fraction (1.0)
    let speed_res = ctx.dispatch_rpc(
        &mut sess, "beyonce", Some(beyonce_id), "CmdSetSpeedFraction",
        &[PyRep::Real(1.0)], &undock_kw, None,
    ).await.expect("CmdSetSpeedFraction");
    assert!(matches!(speed_res, PyRep::Integer(_) | PyRep::None | PyRep::Bool(_)));

    for _ in 0..5 {
        ctx.advance_secs(1);
        let _ = sys_handle.tick().await;
    }
    let (_, ball_moving) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(ball_moving.speed_fraction, 1.0);
    assert!(ball_moving.velocity.length() > 50.0, "Ship must have subwarp propulsion velocity");

    // 5. Stop command: CmdStop halts ship propulsion
    let stop_res = ctx.dispatch_rpc(
        &mut sess, "beyonce", Some(beyonce_id), "CmdStop",
        &[], &undock_kw, None,
    ).await.expect("CmdStop");
    assert!(matches!(stop_res, PyRep::Integer(_) | PyRep::None | PyRep::Bool(_)));

    for _ in 0..8 {
        ctx.advance_secs(1);
        let _ = sys_handle.tick().await;
    }
    let (_, ball_stopped) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(ball_stopped.mode, MovementMode::Stop, "Movement mode must transition to Stop");
    assert_eq!(ball_stopped.speed_fraction, 0.0, "Speed fraction must be 0.0 after CmdStop");
}

// ============================================================================
// 4. journey:market_trade
// ============================================================================

#[tokio::test]
async fn test_journey_market_trade() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(31_004);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)]).await.expect("SelectCharacterID");

    let station_id = 60_003_760i64;
    let sys_id = 30_000_142u32;
    let buyer_id = 90_000_001i64;
    let seller_id = 90_000_002i64;
    let item_type_id = 34u32; // Tritanium

    // Seed a resting sell order for Tritanium at 5.00 ISK owned by seller_id
    let order_id = 880_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            order_id,
            MarketOrderRecord {
                order_id, owner_id: seller_id, station_id, region_id: 10_000_002,
                type_id: item_type_id, is_bid: false, price: IskCents::from_isk(5),
                vol_entered: 1_000, vol_remaining: 1_000, min_volume: 1, is_npc: false, escrow: IskCents(0),
            },
        );
    });

    // 1. Station market orders browse: GetOrders and GetSystemAsks
    let orders_rep = call_rpc(&ctx, &mut sess, "marketProxy", "GetOrders", &[PyRep::Integer(item_type_id as i64)]).await.expect("GetOrders");
    let PyRep::Tuple(ref books) = orders_rep else { panic!("GetOrders must return (sells, buys) Tuple"); };
    assert_eq!(books.len(), 2);
    let sell_rows = books[0].crowset_rows().expect("GetOrders sells CRowset");
    let found_order = sell_rows.iter().find(|o| o.dbrow_get("orderID") == Some(&PyRep::Integer(order_id)));
    assert!(found_order.is_some(), "Resting sell order must be browsable on the market");

    let asks_rep = call_rpc(
        &ctx, &mut sess, "marketProxy", "GetSystemAsks",
        &[PyRep::Integer(sys_id as i64), PyRep::List(vec![PyRep::Integer(item_type_id as i64)])],
    ).await.expect("GetSystemAsks");
    assert!(asks_rep.is_cindexed_rowset(), "GetSystemAsks must return CIndexedRowset");

    // 2. Buy order execution: BuyMultipleItems for 600 units at 5.00 ISK
    let buyer_bal_before = ctx.db.get_character(buyer_id).unwrap().balance;
    let seller_bal_before = ctx.db.get_character(seller_id).unwrap().balance;
    let buy_order_spec = PyRep::keyval([
        ("stationID", PyRep::Integer(station_id)),
        ("typeID", PyRep::Integer(item_type_id as i64)),
        ("quantity", PyRep::Integer(600)),
        ("price", PyRep::Real(5.0)),
        ("orderRange", PyRep::Integer(-1)),
        ("minVolume", PyRep::Integer(1)),
        ("duration", PyRep::Integer(0)),
    ]);
    let buy_rep = call_rpc(
        &ctx, &mut sess, "marketProxy", "BuyMultipleItems",
        &[PyRep::Integer(station_id), PyRep::List(vec![buy_order_spec])],
    ).await.expect("BuyMultipleItems");

    let purchased_cnt = dict_get(&buy_rep, "purchasedCount").and_then(|v| v.as_i64()).unwrap_or(0);
    let total_qty = dict_get(&buy_rep, "totalQuantity").and_then(|v| v.as_i64()).unwrap_or(0);
    let total_cost = dict_get(&buy_rep, "totalCostISK").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert_eq!(purchased_cnt, 1, "Must purchase exactly 1 order");
    assert_eq!(total_qty, 600, "Purchased quantity must equal 600");
    assert_eq!(total_cost, 3_000.0, "Total cost must equal 600 * 5.00 = 3,000 ISK");

    // 3. ISK balance update: buyer deducted, seller credited, wallet journal logged
    let buyer_bal_after = ctx.db.get_character(buyer_id).unwrap().balance;
    let seller_bal_after = ctx.db.get_character(seller_id).unwrap().balance;
    assert!(buyer_bal_after.0 < buyer_bal_before.0, "Buyer balance must decrease after purchase");
    assert!(seller_bal_after.0 > seller_bal_before.0, "Seller balance must increase after filled sell order");
    ctx.db.with_snapshot(|snap| {
        let buyer_entry = snap.wallet_journal.iter().find(|e| e.character_id == buyer_id && e.entry_type == "MarketTransaction");
        assert!(buyer_entry.is_some(), "Buyer wallet journal must contain MarketTransaction entry");
    });

    // 4. Item delivered to station hangar
    let has_purchased_item = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| {
            it.owner_id == buyer_id
                && it.location_id == station_id
                && it.flag == InventoryFlag::Hangar
                && it.type_id == item_type_id
        })
    });
    assert!(has_purchased_item, "Purchased item must be delivered to station hangar");
}

// ============================================================================
// 5. journey:pve_combat
// ============================================================================

#[tokio::test]
async fn test_journey_pve_combat() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let sys_id = 30_000_142u32;
    let turret_mod_id = 200_501i64;
    let ammo_item_id = 200_502i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            turret_mod_id,
            InventoryItemRecord {
                item_id: turret_mod_id, type_id: 486, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1,
                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            ammo_item_id,
            InventoryItemRecord {
                item_id: ammo_item_id, type_id: 185, owner_id: char_id, location_id: ship_id,
                flag: InventoryFlag::HiSlot0, quantity: 100, singleton: false, version: 1,
                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ship_ball = Ball {
        id: ship_id, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO,
        max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball_with_combat(ship_ball, ctx.build_type_combat_state(587)).await;

    // Spawn an authentic NPC pirate target ball at 1,500m
    let target_id = 500_050i64;
    let mut target_cs = ctx.build_type_combat_state(587);
    target_cs.shield_hp = 100.0;
    target_cs.max_shield_hp = 100.0;
    target_cs.armor_hp = 80.0;
    target_cs.max_armor_hp = 80.0;
    target_cs.structure_hp = 60.0;
    target_cs.max_structure_hp = 60.0;
    let target_ball = Ball {
        id: target_id, type_id: 587, position: DVec3::new(1_500.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 150.0, inertia: 1.0, mass: 1_000_000.0, radius: 40.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball_with_combat(target_ball, target_cs).await;

    let mut sess = SessionState::new(31_005);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_170);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    sess.station_id = None;

    // 1. Target locking: AddTarget & GetTargets
    let kw = HashMap::new();
    let lock_res = ctx.dispatch_rpc(
        &mut sess, "dogmaIM", None, "AddTarget",
        &[PyRep::Integer(target_id)], &kw, None,
    ).await.expect("AddTarget");
    let PyRep::KeyVal(ref lock_kv) = lock_res else { panic!("AddTarget must return KeyVal, got {lock_res:?}"); };
    assert_eq!(lock_kv.get("targetID"), Some(&PyRep::Integer(target_id)));

    // Step 4 seconds for lock timer to complete
    for _ in 0..4 {
        ctx.advance_secs(1);
        let _ = sys_handle.tick().await;
    }

    let targets_rep = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "GetTargets", &[], &kw, None).await.expect("GetTargets");
    let PyRep::List(targets_list) = targets_rep else { panic!("GetTargets must return List"); };
    assert!(targets_list.contains(&PyRep::Integer(target_id)), "Locked target must be present in GetTargets");

    // 2. Turret activation & damage application: fire weapon cycle
    ctx.set_invention_roll_override(Some(0.50));
    let fire_res = dispatch_combat_rpc(
        &ctx, &mut sess, "dogmaIM", "Activate",
        &[PyRep::Integer(turret_mod_id), PyRep::Integer(target_id)], &kw,
    ).await.expect("Turret Activate");

    let dmg = dict_get(&fire_res, "appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0);
    assert!(dmg > 0.0, "Turret activation must apply damage (> 0.0)");

    let rem_ammo = ctx.db.with_snapshot(|snap| snap.items[&ammo_item_id].quantity);
    assert_eq!(rem_ammo, 99, "Firing turret must consume 1 projectile ammo");

    // 3. Shield reduction check on target
    let target_cs_after = sys_handle.get_combat_state(target_id).await.unwrap().unwrap();
    assert!(target_cs_after.shield_hp < 100.0, "Target shield HP must be reduced by applied damage");

    // 4. Lethal damage execution & wreck spawning
    ctx.set_invention_roll_override(Some(0.001));
    let mut kill_res = None;
    for _ in 0..4 {
        let res = dispatch_combat_rpc(
            &ctx, &mut sess, "dogmaIM", "Activate",
            &[PyRep::Integer(turret_mod_id), PyRep::Integer(target_id)], &kw,
        ).await.expect("Subsequent Activate");
        if dict_get(&res, "destroyed") == Some(&PyRep::Bool(true)) {
            kill_res = Some(res);
            break;
        }
    }
    let kill_dict = kill_res.expect("Target must be destroyed on lethal damage");
    assert_eq!(dict_get(&kill_dict, "destroyed"), Some(&PyRep::Bool(true)), "Destroyed flag must be true");

    let wreck_id = dict_get(&kill_dict, "wreckItemID").and_then(|v| v.as_i64()).expect("wreckItemID must be returned");
    assert!(wreck_id > 0, "Wreck item ID must be positive");

    let wreck_ball_opt = sys_handle.get_ball_snapshot(wreck_id).await.expect("get_ball_snapshot wreck");
    assert!(wreck_ball_opt.is_some(), "Wreck space ball must exist in system ballpark");

    ctx.db.with_snapshot(|snap| {
        let wreck_item = snap.items.get(&wreck_id).expect("wreck item in DB");
        assert_eq!(wreck_item.location_id, sys_id as i64, "Wreck location must be solar system");
        assert_eq!(wreck_item.type_id, 26550, "Wreck item type must match Minmatar Frigate Wreck (26550)");
    });
}

// ============================================================================
// 6. journey:daily_goals_events
// ============================================================================

#[tokio::test]
async fn test_journey_daily_goals_events() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let mut sess = SessionState::new(31_006);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(1_000_170);
    sess.solar_system_id = Some(30_000_142);
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;

    // 1. AIR daily goals display: GetAllCurrentRequest
    let all_current_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new()).await;
    assert_eq!(all_current_rsp.status_code, STATUS_OK, "GetAllCurrentRequest must return 200 OK");
    assert_eq!(
        all_current_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve_public.dailygoal.api.GetAllCurrentResponse"
    );

    let templates = default_daily_goal_templates();
    assert!(templates.len() >= 4, "Must have at least 4 active AIR daily goals");
    let goal_target = &templates[0];
    let goal_uuid = goal_target.uuid;

    // Query specific goal via GetRequest
    let req_uuid_payload = make_uuid_request(goal_uuid);
    let single_goal_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetRequest", req_uuid_payload.clone()).await;
    assert_eq!(single_goal_rsp.status_code, STATUS_OK, "GetRequest for goal UUID must return 200 OK");

    // 2. Progress contribution update: admin.ProgressRequest
    let prog_payload = make_admin_progress(goal_uuid, goal_target.target);

    let prog_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.admin.ProgressRequest", prog_payload).await;
    assert_eq!(prog_rsp.status_code, STATUS_OK, "ProgressRequest must return 200 OK");

    let progress_check_rsp = quasar_rpc(&ctx, &mut sess, "eve.dailygoal.api.GetProgressRequest", req_uuid_payload.clone()).await;
    assert_eq!(progress_check_rsp.status_code, STATUS_OK, "GetProgressRequest must return 200 OK");

    // 3. Claim reward: RedeemRequest
    let bal_before_redeem = ctx.db.get_character(90_000_001).unwrap().balance;
    let redeem_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.RedeemRequest", req_uuid_payload.clone()).await;
    assert_eq!(redeem_rsp.status_code, STATUS_OK, "RedeemRequest must return 200 OK");

    let bal_after_redeem = ctx.db.get_character(90_000_001).unwrap().balance;
    let expected_isk = goal_target.isk_reward;
    if expected_isk > 0 {
        assert_eq!(
            bal_after_redeem.0 - bal_before_redeem.0,
            IskCents::from_isk(expected_isk).0,
            "Claiming daily goal reward must credit reward ISK to character"
        );
    }

    let ent_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest", Vec::new()).await;
    assert_eq!(ent_rsp.status_code, STATUS_OK, "GetAllCompletedWithEntitlementsRequest must return 200 OK");
}

// ============================================================================
// 7. journey:cosmetics_store
// ============================================================================

#[tokio::test]
async fn test_journey_cosmetics_store() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let ship_type_id = 587u32; // Rifter
    let skin_lic_id = 34_748u32; // Permanent Rifter skin license from SDE

    let mut sess = SessionState::new(31_007);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_170);
    sess.solar_system_id = Some(30_000_142);
    sess.ship_id = Some(ship_id);
    sess.station_id = Some(60_003_760);

    // 1. SKINR Studio design preview: save draft, get draft, list saved drafts
    let mut draft_body = Vec::new();
    encode_bytes_field(1, b"skinr-custom-design-01", &mut draft_body);

    let save_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest", draft_body).await;
    assert_eq!(save_rsp.status_code, STATUS_OK, "SKINR SaveRequest must return 200 OK");

    let get_draft_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest", Vec::new()).await;
    assert_eq!(get_draft_rsp.status_code, STATUS_OK, "SKINR GetRequest must return 200 OK");

    let all_saved_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedRequest", Vec::new()).await;
    assert_eq!(all_saved_rsp.status_code, STATUS_OK, "SKINR GetAllSavedRequest must return 200 OK");

    // 2. Pattern application: grant skin license, activate, and apply to ship
    let lic_item_id = 450_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            lic_item_id,
            InventoryItemRecord {
                item_id: lic_item_id, type_id: skin_lic_id, owner_id: char_id, location_id: 60_003_760,
                flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1,
                dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
            },
        );
    });

    let act_rep = call_rpc(&ctx, &mut sess, "shipCosmeticsMgr", "ActivateSkinLicense", &[PyRep::Integer(skin_lic_id as i64)]).await.expect("ActivateSkinLicense");
    assert!(matches!(act_rep, PyRep::KeyVal(_) | PyRep::Dict(_)), "ActivateSkinLicense must return licence info");

    let apply_rep = call_rpc(
        &ctx, &mut sess, "shipCosmeticsMgr", "ApplySkinToShip",
        &[PyRep::Integer(ship_type_id as i64), PyRep::Integer(skin_lic_id as i64)],
    ).await.expect("ApplySkinToShip");
    assert!(matches!(apply_rep, PyRep::KeyVal(_) | PyRep::Dict(_)), "ApplySkinToShip must return application receipt");
    assert_eq!(dict_get(&apply_rep, "shipID"), Some(&PyRep::Integer(ship_id)));
    assert_eq!(dict_get(&apply_rep, "skinID"), Some(&PyRep::Integer(skin_lic_id as i64)));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.applied_ship_skins.get(&ship_id), Some(&skin_lic_id),
            "Applied ship skin in DB must match activated license"
        );
    });

    // 3. Sequencing initiation: query tier pricing, initiate sequencing job
    let pricing_rsp = quasar_rpc(&ctx, &mut sess, "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest", Vec::new()).await;
    assert_eq!(pricing_rsp.status_code, STATUS_OK, "GetTierPricingRequest must return 200 OK");
    assert_eq!(
        pricing_rsp.payload.as_ref().unwrap().normalized_type_name(),
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingResponse"
    );

    let init_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", Vec::new()).await;
    assert_eq!(init_rsp.status_code, STATUS_OK, "InitiateRequest must return 200 OK");

    let active_jobs_rsp = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest", Vec::new()).await;
    assert_eq!(active_jobs_rsp.status_code, STATUS_OK, "GetAllActiveRequest must return 200 OK");
}
