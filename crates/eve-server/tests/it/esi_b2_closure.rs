//! Live HTTP + MachoNet RPC + SQLite persistence integration tests for 12 ESI routes.

#![forbid(unsafe_code)]

use crate::common;
use crate::esi_market_industry_pi::{issue_token_with_scopes, make_item, make_structure};
use eve_db::{
    corp_roles::{ROLE_DIRECTOR, ROLE_STATION_MANAGER, ROLE_TRADER},
    CorporationRecord, InventoryFlag, IskCents, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{send_raw_esi_request, EsiHttpRequest, EsiServer, OpenApiSpec};
use eve_server::router::ServerContext;
use glam::DVec3;
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_REGION_ID: u32 = 10_000_002;
const JITA_PLANET_ID: i64 = 40_009_082;
const TRITANIUM_TYPE_ID: u32 = 34;

async fn setup_pilot(
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
) -> PyRep {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
        .unwrap_or_else(|e| panic!("RPC {service}.{method} failed: {e:?}"))
}

async fn http_get_json(
    addr: SocketAddr,
    spec: &OpenApiSpec,
    path: &str,
    token: Option<&str>,
) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = send_raw_esi_request(addr, &req)
        .await
        .expect("send_raw_esi_request");
    assert_eq!(
        resp.status_code,
        200,
        "Expected 200 for {path}, got {} ({})",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    let raw_path = path.split('?').next().unwrap_or(path);
    spec.validate_response("GET", raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for {path}: {e}"));
    assert!(resp.header("ETag").is_some(), "200 response for {path} must include ETag");
    assert!(
        resp.header("Expires").is_some() || resp.header("Cache-Control").is_some(),
        "200 response for {path} must include cache headers"
    );
    resp.json().expect("JSON body")
}

async fn http_expect_status(
    addr: SocketAddr,
    path: &str,
    token: Option<&str>,
    expected: u16,
) {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = send_raw_esi_request(addr, &req)
        .await
        .expect("send_raw_esi_request");
    assert_eq!(
        resp.status_code,
        expected,
        "Expected {expected} for {path}, got {} ({})",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
}

#[tokio::test]
async fn test_esi_b2_corp_mining_and_customs_offices_live_http_and_sqlite() {
    // parity: esi:GET /corporation/{corporation_id}/mining/extractions, esi:GET /corporation/{corporation_id}/mining/observers, esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}, esi:GET /corporations/{corporation_id}/customs_offices
    eve_testkit::attest(&[
        "esi:GET /corporation/{corporation_id}/mining/extractions",
        "esi:GET /corporation/{corporation_id}/mining/observers",
        "esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}",
        "esi:GET /corporations/{corporation_id}/customs_offices",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let ceo_id = 90_000_001_i64;
    let miner_id = 90_000_002_i64;
    let corp_id = 98_000_811_i64;
    let athanor_id = 1_000_000_811_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "B2 Mining & Customs Corp".into(),
            ticker: "B2MC".into(),
            ceo_id,
            ..Default::default()
        };
        corp.member_roles.insert(ceo_id, ROLE_DIRECTOR);
        corp.member_roles.insert(miner_id, ROLE_STATION_MANAGER);
        snap.corporations.insert(corp_id, corp);
        for cid in [ceo_id, miner_id] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.corporation_id = corp_id;
            }
        }
        snap.structures.insert(
            athanor_id,
            make_structure(athanor_id, 35835, corp_id, JITA_SYSTEM_ID, "B2 Moon Athanor"),
        );
    });

    let mut ceo_sess = setup_pilot(&ctx, 11, "demo", "demo-secret", ceo_id).await;
    let mut miner_sess = setup_pilot(&ctx, 12, "other", "other-secret", miner_id).await;

    // 1. Start moon extraction via moonExtractions.StartNewExtraction
    let start_res = rpc(
        &ctx,
        &mut ceo_sess,
        "moonExtractions",
        "StartNewExtraction",
        &[PyRep::Integer(athanor_id), PyRep::Integer(864_000)],
    )
    .await;
    assert_eq!(
        start_res.dict_get("status").and_then(|v| v.as_str()),
        Some("Started")
    );

    // 2. Advance time, fracture chunk, and mine via real mining laser cycle
    ctx.advance_time_seconds(864_000);
    let frac_res = rpc(
        &ctx,
        &mut ceo_sess,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(athanor_id)],
    )
    .await;
    let PyRep::List(ast_ids) = frac_res.dict_get("asteroidIDs").expect("asteroidIDs") else {
        panic!("Expected asteroidIDs list");
    };
    let first_ast_id = ast_ids[0].as_i64().unwrap();

    let undock = rpc(&ctx, &mut miner_sess, "stationSvc", "Undock", &[]).await;
    let ship_id = undock.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
    let jita = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
    let miner_mod_id = 800_000_811_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship) = snap.items.get_mut(&ship_id) {
            ship.type_id = 32880;
        }
        snap.skills.entry(miner_id).or_default().insert(
            3386,
            TrainedSkillRecord { type_id: 3386, level: 3, skill_points: 50_000.0 },
        );
        snap.items.retain(|_, item| item.location_id != ship_id);
        snap.items.insert(
            miner_mod_id,
            make_item(miner_mod_id, 483, miner_id, ship_id, InventoryFlag::HiSlot0, 1, true),
        );
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    let ast_pos = jita.get_ball_snapshot(first_ast_id).await.unwrap().unwrap().1.position;
    if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await {
        sb.position = ast_pos + DVec3::new(500.0, 0.0, 0.0);
        let _ = jita.spawn_ball(sb).await;
    }
    let mine_res = crate::mining_cycle_engine::mine_once(&ctx, &mut miner_sess, miner_mod_id, first_ast_id)
        .await
        .expect("mine_once");
    assert!(mine_res.units_delivered > 0);

    // 3. Anchor, online, and configure corporation customs office via real client RPCs (no GM role)
    let lowsec_sys = 30_000_192_u32;
    let lowsec_planet_id = ctx.sde.solar_systems[&lowsec_sys].planet_ids[0];
    let (ceo_ship_id, gantry_id, poco_item_id) = (800_000_812_i64, 800_000_813_i64, 800_000_814_i64);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = None;
            ch.solar_system_id = lowsec_sys;
            ch.active_ship_id = ceo_ship_id;
            ch.standings.insert(500_026, 2.0);
        }
        snap.items.insert(gantry_id, make_item(gantry_id, 3962, corp_id, ceo_ship_id, InventoryFlag::Cargo, 1, false));
        snap.items.insert(poco_item_id, make_item(poco_item_id, 2233, corp_id, ceo_ship_id, InventoryFlag::Cargo, 1, false));
    });
    ceo_sess.station_id = None;
    ceo_sess.solar_system_id = Some(lowsec_sys);
    ceo_sess.ship_id = Some(ceo_ship_id);
    let _ = rpc(&ctx, &mut ceo_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(lowsec_planet_id)]).await;
    let _ = rpc(&ctx, &mut ceo_sess, "posMgr", "OnlineOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(poco_item_id)]).await;
    let tax_kv = PyRep::keyval([
        ("corporation", PyRep::Real(0.02)),
        ("alliance", PyRep::Real(0.05)),
        ("standingHigh", PyRep::Real(0.05)),
        ("standingGood", PyRep::Real(0.08)),
        ("standingNeutral", PyRep::Real(0.12)),
    ]);
    let _ = rpc(&ctx, &mut ceo_sess, "planetOrbitalRegistryBroker", "UpdateSettings", &[
        PyRep::Integer(lowsec_planet_id), PyRep::Integer(18), tax_kv, PyRep::Real(1.0), PyRep::Bool(true), PyRep::Bool(true),
    ]).await;

    let (addr, _router, handle) = EsiServer::spawn(Arc::clone(&ctx), "127.0.0.1:0").await.expect("spawn ESI");
    let spec = OpenApiSpec::pinned();
    let scopes = vec!["esi-industry.read_corporation_mining.v1", "esi-planets.read_customs_offices.v1"];
    let tok_station_mgr = issue_token_with_scopes(&ctx, miner_id, scopes.clone());
    let tok_director = issue_token_with_scopes(&ctx, ceo_id, scopes);

    let ext_path = format!("/v1/corporation/{corp_id}/mining/extractions/");
    let obs_path = format!("/v1/corporation/{corp_id}/mining/observers/");
    let poco_path = format!("/v1/corporations/{corp_id}/customs_offices/");

    // Auth / role / 404 checks
    http_expect_status(addr, &ext_path, None, 401).await;
    http_expect_status(addr, &obs_path, Some(&tok_station_mgr), 403).await;
    http_expect_status(addr, "/v1/corporation/999999999/mining/extractions/", Some(&tok_director), 403).await;

    // Station Manager succeeds on extractions; Director succeeds on customs_offices & observers
    let exts = http_get_json(addr, &spec, &ext_path, Some(&tok_station_mgr)).await;
    assert_eq!(exts.as_array().unwrap().len(), 1);
    assert_eq!(exts[0]["structure_id"], athanor_id);

    http_expect_status(addr, &poco_path, Some(&tok_station_mgr), 403).await;
    let pocos = http_get_json(addr, &spec, &poco_path, Some(&tok_director)).await;
    assert_eq!(pocos.as_array().unwrap().len(), 1);
    assert_eq!(pocos[0]["office_id"], lowsec_planet_id);
    assert_eq!(pocos[0]["system_id"], lowsec_sys);

    let obs = http_get_json(addr, &spec, &obs_path, Some(&tok_director)).await;
    let obs_id = obs[0]["observer_id"].as_i64().unwrap();
    let obs_detail_path = format!("/v1/corporation/{corp_id}/mining/observers/{obs_id}/");
    http_expect_status(addr, &obs_detail_path, Some(&tok_station_mgr), 403).await;
    http_expect_status(addr, &format!("/v1/corporation/{corp_id}/mining/observers/999999999/"), Some(&tok_director), 404).await;
    let entries = http_get_json(addr, &spec, &obs_detail_path, Some(&tok_director)).await;
    assert_eq!(entries[0]["character_id"], miner_id);
    assert_eq!(entries[0]["quantity"], mine_res.units_delivered);

    handle.abort();

    // Reload from SQLite and verify persistence over live HTTP
    let ctx2 = Arc::new(crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "esi_b2_corp_mining_poco"));
    let (addr2, _router2, handle2) = EsiServer::spawn(Arc::clone(&ctx2), "127.0.0.1:0").await.expect("spawn ESI 2");
    let tok_dir2 = issue_token_with_scopes(
        &ctx2,
        ceo_id,
        vec![
            "esi-industry.read_corporation_mining.v1",
            "esi-planets.read_customs_offices.v1",
        ],
    );
    let exts2 = http_get_json(addr2, &spec, &ext_path, Some(&tok_dir2)).await;
    assert_eq!(exts2.as_array().unwrap().len(), 1);
    let pocos2 = http_get_json(addr2, &spec, &poco_path, Some(&tok_dir2)).await;
    assert_eq!(pocos2.as_array().unwrap().len(), 1);
    let entries2 = http_get_json(addr2, &spec, &obs_detail_path, Some(&tok_dir2)).await;
    assert_eq!(entries2[0]["quantity"], mine_res.units_delivered);

    handle2.abort();
}

