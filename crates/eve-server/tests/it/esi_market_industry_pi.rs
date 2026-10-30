//! `ESI-6` integration tests for Market, Contracts, Industry, Mining, and Planetary Interaction
//! routes against live `WorldRepository` and `SdeRegistry` state, validated against `OpenApiSpec::pinned()`.

#![forbid(unsafe_code)]

use eve_db::{
    AllianceRecord, ContractRecord, CorporationRecord, CustomsOfficeRecord, IndustryJobRecord,
    InventoryFlag, InventoryItemRecord, IskCents, MarketHistoryDayRecord, MarketOrderMeta,
    MarketOrderRecord, MiningLedgerRecord, MoonExtractionRecord, ObserverMiningRecord,
    PlanetaryColonyLayoutRecord, PlanetaryColonyRecord, PlanetaryLinkRecord,
    PlanetaryPinContentRecord, PlanetaryPinRecord, PlanetaryRouteRecord, UpwellStructureRecord,
};
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token_with_scopes(ctx: &ServerContext, character_id: i64, scopes: Vec<&str>) -> String {
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
    let bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-esi6-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code");
    bundle.access_token
}

fn validate_and_json(
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
    spec.validate_response(
        &req.method,
        raw_path,
        resp.status_code,
        &resp.headers,
        &resp.body,
    )
    .unwrap_or_else(|e| {
        panic!(
            "OpenAPI schema validation failed for {} {}: {e}",
            req.method, req.raw_target
        )
    });
    assert!(
        resp.header("ETag").is_some(),
        "200 response for {} must include ETag header",
        req.raw_target
    );
    assert!(
        resp.header("Expires").is_some() || resp.header("Cache-Control").is_some(),
        "200 response for {} must include cache headers",
        req.raw_target
    );
    resp.json().expect("valid JSON response")
}

