//! `ESI-6` integration tests for Market routes against live `WorldRepository` and `SdeRegistry`
//! state populated via `marketProxy` RPCs, validated against `OpenApiSpec::pinned()`.

#![forbid(unsafe_code)]

use eve_db::{
    corp_roles::{
        ROLE_ACCOUNTANT, ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR, ROLE_FACTORY_MANAGER,
        ROLE_STATION_MANAGER, ROLE_TRADER,
    },
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    TrainedSkillRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

pub(crate) fn issue_token_with_scopes(
    ctx: &ServerContext,
    character_id: i64,
    scopes: Vec<&str>,
) -> String {
    let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&character_id) {
            ch.user_id = 1;
        }
        Ok(())
    });
    let now = ctx.now_unix();
    let scope_vec: Vec<String> = scopes.into_iter().map(str::to_string).collect();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            character_id,
            "evomu-esi6-test",
            "https://localhost/callback",
            scope_vec,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-esi6-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

pub(crate) fn validate_and_json(
    router: &EsiRouter,
    spec: &OpenApiSpec,
    req: &EsiHttpRequest,
    expected_status: u16,
) -> Value {
    let resp = router.handle_request(req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code,
        expected_status,
        "Expected {expected_status} for {} {}, got {} ({})",
        req.method,
        req.raw_target,
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    spec.validate_response(&req.method, raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| {
            panic!("OpenAPI schema validation failed for {} {}: {e}", req.method, req.raw_target)
        });
    assert!(resp.header("ETag").is_some(), "200 response for {} must include ETag", req.raw_target);
    assert!(
        resp.header("Expires").is_some() || resp.header("Cache-Control").is_some(),
        "200 response for {} must include cache headers",
        req.raw_target
    );
    resp.json().expect("valid JSON response")
}

pub(crate) fn get_json(
    router: &EsiRouter,
    spec: &OpenApiSpec,
    target: impl Into<String>,
    token: Option<&str>,
) -> Value {
    let mut req = EsiHttpRequest::new("GET", target);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    validate_and_json(router, spec, &req, 200)
}

pub(crate) fn assert_status(
    router: &EsiRouter,
    target: impl Into<String>,
    token: Option<&str>,
    expected: u16,
) {
    let mut req = EsiHttpRequest::new("GET", target);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    assert_eq!(
        resp.status_code,
        expected,
        "Expected {expected} for {}, got {} ({})",
        req.raw_target,
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
}

pub(crate) fn make_corp(
    corporation_id: i64,
    name: &str,
    ticker: &str,
    ceo_id: i64,
    alliance_id: Option<i64>,
    tax_rate: f64,
) -> CorporationRecord {
    let roles = ROLE_DIRECTOR
        | ROLE_ACCOUNTANT
        | ROLE_TRADER
        | ROLE_CONTRACT_MANAGER
        | ROLE_FACTORY_MANAGER
        | ROLE_STATION_MANAGER;
    let mut corp = CorporationRecord {
        corporation_id,
        name: name.to_string(),
        ticker: ticker.to_string(),
        ceo_id,
        alliance_id,
        tax_rate,
        members: HashSet::from([ceo_id]),
        member_roles: HashMap::from([(ceo_id, roles)]),
        master_wallet: IskCents::from_isk(500_000_000),
        ..Default::default()
    };
    corp.ensure_wallet_divisions();
    for d in &mut corp.wallet_divisions {
        d.balance = IskCents::from_isk(500_000_000);
    }
    corp
}

pub(crate) fn make_structure(
    structure_id: i64,
    type_id: u32,
    owner_corp_id: i64,
    solar_system_id: u32,
    name: &str,
) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id,
        type_id,
        owner_corp_id,
        solar_system_id,
        name: name.to_string(),
        position: [1000.0, 2000.0, 3000.0],
        state: "ShieldVulnerable".to_string(),
        quantum_core_installed: true,
        fuel_blocks: 500,
        shield_hp: 10000.0,
        armor_hp: 10000.0,
        hull_hp: 10000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

pub(crate) fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

pub(crate) async fn rpc_call(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    ctx.dispatch_rpc(session, service, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("RPC {service}.{method} failed: {e:?}"))
}

#[tokio::test]
async fn test_esi6_market_public_and_structure_routes() {
    // parity: esi:GET /markets/groups, esi:GET /markets/groups/{market_group_id}
    // parity: esi:GET /markets/prices, esi:GET /markets/{region_id}/orders
    // parity: esi:GET /markets/{region_id}/history, esi:GET /markets/{region_id}/types
    // parity: esi:GET /markets/structures/{structure_id}
    // parity: feat:esi.market.groups, feat:esi.market.prices, feat:esi.market.region-orders
    // parity: feat:esi.market.history, feat:esi.market.types, feat:esi.market.structure-orders
    eve_testkit::attest(&[
        "esi:GET /markets/groups",
        "esi:GET /markets/groups/{market_group_id}",
        "esi:GET /markets/prices",
        "esi:GET /markets/{region_id}/orders",
        "esi:GET /markets/{region_id}/history",
        "esi:GET /markets/{region_id}/types",
        "esi:GET /markets/structures/{structure_id}",
        "feat:esi.market.groups",
        "feat:esi.market.prices",
        "feat:esi.market.region-orders",
        "feat:esi.market.history",
        "feat:esi.market.types",
        "feat:esi.market.structure-orders",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001_i64;
    let rival_char_id = 90_000_002_i64;
    let corp_id = 98_000_601_i64;
    let rival_corp_id = 98_000_602_i64;
    let alliance_id = 99_000_601_i64;
    let jita_station = 60_003_760_i64;
    let structure_id = 1_030_000_601_001_i64;
    let private_structure_id = 1_030_000_601_002_i64;
    let zydrine_item_id = 910_010_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(250_000_000);
            ch.standings.insert(rival_corp_id, -5.0);
        }
        if let Some(ch2) = snap.characters.get_mut(&rival_char_id) {
            ch2.corporation_id = rival_corp_id;
            ch2.alliance_id = None;
            ch2.station_id = Some(jita_station);
            ch2.solar_system_id = 30_000_142;
            ch2.balance = IskCents::from_isk(250_000_000);
        }
        snap.corporations
            .insert(corp_id, make_corp(corp_id, "ESI6 Market Corp", "E6MK", char_id, Some(alliance_id), 0.0));
        snap.corporations
            .insert(rival_corp_id, make_corp(rival_corp_id, "Rival Market Corp", "RVMK", rival_char_id, None, 0.0));
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "ESI6 Market Alliance".to_string(),
                short_name: "E6MA".to_string(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
        snap.structures
            .insert(structure_id, make_structure(structure_id, 35833, corp_id, 30_000_142, "Jita Trade Fortizar"));
        snap.items.insert(
            910_001,
            make_item(910_001, 35_892, corp_id, structure_id, InventoryFlag::ServiceSlot0, 1, true),
        );
        snap.structures.insert(
            private_structure_id,
            make_structure(private_structure_id, 35833, rival_corp_id, 30_000_142, "Private Rival Fortizar"),
        );
        snap.items.insert(
            910_002,
            make_item(910_002, 35_892, rival_corp_id, private_structure_id, InventoryFlag::ServiceSlot0, 1, true),
        );
        snap.items.insert(
            zydrine_item_id,
            make_item(zydrine_item_id, 39, char_id, jita_station, InventoryFlag::Hangar, 20_000, false),
        );
    });

    // Place a 30-day sell order for 10,000 Zydrine (39) at 6.0 ISK in Jita via marketProxy.PlaceMultiSellOrder,
    // then fill 2,500 units via rival_char_id's marketProxy.PlaceBuyOrder so 7,500 remain and OHLCV history is recorded.
    let mut seller_sess = SessionState::new(9801);
    seller_sess.user_id = Some(1);
    seller_sess.character_id = Some(char_id);
    seller_sess.corporation_id = Some(corp_id);
    seller_sess.station_id = Some(jita_station);
    seller_sess.solar_system_id = Some(30_000_142);
    seller_sess.region_id = Some(10_000_002);

    let sell_items = PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(zydrine_item_id)),
        (PyRep::String("typeID".into()), PyRep::Integer(39)),
        (PyRep::String("price".into()), PyRep::Real(6.0)),
        (PyRep::String("quantity".into()), PyRep::Integer(10_000)),
        (PyRep::String("stationID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("duration".into()), PyRep::Integer(30)),
    ])]);
    let sell_rep = rpc_call(
        &ctx,
        &mut seller_sess,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[sell_items, PyRep::Bool(false), PyRep::Integer(30)],
    )
    .await;
    let sell_oid = sell_rep.dict_get("orderID").and_then(|v| v.as_i64()).expect("orderID");

    let mut buyer_sess = SessionState::new(9802);
    buyer_sess.user_id = Some(1);
    buyer_sess.character_id = Some(rival_char_id);
    buyer_sess.corporation_id = Some(rival_corp_id);
    buyer_sess.station_id = Some(jita_station);
    buyer_sess.solar_system_id = Some(30_000_142);
    buyer_sess.region_id = Some(10_000_002);

    let buy_args = [
        PyRep::Integer(jita_station),
        PyRep::Integer(39),
        PyRep::Real(6.0),
        PyRep::Integer(2_500),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(30),
        PyRep::Bool(false),
    ];
    rpc_call(&ctx, &mut buyer_sess, "marketProxy", "PlaceBuyOrder", &buy_args).await;

    // Place a resting buy order for Rifter (587) at structure_id via marketProxy.PlaceBuyOrder
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = Some(structure_id);
        }
    });
    seller_sess.station_id = None;
    seller_sess.structure_id = Some(structure_id);
    let struct_buy_args = [
        PyRep::Integer(structure_id),
        PyRep::Integer(587),
        PyRep::Real(420_000.0),
        PyRep::Integer(3),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(14),
        PyRep::Bool(false),
    ];
    let struct_buy_rep =
        rpc_call(&ctx, &mut seller_sess, "marketProxy", "PlaceBuyOrder", &struct_buy_args).await;
    let struct_buy_oid = struct_buy_rep.dict_get("orderID").and_then(|v| v.as_i64()).expect("orderID");

    // 1. GET /v1/markets/groups/ and /v1/markets/groups/{market_group_id}/
    let groups_req = EsiHttpRequest::new("GET", "/v1/markets/groups/");
    let groups_resp = router.handle_request(&groups_req);
    assert_eq!(groups_resp.status_code, 200);
    let groups_etag = groups_resp.header("ETag").expect("ETag").to_string();
    let groups_json = validate_and_json(&router, &spec, &groups_req, 200);
    let groups_arr = groups_json.as_array().expect("groups array");
    assert!(groups_arr.len() > 100);
    let first_mg_id = groups_arr[0].as_i64().expect("market_group_id");

    let cond_groups =
        EsiHttpRequest::new("GET", "/v1/markets/groups/").with_header("If-None-Match", &groups_etag);
    assert_eq!(router.handle_request(&cond_groups).status_code, 304);

    let mg_detail = get_json(&router, &spec, format!("/v1/markets/groups/{first_mg_id}/"), None);
    assert_eq!(mg_detail["market_group_id"], json!(first_mg_id));
    let expected_sde_mg = ctx.sde.market_groups.get(&(first_mg_id as u32)).expect("SDE market group");
    assert_eq!(mg_detail["name"], json!(expected_sde_mg.name));
    assert_eq!(
        mg_detail["description"],
        json!(ctx.sde.market_group_description(first_mg_id as u32).unwrap_or_default())
    );
    assert_status(&router, "/v1/markets/groups/999999999/", None, 404);

    // 2. GET /v1/markets/prices/
    let prices = get_json(&router, &spec, "/v1/markets/prices/", None);
    let prices_arr = prices.as_array().unwrap();
    let zyd_price = prices_arr.iter().find(|p| p["type_id"] == json!(39)).expect("Zydrine price");
    assert!(zyd_price["average_price"].as_f64().unwrap() >= 6.0);
    let rifter_price = prices_arr.iter().find(|p| p["type_id"] == json!(587)).expect("Rifter price");
    assert_eq!(rifter_price["average_price"], json!(420_000.0));

    // 3. GET /v1/markets/{region_id}/orders/
    let reg_orders_req =
        EsiHttpRequest::new("GET", "/v1/markets/10000002/orders/?order_type=sell&type_id=39");
    let reg_orders_resp = router.handle_request(&reg_orders_req);
    assert_eq!(reg_orders_resp.status_code, 200);
    assert_eq!(reg_orders_resp.header("X-Pages"), Some("1"));
    let reg_orders = validate_and_json(&router, &spec, &reg_orders_req, 200);
    let sell_entry = reg_orders
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["order_id"] == json!(sell_oid))
        .expect("placed Zydrine sell order in region orders");
    assert_eq!(sell_entry["system_id"], json!(30_000_142));
    assert_eq!(sell_entry["volume_remain"], json!(7_000));
    assert_eq!(sell_entry["duration"], json!(30));
    assert_eq!(sell_entry["range"], json!("region"));
    assert_eq!(sell_entry["price"], json!(6.0));
    assert_status(&router, "/v1/markets/10000002/orders/?order_type=sell&page=2", None, 404);
    assert_status(&router, "/v1/markets/10000002/orders/?order_type=invalid", None, 400);
    assert_status(&router, "/v1/markets/99999999/orders/?order_type=all", None, 404);

    // 4. GET /v1/markets/{region_id}/history/
    let hist = get_json(&router, &spec, "/v1/markets/10000002/history/?type_id=39", None);
    let hist_arr = hist.as_array().unwrap();
    assert!(!hist_arr.is_empty());
    let latest_hist = hist_arr.last().unwrap();
    assert!(latest_hist["volume"].as_i64().unwrap() >= 2_500);
    assert!(latest_hist["order_count"].as_i64().unwrap() >= 1);
    assert!(latest_hist["average"].as_f64().unwrap() >= 6.0);
    assert_status(&router, "/v1/markets/10000002/history/", None, 400);
    assert_status(&router, "/v1/markets/10000002/history/?type_id=999999999", None, 404);

    // 5. GET /v1/markets/{region_id}/types/
    let types = get_json(&router, &spec, "/v1/markets/10000002/types/", None);
    let types_arr = types.as_array().unwrap();
    assert!(types_arr.contains(&json!(39)));
    assert!(types_arr.contains(&json!(587)));
    assert_status(&router, "/v1/markets/99999999/types/", None, 404);

    // 6. GET /v1/markets/structures/{structure_id}/ (scope + ACL enforcement)
    let token = issue_token_with_scopes(&ctx, char_id, vec!["esi-markets.structure_markets.v1"]);
    let no_scope_token = issue_token_with_scopes(&ctx, char_id, vec![]);
    assert_status(&router, format!("/v1/markets/structures/{structure_id}/"), None, 401);
    assert_status(&router, format!("/v1/markets/structures/{structure_id}/"), Some(&no_scope_token), 403);
    assert_status(&router, format!("/v1/markets/structures/{private_structure_id}/"), Some(&token), 403);
    let struct_orders =
        get_json(&router, &spec, format!("/v1/markets/structures/{structure_id}/"), Some(&token));
    let struct_orders_arr = struct_orders.as_array().unwrap();
    assert_eq!(struct_orders_arr.len(), 1);
    assert_eq!(struct_orders_arr[0]["order_id"], json!(struct_buy_oid));
    assert_eq!(struct_orders_arr[0]["type_id"], json!(587));
    assert_eq!(struct_orders_arr[0]["duration"], json!(14));
    assert_eq!(struct_orders_arr[0]["range"], json!("station"));
}