#[tokio::test]
async fn test_esi_b2_character_planets_and_orders_live_http_and_sqlite() {
    // parity: esi:GET /characters/{character_id}/planets, esi:GET /characters/{character_id}/planets/{planet_id}, esi:GET /characters/{character_id}/orders, esi:GET /characters/{character_id}/orders/history
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/planets",
        "esi:GET /characters/{character_id}/planets/{planet_id}",
        "esi:GET /characters/{character_id}/orders",
        "esi:GET /characters/{character_id}/orders/history",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let char_id = 90_000_001_i64;
    let other_id = 90_000_002_i64;
    let mut s1 = setup_pilot(&ctx, 21, "demo", "demo-secret", char_id).await;

    // 1. Deploy Command Center + Storage Facility + Link via planetMgr.UserUpdateNetwork
    let undock = rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await;
    let ship_id = undock.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            2495,
            TrainedSkillRecord { type_id: 2495, level: 4, skill_points: 100_000.0 },
        );
        snap.items.insert(
            800_000_821,
            make_item(800_000_821, 2534, char_id, ship_id, InventoryFlag::Cargo, 1, false),
        );
    });
    let _ = rpc(&ctx, &mut s1, "planetMgr", "UserUpdateNetwork", &[
        PyRep::Integer(JITA_PLANET_ID),
        PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(2534), PyRep::Real(0.1), PyRep::Real(0.2)]),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(2541), PyRep::Real(0.15), PyRep::Real(0.25)]),
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(1)]),
        ]),
    ]).await;

    // Dock back at Jita 4-4 and place two character orders, cancelling one via marketProxy
    s1.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(JITA_STATION_ID);
        }
    });
    let sell_open = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("price", PyRep::Real(12.0)),
                ("quantity", PyRep::Integer(200)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await;
    let open_oid = sell_open.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    let sell_cancel = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("price", PyRep::Real(14.0)),
                ("quantity", PyRep::Integer(150)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await;
    let cancel_oid = sell_cancel.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    let _ = rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(cancel_oid)]).await;

    let (addr, _router, handle) = EsiServer::spawn(Arc::clone(&ctx), "127.0.0.1:0").await.expect("spawn ESI");
    let spec = OpenApiSpec::pinned();
    let scopes = vec!["esi-planets.manage_planets.v1", "esi-markets.read_character_orders.v1"];
    let tok_owner = issue_token_with_scopes(&ctx, char_id, scopes.clone());
    let tok_other = issue_token_with_scopes(&ctx, other_id, scopes);

    let planets_path = format!("/v1/characters/{char_id}/planets/");
    let planet_detail_path = format!("/v1/characters/{char_id}/planets/{JITA_PLANET_ID}/");
    let orders_path = format!("/v1/characters/{char_id}/orders/");
    let history_path = format!("/v1/characters/{char_id}/orders/history/");

    http_expect_status(addr, &planets_path, None, 401).await;
    http_expect_status(addr, &planets_path, Some(&tok_other), 403).await;
    http_expect_status(addr, &format!("/v1/characters/{char_id}/planets/40000001/"), Some(&tok_owner), 404).await;

    let planets = http_get_json(addr, &spec, &planets_path, Some(&tok_owner)).await;
    assert_eq!(planets.as_array().unwrap().len(), 1);
    assert_eq!(planets[0]["planet_id"], JITA_PLANET_ID);
    assert_eq!(planets[0]["num_pins"], 2);

    let layout = http_get_json(addr, &spec, &planet_detail_path, Some(&tok_owner)).await;
    assert_eq!(layout["pins"].as_array().unwrap().len(), 2);
    assert_eq!(layout["links"].as_array().unwrap().len(), 1);

    let orders = http_get_json(addr, &spec, &orders_path, Some(&tok_owner)).await;
    assert!(orders.as_array().unwrap().iter().any(|o| o["order_id"] == open_oid));

    let hist = http_get_json(addr, &spec, &history_path, Some(&tok_owner)).await;
    let cancelled_row = hist
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["order_id"] == cancel_oid)
        .expect("cancelled order in history");
    assert_eq!(cancelled_row["state"], "cancelled");

    handle.abort();

    let ctx2 = Arc::new(crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "esi_b2_char_pi_orders"));
    let (addr2, _router2, handle2) = EsiServer::spawn(Arc::clone(&ctx2), "127.0.0.1:0").await.expect("spawn ESI 2");
    let tok_owner2 = issue_token_with_scopes(
        &ctx2,
        char_id,
        vec![
            "esi-planets.manage_planets.v1",
            "esi-markets.read_character_orders.v1",
        ],
    );
    let layout2 = http_get_json(addr2, &spec, &planet_detail_path, Some(&tok_owner2)).await;
    assert_eq!(layout2["pins"].as_array().unwrap().len(), 2);
    let hist2 = http_get_json(addr2, &spec, &history_path, Some(&tok_owner2)).await;
    assert!(hist2.as_array().unwrap().iter().any(|o| o["order_id"] == cancel_oid));

    handle2.abort();
}