fn get_json(
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

fn assert_status(
    router: &EsiRouter,
    target: impl Into<String>,
    token: Option<&str>,
    expected: u16,
) {
    let mut req = EsiHttpRequest::new("GET", target);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    assert_eq!(router.handle_request(&req).status_code, expected);
}

fn make_corp(
    corporation_id: i64,
    name: &str,
    ticker: &str,
    ceo_id: i64,
    alliance_id: Option<i64>,
    tax_rate: f64,
) -> CorporationRecord {
    CorporationRecord {
        corporation_id,
        name: name.to_string(),
        ticker: ticker.to_string(),
        ceo_id,
        alliance_id,
        tax_rate,
        members: HashSet::from([ceo_id]),
        ..Default::default()
    }
}

fn make_structure(
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
        last_damage_unix: 0, self_destruct_at_unix: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn seed_order(
    snap: &mut eve_db::WorldStateSnapshot,
    order_id: i64,
    type_id: u32,
    owner_id: i64,
    station_id: i64,
    region_id: u32,
    price_isk: i64,
    vol_entered: i32,
    vol_remaining: i32,
    min_volume: i32,
    is_bid: bool,
    escrow_isk: i64,
    meta: MarketOrderMeta,
) {
    snap.market_orders.insert(
        order_id,
        MarketOrderRecord {
            order_id,
            type_id,
            owner_id,
            station_id,
            region_id,
            price: IskCents::from_isk(price_isk),
            vol_entered,
            vol_remaining,
            min_volume,
            is_bid,
            is_npc: false,
            escrow: IskCents::from_isk(escrow_isk),
        },
    );
    snap.market_order_meta.insert(order_id, meta);
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
    let now = ctx.now_unix();

    let char_id = 90_000_001_i64;
    let rival_char_id = 90_000_002_i64;
    let corp_id = 98_000_601_i64;
    let rival_corp_id = 98_000_602_i64;
    let alliance_id = 99_000_601_i64;
    let structure_id = 1_030_000_601_001_i64;
    let private_structure_id = 1_030_000_601_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
            ch.standings.insert(rival_corp_id, -5.0);
        }
        if let Some(ch2) = snap.characters.get_mut(&rival_char_id) {
            ch2.corporation_id = rival_corp_id;
            ch2.alliance_id = None;
        }
        snap.corporations.insert(
            corp_id,
            make_corp(
                corp_id,
                "ESI6 Market Corp",
                "E6MK",
                char_id,
                Some(alliance_id),
                0.0,
            ),
        );
        snap.corporations.insert(
            rival_corp_id,
            make_corp(
                rival_corp_id,
                "Rival Market Corp",
                "RVMK",
                rival_char_id,
                None,
                0.0,
            ),
        );
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
        snap.structures.insert(
            structure_id,
            make_structure(
                structure_id,
                35833,
                corp_id,
                30_000_142,
                "Jita Trade Fortizar",
            ),
        );
        snap.items.insert(
            910_001,
            InventoryItemRecord {
                item_id: 910_001,
                type_id: 35_892, // Standup Market Hub I
                owner_id: corp_id,
                location_id: structure_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.structures.insert(
            private_structure_id,
            make_structure(
                private_structure_id,
                35833,
                rival_corp_id,
                30_000_142,
                "Private Rival Fortizar",
            ),
        );
        snap.items.insert(
            910_002,
            InventoryItemRecord {
                item_id: 910_002,
                type_id: 35_892, // Standup Market Hub I
                owner_id: rival_corp_id,
                location_id: private_structure_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.market_orders.clear();
        snap.market_order_meta.clear();
        seed_order(
            snap,
            6101,
            34,
            char_id,
            60_003_760,
            10_000_002,
            6,
            10_000,
            7_500,
            10,
            false,
            0,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(1800),
                duration_days: 30,
                range: "region".to_string(),
                issued_by: Some(char_id),
                wallet_division: None,
                is_corporation: false,
                state: None,
            },
        );
        seed_order(
            snap,
            6102,
            587,
            char_id,
            structure_id,
            10_000_002,
            420_000,
            5,
            3,
            1,
            true,
            1_260_000,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(900),
                duration_days: 14,
                range: "station".to_string(),
                issued_by: Some(char_id),
                wallet_division: None,
                is_corporation: false,
                state: None,
            },
        );
        snap.market_history.insert(
            (10_000_002, 34),
            vec![MarketHistoryDayRecord {
                date: "2025-12-15".to_string(), average: 5.8, highest: 6.2, lowest: 5.4, order_count: 42, volume: 850_000,
                open_price: Some(5.5), close_price: Some(6.0),
            }],
        );
    });

    // 1. GET /v1/markets/groups/ and /v1/markets/groups/{market_group_id}/
    let groups_req = EsiHttpRequest::new("GET", "/v1/markets/groups/");
    let groups_resp = router.handle_request(&groups_req);
    assert_eq!(groups_resp.status_code, 200);
    let groups_etag = groups_resp.header("ETag").expect("ETag").to_string();
    let groups_json = validate_and_json(&router, &spec, &groups_req, 200);
    let groups_arr = groups_json.as_array().expect("groups array");
    assert!(groups_arr.len() > 100);
    let first_mg_id = groups_arr[0].as_i64().expect("market_group_id");

    let cond_groups = EsiHttpRequest::new("GET", "/v1/markets/groups/")
        .with_header("If-None-Match", &groups_etag);
    assert_eq!(router.handle_request(&cond_groups).status_code, 304);

    let mg_detail = get_json(
        &router,
        &spec,
        format!("/v1/markets/groups/{first_mg_id}/"),
        None,
    );
    assert_eq!(mg_detail["market_group_id"], json!(first_mg_id));
    let expected_sde_mg = ctx
        .sde
        .market_groups
        .get(&(first_mg_id as u32))
        .expect("SDE market group");
    assert_eq!(mg_detail["name"], json!(expected_sde_mg.name));
    assert_eq!(
        mg_detail["description"],
        json!(ctx.sde.market_group_description(first_mg_id as u32).unwrap_or_default())
    );
    assert_status(&router, "/v1/markets/groups/999999999/", None, 404);

    // 2. GET /v1/markets/prices/
    let prices = get_json(&router, &spec, "/v1/markets/prices/", None);
    let prices_arr = prices.as_array().unwrap();
    let trit_price = prices_arr
        .iter()
        .find(|p| p["type_id"] == json!(34))
        .expect("Tritanium price entry");
    assert_eq!(trit_price["average_price"], json!(6.0));
    let rifter_price = prices_arr
        .iter()
        .find(|p| p["type_id"] == json!(587))
        .expect("Rifter price entry");
    assert_eq!(rifter_price["average_price"], json!(420_000.0));

    // 3. GET /v1/markets/{region_id}/orders/
    let reg_orders_req = EsiHttpRequest::new(
        "GET",
        "/v1/markets/10000002/orders/?order_type=sell&type_id=34",
    );
    let reg_orders_resp = router.handle_request(&reg_orders_req);
    assert_eq!(reg_orders_resp.status_code, 200);
    assert_eq!(reg_orders_resp.header("X-Pages"), Some("1"));
    let reg_orders = validate_and_json(&router, &spec, &reg_orders_req, 200);
    let reg_orders_arr = reg_orders.as_array().unwrap();
    assert_eq!(reg_orders_arr.len(), 1);
    assert_eq!(reg_orders_arr[0]["order_id"], json!(6101));
    assert_eq!(reg_orders_arr[0]["system_id"], json!(30_000_142));
    assert_eq!(reg_orders_arr[0]["volume_remain"], json!(7_500));
    assert_eq!(reg_orders_arr[0]["min_volume"], json!(10));
    assert_eq!(reg_orders_arr[0]["duration"], json!(30));
    assert_eq!(reg_orders_arr[0]["range"], json!("region"));
    assert_eq!(reg_orders_arr[0]["price"], json!(6.0));
    assert_status(
        &router,
        "/v1/markets/10000002/orders/?order_type=sell&page=2",
        None,
        404,
    );
    assert_status(
        &router,
        "/v1/markets/10000002/orders/?order_type=invalid",
        None,
        400,
    );
    assert_status(
        &router,
        "/v1/markets/99999999/orders/?order_type=all",
        None,
        404,
    );

    // 4. GET /v1/markets/{region_id}/history/
    let hist = get_json(
        &router,
        &spec,
        "/v1/markets/10000002/history/?type_id=34",
        None,
    );
    let hist_arr = hist.as_array().unwrap();
    assert_eq!(hist_arr.len(), 1);
    assert_eq!(hist_arr[0]["date"], json!("2025-12-15"));
    assert_eq!(hist_arr[0]["average"], json!(5.8));
    assert_eq!(hist_arr[0]["highest"], json!(6.2));
    assert_eq!(hist_arr[0]["lowest"], json!(5.4));
    assert_eq!(hist_arr[0]["order_count"], json!(42));
    assert_eq!(hist_arr[0]["volume"], json!(850_000));
    assert_status(&router, "/v1/markets/10000002/history/", None, 400);
    assert_status(
        &router,
        "/v1/markets/10000002/history/?type_id=999999999",
        None,
        404,
    );

    // 5. GET /v1/markets/{region_id}/types/
    let types = get_json(&router, &spec, "/v1/markets/10000002/types/", None);
    let types_arr = types.as_array().unwrap();
    assert!(types_arr.contains(&json!(34)));
    assert!(types_arr.contains(&json!(587)));
    assert_status(&router, "/v1/markets/99999999/types/", None, 404);

    // 6. GET /v1/markets/structures/{structure_id}/ (scope + ACL enforcement)
    let token = issue_token_with_scopes(&ctx, char_id, vec!["esi-markets.structure_markets.v1"]);
    let no_scope_token = issue_token_with_scopes(&ctx, char_id, vec![]);
    assert_status(
        &router,
        format!("/v1/markets/structures/{structure_id}/"),
        None,
        401,
    );
    assert_status(
        &router,
        format!("/v1/markets/structures/{structure_id}/"),
        Some(&no_scope_token),
        403,
    );
    assert_status(
        &router,
        format!("/v1/markets/structures/{private_structure_id}/"),
        Some(&token),
        403,
    );
    let struct_orders = get_json(
        &router,
        &spec,
        format!("/v1/markets/structures/{structure_id}/"),
        Some(&token),
    );
    let struct_orders_arr = struct_orders.as_array().unwrap();
    assert_eq!(struct_orders_arr.len(), 1);
    assert_eq!(struct_orders_arr[0]["order_id"], json!(6102));
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
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_611_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            make_corp(corp_id, "ESI6 Order Corp", "E6OC", char_id, None, 0.0),
        );
        seed_order(
            snap,
            6201,
            34,
            char_id,
            60_003_760,
            10_000_002,
            7,
            5_000,
            2_000,
            5,
            false,
            0,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(600),
                duration_days: 7,
                range: "solarsystem".to_string(),
                issued_by: Some(char_id),
                wallet_division: None,
                is_corporation: false,
                state: None,
            },
        );
        seed_order(
            snap,
            6202,
            35,
            char_id,
            60_003_760,
            10_000_002,
            12,
            4_000,
            0,
            1,
            true,
            0,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(86_400),
                duration_days: 14,
                range: "station".to_string(),
                issued_by: Some(char_id),
                wallet_division: None,
                is_corporation: false,
                state: Some("cancelled".to_string()),
            },
        );
        seed_order(
            snap,
            6203,
            36,
            corp_id,
            60_003_760,
            10_000_002,
            55,
            1_000,
            800,
            10,
            true,
            44_000,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(1200),
                duration_days: 30,
                range: "region".to_string(),
                issued_by: Some(char_id),
                wallet_division: Some(2),
                is_corporation: true,
                state: None,
            },
        );
        seed_order(
            snap,
            6204,
            37,
            corp_id,
            60_003_760,
            10_000_002,
            95,
            2_500,
            0,
            1,
            false,
            0,
            MarketOrderMeta {
                issued_unix: now.saturating_sub(172_800),
                duration_days: 90,
                range: "station".to_string(),
                issued_by: Some(char_id),
                wallet_division: Some(3),
                is_corporation: true,
                state: Some("expired".to_string()),
            },
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-markets.read_character_orders.v1",
            "esi-markets.read_corporation_orders.v1",
        ],
    );
    let no_scope_token = issue_token_with_scopes(&ctx, char_id, vec![]);

    assert_status(
        &router,
        format!("/v2/characters/{char_id}/orders/"),
        None,
        401,
    );
    assert_status(
        &router,
        format!("/v2/characters/{char_id}/orders/"),
        Some(&no_scope_token),
        403,
    );

    let char_orders = get_json(
        &router,
        &spec,
        format!("/v2/characters/{char_id}/orders/"),
        Some(&token),
    );
    let char_orders_arr = char_orders.as_array().unwrap();
    assert_eq!(char_orders_arr.len(), 1);
    assert_eq!(char_orders_arr[0]["order_id"], json!(6201));
    assert_eq!(char_orders_arr[0]["volume_remain"], json!(2_000));
    assert_eq!(char_orders_arr[0]["min_volume"], json!(5));
    assert_eq!(char_orders_arr[0]["duration"], json!(7));
    assert_eq!(char_orders_arr[0]["range"], json!("solarsystem"));
    assert_eq!(char_orders_arr[0]["is_corporation"], json!(false));

    let char_hist = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/orders/history/"),
        Some(&token),
    );
    let char_hist_arr = char_hist.as_array().unwrap();
    assert_eq!(char_hist_arr.len(), 1);
    assert_eq!(char_hist_arr[0]["order_id"], json!(6202));
    assert_eq!(char_hist_arr[0]["state"], json!("cancelled"));
    assert_eq!(char_hist_arr[0]["duration"], json!(14));
    assert_status(
        &router,
        format!("/v1/characters/{char_id}/orders/history/?page=2"),
        Some(&token),
        404,
    );

    let corp_orders = get_json(
        &router,
        &spec,
        format!("/v3/corporations/{corp_id}/orders/"),
        Some(&token),
    );
    let corp_orders_arr = corp_orders.as_array().unwrap();
    assert_eq!(corp_orders_arr.len(), 1);
    assert_eq!(corp_orders_arr[0]["order_id"], json!(6203));
    assert_eq!(corp_orders_arr[0]["wallet_division"], json!(2));
    assert_eq!(corp_orders_arr[0]["issued_by"], json!(char_id));
    assert_eq!(corp_orders_arr[0]["duration"], json!(30));
    assert_eq!(corp_orders_arr[0]["range"], json!("region"));

    let corp_hist = get_json(
        &router,
        &spec,
        format!("/v2/corporations/{corp_id}/orders/history/"),
        Some(&token),
    );
    let corp_hist_arr = corp_hist.as_array().unwrap();
    assert_eq!(corp_hist_arr.len(), 1);
    assert_eq!(corp_hist_arr[0]["order_id"], json!(6204));
    assert_eq!(corp_hist_arr[0]["wallet_division"], json!(3));
    assert_eq!(corp_hist_arr[0]["state"], json!("expired"));
    assert_eq!(corp_hist_arr[0]["duration"], json!(90));
}