#[tokio::test]
async fn test_esi6_market_character_and_corporation_orders_routes() {
    // parity: esi:GET /characters/{character_id}/orders, esi:GET /characters/{character_id}/orders/history
    // parity: esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history
    // parity: feat:esi.market.char-orders, feat:esi.market.corp-orders
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/orders",
        "esi:GET /characters/{character_id}/orders/history",
        "esi:GET /corporations/{corporation_id}/orders",
        "esi:GET /corporations/{corporation_id}/orders/history",
        "feat:esi.market.char-orders",
        "feat:esi.market.corp-orders",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_611_i64;
    let jita_station = 60_003_760_i64;
    let isogen_item_id = 920_010_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(250_000_000);
        }
        snap.corporations
            .insert(corp_id, make_corp(corp_id, "ESI6 Order Corp", "E6OC", char_id, None, 0.0));
        snap.skills.entry(char_id).or_default().insert(
            3447,
            TrainedSkillRecord {
                type_id: 3447,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.insert(
            isogen_item_id,
            make_item(isogen_item_id, 37, corp_id, jita_station, InventoryFlag::Hangar, 2_500, false),
        );
    });

    let mut sess = SessionState::new(9810);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = ROLE_DIRECTOR | ROLE_ACCOUNTANT | ROLE_TRADER;
    sess.station_id = Some(jita_station);
    sess.solar_system_id = Some(30_000_142);
    sess.region_id = Some(10_000_002);

    // 1. Place active personal buy order (range = 0 solarsystem, min_volume = 5, duration = 7)
    let char_active_args = [
        PyRep::Integer(jita_station),
        PyRep::Integer(34),
        PyRep::Real(3.5),
        PyRep::Integer(2_000),
        PyRep::Integer(0),
        PyRep::Integer(5),
        PyRep::Integer(7),
        PyRep::Bool(false),
    ];
    let char_active_rep =
        rpc_call(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &char_active_args).await;
    let char_active_oid = char_active_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    // 2. Place and cancel personal buy order (duration = 14) so it appears in character order history
    let char_hist_args = [
        PyRep::Integer(jita_station),
        PyRep::Integer(35),
        PyRep::Real(4.0),
        PyRep::Integer(4_000),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(14),
        PyRep::Bool(false),
    ];
    let char_hist_rep =
        rpc_call(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &char_hist_args).await;
    let char_hist_oid = char_hist_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc_call(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(char_hist_oid)])
        .await;

    // 3. Place active corporate buy order (accountKey = 1001 -> wallet_division 2, duration = 30)
    let corp_active_args = [
        PyRep::Integer(jita_station),
        PyRep::Integer(36),
        PyRep::Real(25.0),
        PyRep::Integer(800),
        PyRep::Integer(-1),
        PyRep::Integer(10),
        PyRep::Integer(30),
        PyRep::Bool(true),
        PyRep::Integer(1001),
    ];
    let corp_active_rep =
        rpc_call(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &corp_active_args).await;
    let corp_active_oid = corp_active_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    // 4. Place and cancel corporate sell order (accountKey = 1002 -> wallet_division 3, duration = 90)
    let corp_sell_items = PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(isogen_item_id)),
        (PyRep::String("typeID".into()), PyRep::Integer(37)),
        (PyRep::String("price".into()), PyRep::Real(95.0)),
        (PyRep::String("quantity".into()), PyRep::Integer(2_500)),
        (PyRep::String("stationID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("duration".into()), PyRep::Integer(90)),
    ])]);
    let corp_hist_args = [
        corp_sell_items,
        PyRep::Bool(true),
        PyRep::Integer(90),
        PyRep::None,
        PyRep::Integer(1002),
    ];
    let corp_hist_rep =
        rpc_call(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &corp_hist_args).await;
    let corp_hist_oid = corp_hist_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc_call(&ctx, &mut sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(corp_hist_oid)])
        .await;

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-markets.read_character_orders.v1",
            "esi-markets.read_corporation_orders.v1",
        ],
    );
    let no_scope_token = issue_token_with_scopes(&ctx, char_id, vec![]);

    assert_status(&router, format!("/v2/characters/{char_id}/orders/"), None, 401);
    assert_status(&router, format!("/v2/characters/{char_id}/orders/"), Some(&no_scope_token), 403);

    let char_orders =
        get_json(&router, &spec, format!("/v2/characters/{char_id}/orders/"), Some(&token));
    let char_orders_arr = char_orders.as_array().unwrap();
    assert_eq!(char_orders_arr.len(), 1);
    assert_eq!(char_orders_arr[0]["order_id"], json!(char_active_oid));
    assert_eq!(char_orders_arr[0]["volume_remain"], json!(2_000));
    assert_eq!(char_orders_arr[0]["min_volume"], json!(5));
    assert_eq!(char_orders_arr[0]["duration"], json!(7));
    assert_eq!(char_orders_arr[0]["range"], json!("solarsystem"));
    assert_eq!(char_orders_arr[0]["is_corporation"], json!(false));

    let char_hist =
        get_json(&router, &spec, format!("/v1/characters/{char_id}/orders/history/"), Some(&token));
    let char_hist_arr = char_hist.as_array().unwrap();
    assert_eq!(char_hist_arr.len(), 1);
    assert_eq!(char_hist_arr[0]["order_id"], json!(char_hist_oid));
    assert_eq!(char_hist_arr[0]["state"], json!("cancelled"));
    assert_eq!(char_hist_arr[0]["duration"], json!(14));
    assert_status(&router, format!("/v1/characters/{char_id}/orders/history/?page=2"), Some(&token), 404);

    let corp_orders =
        get_json(&router, &spec, format!("/v3/corporations/{corp_id}/orders/"), Some(&token));
    let corp_orders_arr = corp_orders.as_array().unwrap();
    assert_eq!(corp_orders_arr.len(), 1);
    assert_eq!(corp_orders_arr[0]["order_id"], json!(corp_active_oid));
    assert_eq!(corp_orders_arr[0]["wallet_division"], json!(2));
    assert_eq!(corp_orders_arr[0]["issued_by"], json!(char_id));
    assert_eq!(corp_orders_arr[0]["duration"], json!(30));
    assert_eq!(corp_orders_arr[0]["range"], json!("station"));

    let corp_hist = get_json(
        &router,
        &spec,
        format!("/v2/corporations/{corp_id}/orders/history/"),
        Some(&token),
    );
    let corp_hist_arr = corp_hist.as_array().unwrap();
    assert_eq!(corp_hist_arr.len(), 1);
    assert_eq!(corp_hist_arr[0]["order_id"], json!(corp_hist_oid));
    assert_eq!(corp_hist_arr[0]["wallet_division"], json!(3));
    assert_eq!(corp_hist_arr[0]["state"], json!("cancelled"));
    assert_eq!(corp_hist_arr[0]["duration"], json!(90));
}