#[tokio::test]
async fn test_esi_b2_market_history_structure_and_corp_orders_live_http_and_sqlite() {
    // parity: esi:GET /markets/{region_id}/history, esi:GET /markets/structures/{structure_id}, esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history
    eve_testkit::attest(&[
        "esi:GET /markets/{region_id}/history",
        "esi:GET /markets/structures/{structure_id}",
        "esi:GET /corporations/{corporation_id}/orders",
        "esi:GET /corporations/{corporation_id}/orders/history",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    let ceo_id = 90_000_001_i64;
    let trader_id = 90_000_002_i64;
    let corp_id = 98_000_831_i64;
    let fortizar_id = 1_000_000_831_001_i64;
    let bare_astrahus_id = 1_000_000_831_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "B2 Market & Citadel Corp".into(),
            ticker: "B2MC".into(),
            ceo_id,
            master_wallet: IskCents(50_000_000_00),
            ..Default::default()
        };
        corp.member_roles.insert(ceo_id, ROLE_DIRECTOR);
        corp.member_roles.insert(trader_id, ROLE_TRADER);
        snap.corporations.insert(corp_id, corp);
        for cid in [ceo_id, trader_id] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.corporation_id = corp_id;
                ch.balance = IskCents(50_000_000_00);
            }
        }
        snap.structures.insert(
            fortizar_id,
            make_structure(fortizar_id, 35833, corp_id, JITA_SYSTEM_ID, "B2 Market Fortizar"),
        );
        snap.structures.insert(
            bare_astrahus_id,
            make_structure(bare_astrahus_id, 35832, corp_id, JITA_SYSTEM_ID, "B2 Bare Astrahus"),
        );
        // Fit online Standup Market Hub I (35892) on fortizar_id
        snap.items.insert(
            800_000_831,
            make_item(800_000_831, 35892, corp_id, fortizar_id, InventoryFlag::ServiceSlot0, 1, true),
        );
    });

    let mut s1 = setup_pilot(&ctx, 31, "demo", "demo-secret", ceo_id).await;
    let mut s2 = setup_pilot(&ctx, 32, "other", "other-secret", trader_id).await;

    // 1. Execute real trade at Jita 4-4 to generate server-aggregated OHLCV market_history
    let _ = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TRITANIUM_TYPE_ID as i64)),
                ("price", PyRep::Real(6.0)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await;
    let _ = rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(TRITANIUM_TYPE_ID as i64),
            PyRep::Real(6.0),
            PyRep::Integer(50),
            PyRep::Integer(90),
            PyRep::Integer(1),
        ],
    )
    .await;

    // 2. Place structure market order on fortizar_id
    s1.structure_id = Some(fortizar_id);
    s1.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = None;
            ch.structure_id = Some(fortizar_id);
        }
    });
    let _ = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(fortizar_id), PyRep::Integer(TRITANIUM_TYPE_ID as i64), PyRep::Real(5.5),
        PyRep::Integer(100), PyRep::Integer(90), PyRep::Integer(1),
    ]).await;

    // 3. Place two corporation orders (one active, one cancelled) via useCorp=true
    s2.station_id = Some(JITA_STATION_ID);
    let mut corp_kw = HashMap::new();
    corp_kw.insert("useCorp".to_string(), PyRep::Bool(true));
    corp_kw.insert("accountKey".to_string(), PyRep::Integer(1000));
    let corp_open = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(4.5),
                PyRep::Integer(200),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &corp_kw,
            None,
        )
        .await
        .expect("corp open order");
    let corp_open_oid = corp_open.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    let corp_cancel = ctx
        .dispatch_rpc(
            &mut s2,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TRITANIUM_TYPE_ID as i64),
                PyRep::Real(4.2),
                PyRep::Integer(100),
                PyRep::Integer(90),
                PyRep::Integer(1),
            ],
            &corp_kw,
            None,
        )
        .await
        .expect("corp cancel order");
    let corp_cancel_oid = corp_cancel.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    let _ = rpc(&ctx, &mut s2, "marketProxy", "CancelCharOrder", &[PyRep::Integer(corp_cancel_oid)]).await;

    let (addr, _router, handle) = EsiServer::spawn(Arc::clone(&ctx), "127.0.0.1:0").await.expect("spawn ESI");
    let spec = OpenApiSpec::pinned();
    let tok_trader = issue_token_with_scopes(&ctx, trader_id, vec!["esi-markets.structure_markets.v1", "esi-markets.read_corporation_orders.v1"]);

    // Public market history
    http_expect_status(addr, &format!("/v1/markets/{JITA_REGION_ID}/history/"), None, 400).await;
    let hist = http_get_json(addr, &spec, &format!("/v1/markets/{JITA_REGION_ID}/history/?type_id={TRITANIUM_TYPE_ID}"), None,).await;
    assert!(hist[0]["volume"].as_i64().unwrap() >= 50);
    assert!(hist[0]["average"].as_f64().unwrap() > 0.0);

    // Structure market: 403 on bare Astrahus, 200 on Fortizar with online Market Hub
    http_expect_status(addr, &format!("/v1/markets/structures/{bare_astrahus_id}/"), Some(&tok_trader), 403).await;
    let struct_orders = http_get_json(addr, &spec, &format!("/v1/markets/structures/{fortizar_id}/"), Some(&tok_trader),).await;
    assert_eq!(struct_orders.as_array().unwrap().len(), 1);
    assert_eq!(struct_orders[0]["location_id"], fortizar_id);

    // Corporation orders & history accessible by Junior_Accountant | Trader
    let corp_orders = http_get_json(addr, &spec, &format!("/v1/corporations/{corp_id}/orders/"), Some(&tok_trader),).await;
    assert!(corp_orders.as_array().unwrap().iter().any(|o| o["order_id"] == corp_open_oid));

    let corp_hist = http_get_json(addr, &spec, &format!("/v1/corporations/{corp_id}/orders/history/"), Some(&tok_trader),).await;
    assert!(corp_hist.as_array().unwrap().iter().any(|o| o["order_id"] == corp_cancel_oid && o["state"] == "cancelled"));

    handle.abort();

    let ctx2 = Arc::new(crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "esi_b2_market_struct_corp"));
    let (addr2, _router2, handle2) = EsiServer::spawn(Arc::clone(&ctx2), "127.0.0.1:0").await.expect("spawn ESI 2");
    let tok_trader2 = issue_token_with_scopes(&ctx2, trader_id, vec!["esi-markets.structure_markets.v1", "esi-markets.read_corporation_orders.v1"]);
    let hist2 = http_get_json(addr2, &spec, &format!("/v1/markets/{JITA_REGION_ID}/history/?type_id={TRITANIUM_TYPE_ID}"), None,).await;
    assert!(hist2[0]["volume"].as_i64().unwrap() >= 50);
    let corp_orders2 = http_get_json(addr2, &spec, &format!("/v1/corporations/{corp_id}/orders/"), Some(&tok_trader2),).await;
    assert!(corp_orders2.as_array().unwrap().iter().any(|o| o["order_id"] == corp_open_oid));

    handle2.abort();
}