#[tokio::test]
async fn test_esi6_contracts_character_corporation_and_public_routes() {
    // parity: esi:GET /characters/{character_id}/contracts, esi:GET /characters/{character_id}/contracts/{contract_id}/items
    // parity: esi:GET /characters/{character_id}/contracts/{contract_id}/bids, esi:GET /contracts/public/{region_id}
    // parity: esi:GET /contracts/public/items/{contract_id}, esi:GET /contracts/public/bids/{contract_id}
    // parity: esi:GET /corporations/{corporation_id}/contracts, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items
    // parity: esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids
    // parity: feat:esi.contracts.char-list, feat:esi.contracts.char-items-bids, feat:esi.contracts.corp, feat:esi.contracts.public
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/contracts",
        "esi:GET /characters/{character_id}/contracts/{contract_id}/items",
        "esi:GET /characters/{character_id}/contracts/{contract_id}/bids",
        "esi:GET /contracts/public/{region_id}",
        "esi:GET /contracts/public/items/{contract_id}",
        "esi:GET /contracts/public/bids/{contract_id}",
        "esi:GET /corporations/{corporation_id}/contracts",
        "esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items",
        "esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids",
        "feat:esi.contracts.char-list",
        "feat:esi.contracts.char-items-bids",
        "feat:esi.contracts.corp",
        "feat:esi.contracts.public",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_621_i64;
    let public_contract_id = 6_301_i64;
    let private_contract_id = 6_302_i64;
    let escrow_item_id = 880_621_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            make_corp(corp_id, "ESI6 Contracts Corp", "E6CT", char_id, None, 0.0),
        );
        snap.items.insert(
            escrow_item_id,
            InventoryItemRecord {
                item_id: escrow_item_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.contracts.insert(
            public_contract_id,
            ContractRecord {
                contract_id: public_contract_id,
                issuer_id: char_id,
                issuer_corp_id: corp_id,
                assignee_id: 0,
                acceptor_id: None,
                contract_type: "Auction".to_string(),
                status: "Outstanding".to_string(),
                title: "Jita Rifter Auction".to_string(),
                for_corp: true,
                availability: "Public".to_string(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                date_issued_unix: now.saturating_sub(1800),
                expiration_unix: now + 86_400,
                num_days: 7,
                price: IskCents::from_isk(1_500_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                buyout: IskCents::from_isk(5_000_000),
                item_ids: vec![escrow_item_id],
                requested_items: vec![(34, 1_000)],
                current_bid: IskCents::from_isk(2_500_000),
                current_bidder_id: Some(90_000_002),
                bid_history: vec![
                    (90_000_002, IskCents::from_isk(2_000_000), now - 900),
                    (90_000_002, IskCents::from_isk(2_500_000), now - 300),
                ],
            },
        );
        snap.contracts.insert(
            private_contract_id,
            ContractRecord {
                contract_id: private_contract_id,
                issuer_id: 90_000_002,
                issuer_corp_id: 98_000_699,
                assignee_id: 90_000_002,
                acceptor_id: None,
                contract_type: "ItemExchange".to_string(),
                status: "Outstanding".to_string(),
                title: "Private Rival Contract".to_string(),
                for_corp: false,
                availability: "Personal".to_string(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                date_issued_unix: now.saturating_sub(600),
                expiration_unix: now + 86_400,
                num_days: 1,
                price: IskCents::from_isk(100_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                buyout: IskCents(0),
                item_ids: vec![],
                requested_items: vec![],
                current_bid: IskCents(0),
                current_bidder_id: None,
                bid_history: vec![],
            },
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-contracts.read_character_contracts.v1",
            "esi-contracts.read_corporation_contracts.v1",
        ],
    );

    let char_contracts = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/contracts/"),
        Some(&token),
    );
    let char_contracts_arr = char_contracts.as_array().unwrap();
    assert_eq!(char_contracts_arr.len(), 1);
    assert_eq!(
        char_contracts_arr[0]["contract_id"],
        json!(public_contract_id)
    );
    assert_eq!(char_contracts_arr[0]["type"], json!("auction"));
    assert_eq!(char_contracts_arr[0]["buyout"], json!(5_000_000.0));

    let char_items = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/contracts/{public_contract_id}/items/"),
        Some(&token),
    );
    let char_items_arr = char_items.as_array().unwrap();
    assert_eq!(char_items_arr.len(), 2);
    assert!(char_items_arr
        .iter()
        .any(|i| i["type_id"] == json!(587) && i["is_included"] == json!(true)));
    assert!(char_items_arr
        .iter()
        .any(|i| i["type_id"] == json!(34) && i["is_included"] == json!(false)));

    assert_status(
        &router,
        format!("/v1/characters/{char_id}/contracts/{private_contract_id}/items/"),
        Some(&token),
        403,
    );

    let char_bids = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/contracts/{public_contract_id}/bids/"),
        Some(&token),
    );
    let char_bids_arr = char_bids.as_array().unwrap();
    assert_eq!(char_bids_arr.len(), 2);
    assert_eq!(char_bids_arr[0]["amount"], json!(2_500_000.0));
    assert_eq!(char_bids_arr[0]["bidder_id"], json!(90_000_002));

    let pub_contracts = get_json(&router, &spec, "/v1/contracts/public/10000002/", None);
    assert!(pub_contracts
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["contract_id"] == json!(public_contract_id)));

    let pub_items = get_json(
        &router,
        &spec,
        format!("/v1/contracts/public/items/{public_contract_id}/"),
        None,
    );
    assert_eq!(pub_items.as_array().unwrap().len(), 2);
    assert_status(
        &router,
        format!("/v1/contracts/public/items/{private_contract_id}/"),
        None,
        404,
    );

    let pub_bids = get_json(
        &router,
        &spec,
        format!("/v1/contracts/public/bids/{public_contract_id}/"),
        None,
    );
    assert_eq!(pub_bids.as_array().unwrap().len(), 2);
    assert_eq!(pub_bids[0]["amount"], json!(2_500_000.0));

    let corp_contracts = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp_id}/contracts/"),
        Some(&token),
    );
    assert!(corp_contracts
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["contract_id"] == json!(public_contract_id)));

    let corp_items = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp_id}/contracts/{public_contract_id}/items/"),
        Some(&token),
    );
    assert_eq!(corp_items.as_array().unwrap().len(), 2);

    let corp_bids = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp_id}/contracts/{public_contract_id}/bids/"),
        Some(&token),
    );
    assert_eq!(corp_bids.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn test_esi6_industry_and_mining_routes() {
    // parity: esi:GET /industry/facilities, esi:GET /industry/systems
    // parity: esi:GET /characters/{character_id}/industry/jobs, esi:GET /characters/{character_id}/mining
    // parity: esi:GET /corporations/{corporation_id}/industry/jobs, esi:GET /corporation/{corporation_id}/mining/extractions
    // parity: esi:GET /corporation/{corporation_id}/mining/observers, esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}
    // parity: feat:esi.industry.facilities, feat:esi.industry.systems
    // parity: feat:esi.industry.char-jobs, feat:esi.industry.char-mining
    // parity: feat:esi.industry.corp-jobs, feat:esi.industry.corp-extractions, feat:esi.industry.corp-observers
    eve_testkit::attest(&[
        "esi:GET /industry/facilities",
        "esi:GET /industry/systems",
        "esi:GET /characters/{character_id}/industry/jobs",
        "esi:GET /characters/{character_id}/mining",
        "esi:GET /corporations/{corporation_id}/industry/jobs",
        "esi:GET /corporation/{corporation_id}/mining/extractions",
        "esi:GET /corporation/{corporation_id}/mining/observers",
        "esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}",
        "feat:esi.industry.facilities",
        "feat:esi.industry.systems",
        "feat:esi.industry.char-jobs",
        "feat:esi.industry.char-mining",
        "feat:esi.industry.corp-jobs",
        "feat:esi.industry.corp-extractions",
        "feat:esi.industry.corp-observers",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_631_i64;
    let struct_id = 1_030_000_631_001_i64;
    let dodixie_stn = ctx.sde.solar_systems[&30_002_659].stations[0].station_id;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            make_corp(corp_id, "ESI6 Industrial Corp", "E6IC", char_id, None, 0.08),
        );
        snap.structures.insert(
            struct_id,
            make_structure(
                struct_id,
                35835,
                corp_id,
                30_000_144,
                "Perimeter Moon Refinery",
            ),
        );
        snap.industry_jobs.insert(
            6401,
            IndustryJobRecord {
                job_id: 6401,
                installer_id: char_id,
                facility_id: 60_003_760, // Jita 4-4 (30_000_142)
                blueprint_item_id: 100_000_007,
                blueprint_type_id: 691, // Rifter Blueprint
                product_type_id: 587,   // Rifter
                activity_id: 1,
                runs: 3,
                duration_seconds: 7_200,
                start_unix: now.saturating_sub(300),
                status: "Active".to_string(),
            },
        );
        snap.industry_jobs.insert(
            6402,
            IndustryJobRecord {
                job_id: 6402,
                installer_id: char_id,
                facility_id: struct_id, // Perimeter (30_000_144)
                blueprint_item_id: 100_000_008,
                blueprint_type_id: 691,
                product_type_id: 587,
                activity_id: 1,
                runs: 1,
                duration_seconds: 7_200,
                start_unix: now.saturating_sub(9_000),
                status: "Delivered".to_string(),
            },
        );
        snap.industry_jobs.insert(
            6403,
            IndustryJobRecord {
                job_id: 6403,
                installer_id: 99_999_999, // non-corp installer
                facility_id: 60_003_760, // Jita 4-4 (30_000_142)
                blueprint_item_id: 100_000_009,
                blueprint_type_id: 691,
                product_type_id: 587,
                activity_id: 1,
                runs: 10,
                duration_seconds: 7_200,
                start_unix: now.saturating_sub(35 * 86_400), // 35 days old: outside 28-day rolling window
                status: "Delivered".to_string(),
            },
        );
        snap.industry_jobs.insert(
            6404,
            IndustryJobRecord {
                job_id: 6404,
                installer_id: 99_999_999, // non-corp installer
                facility_id: dodixie_stn, // Dodixie (30_002_659): 57,600s = 80% of 72,000s universe work -> clamped to 0.50
                blueprint_item_id: 100_000_010,
                blueprint_type_id: 691,
                product_type_id: 587,
                activity_id: 1,
                runs: 1,
                duration_seconds: 57_600,
                start_unix: now.saturating_sub(600),
                status: "Active".to_string(),
            },
        );
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1228, // Scordite (within 30-day window)
            quantity: 6_400,
            date: "2025-12-16".to_string(),
            timestamp_unix: now,
        });
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1230, // Veldspar (45 days old — outside 30-day window)
            quantity: 1_500,
            date: "2025-11-01".to_string(),
            timestamp_unix: now.saturating_sub(45 * 86_400),
        });
        snap.moon_extractions.insert(
            struct_id,
            MoonExtractionRecord {
                structure_id: struct_id,
                moon_id: 40_009_082,
                extraction_start_unix: now.saturating_sub(3600),
                chunk_arrival_unix: now + 72_000,
                natural_decay_unix: now + 82_800,
                status: "Started".to_string(),
                auto_fracture: false,
                event_id: 1,
                moon_ores: vec![(45492, 12_000.0)],
                spawned_asteroid_ids: vec![],
            },
        );
        snap.observer_mining_ledger.insert(
            struct_id,
            vec![ObserverMiningRecord {
                observer_id: struct_id,
                character_id: char_id,
                recorded_corporation_id: corp_id,
                type_id: 45492,
                quantity: 9_250,
                last_updated_date: "2025-12-16".to_string(),
                timestamp_unix: now,
            }],
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-industry.read_character_jobs.v1",
            "esi-industry.read_character_mining.v1",
            "esi-industry.read_corporation_jobs.v1",
            "esi-industry.read_corporation_mining.v1",
        ],
    );

    let facilities = get_json(&router, &spec, "/v1/industry/facilities/", None);
    let fac_arr = facilities.as_array().unwrap();
    assert!(fac_arr.iter().any(|f| {
        f["facility_id"] == json!(struct_id)
            && f["owner_id"] == json!(corp_id)
            && f["solar_system_id"] == json!(30_000_144)
            && f["tax"] == json!(0.08)
    }));
    assert!(fac_arr
        .iter()
        .any(|f| f["facility_id"] == json!(60_003_760)
            && f["solar_system_id"] == json!(30_000_142)));

    let systems = get_json(&router, &spec, "/v1/industry/systems/", None);
    let sys_arr = systems.as_array().unwrap();
    assert_eq!(sys_arr.len(), ctx.sde.solar_systems.len(), "GET /industry/systems must cover all SDE systems");
    let sci_for = |sid: u32, act: &str| -> f64 {
        sys_arr.iter().find(|s| s["solar_system_id"] == json!(sid)).unwrap()["cost_indices"]
            .as_array().unwrap().iter().find(|ci| ci["activity"] == json!(act)).unwrap()["cost_index"].as_f64().unwrap()
    };
    assert!((sci_for(30_000_142, "manufacturing") - 0.10_f64.sqrt()).abs() < 1e-6, "Jita SCI = sqrt(7200/72000)");
    assert!((sci_for(30_000_144, "manufacturing") - 0.10_f64.sqrt()).abs() < 1e-6, "Perimeter SCI = sqrt(7200/72000)");
    assert!((sci_for(30_002_659, "manufacturing") - 0.50).abs() < 1e-6, "Dodixie SCI clamped at 0.50");
    assert!((sci_for(30_002_187, "manufacturing") - 0.01).abs() < 1e-6, "Idle Amarr SCI = 0.01 floor");

    // Verify exact parity with `map.GetIndustryCostModifier` RPC across active, clamped, and idle systems:
    let mut rpc_sess = eve_machonet::SessionState::new(9910);
    rpc_sess.user_id = Some(1);
    rpc_sess.character_id = Some(char_id);
    for sid in [30_000_142u32, 30_000_144, 30_002_659, 30_002_187] {
        let rep = ctx
            .dispatch_rpc(&mut rpc_sess, "map", None, "GetIndustryCostModifier", &[eve_proto::PyRep::Integer(i64::from(sid))], &HashMap::new(), None)
            .await
            .expect("map.GetIndustryCostModifier");
        for (rpc_k, esi_k) in [
            ("manufacturing", "manufacturing"),
            ("research_te", "researching_time_efficiency"),
            ("research_me", "researching_material_efficiency"),
            ("copying", "copying"),
            ("invention", "invention"),
            ("reaction", "reaction"),
        ] {
            let Some(eve_proto::PyRep::Real(rpc_v)) = rep.dict_get(rpc_k) else { panic!("missing {rpc_k} in {rep:?}") };
            assert!((sci_for(sid, esi_k) - rpc_v).abs() < 1e-9, "ESI/RPC SCI mismatch for sys {sid} {esi_k}");
        }
    }

    let char_jobs = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/industry/jobs/?include_completed=false"),
        Some(&token),
    );
    let char_jobs_arr = char_jobs.as_array().unwrap();
    assert_eq!(char_jobs_arr.len(), 1);
    assert_eq!(char_jobs_arr[0]["job_id"], json!(6401));
    assert_eq!(char_jobs_arr[0]["blueprint_type_id"], json!(691));
    assert_eq!(char_jobs_arr[0]["product_type_id"], json!(587));
    assert_eq!(char_jobs_arr[0]["runs"], json!(3));

    let char_mining = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/mining/"),
        Some(&token),
    );
    let char_mining_arr = char_mining.as_array().unwrap();
    assert!(char_mining_arr
        .iter()
        .any(|m| m["type_id"] == json!(1228) && m["quantity"] == json!(6_400)));
    assert!(
        !char_mining_arr.iter().any(|m| m["type_id"] == json!(1230)),
        "Mining ledger entries older than 30 days must be excluded"
    );

    let corp_jobs = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp_id}/industry/jobs/"),
        Some(&token),
    );
    assert_eq!(corp_jobs.as_array().unwrap().len(), 2);
    assert_eq!(corp_jobs[0]["job_id"], json!(6401));

    let extractions = get_json(
        &router,
        &spec,
        format!("/v1/corporation/{corp_id}/mining/extractions/"),
        Some(&token),
    );
    assert_eq!(extractions[0]["structure_id"], json!(struct_id));
    assert_eq!(extractions[0]["moon_id"], json!(40_009_082));

    let observers = get_json(
        &router,
        &spec,
        format!("/v1/corporation/{corp_id}/mining/observers/"),
        Some(&token),
    );
    assert!(observers
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["observer_id"] == json!(struct_id) && o["last_updated"] == json!("2025-12-16")));

    let obs_detail = get_json(
        &router,
        &spec,
        format!("/v1/corporation/{corp_id}/mining/observers/{struct_id}/"),
        Some(&token),
    );
    assert_eq!(obs_detail[0]["character_id"], json!(char_id));
    assert_eq!(obs_detail[0]["quantity"], json!(9_250));

    // IND-01 regression: cancelling an active job via `industryManager.CancelJob` must exclude its
    // `duration_seconds` from both `GET /v1/industry/systems/` and `map.GetIndustryCostModifier`.
    let amarr_stn = ctx.sde.solar_systems[&30_002_187].stations[0].station_id;
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.insert(
            6405,
            IndustryJobRecord {
                job_id: 6405, installer_id: char_id, facility_id: amarr_stn, blueprint_item_id: 100_000_011,
                blueprint_type_id: 691, product_type_id: 587, activity_id: 1, runs: 100,
                duration_seconds: 288_000, start_unix: now.saturating_sub(60), status: "Active".to_string(),
            },
        );
    });
    let sys_active = get_json(&router, &spec, "/v1/industry/systems/", None);
    let sci_from = |arr: &serde_json::Value, sid: u32| -> f64 {
        arr.as_array().unwrap().iter().find(|s| s["solar_system_id"] == json!(sid)).unwrap()["cost_indices"]
            .as_array().unwrap().iter().find(|ci| ci["activity"] == json!("manufacturing")).unwrap()["cost_index"].as_f64().unwrap()
    };
    assert!((sci_from(&sys_active, 30_002_187) - 0.50).abs() < 1e-6, "Active Amarr job raises SCI to 0.50");
    ctx.dispatch_rpc(&mut rpc_sess, "industryManager", None, "CancelJob", &[eve_proto::PyRep::Integer(6405)], &HashMap::new(), None)
        .await
        .expect("industryManager.CancelJob");
    let sys_cancelled = get_json(&router, &spec, "/v1/industry/systems/", None);
    assert!((sci_from(&sys_cancelled, 30_002_187) - 0.01).abs() < 1e-6, "Cancelled Amarr job excluded from local SCI");
    assert!((sci_from(&sys_cancelled, 30_000_142) - 0.10_f64.sqrt()).abs() < 1e-6, "Cancelled job does not dilute Jita SCI");
    let amarr_rpc = ctx
        .dispatch_rpc(&mut rpc_sess, "map", None, "GetIndustryCostModifier", &[eve_proto::PyRep::Integer(30_002_187)], &HashMap::new(), None)
        .await
        .expect("map.GetIndustryCostModifier Amarr after cancel");
    assert!((amarr_rpc.dict_get("manufacturing").and_then(|v| v.as_f64()).unwrap() - 0.01).abs() < 1e-9);
}

#[tokio::test]
async fn test_esi6_planetary_interaction_and_plan_closure() {
    // parity: esi:GET /characters/{character_id}/planets, esi:GET /characters/{character_id}/planets/{planet_id}
    // parity: esi:GET /corporations/{corporation_id}/customs_offices, esi:GET /universe/schematics/{schematic_id}
    // parity: feat:esi.planetary-interaction.colonies, feat:esi.planetary-interaction.layout
    // parity: feat:esi.planetary-interaction.customs, feat:esi.planetary-interaction.schematics
    // parity: plan:ESI-6
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/planets",
        "esi:GET /characters/{character_id}/planets/{planet_id}",
        "esi:GET /corporations/{corporation_id}/customs_offices",
        "esi:GET /universe/schematics/{schematic_id}",
        "feat:esi.planetary-interaction.colonies",
        "feat:esi.planetary-interaction.layout",
        "feat:esi.planetary-interaction.customs",
        "feat:esi.planetary-interaction.schematics",
        "plan:ESI-6",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let now = ctx.now_unix();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_641_i64;
    let planet_id = 40_009_080_i64; // Jita III (Planet (Temperate))

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            make_corp(corp_id, "ESI6 Planetary Corp", "E6PI", char_id, None, 0.0),
        );
        snap.planetary_colonies.insert(
            (char_id, planet_id),
            PlanetaryColonyRecord {
                owner_id: char_id,
                planet_id,
                solar_system_id: 30_000_142,
                command_center_level: 4,
                pins_count: 2,
                extracted_commodities: HashMap::from([(2393, 600)]),
                last_extracted_unix: now,
            },
        );
        snap.planetary_colony_layouts.insert(
            (char_id, planet_id),
            PlanetaryColonyLayoutRecord {
                pins: vec![
                    PlanetaryPinRecord {
                        pin_id: 400_090_801,
                        type_id: 2254,
                        latitude: 0.45,
                        longitude: 1.15,
                        schematic_id: None,
                        install_time_unix: None,
                        expiry_time_unix: None,
                        last_cycle_start_unix: None,
                        contents: vec![PlanetaryPinContentRecord {
                            type_id: 2393,
                            amount: 600,
                        }],
                    },
                    PlanetaryPinRecord {
                        pin_id: 400_090_802,
                        type_id: 2848,
                        latitude: 0.52,
                        longitude: 1.24,
                        schematic_id: Some(65),
                        install_time_unix: Some(now.saturating_sub(3600)),
                        expiry_time_unix: Some(now + 86_400),
                        last_cycle_start_unix: Some(now.saturating_sub(1800)),
                        contents: Vec::new(),
                    },
                ],
                links: vec![PlanetaryLinkRecord {
                    source_pin_id: 400_090_802,
                    destination_pin_id: 400_090_801,
                    link_level: 2,
                }],
                routes: vec![PlanetaryRouteRecord {
                    route_id: 7,
                    source_pin_id: 400_090_802,
                    destination_pin_id: 400_090_801,
                    content_type_id: 2393,
                    quantity: 20,
                    waypoints: vec![400_090_801],
                }],
            },
        );
        snap.corp_customs_offices.insert(
            corp_id,
            vec![CustomsOfficeRecord {
                office_id: 10_040_009_080,
                system_id: 30_000_142,
                reinforce_exit_start: 16,
                reinforce_exit_end: 18,
                allow_alliance_access: true,
                allow_access_with_standings: true,
                standing_level: Some("neutral".to_string()),
                corporation_tax_rate: Some(0.04),
                alliance_tax_rate: Some(0.06),
                excellent_standing_tax_rate: Some(0.05),
                good_standing_tax_rate: Some(0.08),
                neutral_standing_tax_rate: Some(0.10),
                bad_standing_tax_rate: Some(0.15),
                terrible_standing_tax_rate: Some(0.20),
            }],
        );
    });

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-planets.manage_planets.v1",
            "esi-planets.read_customs_offices.v1",
        ],
    );

    let planets = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/planets/"),
        Some(&token),
    );
    let planets_arr = planets.as_array().unwrap();
    assert_eq!(planets_arr.len(), 1);
    assert_eq!(planets_arr[0]["planet_id"], json!(planet_id));
    assert_eq!(planets_arr[0]["upgrade_level"], json!(4));
    assert_eq!(planets_arr[0]["num_pins"], json!(2));
    assert_eq!(planets_arr[0]["planet_type"], json!("temperate"));

    let layout = get_json(
        &router,
        &spec,
        format!("/v3/characters/{char_id}/planets/{planet_id}/"),
        Some(&token),
    );
    assert_eq!(layout["pins"].as_array().unwrap().len(), 2);
    assert_eq!(layout["pins"][0]["pin_id"], json!(400_090_801));
    assert_eq!(layout["pins"][0]["contents"][0]["type_id"], json!(2393));
    assert_eq!(layout["pins"][0]["contents"][0]["amount"], json!(600));
    assert_eq!(layout["pins"][1]["schematic_id"], json!(65));
    assert_eq!(layout["links"][0]["link_level"], json!(2));
    assert_eq!(layout["routes"][0]["route_id"], json!(7));
    assert_eq!(layout["routes"][0]["quantity"], json!(20.0));
    assert_status(
        &router,
        format!("/v3/characters/{char_id}/planets/99999999/"),
        Some(&token),
        404,
    );

    let pocos = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp_id}/customs_offices/"),
        Some(&token),
    );
    let pocos_arr = pocos.as_array().unwrap();
    assert_eq!(pocos_arr.len(), 1);
    assert_eq!(pocos_arr[0]["office_id"], json!(10_040_009_080_i64));
    assert_eq!(pocos_arr[0]["system_id"], json!(30_000_142));
    assert_eq!(pocos_arr[0]["corporation_tax_rate"], json!(0.04));
    assert_eq!(pocos_arr[0]["alliance_tax_rate"], json!(0.06));
    assert_status(
        &router,
        format!("/v1/corporations/{corp_id}/customs_offices/?page=2"),
        Some(&token),
        404,
    );

    // Schematic lookup from real SDE planet_schematics
    let (&schem_id, schem_rec) = ctx
        .sde
        .planet_schematics
        .iter()
        .next()
        .expect("SDE must contain planet schematics");
    let schem_json = get_json(
        &router,
        &spec,
        format!("/v1/universe/schematics/{schem_id}/"),
        None,
    );
    assert_eq!(schem_json["schematic_name"], json!(schem_rec.name));
    assert_eq!(schem_json["cycle_time"], json!(schem_rec.cycle_time as i64));
    assert_status(&router, "/v1/universe/schematics/999999999/", None, 404);
}
