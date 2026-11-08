//! `ESI-6` integration tests for Market routes against live `WorldRepository` and `SdeRegistry`
//! state populated via `marketProxy` RPCs, validated against `OpenApiSpec::pinned()`.

#![forbid(unsafe_code)]

use eve_db::{
    corp_roles::{
        ROLE_ACCOUNTANT, ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR, ROLE_FACTORY_MANAGER,
        ROLE_STATION_MANAGER, ROLE_TRADER,
    },
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
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
    let mut corp = { let mut __corp = CorporationRecord::default();
        __corp.corporation_id = corporation_id;
        __corp.name = name.to_string();
        __corp.ticker = ticker.to_string();
        __corp.ceo_id = ceo_id;
        __corp.alliance_id = alliance_id;
        __corp.tax_rate = tax_rate;
        __corp.members = HashSet::from([ceo_id]);
        __corp.member_roles = HashMap::from([(ceo_id, roles)]);
        __corp.set_division_balance(1, IskCents::from_isk(500_000_000));
        __corp };
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
    // parity: esi:GET /markets/{region_id}/orders
    // parity: esi:GET /markets/{region_id}/types
    // parity: feat:esi.market.groups, feat:esi.market.region-orders
    // parity: feat:esi.market.types
    eve_testkit::attest(&[
        "esi:GET /markets/groups",
        "esi:GET /markets/groups/{market_group_id}",
        "esi:GET /markets/{region_id}/orders",
        "esi:GET /markets/{region_id}/types",
        "feat:esi.market.groups",
        "feat:esi.market.region-orders",
        "feat:esi.market.types",
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
    assert!(
        !types_arr.contains(&json!(587)),
        "private Upwell structure orders must not leak into unauthenticated /markets/{{region_id}}/types/ (#524)"
    );
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
async fn test_esi6_industry_jobs_fields_and_completion() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let fmt = eve_server::esi::sample_routes::format_iso8601_utc;

    let char_id = 90_000_001_i64;
    let corp_id = 98_000_621_i64;
    let jita_station = 60_003_760_i64;
    let char_bpo_id = 925_101_i64;
    let corp_bpo_id = 925_102_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        snap.corporations
            .insert(corp_id, make_corp(corp_id, "ESI6 Job Fields Corp", "E6JF", char_id, None, 0.0));
        snap.items.insert(
            char_bpo_id,
            make_item(char_bpo_id, 688, char_id, jita_station, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            corp_bpo_id,
            make_item(corp_bpo_id, 688, corp_id, jita_station, InventoryFlag::CorpSAG1, 1, true),
        );
        for sid in [3406_u32, 11448, 11452, 21791] {
            snap.skills.entry(char_id).or_default().insert(
                sid,
                eve_db::TrainedSkillRecord { type_id: sid, level: 3, skill_points: 8_000.0 },
            );
        }
        for (idx, dc_tid) in [20424_u32, 20172, 20411, 20412, 20414, 20416, 25887].into_iter().enumerate() {
            snap.items.insert(
                925_200 + idx as i64,
                make_item(925_200 + idx as i64, dc_tid, char_id, jita_station, InventoryFlag::Hangar, 50, false),
            );
        }
    });

    let mut sess = SessionState::new(9820);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.corp_role = ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER;
    sess.station_id = Some(jita_station);
    sess.solar_system_id = Some(30_000_142);

    // 1. Personal BPC copy job (activityID = 5, runs = 3) -> CompleteJob
    let char_copy_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(char_bpo_id)),
        (PyRep::String("activityID".into()), PyRep::Integer(5)),
        (PyRep::String("runs".into()), PyRep::Integer(3)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("useCorp".into()), PyRep::Bool(false)),
    ]);
    let char_copy_rep =
        rpc_call(&ctx, &mut sess, "industryManager", "InstallJob", &[char_copy_dict]).await;
    let char_copy_job_id = char_copy_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let char_copy_cost = match char_copy_rep.dict_get("totalJobCostISK") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("missing totalJobCostISK: {other:?}"),
    };
    assert!(char_copy_cost > 0.0);
    let char_copy_dur = char_copy_rep.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap_or(100_000) as u64;
    ctx.db.clock().advance_secs(char_copy_dur + 1);
    let char_completed_ts = ctx.now_unix();
    let char_copy_done = rpc_call(
        &ctx,
        &mut sess,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(char_copy_job_id)],
    )
    .await;
    let char_bpc_id = char_copy_done
        .dict_get("createdBpcItemID")
        .and_then(|v| v.as_i64())
        .unwrap();

    // 2. Personal Invention job (activityID = 8, runs = 1) on the created BPC
    let inv_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(char_bpc_id)),
        (PyRep::String("activityID".into()), PyRep::Integer(8)),
        (PyRep::String("runs".into()), PyRep::Integer(1)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("useCorp".into()), PyRep::Bool(false)),
    ]);
    let inv_rep = rpc_call(&ctx, &mut sess, "industryManager", "InstallJob", &[inv_dict]).await;
    let inv_job_id = inv_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let inv_cost = match inv_rep.dict_get("totalJobCostISK") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("missing totalJobCostISK: {other:?}"),
    };
    let inv_prob = match inv_rep.dict_get("probability") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("missing probability: {other:?}"),
    };
    let inv_runs = inv_rep.dict_get("inventedRuns").and_then(|v| v.as_i64()).unwrap();
    assert!(inv_prob > 0.0);
    assert!(inv_runs > 0);

    // 3. Corporate BPC copy job (activityID = 5, runs = 4, useCorp = true) -> CompleteJob
    let corp_copy_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(corp_bpo_id)),
        (PyRep::String("activityID".into()), PyRep::Integer(5)),
        (PyRep::String("runs".into()), PyRep::Integer(4)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("useCorp".into()), PyRep::Bool(true)),
    ]);
    let corp_copy_rep =
        rpc_call(&ctx, &mut sess, "industryManager", "InstallJob", &[corp_copy_dict]).await;
    let corp_copy_job_id = corp_copy_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let corp_copy_cost = match corp_copy_rep.dict_get("totalJobCostISK") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("missing totalJobCostISK: {other:?}"),
    };
    let corp_copy_dur = corp_copy_rep.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap_or(100_000) as u64;
    ctx.db.clock().advance_secs(corp_copy_dur + 1);
    let corp_completed_ts = ctx.now_unix();
    rpc_call(
        &ctx,
        &mut sess,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(corp_copy_job_id)],
    )
    .await;

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-industry.read_character_jobs.v1",
            "esi-industry.read_corporation_jobs.v1",
        ],
    );

    let char_jobs = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/industry/jobs/?include_completed=true"),
        Some(&token),
    );
    let char_jobs_arr = char_jobs.as_array().unwrap();
    let completed_char_job = char_jobs_arr
        .iter()
        .find(|j| j["job_id"] == json!(char_copy_job_id))
        .expect("completed char copy job");
    assert_eq!(completed_char_job["blueprint_location_id"], json!(jita_station));
    assert_eq!(completed_char_job["output_location_id"], json!(jita_station));
    assert_eq!(completed_char_job["cost"], json!(char_copy_cost));
    assert_eq!(completed_char_job["licensed_runs"], json!(3));
    assert_eq!(completed_char_job["completed_date"], json!(fmt(char_completed_ts)));
    assert_eq!(completed_char_job["completed_character_id"], json!(char_id));

    let active_inv_job = char_jobs_arr
        .iter()
        .find(|j| j["job_id"] == json!(inv_job_id))
        .expect("active invention job");
    assert_eq!(active_inv_job["blueprint_location_id"], json!(jita_station));
    assert_eq!(active_inv_job["output_location_id"], json!(jita_station));
    assert_eq!(active_inv_job["cost"], json!(inv_cost));
    assert_eq!(active_inv_job["licensed_runs"], json!(inv_runs));
    assert!((active_inv_job["probability"].as_f64().unwrap() - inv_prob).abs() < 1e-5);

    let corp_jobs = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp_id}/industry/jobs/?include_completed=true"),
        Some(&token),
    );
    let corp_jobs_arr = corp_jobs.as_array().unwrap();
    assert_eq!(corp_jobs_arr.len(), 1);
    assert_eq!(corp_jobs_arr[0]["job_id"], json!(corp_copy_job_id));
    assert_eq!(corp_jobs_arr[0]["blueprint_location_id"], json!(jita_station));
    assert_eq!(corp_jobs_arr[0]["output_location_id"], json!(jita_station));
    assert_eq!(corp_jobs_arr[0]["cost"], json!(corp_copy_cost));
    assert_eq!(corp_jobs_arr[0]["licensed_runs"], json!(4));
    assert_eq!(corp_jobs_arr[0]["completed_date"], json!(fmt(corp_completed_ts)));
    assert_eq!(corp_jobs_arr[0]["completed_character_id"], json!(char_id));
}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_market_industry_pi_ext_2 {
    use super::*;

// Remediation integration tests for:
// - ESI Market Groups localized SDE descriptions (`esi:GET /markets/groups/{market_group_id}`, `feat:esi.market.groups`)
// - ESI Structure Market Orders Hub & ACL enforcement (`esi:GET /markets/structures/{structure_id}`)
// - ESI Corporation endpoint-specific OpenAPI role gates & route registration across all 6 corp endpoints


use crate::esi_market_industry_pi::{
    assert_status, get_json, issue_token_with_scopes, make_item, make_structure, rpc_call,
};
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, IskCents, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, StructureAccessProfile,
};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[tokio::test]
async fn test_esi_market_groups_sde_descriptions_and_structure_orders_acl_hub() {
    // parity: esi:GET /markets/groups/{market_group_id}, feat:esi.market.groups
    eve_testkit::attest(&[
        "esi:GET /markets/groups/{market_group_id}",
        "feat:esi.market.groups",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    // 1. Verify market group descriptions come from V24 marketgroups.fsdbinary + localization pickle (not name)
    for mg_id in [4u32, 9u32] {
        let expected_desc = ctx
            .sde
            .market_group_description(mg_id)
            .expect("SDE localized market group description");
        assert!(!expected_desc.is_empty());
        let detail = get_json(&router, &spec, format!("/v1/markets/groups/{mg_id}/"), None);
        assert_eq!(detail["description"], json!(expected_desc));
        assert_ne!(detail["description"], detail["name"]);
    }

    // 2. Verify structure market orders require online Standup Market Hub I (35892) AND docking/market ACL access
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_711_i64;
    let fortizar_hub_id = 1_000_000_711_001_i64;
    let bare_astrahus_id = 1_000_000_711_002_i64;
    let offline_hub_fortizar_id = 1_000_000_711_003_i64;
    let acl_blocked_fortizar_id = 1_000_000_711_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.structure_id = Some(fortizar_hub_id);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(100_000_000);
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Market Hub Test Corp".into();
                __corp.ticker = "MHTC".into();
                __corp.ceo_id = char_id;
                __corp.members = HashSet::from([char_id]);
                __corp },
        );

        // (a) Fortizar with online Standup Market Hub I (35892)
        snap.structures.insert(
            fortizar_hub_id,
            make_structure(fortizar_hub_id, 35833, corp_id, 30_000_142, "Hub Fortizar"),
        );
        snap.items.insert(
            950_001,
            make_item(950_001, 35_892, corp_id, fortizar_hub_id, InventoryFlag::ServiceSlot0, 1, true),
        );

        // (b) Bare Astrahus (35832) without Market Hub
        snap.structures.insert(
            bare_astrahus_id,
            make_structure(bare_astrahus_id, 35832, corp_id, 30_000_142, "Bare Astrahus"),
        );

        // (c) Fortizar with offline Standup Market Hub I
        snap.structures.insert(
            offline_hub_fortizar_id,
            make_structure(offline_hub_fortizar_id, 35833, corp_id, 30_000_142, "Offline Hub Fortizar"),
        );
        snap.items.insert(
            950_002,
            make_item(
                950_002,
                35_892,
                corp_id,
                offline_hub_fortizar_id,
                InventoryFlag::ServiceSlot0,
                1,
                true,
            ),
        );
        snap.module_online_states.insert(950_002, false);

        // (d) Fortizar with online Market Hub but docking ACL denied for caller
        snap.structures.insert(
            acl_blocked_fortizar_id,
            make_structure(acl_blocked_fortizar_id, 35833, corp_id, 30_000_142, "ACL Blocked Fortizar"),
        );
        snap.items.insert(
            950_003,
            make_item(
                950_003,
                35_892,
                corp_id,
                acl_blocked_fortizar_id,
                InventoryFlag::ServiceSlot0,
                1,
                true,
            ),
        );
        let mut ext = load_ext_state(snap);
        ext.access_profiles.insert(
            acl_blocked_fortizar_id,
            StructureAccessProfile {
                profile_id: 1,
                owner_corp_id: corp_id,
                blocked_characters: HashSet::from([char_id]),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);
    });

    // Place a real buy order at fortizar_hub_id via marketProxy.PlaceBuyOrder
    let mut sess = SessionState::new(9851);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.structure_id = Some(fortizar_hub_id);
    sess.solar_system_id = Some(30_000_142);
    sess.region_id = Some(10_000_002);
    let buy_args = [
        PyRep::Integer(fortizar_hub_id),
        PyRep::Integer(34),
        PyRep::Real(8.0),
        PyRep::Integer(1_000),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(30),
        PyRep::Bool(false),
    ];
    let buy_rep = rpc_call(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &buy_args).await;
    let placed_oid = buy_rep.dict_get("orderID").and_then(|v| v.as_i64()).expect("orderID");

    let token = issue_token_with_scopes(&ctx, char_id, vec!["esi-markets.structure_markets.v1"]);
    let ok_orders = get_json(
        &router,
        &spec,
        format!("/v1/markets/structures/{fortizar_hub_id}/"),
        Some(&token),
    );
    assert_eq!(ok_orders.as_array().unwrap().len(), 1);
    assert_eq!(ok_orders[0]["order_id"], json!(placed_oid));

    assert_status(&router, format!("/v1/markets/structures/{bare_astrahus_id}/"), Some(&token), 403);
    assert_status(
        &router,
        format!("/v1/markets/structures/{offline_hub_fortizar_id}/"),
        Some(&token),
        403,
    );
    assert_status(
        &router,
        format!("/v1/markets/structures/{acl_blocked_fortizar_id}/"),
        Some(&token),
        403,
    );
}

#[tokio::test]
async fn test_esi_corp_endpoint_specific_role_gates_and_routes() {
    // parity: esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/orders",
        "esi:GET /corporations/{corporation_id}/orders/history",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let corp_id = 98_000_722_i64;
    let ceo_id = 90_000_001_i64;
    let no_role_id = 90_000_721_i64;
    let sec_officer_id = 90_000_722_i64;
    let accountant_id = 90_000_723_i64;
    let jr_accountant_id = 90_000_724_i64;
    let trader_id = 90_000_725_i64;
    let station_mgr_id = 90_000_726_i64;
    let observer_id = 1_000_000_722_001_i64;
    let ship_id = 100_000_001_i64;
    let jita_station = 60_003_760_i64;
    let lowsec_sys = 30_000_192_u32;
    let lowsec_planet_id = ctx.sde.solar_systems[&lowsec_sys].planet_ids[0];
    let perim_moon = ctx
        .sde
        .celestials_in_system(30_000_144)
        .into_iter()
        .find(|c| c.group_id == 8)
        .expect("SDE moon in Perimeter");
    let perim_pos = [
        perim_moon.position.x + 2_000.0,
        perim_moon.position.y,
        perim_moon.position.z,
    ];

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters[&ceo_id].clone();
        for cid in [
            ceo_id,
            no_role_id,
            sec_officer_id,
            accountant_id,
            jr_accountant_id,
            trader_id,
            station_mgr_id,
        ] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = 1;
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_000_144;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(250_000_000);
            snap.characters.insert(cid, ch);
        }
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Role Gate Remediation Corp".into();
            __corp.ticker = "RGRC".into();
            __corp.ceo_id = ceo_id;
            __corp.members = HashSet::from([
                ceo_id,
                no_role_id,
                sec_officer_id,
                accountant_id,
                jr_accountant_id,
                trader_id,
                station_mgr_id,
            ]);
            __corp.member_roles = HashMap::from([
                (
                    ceo_id,
                    corp_roles::ROLE_DIRECTOR
                        | corp_roles::ROLE_STATION_MANAGER
                        | corp_roles::ROLE_ACCOUNTANT
                        | corp_roles::ROLE_TRADER,
                ),
                (no_role_id, 0),
                (sec_officer_id, corp_roles::ROLE_SECURITY_OFFICER),
                (accountant_id, corp_roles::ROLE_ACCOUNTANT),
                (jr_accountant_id, corp_roles::ROLE_JUNIOR_ACCOUNTANT),
                (trader_id, corp_roles::ROLE_TRADER),
                (station_mgr_id, corp_roles::ROLE_STATION_MANAGER),
            ]);
            __corp.set_division_balance(1, IskCents::from_isk(250_000_000));
            __corp };
        corp.ensure_wallet_divisions();
        for d in &mut corp.wallet_divisions {
            d.balance = IskCents::from_isk(250_000_000);
        }
        snap.corporations.insert(corp_id, corp);

        // Refinery with online Standup Moon Drill I (45009) + Gantry/POCO items in cargo
        snap.structures.insert(
            observer_id,
            UpwellStructureRecord {
                structure_id: observer_id,
                type_id: 35835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_144,
                name: "Athanor Moon Drill".into(),
                position: perim_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            951_001,
            make_item(951_001, 45_009, corp_id, observer_id, InventoryFlag::ServiceSlot0, 1, true),
        );
        snap.module_online_states.insert(951_001, true);
    });

    // 1. Populate moon extraction & observer mining ledger via live RPCs
    let mut ceo_sess = SessionState::new(9852);
    ceo_sess.user_id = Some(1);
    ceo_sess.character_id = Some(ceo_id);
    ceo_sess.corporation_id = Some(corp_id);
    ceo_sess.corp_role = corp_roles::ROLE_DIRECTOR
        | corp_roles::ROLE_STATION_MANAGER
        | corp_roles::ROLE_ACCOUNTANT
        | corp_roles::ROLE_TRADER;
    ceo_sess.solar_system_id = Some(30_000_144);
    ceo_sess.ship_id = Some(ship_id);

    let ext_args = [PyRep::Integer(observer_id), PyRep::Integer(518_400)];
    rpc_call(&ctx, &mut ceo_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;
    ctx.db.clock().advance_secs(520_000);
    let frac = rpc_call(
        &ctx,
        &mut ceo_sess,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(observer_id)],
    )
    .await;
    let ast_id = match frac.dict_get("asteroidIDs") {
        Some(PyRep::List(v)) => v[0].as_i64().expect("fractured asteroid ID"),
        other => panic!("unexpected asteroidIDs: {other:?}"),
    };
    let _ = ctx
        .systems
        .get_or_spawn(30_000_144)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 606,
            position: DVec3::from_array(perim_pos),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&ceo_id) {
            let mut ch = snap.characters.values().next().unwrap().clone();
            ch.character_id = ceo_id;
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_144;
            ch.active_ship_id = ship_id;
            snap.characters.insert(ceo_id, ch);
        }
        snap.skills.entry(ceo_id).or_default().insert(3386, eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 });
        snap.items.insert(ship_id, make_item(ship_id, 606, ceo_id, 30_000_144, InventoryFlag::None, 1, true));
        snap.items.insert(91_000_482, make_item(91_000_482, 483, ceo_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(91_000_482, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    rpc_call(&ctx, &mut ceo_sess, "dogmaIM", "Mine", &[PyRep::Integer(ast_id)]).await;
    rpc_call(&ctx, &mut ceo_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;

    // 2. Populate active & historical corporate orders via marketProxy.PlaceBuyOrder + CancelCharOrder
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
        }
    });
    ceo_sess.station_id = Some(jita_station);
    ceo_sess.solar_system_id = Some(30_000_142);
    ceo_sess.region_id = Some(10_000_002);
    let corp_buy_args = [
        PyRep::Integer(jita_station),
        PyRep::Integer(36),
        PyRep::Real(25.0),
        PyRep::Integer(500),
        PyRep::Integer(-1),
        PyRep::Integer(1),
        PyRep::Integer(30),
        PyRep::Bool(true),
        PyRep::Integer(1000),
    ];
    rpc_call(&ctx, &mut ceo_sess, "marketProxy", "PlaceBuyOrder", &corp_buy_args).await;
    let hist_rep = rpc_call(&ctx, &mut ceo_sess, "marketProxy", "PlaceBuyOrder", &corp_buy_args).await;
    let hist_oid = hist_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
    rpc_call(&ctx, &mut ceo_sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(hist_oid)])
        .await;

    // 3. Populate customs office via posMgr.AnchorOrbital + OnlineOrbital + planetOrbitalRegistryBroker.UpdateSettings
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = None;
            ch.solar_system_id = lowsec_sys;
            ch.standings.insert(500_026, 2.0);
        }
        snap.items
            .insert(951_002, make_item(951_002, 3962, corp_id, ship_id, InventoryFlag::Cargo, 1, false));
        snap.items
            .insert(951_003, make_item(951_003, 2233, corp_id, ship_id, InventoryFlag::Cargo, 1, false));
    });
    ceo_sess.station_id = None;
    ceo_sess.solar_system_id = Some(lowsec_sys);
    let lowsec_planet = ctx.sde.celestials_in_system(lowsec_sys).into_iter().find(|c| c.celestial_id == lowsec_planet_id).unwrap();
    let near_planet = lowsec_planet.position + glam::DVec3::new(lowsec_planet.radius + 5_000.0, 0.0, 0.0);
    ctx.systems.get_or_spawn(lowsec_sys).spawn_ball(eve_destiny::Ball {
        id: ship_id,
        type_id: 587,
        position: near_planet,
        velocity: glam::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }).await.unwrap();
    rpc_call(
        &ctx,
        &mut ceo_sess,
        "posMgr",
        "AnchorOrbital",
        &[PyRep::Integer(951_002), PyRep::Integer(lowsec_planet_id)],
    )
    .await;
    rpc_call(
        &ctx,
        &mut ceo_sess,
        "posMgr",
        "OnlineOrbital",
        &[PyRep::Integer(951_002), PyRep::Integer(951_003)],
    )
    .await;
    let poco_args = [
        PyRep::Integer(lowsec_planet_id),
        PyRep::Integer(18),
        PyRep::keyval([
            ("corporation", PyRep::Real(0.05)),
            ("alliance", PyRep::Real(0.08)),
            ("standingHigh", PyRep::Real(0.05)),
            ("standingGood", PyRep::Real(0.08)),
            ("standingNeutral", PyRep::Real(0.10)),
            ("standingBad", PyRep::Real(0.15)),
            ("standingHorrible", PyRep::Real(0.20)),
        ]),
        PyRep::Real(0.0),
        PyRep::Bool(true),
        PyRep::Bool(false),
    ];
    rpc_call(&ctx, &mut ceo_sess, "planetOrbitalRegistryBroker", "UpdateSettings", &poco_args)
        .await;

    let all_scopes = vec![
        "esi-markets.read_corporation_orders.v1",
        "esi-industry.read_corporation_mining.v1",
        "esi-planets.read_customs_offices.v1",
    ];
    let tok_ceo = issue_token_with_scopes(&ctx, ceo_id, all_scopes.clone());
    let tok_no_role = issue_token_with_scopes(&ctx, no_role_id, all_scopes.clone());
    let tok_sec = issue_token_with_scopes(&ctx, sec_officer_id, all_scopes.clone());
    let tok_acct = issue_token_with_scopes(&ctx, accountant_id, all_scopes.clone());
    let tok_jr_acct = issue_token_with_scopes(&ctx, jr_accountant_id, all_scopes.clone());
    let tok_trader = issue_token_with_scopes(&ctx, trader_id, all_scopes.clone());
    let tok_station_mgr = issue_token_with_scopes(&ctx, station_mgr_id, all_scopes);

    let orders_url = format!("/v1/corporations/{corp_id}/orders/");
    let orders_hist_url = format!("/v1/corporations/{corp_id}/orders/history/");
    let extractions_url = format!("/v1/corporation/{corp_id}/mining/extractions/");
    let observers_url = format!("/v1/corporation/{corp_id}/mining/observers/");
    let observer_detail_url = format!("/v1/corporation/{corp_id}/mining/observers/{observer_id}/");
    let customs_url = format!("/v1/corporations/{corp_id}/customs_offices/");

    // Negative: zero-role & Security_Officer members receive 403 on all 6 endpoints despite valid SSO scopes
    for url in [
        &orders_url,
        &orders_hist_url,
        &extractions_url,
        &observers_url,
        &observer_detail_url,
        &customs_url,
    ] {
        assert_status(&router, url, Some(&tok_no_role), 403);
        assert_status(&router, url, Some(&tok_sec), 403);
    }

    // Orders & Order History: OpenAPI x-required-roles = ["Accountant", "Trader"]
    for tok in [&tok_acct, &tok_trader] {
        let ord = get_json(&router, &spec, &orders_url, Some(tok));
        assert_eq!(ord.as_array().unwrap().len(), 1);
        let hist = get_json(&router, &spec, &orders_hist_url, Some(tok));
        assert_eq!(hist.as_array().unwrap().len(), 1);
    }
    for tok in [&tok_jr_acct, &tok_station_mgr] {
        assert_status(&router, &orders_url, Some(tok), 403);
        assert_status(&router, &orders_hist_url, Some(tok), 403);
    }

    // Mining Extractions: OpenAPI x-required-roles = ["Station_Manager"]
    let ext = get_json(&router, &spec, &extractions_url, Some(&tok_station_mgr));
    assert_eq!(ext.as_array().unwrap().len(), 1);
    for tok in [&tok_acct, &tok_trader, &tok_jr_acct] {
        assert_status(&router, &extractions_url, Some(tok), 403);
    }

    // Mining Observers & Observer Ledger: OpenAPI x-required-roles = ["Accountant"]
    let obs = get_json(&router, &spec, &observers_url, Some(&tok_acct));
    assert_eq!(obs.as_array().unwrap().len(), 1);
    let obs_det = get_json(&router, &spec, &observer_detail_url, Some(&tok_acct));
    assert_eq!(obs_det.as_array().unwrap().len(), 1);
    for tok in [&tok_station_mgr, &tok_trader, &tok_jr_acct] {
        assert_status(&router, &observers_url, Some(tok), 403);
        assert_status(&router, &observer_detail_url, Some(tok), 403);
    }

    // Customs Offices: OpenAPI x-required-roles = ["Director"]
    let pocos = get_json(&router, &spec, &customs_url, Some(&tok_ceo));
    assert_eq!(pocos.as_array().unwrap().len(), 1);
    for tok in [&tok_station_mgr, &tok_acct, &tok_trader, &tok_jr_acct] {
        assert_status(&router, &customs_url, Some(tok), 403);
    }
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_market_industry_pi_ext_3 {
    use super::*;

// `ESI-6` integration tests for Contracts and Planetary Interaction routes against live
// `WorldRepository` and `SdeRegistry` state populated via `contractProxy`, `planetMgr`, `posMgr`,
// and `planetOrbitalRegistryBroker` RPCs, validated against `OpenApiSpec::pinned()`.


use crate::esi_market_industry_pi::{
    assert_status, get_json, issue_token_with_scopes, make_corp, make_item, rpc_call,
};
use eve_db::{
    corp_roles::{ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR, ROLE_STATION_MANAGER},
    InventoryFlag, IskCents, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn test_esi6_contracts_character_corporation_and_public_routes() {
    // parity: esi:GET /characters/{character_id}/contracts, esi:GET /characters/{character_id}/contracts/{contract_id}/items
    // parity: esi:GET /characters/{character_id}/contracts/{contract_id}/bids, esi:GET /contracts/public/{region_id}
    // parity: esi:GET /contracts/public/items/{contract_id}, esi:GET /contracts/public/bids/{contract_id}
    // parity: esi:GET /corporations/{corporation_id}/contracts, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items
    // parity: esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids
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
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let char_id = 90_000_001_i64;
    let rival_char_id = 90_000_002_i64;
    let corp_id = 98_000_621_i64;
    let rival_corp_id = 98_000_699_i64;
    let jita_station = 60_003_760_i64;
    let escrow_item_id = 880_621_i64;
    let private_item_id = 880_622_i64;
    let personal_item_id = 880_623_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            ch.balance = IskCents::from_isk(250_000_000);
        }
        if let Some(ch2) = snap.characters.get_mut(&rival_char_id) {
            ch2.corporation_id = rival_corp_id;
            ch2.station_id = Some(jita_station);
            ch2.solar_system_id = 30_000_142;
            ch2.balance = IskCents::from_isk(250_000_000);
        }
        snap.corporations.insert(
            corp_id,
            make_corp(corp_id, "ESI6 Contracts Corp", "E6CT", char_id, None, 0.0),
        );
        snap.corporations.insert(
            rival_corp_id,
            make_corp(rival_corp_id, "Rival Contracts Corp", "RVCT", rival_char_id, None, 0.0),
        );
        snap.items.insert(
            escrow_item_id,
            make_item(escrow_item_id, 587, corp_id, jita_station, InventoryFlag::CorpSAG1, 1, true),
        );
        snap.items.insert(
            private_item_id,
            make_item(private_item_id, 587, rival_char_id, jita_station, InventoryFlag::Hangar, 1, true),
        );
        snap.items.insert(
            personal_item_id,
            make_item(personal_item_id, 587, char_id, jita_station, InventoryFlag::Hangar, 1, true),
        );
    });

    // 1. Create public corporation Auction contract + personal character Auction contract via contractProxy.CreateContract
    let mut issuer_sess = SessionState::new(9821);
    issuer_sess.user_id = Some(1);
    issuer_sess.character_id = Some(char_id);
    issuer_sess.corporation_id = Some(corp_id);
    issuer_sess.corp_role = ROLE_DIRECTOR | ROLE_CONTRACT_MANAGER;
    issuer_sess.station_id = Some(jita_station);
    issuer_sess.solar_system_id = Some(30_000_142);

    let pub_args = [
        PyRep::Integer(2), // Auction
        PyRep::Integer(0), // Public
        PyRep::Integer(0), // assigneeID
        PyRep::Integer(1440),
        PyRep::Integer(7),
        PyRep::Integer(jita_station),
        PyRep::Integer(jita_station),
        PyRep::Real(1_500_000.0),
        PyRep::Real(0.0),
        PyRep::Real(5_000_000.0), // buyout
        PyRep::String("Jita Rifter Auction".into()),
        PyRep::String(String::new()),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(escrow_item_id), PyRep::Integer(1)])]),
        PyRep::Integer(4),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(1_000)])]),
        PyRep::Bool(true), // forCorp
    ];
    let pub_rep = rpc_call(&ctx, &mut issuer_sess, "contractProxy", "CreateContract", &pub_args).await;
    let public_contract_id = pub_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    let personal_args = [
        PyRep::Integer(2), // Auction
        PyRep::Integer(0), // Public
        PyRep::Integer(0),
        PyRep::Integer(1440),
        PyRep::Integer(7),
        PyRep::Integer(jita_station),
        PyRep::Integer(jita_station),
        PyRep::Real(1_500_000.0),
        PyRep::Real(0.0),
        PyRep::Real(5_000_000.0),
        PyRep::String("Personal Rifter Auction".into()),
        PyRep::String(String::new()),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(personal_item_id), PyRep::Integer(1)])]),
        PyRep::Integer(4),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(1_000)])]),
        PyRep::Bool(false), // personal
    ];
    let personal_rep = rpc_call(&ctx, &mut issuer_sess, "contractProxy", "CreateContract", &personal_args).await;
    let personal_contract_id = personal_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    // 2. Place two bids from rival_char_id via contractProxy.PlaceBid and create a private personal contract
    let mut rival_sess = SessionState::new(9822);
    rival_sess.user_id = Some(1);
    rival_sess.character_id = Some(rival_char_id);
    rival_sess.corporation_id = Some(rival_corp_id);
    rival_sess.station_id = Some(jita_station);
    rival_sess.solar_system_id = Some(30_000_142);

    let bid1 = [PyRep::Integer(public_contract_id), PyRep::Real(2_000_000.0)];
    rpc_call(&ctx, &mut rival_sess, "contractProxy", "PlaceBid", &bid1).await;
    let p_bid1 = [PyRep::Integer(personal_contract_id), PyRep::Real(2_000_000.0)];
    rpc_call(&ctx, &mut rival_sess, "contractProxy", "PlaceBid", &p_bid1).await;
    ctx.db.clock().advance_secs(600);
    let bid2 = [PyRep::Integer(public_contract_id), PyRep::Real(2_500_000.0)];
    rpc_call(&ctx, &mut rival_sess, "contractProxy", "PlaceBid", &bid2).await;
    let p_bid2 = [PyRep::Integer(personal_contract_id), PyRep::Real(2_500_000.0)];
    rpc_call(&ctx, &mut rival_sess, "contractProxy", "PlaceBid", &p_bid2).await;

    let priv_args = [
        PyRep::Integer(1), // ItemExchange
        PyRep::Integer(1), // Personal
        PyRep::Integer(rival_char_id),
        PyRep::Integer(1440),
        PyRep::Integer(1),
        PyRep::Integer(jita_station),
        PyRep::Integer(jita_station),
        PyRep::Real(100_000.0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
        PyRep::String("Private Rival Contract".into()),
        PyRep::String(String::new()),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(private_item_id), PyRep::Integer(1)])]),
        PyRep::Integer(4),
        PyRep::List(vec![]),
        PyRep::Bool(false),
    ];
    let priv_rep = rpc_call(&ctx, &mut rival_sess, "contractProxy", "CreateContract", &priv_args).await;
    let private_contract_id = priv_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-contracts.read_character_contracts.v1",
            "esi-contracts.read_corporation_contracts.v1",
        ],
    );

    let char_contracts =
        get_json(&router, &spec, format!("/v1/characters/{char_id}/contracts/"), Some(&token));
    let char_contracts_arr = char_contracts.as_array().unwrap();
    assert_eq!(char_contracts_arr.len(), 1);
    assert_eq!(char_contracts_arr[0]["contract_id"], json!(personal_contract_id));
    assert_eq!(char_contracts_arr[0]["type"], json!("auction"));
    assert_eq!(char_contracts_arr[0]["buyout"], json!(5_000_000.0));

    let char_items = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/contracts/{personal_contract_id}/items/"),
        Some(&token),
    );
    let char_items_arr = char_items.as_array().unwrap();
    assert_eq!(char_items_arr.len(), 2);
    assert!(char_items_arr.iter().any(|i| i["type_id"] == json!(587) && i["is_included"] == json!(true)));
    assert!(char_items_arr.iter().any(|i| i["type_id"] == json!(34) && i["is_included"] == json!(false)));

    assert_status(
        &router,
        format!("/v1/characters/{char_id}/contracts/{public_contract_id}/items/"),
        Some(&token),
        403,
    );
    assert_status(
        &router,
        format!("/v1/characters/{char_id}/contracts/{private_contract_id}/items/"),
        Some(&token),
        403,
    );

    let char_bids = get_json(
        &router,
        &spec,
        format!("/v1/characters/{char_id}/contracts/{personal_contract_id}/bids/"),
        Some(&token),
    );
    let char_bids_arr = char_bids.as_array().unwrap();
    assert_eq!(char_bids_arr.len(), 2);
    assert_eq!(char_bids_arr[0]["amount"], json!(2_500_000.0));
    assert_eq!(char_bids_arr[0]["bidder_id"], json!(rival_char_id));

    let pub_contracts = get_json(&router, &spec, "/v1/contracts/public/10000002/", None);
    assert!(pub_contracts
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["contract_id"] == json!(public_contract_id)));

    let pub_items =
        get_json(&router, &spec, format!("/v1/contracts/public/items/{public_contract_id}/"), None);
    assert_eq!(pub_items.as_array().unwrap().len(), 2);
    assert_status(&router, format!("/v1/contracts/public/items/{private_contract_id}/"), None, 404);

    let pub_bids =
        get_json(&router, &spec, format!("/v1/contracts/public/bids/{public_contract_id}/"), None);
    assert_eq!(pub_bids.as_array().unwrap().len(), 2);
    assert_eq!(pub_bids[0]["amount"], json!(2_500_000.0));

    let corp_contracts =
        get_json(&router, &spec, format!("/v1/corporations/{corp_id}/contracts/"), Some(&token));
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
async fn test_esi_planetary_interaction_schematics_and_colonies() {
    // parity: esi:GET /universe/schematics/{schematic_id}
    // parity: feat:esi.planetary-interaction.schematics
    // parity: plan:ESI-6
    eve_testkit::attest(&[
        "esi:GET /universe/schematics/{schematic_id}",
        "feat:esi.planetary-interaction.schematics",
        "plan:ESI-6",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_641_i64;
    let ship_id = 100_000_001_i64;
    let planet_id = 40_009_080_i64; // Jita III (Planet (Temperate))
    let lowsec_sys = 30_000_192_u32;
    let lowsec_planet_id = ctx.sde.solar_systems[&lowsec_sys].planet_ids[0];
    let cc_item_id = 890_001_i64;
    let gantry_item_id = 890_010_i64;
    let poco_upgrade_item_id = 890_011_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = None;
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(250_000_000);
        }
        snap.corporations.insert(
            corp_id,
            make_corp(corp_id, "ESI6 Planetary Corp", "E6PI", char_id, None, 0.0),
        );
        let sk = snap.skills.entry(char_id).or_default();
        for tid in [2495u32, 2505u32] {
            sk.insert(
                tid,
                TrainedSkillRecord {
                    type_id: tid,
                    level: 4,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.items.insert(
            cc_item_id,
            make_item(cc_item_id, 2254, char_id, ship_id, InventoryFlag::Cargo, 1, false),
        );
        snap.items.insert(
            gantry_item_id,
            make_item(gantry_item_id, 3962, corp_id, ship_id, InventoryFlag::Cargo, 1, false),
        );
        snap.items.insert(
            poco_upgrade_item_id,
            make_item(poco_upgrade_item_id, 2233, corp_id, ship_id, InventoryFlag::Cargo, 1, false),
        );
    });

    // 1. Deploy Temperate Command Center, upgrade to level 4, place ECU + Factory, install schematic & extractor program,
    //    link pins, and route extracted Aqueous Liquids (2268) into the Command Center via planetMgr.UserUpdateNetwork.
    let mut pi_sess = SessionState::new(9830);
    pi_sess.user_id = Some(1);
    pi_sess.character_id = Some(char_id);
    pi_sess.corporation_id = Some(corp_id);
    pi_sess.corp_role = ROLE_DIRECTOR | ROLE_STATION_MANAGER;
    pi_sess.solar_system_id = Some(30_000_142);
    pi_sess.ship_id = Some(ship_id);

    let net_cmds = PyRep::List(vec![
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(101),
            PyRep::Integer(2254),
            PyRep::Real(0.45),
            PyRep::Real(1.15),
        ]),
        PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(4)]),
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(102),
            PyRep::Integer(3068),
            PyRep::Real(0.50),
            PyRep::Real(1.20),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(103),
            PyRep::Integer(2469),
            PyRep::Real(0.52),
            PyRep::Real(1.24),
        ]),
        PyRep::Tuple(vec![PyRep::Integer(8), PyRep::Integer(103), PyRep::Integer(121)]),
        PyRep::Tuple(vec![
            PyRep::Integer(10),
            PyRep::Integer(102),
            PyRep::Integer(2268),
            PyRep::Integer(300),
            PyRep::Integer(1),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(3),
            PyRep::Integer(102),
            PyRep::Integer(101),
            PyRep::Integer(2),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(6),
            PyRep::Integer(7),
            PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(101)]),
            PyRep::Integer(2268),
            PyRep::Integer(20),
        ]),
    ]);
    rpc_call(
        &ctx,
        &mut pi_sess,
        "planetMgr",
        "UserUpdateNetwork",
        &[PyRep::Integer(planet_id), net_cmds],
    )
    .await;

    // Advance simulated clock by 600s and call planetMgr.GetColony so extraction cycles populate pin contents
    ctx.db.clock().advance_secs(600);
    rpc_call(&ctx, &mut pi_sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_id)]).await;

    // 2. Anchor Customs Office Gantry in lowsec 30_000_192, upgrade to POCO, and configure tax rates via RPCs
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = lowsec_sys;
            ch.standings.insert(500_026, 2.0);
        }
    });
    pi_sess.solar_system_id = Some(lowsec_sys);
    let lowsec_planet = ctx.sde.celestials_in_system(lowsec_sys).into_iter().find(|c| c.celestial_id == lowsec_planet_id).unwrap();
    let near_planet = lowsec_planet.position + glam::DVec3::new(lowsec_planet.radius + 5_000.0, 0.0, 0.0);
    ctx.systems.get_or_spawn(lowsec_sys).spawn_ball(eve_destiny::Ball {
        id: ship_id,
        type_id: 587,
        position: near_planet,
        velocity: glam::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }).await.unwrap();
    rpc_call(
        &ctx,
        &mut pi_sess,
        "posMgr",
        "AnchorOrbital",
        &[PyRep::Integer(gantry_item_id), PyRep::Integer(lowsec_planet_id)],
    )
    .await;
    rpc_call(
        &ctx,
        &mut pi_sess,
        "posMgr",
        "OnlineOrbital",
        &[PyRep::Integer(gantry_item_id), PyRep::Integer(poco_upgrade_item_id)],
    )
    .await;
    let poco_args = [
        PyRep::Integer(lowsec_planet_id),
        PyRep::Integer(18),
        PyRep::keyval([
            ("corporation", PyRep::Real(0.04)),
            ("alliance", PyRep::Real(0.06)),
            ("standingHigh", PyRep::Real(0.05)),
            ("standingGood", PyRep::Real(0.08)),
            ("standingNeutral", PyRep::Real(0.10)),
            ("standingBad", PyRep::Real(0.15)),
            ("standingHorrible", PyRep::Real(0.20)),
        ]),
        PyRep::Real(0.0),
        PyRep::Bool(true),
        PyRep::Bool(true),
    ];
    rpc_call(&ctx, &mut pi_sess, "planetOrbitalRegistryBroker", "UpdateSettings", &poco_args)
        .await;

    let token = issue_token_with_scopes(
        &ctx,
        char_id,
        vec![
            "esi-planets.manage_planets.v1",
            "esi-planets.read_customs_offices.v1",
        ],
    );

    let planets =
        get_json(&router, &spec, format!("/v1/characters/{char_id}/planets/"), Some(&token));
    let planets_arr = planets.as_array().unwrap();
    assert_eq!(planets_arr.len(), 1);
    assert_eq!(planets_arr[0]["planet_id"], json!(planet_id));
    assert_eq!(planets_arr[0]["upgrade_level"], json!(4));
    assert_eq!(planets_arr[0]["num_pins"], json!(3));
    assert_eq!(planets_arr[0]["planet_type"], json!("temperate"));

    let layout = get_json(
        &router,
        &spec,
        format!("/v3/characters/{char_id}/planets/{planet_id}/"),
        Some(&token),
    );
    let pins_arr = layout["pins"].as_array().unwrap();
    assert_eq!(pins_arr.len(), 3);
    let cc_pin = pins_arr.iter().find(|p| p["type_id"] == json!(2254)).expect("CC pin");
    assert_eq!(cc_pin["contents"][0]["type_id"], json!(2268));
    assert!(cc_pin["contents"][0]["amount"].as_i64().unwrap() > 0);
    let fac_pin = pins_arr.iter().find(|p| p["type_id"] == json!(2469)).expect("Factory pin");
    assert_eq!(fac_pin["schematic_id"], json!(121));
    assert_eq!(layout["links"][0]["link_level"], json!(2));
    assert_eq!(layout["routes"][0]["route_id"], json!(7));
    assert_eq!(layout["routes"][0]["quantity"], json!(20.0));
    assert_status(&router, format!("/v3/characters/{char_id}/planets/99999999/"), Some(&token), 404);

    let pocos = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp_id}/customs_offices/"),
        Some(&token),
    );
    let pocos_arr = pocos.as_array().unwrap();
    assert_eq!(pocos_arr.len(), 1);
    assert_eq!(pocos_arr[0]["office_id"], json!(gantry_item_id));
    assert_eq!(pocos_arr[0]["system_id"], json!(lowsec_sys));
    assert_eq!(pocos_arr[0]["corporation_tax_rate"], json!(0.04));
    assert_eq!(pocos_arr[0]["alliance_tax_rate"], json!(0.06));
    assert_status(
        &router,
        format!("/v1/corporations/{corp_id}/customs_offices/?page=2"),
        Some(&token),
        404,
    );

    // Schematic lookup from real SDE planet_schematics
    let (&schem_id, schem_rec) = ctx.sde.planet_schematics.iter().next().expect("SDE schematics");
    let schem_json = get_json(&router, &spec, format!("/v1/universe/schematics/{schem_id}/"), None);
    assert_eq!(schem_json["schematic_name"], json!(schem_rec.name));
    assert_eq!(schem_json["cycle_time"], json!(schem_rec.cycle_time as i64));
    assert_status(&router, "/v1/universe/schematics/999999999/", None, 404);
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_market_industry_pi_ext_4 {
    use super::*;

// `ESI-6` integration tests for Industry & Mining routes against live `WorldRepository` and
// `SdeRegistry` state populated via `industryManager`, `moonExtractions`, and `dogmaIM` RPCs,
// validated against `OpenApiSpec::pinned()`.


use crate::esi_market_industry_pi::{
    get_json, issue_token_with_scopes, make_corp, make_item, rpc_call,
};
use eve_db::{
    corp_roles::{ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER},
    InventoryFlag, IskCents, TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use glam::DVec3;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn test_esi6_industry_and_mining_routes() {
    // parity: esi:GET /industry/systems
    // parity: feat:esi.industry.systems
    eve_testkit::attest(&[
        "esi:GET /industry/systems",
        "feat:esi.industry.systems",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let char_id = 90_000_001_i64;
    let other_char_id = 90_000_002_i64;
    let corp_id = 98_000_631_i64;
    let other_corp_id = 98_000_632_i64;
    let ship_id = 100_000_001_i64;
    let struct_id = 1_030_000_631_001_i64;
    let jita_stn = 60_003_760_i64;
    let dodixie_stn = ctx.sde.solar_systems[&30_002_659].stations[0].station_id;
    let amarr_stn = ctx.sde.solar_systems[&30_002_187].stations[0].station_id;
    let perim_moon = ctx
        .sde
        .celestials_in_system(30_000_144)
        .into_iter()
        .find(|c| c.group_id == 8)
        .expect("SDE moon in Perimeter");
    let perim_pos = [
        perim_moon.position.x + 2_000.0,
        perim_moon.position.y,
        perim_moon.position.z,
    ];
    let bp_mats: Vec<(u32, i32)> = ctx.sde.blueprints[&691]
        .materials
        .iter()
        .map(|m| (m.type_id, (m.quantity as i32) * 500))
        .collect();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_144;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(500_000_000);
        }
        if let Some(ch2) = snap.characters.get_mut(&other_char_id) {
            ch2.corporation_id = other_corp_id;
            ch2.solar_system_id = 30_000_142;
            ch2.balance = IskCents::from_isk(500_000_000);
        }
        snap.corporations.insert(
            corp_id,
            make_corp(corp_id, "ESI6 Industrial Corp", "E6IC", char_id, None, 0.08),
        );
        snap.corporations.insert(
            other_corp_id,
            make_corp(other_corp_id, "Other Industrial Corp", "OTHC", other_char_id, None, 0.05),
        );
        for cid in [char_id, other_char_id] {
            let sk = snap.skills.entry(cid).or_default();
            for &(sid, lvl) in &[(3380, 1), (3387, 5), (24625, 5), (24268, 5)] {
                sk.insert(
                    sid,
                    TrainedSkillRecord {
                        type_id: sid,
                        level: lvl,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35835,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_144,
                name: "Perimeter Moon Refinery".into(),
                position: perim_pos,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 10000.0,
                armor_hp: 10000.0,
                hull_hp: 10000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        for (iid, tid, flag) in [
            (940_001, 35_878, InventoryFlag::ServiceSlot0),
            (940_002, 45_009, InventoryFlag::ServiceSlot1),
        ] {
            snap.items
                .insert(iid, make_item(iid, tid, corp_id, struct_id, flag, 1, true));
            snap.module_online_states.insert(iid, true);
        }
        eve_server::services::industry_pi::industry_facilities_bps::save_facility_taxes(
            snap,
            struct_id,
            &std::collections::HashMap::from([(1_u8, 0.08)]),
        );
        let mut ext =
            eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.access_profiles.insert(
            struct_id,
            eve_server::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                corp_only: false,
                ..Default::default()
            },
        );
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        let mut next_iid = 941_000_i64;
        for (owner, loc) in [
            (other_char_id, jita_stn),
            (char_id, struct_id),
            (char_id, jita_stn),
            (other_char_id, dodixie_stn),
            (char_id, amarr_stn),
        ] {
            next_iid += 1;
            snap.items.insert(
                next_iid,
                make_item(next_iid, 691, owner, loc, InventoryFlag::Hangar, 1, true),
            );
            for &(mat_tid, mat_qty) in &bp_mats {
                next_iid += 1;
                snap.items.insert(
                    next_iid,
                    make_item(next_iid, mat_tid, owner, loc, InventoryFlag::Hangar, mat_qty, false),
                );
            }
        }
    });

    // 1. Start moon extraction #1 on struct_id, advance 6d, fracture chunk #1, and mine the fractured moon asteroid via dogmaIM.Mine
    let mut char_sess = SessionState::new(9841);
    char_sess.user_id = Some(1);
    char_sess.character_id = Some(char_id);
    char_sess.corporation_id = Some(corp_id);
    char_sess.corp_role =
        ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER | ROLE_ACCOUNTANT;
    char_sess.solar_system_id = Some(30_000_144);
    char_sess.ship_id = Some(ship_id);

    let ext_args = [PyRep::Integer(struct_id), PyRep::Integer(518_400)];
    rpc_call(&ctx, &mut char_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;
    ctx.db.clock().advance_secs(520_000);
    let frac1 = rpc_call(
        &ctx,
        &mut char_sess,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(struct_id)],
    )
    .await;
    let ast_id_1 = match frac1.dict_get("asteroidIDs") {
        Some(PyRep::List(v)) => v[0].as_i64().expect("first fractured asteroid ID"),
        other => panic!("unexpected asteroidIDs: {other:?}"),
    };
    let _ = ctx
        .systems
        .get_or_spawn(30_000_144)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 606,
            position: DVec3::from_array(perim_pos),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            let mut ch = snap.characters.values().next().unwrap().clone();
            ch.character_id = char_id;
            ch.corporation_id = corp_id;
            ch.solar_system_id = 30_000_144;
            ch.active_ship_id = ship_id;
            snap.characters.insert(char_id, ch);
        }
        snap.skills.entry(char_id).or_default().insert(3386, eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 });
        snap.items.insert(ship_id, make_item(ship_id, 606, char_id, 30_000_144, InventoryFlag::None, 1, true));
        snap.items.insert(93_000_483, make_item(93_000_483, 483, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(93_000_483, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    rpc_call(&ctx, &mut char_sess, "dogmaIM", "Mine", &[PyRep::Integer(ast_id_1)]).await;

    // 2. Install & deliver an old industry job in Jita via other_char_id, then advance 35 days so both the first mining ledger entry (>30d) and first Jita job (>28d) age out
    let mut other_sess = SessionState::new(9842);
    other_sess.user_id = Some(1);
    other_sess.character_id = Some(other_char_id);
    other_sess.corporation_id = Some(other_corp_id);
    other_sess.station_id = Some(jita_stn);
    other_sess.solar_system_id = Some(30_000_142);

    let old_job_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(10)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_stn)),
    ]);
    let old_job_rep =
        rpc_call(&ctx, &mut other_sess, "industryManager", "InstallJob", &[old_job_dict]).await;
    let old_job_id = old_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.clock().advance_secs(100_000);
    rpc_call(&ctx, &mut other_sess, "industryManager", "CompleteJob", &[PyRep::Integer(old_job_id)])
        .await;
    ctx.db.clock().advance_secs(35 * 86_400);

    // 3. Start moon extraction #2, fracture chunk #2, mine a fresh moon asteroid via dogmaIM.Mine, and start active moon extraction #3
    rpc_call(&ctx, &mut char_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;
    ctx.db.clock().advance_secs(520_000);
    let frac2 = rpc_call(
        &ctx,
        &mut char_sess,
        "moonExtractions",
        "FractureChunkWithStructure",
        &[PyRep::Integer(struct_id)],
    )
    .await;
    let ast_id_2 = match frac2.dict_get("asteroidIDs") {
        Some(PyRep::List(v)) => v.get(1).or_else(|| v.first()).and_then(|x| x.as_i64()).unwrap(),
        other => panic!("unexpected asteroidIDs: {other:?}"),
    };
    let fresh_ore_tid = ctx.db.get_asteroid(ast_id_2).unwrap().type_id;
    rpc_call(&ctx, &mut char_sess, "dogmaIM", "Mine", &[PyRep::Integer(ast_id_2)]).await;
    rpc_call(&ctx, &mut char_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;

    // 4. Install & deliver Perimeter corp job (runs = 1), install active Jita corp job (runs = 1), and install active Dodixie job (runs = 8)
    char_sess.structure_id = Some(struct_id);
    char_sess.station_id = None;
    let perim_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(1)),
        (PyRep::String("facilityID".into()), PyRep::Integer(struct_id)),
        (PyRep::String("useCorp".into()), PyRep::Bool(true)),
    ]);
    let perim_job_rep =
        rpc_call(&ctx, &mut char_sess, "industryManager", "InstallJob", &[perim_dict]).await;
    let perim_job_id = perim_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let perim_tax_isk = match perim_job_rep.dict_get("facilityTaxISK") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("missing facilityTaxISK in perim_job_rep: {other:?}"),
    };
    ctx.db.clock().advance_secs(10_000);
    rpc_call(&ctx, &mut char_sess, "industryManager", "CompleteJob", &[PyRep::Integer(perim_job_id)])
        .await;

    char_sess.structure_id = None;
    char_sess.station_id = Some(jita_stn);
    char_sess.solar_system_id = Some(30_000_142);
    let jita_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(1)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_stn)),
        (PyRep::String("useCorp".into()), PyRep::Bool(false)),
    ]);
    let jita_job_rep =
        rpc_call(&ctx, &mut char_sess, "industryManager", "InstallJob", &[jita_dict]).await;
    let jita_job_id = jita_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let jita_tax_isk = match jita_job_rep.dict_get("facilityTaxISK") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("missing facilityTaxISK in jita_job_rep: {other:?}"),
    };

    other_sess.station_id = Some(dodixie_stn);
    other_sess.solar_system_id = Some(30_002_659);
    let dodixie_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(8)),
        (PyRep::String("facilityID".into()), PyRep::Integer(dodixie_stn)),
    ]);
    rpc_call(&ctx, &mut other_sess, "industryManager", "InstallJob", &[dodixie_dict]).await;

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
    let perim_fac = fac_arr
        .iter()
        .find(|f| f["facility_id"] == json!(struct_id))
        .expect("Perimeter structure in /industry/facilities");
    assert_eq!(perim_fac["owner_id"], json!(corp_id));
    assert_eq!(perim_fac["solar_system_id"], json!(30_000_144));
    // Upwell structure facility tax matches configured 0.08 and InstallJob
    let perim_reported_tax = perim_fac["tax"].as_f64().unwrap();
    assert!((perim_reported_tax - 0.08).abs() < 1e-9);
    let expected_eiv = jita_tax_isk / 0.0025;
    let expected_perim_tax_isk = ((expected_eiv * perim_reported_tax) * 100.0).round() / 100.0;
    assert!((perim_tax_isk - expected_perim_tax_isk).abs() < 1e-6);

    // Updating the structure's configured facility tax is reflected in /industry/facilities
    ctx.db.with_mut_snapshot(|snap| {
        let mut taxes = eve_server::services::industry_pi::industry_facilities_bps::load_facility_taxes(
            snap, struct_id,
        );
        taxes.insert(1, 0.035);
        eve_server::services::industry_pi::industry_facilities_bps::save_facility_taxes(
            snap, struct_id, &taxes,
        );
    });
    let facilities_updated = get_json(&router, &spec, "/v1/industry/facilities/", None);
    let perim_fac_updated = facilities_updated
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["facility_id"] == json!(struct_id))
        .unwrap();
    assert!((perim_fac_updated["tax"].as_f64().unwrap() - 0.035).abs() < 1e-9);

    assert!(fac_arr
        .iter()
        .any(|f| f["facility_id"] == json!(jita_stn) && f["solar_system_id"] == json!(30_000_142)));

    let systems = get_json(&router, &spec, "/v1/industry/systems/", None);
    let sys_arr = systems.as_array().unwrap();
    assert_eq!(sys_arr.len(), ctx.sde.solar_systems.len());
    let sci_for = |sid: u32, act: &str| -> f64 {
        sys_arr
            .iter()
            .find(|s| s["solar_system_id"] == json!(sid))
            .unwrap()["cost_indices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ci| ci["activity"] == json!(act))
            .unwrap()["cost_index"]
            .as_f64()
            .unwrap()
    };
    assert!((sci_for(30_000_142, "manufacturing") - 0.10_f64.sqrt()).abs() < 1e-6);
    assert!((sci_for(30_000_144, "manufacturing") - 0.10_f64.sqrt()).abs() < 1e-6);
    assert!((sci_for(30_002_659, "manufacturing") - 0.50).abs() < 1e-6);
    assert!((sci_for(30_002_187, "manufacturing") - 0.01).abs() < 1e-6);

    // Verify exact parity with `map.GetIndustryCostModifier` RPC across active, clamped, and idle systems:
    for sid in [30_000_142u32, 30_000_144, 30_002_659, 30_002_187] {
        let rep = rpc_call(
            &ctx,
            &mut char_sess,
            "map",
            "GetIndustryCostModifier",
            &[PyRep::Integer(i64::from(sid))],
        )
        .await;
        for (rpc_k, esi_k) in [
            ("manufacturing", "manufacturing"),
            ("research_te", "researching_time_efficiency"),
            ("research_me", "researching_material_efficiency"),
            ("copying", "copying"),
            ("invention", "invention"),
            ("reaction", "reaction"),
        ] {
            let Some(PyRep::Real(rpc_v)) = rep.dict_get(rpc_k) else {
                panic!("missing {rpc_k} in {rep:?}")
            };
            assert!((sci_for(sid, esi_k) - rpc_v).abs() < 1e-9);
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
    assert_eq!(char_jobs_arr[0]["job_id"], json!(jita_job_id));
    assert_eq!(char_jobs_arr[0]["blueprint_type_id"], json!(691));
    assert_eq!(char_jobs_arr[0]["product_type_id"], json!(587));
    assert_eq!(char_jobs_arr[0]["runs"], json!(1));

    let char_mining =
        get_json(&router, &spec, format!("/v1/characters/{char_id}/mining/"), Some(&token));
    let char_mining_arr = char_mining.as_array().unwrap();
    assert_eq!(char_mining_arr.len(), 1, "Mining ledger entries >30d excluded");
    assert_eq!(char_mining_arr[0]["type_id"], json!(fresh_ore_tid));
    assert!(char_mining_arr[0]["quantity"].as_i64().unwrap() > 0);

    let corp_jobs = get_json(
        &router,
        &spec,
        format!("/v1/corporations/{corp_id}/industry/jobs/?include_completed=true"),
        Some(&token),
    );
    let corp_jobs_arr = corp_jobs.as_array().unwrap();
    assert_eq!(corp_jobs_arr.len(), 1);
    assert_eq!(corp_jobs_arr[0]["job_id"], json!(perim_job_id));

    let extractions = get_json(
        &router,
        &spec,
        format!("/v1/corporation/{corp_id}/mining/extractions/"),
        Some(&token),
    );
    assert_eq!(extractions[0]["structure_id"], json!(struct_id));
    assert_eq!(extractions[0]["moon_id"], json!(perim_moon.celestial_id));

    let observers = get_json(
        &router,
        &spec,
        format!("/v1/corporation/{corp_id}/mining/observers/"),
        Some(&token),
    );
    assert!(observers.as_array().unwrap().iter().any(|o| o["observer_id"] == json!(struct_id)));

    let obs_detail = get_json(
        &router,
        &spec,
        format!("/v1/corporation/{corp_id}/mining/observers/{struct_id}/"),
        Some(&token),
    );
    assert_eq!(obs_detail[0]["character_id"], json!(char_id));
    assert!(obs_detail[0]["quantity"].as_i64().unwrap() > 0);

    // IND-01 regression: cancelling an active job via `industryManager.CancelJob` must exclude its
    // `duration_seconds` from both `GET /v1/industry/systems/` and `map.GetIndustryCostModifier`.
    char_sess.station_id = Some(amarr_stn);
    char_sess.solar_system_id = Some(30_002_187);
    let amarr_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(100)),
        (PyRep::String("facilityID".into()), PyRep::Integer(amarr_stn)),
    ]);
    let amarr_job_rep =
        rpc_call(&ctx, &mut char_sess, "industryManager", "InstallJob", &[amarr_dict]).await;
    let amarr_job_id = amarr_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    let sys_active = get_json(&router, &spec, "/v1/industry/systems/", None);
    let sci_from = |arr: &serde_json::Value, sid: u32| -> f64 {
        arr.as_array()
            .unwrap()
            .iter()
            .find(|s| s["solar_system_id"] == json!(sid))
            .unwrap()["cost_indices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ci| ci["activity"] == json!("manufacturing"))
            .unwrap()["cost_index"]
            .as_f64()
            .unwrap()
    };
    assert!((sci_from(&sys_active, 30_002_187) - 0.50).abs() < 1e-6);
    rpc_call(
        &ctx,
        &mut char_sess,
        "industryManager",
        "CancelJob",
        &[PyRep::Integer(amarr_job_id)],
    )
    .await;
    let sys_cancelled = get_json(&router, &spec, "/v1/industry/systems/", None);
    assert!((sci_from(&sys_cancelled, 30_002_187) - 0.01).abs() < 1e-6);
    assert!((sci_from(&sys_cancelled, 30_000_142) - 0.10_f64.sqrt()).abs() < 1e-6);
    let amarr_rpc = rpc_call(
        &ctx,
        &mut char_sess,
        "map",
        "GetIndustryCostModifier",
        &[PyRep::Integer(30_002_187)],
    )
    .await;
    assert!((amarr_rpc.dict_get("manufacturing").and_then(|v| v.as_f64()).unwrap() - 0.01).abs() < 1e-9);
}

}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod esi_market_industry_pi_ext_5 {
    use super::*;

// `ESI-6` integration tests (part 5) for character and corporation market orders.


use super::{
    assert_status, get_json, issue_token_with_scopes, make_corp, make_item, rpc_call,
};
use eve_db::{
    corp_roles::{ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_TRADER},
    InventoryFlag, IskCents, TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn test_esi6_market_character_and_corporation_orders_routes() {
    // parity: esi:GET /characters/{character_id}/orders, esi:GET /characters/{character_id}/orders/history
    // parity: esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/orders",
        "esi:GET /characters/{character_id}/orders/history",
        "esi:GET /corporations/{corporation_id}/orders",
        "esi:GET /corporations/{corporation_id}/orders/history",
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
}

pub(crate) mod esi_market_groups_sde_descriptions {
    // behavioral verification integration tests for:
    // - ESI Market Groups localized SDE descriptions (`esi:GET /markets/groups/{market_group_id}`, `feat:esi.market.groups`)
    // - ESI Structure Market Orders Hub & ACL enforcement (`esi:GET /markets/structures/{structure_id}`, `feat:esi.market.structure-orders`)
    // - ESI Corporation endpoint-specific OpenAPI role gates & route registration across all 6 corp endpoints


    use crate::esi_market_industry_pi::{
        assert_status, get_json, issue_token_with_scopes, make_item, make_structure, rpc_call,
    };
    use eve_db::{
        corp_roles, CorporationRecord, InventoryFlag, IskCents, UpwellStructureRecord,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::{EsiRouter, OpenApiSpec};
    use eve_server::router::ServerContext;
    use eve_server::services::station_structure::structure_upwell_ops::{
        load_ext_state, save_ext_state, StructureAccessProfile,
    };
    use eve_destiny::{Ball, MovementMode};
    use glam::DVec3;
    use serde_json::json;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_esi_market_groups_sde_descriptions_and_structure_orders_acl_hub() {
        // parity: esi:GET /markets/groups/{market_group_id}, feat:esi.market.groups
        eve_testkit::attest(&[
            "esi:GET /markets/groups/{market_group_id}",
            "feat:esi.market.groups",
        ]);

        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        // 1. Verify market group descriptions come from V24 marketgroups.fsdbinary + localization pickle (not name)
        for mg_id in [4u32, 9u32] {
            let expected_desc = ctx
                .sde
                .market_group_description(mg_id)
                .expect("SDE localized market group description");
            assert!(!expected_desc.is_empty());
            let detail = get_json(&router, &spec, format!("/v1/markets/groups/{mg_id}/"), None);
            assert_eq!(detail["description"], json!(expected_desc));
            assert_ne!(detail["description"], detail["name"]);
        }

        // 2. Verify structure market orders require online Standup Market Hub I (35892) AND docking/market ACL access
        let char_id = 90_000_001_i64;
        let corp_id = 98_000_711_i64;
        let fortizar_hub_id = 1_000_000_711_001_i64;
        let bare_astrahus_id = 1_000_000_711_002_i64;
        let offline_hub_fortizar_id = 1_000_000_711_003_i64;
        let acl_blocked_fortizar_id = 1_000_000_711_004_i64;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.user_id = 1;
                ch.corporation_id = corp_id;
                ch.station_id = None;
                ch.structure_id = Some(fortizar_hub_id);
                ch.solar_system_id = 30_000_142;
                ch.balance = IskCents::from_isk(100_000_000);
            }
            snap.corporations.insert(
                corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = corp_id;
                    __corp.name = "Market Hub Test Corp".into();
                    __corp.ticker = "MHTC".into();
                    __corp.ceo_id = char_id;
                    __corp.members = HashSet::from([char_id]);
                    __corp },
            );

            // (a) Fortizar with online Standup Market Hub I (35892)
            snap.structures.insert(
                fortizar_hub_id,
                make_structure(fortizar_hub_id, 35833, corp_id, 30_000_142, "Hub Fortizar"),
            );
            snap.items.insert(
                950_001,
                make_item(950_001, 35_892, corp_id, fortizar_hub_id, InventoryFlag::ServiceSlot0, 1, true),
            );

            // (b) Bare Astrahus (35832) without Market Hub
            snap.structures.insert(
                bare_astrahus_id,
                make_structure(bare_astrahus_id, 35832, corp_id, 30_000_142, "Bare Astrahus"),
            );

            // (c) Fortizar with offline Standup Market Hub I
            snap.structures.insert(
                offline_hub_fortizar_id,
                make_structure(offline_hub_fortizar_id, 35833, corp_id, 30_000_142, "Offline Hub Fortizar"),
            );
            snap.items.insert(
                950_002,
                make_item(
                    950_002,
                    35_892,
                    corp_id,
                    offline_hub_fortizar_id,
                    InventoryFlag::ServiceSlot0,
                    1,
                    true,
                ),
            );
            snap.module_online_states.insert(950_002, false);

            // (d) Fortizar with online Market Hub but docking ACL denied for caller
            snap.structures.insert(
                acl_blocked_fortizar_id,
                make_structure(acl_blocked_fortizar_id, 35833, corp_id, 30_000_142, "ACL Blocked Fortizar"),
            );
            snap.items.insert(
                950_003,
                make_item(
                    950_003,
                    35_892,
                    corp_id,
                    acl_blocked_fortizar_id,
                    InventoryFlag::ServiceSlot0,
                    1,
                    true,
                ),
            );
            let mut ext = load_ext_state(snap);
            ext.access_profiles.insert(
                acl_blocked_fortizar_id,
                StructureAccessProfile {
                    profile_id: 1,
                    owner_corp_id: corp_id,
                    blocked_characters: HashSet::from([char_id]),
                    ..Default::default()
                },
            );
            save_ext_state(snap, &ext);
        });

        // Place a real buy order at fortizar_hub_id via marketProxy.PlaceBuyOrder
        let mut sess = SessionState::new(9851);
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.structure_id = Some(fortizar_hub_id);
        sess.solar_system_id = Some(30_000_142);
        sess.region_id = Some(10_000_002);
        let buy_args = [
            PyRep::Integer(fortizar_hub_id),
            PyRep::Integer(34),
            PyRep::Real(8.0),
            PyRep::Integer(1_000),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(30),
            PyRep::Bool(false),
        ];
        let buy_rep = rpc_call(&ctx, &mut sess, "marketProxy", "PlaceBuyOrder", &buy_args).await;
        let placed_oid = buy_rep.dict_get("orderID").and_then(|v| v.as_i64()).expect("orderID");

        let token = issue_token_with_scopes(&ctx, char_id, vec!["esi-markets.structure_markets.v1"]);
        let ok_orders = get_json(
            &router,
            &spec,
            format!("/v1/markets/structures/{fortizar_hub_id}/"),
            Some(&token),
        );
        assert_eq!(ok_orders.as_array().unwrap().len(), 1);
        assert_eq!(ok_orders[0]["order_id"], json!(placed_oid));

        assert_status(&router, format!("/v1/markets/structures/{bare_astrahus_id}/"), Some(&token), 403);
        assert_status(
            &router,
            format!("/v1/markets/structures/{offline_hub_fortizar_id}/"),
            Some(&token),
            403,
        );
        assert_status(
            &router,
            format!("/v1/markets/structures/{acl_blocked_fortizar_id}/"),
            Some(&token),
            403,
        );
    }

    #[tokio::test]
    async fn test_esi_corp_endpoint_specific_role_gates_and_routes() {
        // parity: esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history, feat:esi.market.corp-orders, esi:GET /corporation/{corporation_id}/mining/extractions, feat:esi.industry.corp-extractions, esi:GET /corporation/{corporation_id}/mining/observers, esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}, feat:esi.industry.corp-observers, esi:GET /corporations/{corporation_id}/customs_offices, feat:esi.planetary-interaction.customs
        eve_testkit::attest(&[
            "esi:GET /corporations/{corporation_id}/orders",
            "esi:GET /corporations/{corporation_id}/orders/history",
            "feat:esi.market.corp-orders",
            "esi:GET /corporation/{corporation_id}/mining/extractions",
            "feat:esi.industry.corp-extractions",
            "esi:GET /corporation/{corporation_id}/mining/observers",
            "esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}",
            "feat:esi.industry.corp-observers",
            "esi:GET /corporations/{corporation_id}/customs_offices",
            "feat:esi.planetary-interaction.customs",
        ]);

        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        let corp_id = 98_000_722_i64;
        let ceo_id = 90_000_001_i64;
        let no_role_id = 90_000_721_i64;
        let sec_officer_id = 90_000_722_i64;
        let accountant_id = 90_000_723_i64;
        let jr_accountant_id = 90_000_724_i64;
        let trader_id = 90_000_725_i64;
        let station_mgr_id = 90_000_726_i64;
        let observer_id = 1_000_000_722_001_i64;
        let ship_id = 100_000_001_i64;
        let jita_station = 60_003_760_i64;
        let lowsec_sys = 30_000_192_u32;
        let lowsec_planet_id = ctx.sde.solar_systems[&lowsec_sys].planet_ids[0];
        let perim_moon = ctx
            .sde
            .celestials_in_system(30_000_144)
            .into_iter()
            .find(|c| c.group_id == 8)
            .expect("SDE moon in Perimeter");
        let perim_pos = [
            perim_moon.position.x + 2_000.0,
            perim_moon.position.y,
            perim_moon.position.z,
        ];

        ctx.db.with_mut_snapshot(|snap| {
            let base_char = snap.characters[&ceo_id].clone();
            for cid in [
                ceo_id,
                no_role_id,
                sec_officer_id,
                accountant_id,
                jr_accountant_id,
                trader_id,
                station_mgr_id,
            ] {
                let mut ch = base_char.clone();
                ch.character_id = cid;
                ch.user_id = 1;
                ch.corporation_id = corp_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.solar_system_id = 30_000_144;
                ch.active_ship_id = ship_id;
                ch.balance = IskCents::from_isk(250_000_000);
                snap.characters.insert(cid, ch);
            }
            let mut corp = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Role Gate Remediation Corp".into();
                __corp.ticker = "RGRC".into();
                __corp.ceo_id = ceo_id;
                __corp.members = HashSet::from([
                    ceo_id,
                    no_role_id,
                    sec_officer_id,
                    accountant_id,
                    jr_accountant_id,
                    trader_id,
                    station_mgr_id,
                ]);
                __corp.member_roles = HashMap::from([
                    (
                        ceo_id,
                        corp_roles::ROLE_DIRECTOR
                            | corp_roles::ROLE_STATION_MANAGER
                            | corp_roles::ROLE_ACCOUNTANT
                            | corp_roles::ROLE_TRADER,
                    ),
                    (no_role_id, 0),
                    (sec_officer_id, corp_roles::ROLE_SECURITY_OFFICER),
                    (accountant_id, corp_roles::ROLE_ACCOUNTANT),
                    (jr_accountant_id, corp_roles::ROLE_JUNIOR_ACCOUNTANT),
                    (trader_id, corp_roles::ROLE_TRADER),
                    (station_mgr_id, corp_roles::ROLE_STATION_MANAGER),
                ]);
                __corp.set_division_balance(1, IskCents::from_isk(250_000_000));
                __corp };
            corp.ensure_wallet_divisions();
            for d in &mut corp.wallet_divisions {
                d.balance = IskCents::from_isk(250_000_000);
            }
            snap.corporations.insert(corp_id, corp);

            // Refinery with online Standup Moon Drill I (45009) + Gantry/POCO items in cargo
            snap.structures.insert(
                observer_id,
                UpwellStructureRecord {
                    structure_id: observer_id,
                    type_id: 35835,
                    owner_corp_id: corp_id,
                    solar_system_id: 30_000_144,
                    name: "Athanor Moon Drill".into(),
                    position: perim_pos,
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 1_000,
                    shield_hp: 10000.0,
                    armor_hp: 10000.0,
                    hull_hp: 10000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            snap.items.insert(
                951_001,
                make_item(951_001, 45_009, corp_id, observer_id, InventoryFlag::ServiceSlot0, 1, true),
            );
            snap.module_online_states.insert(951_001, true);
        });

        // 1. Populate moon extraction & observer mining ledger via live RPCs
        let mut ceo_sess = SessionState::new(9852);
        ceo_sess.user_id = Some(1);
        ceo_sess.character_id = Some(ceo_id);
        ceo_sess.corporation_id = Some(corp_id);
        ceo_sess.corp_role = corp_roles::ROLE_DIRECTOR
            | corp_roles::ROLE_STATION_MANAGER
            | corp_roles::ROLE_ACCOUNTANT
            | corp_roles::ROLE_TRADER;
        ceo_sess.solar_system_id = Some(30_000_144);
        ceo_sess.ship_id = Some(ship_id);

        let ext_args = [PyRep::Integer(observer_id), PyRep::Integer(518_400)];
        rpc_call(&ctx, &mut ceo_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;
        ctx.db.clock().advance_secs(520_000);
        let frac = rpc_call(
            &ctx,
            &mut ceo_sess,
            "moonExtractions",
            "FractureChunkWithStructure",
            &[PyRep::Integer(observer_id)],
        )
        .await;
        let ast_id = match frac.dict_get("asteroidIDs") {
            Some(PyRep::List(v)) => v[0].as_i64().expect("fractured asteroid ID"),
            other => panic!("unexpected asteroidIDs: {other:?}"),
        };
        let _ = ctx
            .systems
            .get_or_spawn(30_000_144)
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 606,
                position: DVec3::from_array(perim_pos),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        ctx.db.with_mut_snapshot(|snap| {
            if !snap.characters.contains_key(&ceo_id) {
                let mut ch = snap.characters.values().next().unwrap().clone();
                ch.character_id = ceo_id;
                ch.corporation_id = corp_id;
                ch.solar_system_id = 30_000_144;
                ch.active_ship_id = ship_id;
                snap.characters.insert(ceo_id, ch);
            }
            snap.skills.entry(ceo_id).or_default().insert(3386, eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 });
            snap.items.insert(ship_id, make_item(ship_id, 606, ceo_id, 30_000_144, InventoryFlag::None, 1, true));
            snap.items.insert(91_000_482, make_item(91_000_482, 483, ceo_id, ship_id, InventoryFlag::HiSlot0, 1, true));
            snap.module_online_states.insert(91_000_482, true);
        });
        ctx.invalidate_ship_fitting(ship_id);
        rpc_call(&ctx, &mut ceo_sess, "dogmaIM", "Mine", &[PyRep::Integer(ast_id)]).await;
        rpc_call(&ctx, &mut ceo_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;

        // 2. Populate active & historical corporate orders via marketProxy.PlaceBuyOrder + CancelCharOrder
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&ceo_id) {
                ch.station_id = Some(jita_station);
                ch.solar_system_id = 30_000_142;
            }
        });
        ceo_sess.station_id = Some(jita_station);
        ceo_sess.solar_system_id = Some(30_000_142);
        ceo_sess.region_id = Some(10_000_002);
        let corp_buy_args = [
            PyRep::Integer(jita_station),
            PyRep::Integer(36),
            PyRep::Real(25.0),
            PyRep::Integer(500),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(30),
            PyRep::Bool(true),
            PyRep::Integer(1000),
        ];
        rpc_call(&ctx, &mut ceo_sess, "marketProxy", "PlaceBuyOrder", &corp_buy_args).await;
        let hist_rep = rpc_call(&ctx, &mut ceo_sess, "marketProxy", "PlaceBuyOrder", &corp_buy_args).await;
        let hist_oid = hist_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
        rpc_call(&ctx, &mut ceo_sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(hist_oid)])
            .await;

        // 3. Populate customs office via posMgr.AnchorOrbital + OnlineOrbital + planetOrbitalRegistryBroker.UpdateSettings
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&ceo_id) {
                ch.station_id = None;
                ch.solar_system_id = lowsec_sys;
                ch.standings.insert(500_026, 2.0);
            }
            snap.items
                .insert(951_002, make_item(951_002, 3962, corp_id, ship_id, InventoryFlag::Cargo, 1, false));
            snap.items
                .insert(951_003, make_item(951_003, 2233, corp_id, ship_id, InventoryFlag::Cargo, 1, false));
        });
        ceo_sess.station_id = None;
        ceo_sess.solar_system_id = Some(lowsec_sys);
        let lowsec_planet = ctx.sde.celestials_in_system(lowsec_sys).into_iter().find(|c| c.celestial_id == lowsec_planet_id).unwrap();
        let near_planet = lowsec_planet.position + glam::DVec3::new(lowsec_planet.radius + 5_000.0, 0.0, 0.0);
        ctx.systems.get_or_spawn(lowsec_sys).spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: near_planet,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        }).await.unwrap();
        rpc_call(
            &ctx,
            &mut ceo_sess,
            "posMgr",
            "AnchorOrbital",
            &[PyRep::Integer(951_002), PyRep::Integer(lowsec_planet_id)],
        )
        .await;
        rpc_call(
            &ctx,
            &mut ceo_sess,
            "posMgr",
            "OnlineOrbital",
            &[PyRep::Integer(951_002), PyRep::Integer(951_003)],
        )
        .await;
        let poco_args = [
            PyRep::Integer(lowsec_planet_id),
            PyRep::Integer(18),
            PyRep::keyval([
                ("corporation", PyRep::Real(0.05)),
                ("alliance", PyRep::Real(0.08)),
                ("standingHigh", PyRep::Real(0.05)),
                ("standingGood", PyRep::Real(0.08)),
                ("standingNeutral", PyRep::Real(0.10)),
                ("standingBad", PyRep::Real(0.15)),
                ("standingHorrible", PyRep::Real(0.20)),
            ]),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Bool(false),
        ];
        rpc_call(&ctx, &mut ceo_sess, "planetOrbitalRegistryBroker", "UpdateSettings", &poco_args)
            .await;

        let all_scopes = vec![
            "esi-markets.read_corporation_orders.v1",
            "esi-industry.read_corporation_mining.v1",
            "esi-planets.read_customs_offices.v1",
        ];
        let tok_ceo = issue_token_with_scopes(&ctx, ceo_id, all_scopes.clone());
        let tok_no_role = issue_token_with_scopes(&ctx, no_role_id, all_scopes.clone());
        let tok_sec = issue_token_with_scopes(&ctx, sec_officer_id, all_scopes.clone());
        let tok_acct = issue_token_with_scopes(&ctx, accountant_id, all_scopes.clone());
        let tok_jr_acct = issue_token_with_scopes(&ctx, jr_accountant_id, all_scopes.clone());
        let tok_trader = issue_token_with_scopes(&ctx, trader_id, all_scopes.clone());
        let tok_station_mgr = issue_token_with_scopes(&ctx, station_mgr_id, all_scopes);

        let orders_url = format!("/v1/corporations/{corp_id}/orders/");
        let orders_hist_url = format!("/v1/corporations/{corp_id}/orders/history/");
        let extractions_url = format!("/v1/corporation/{corp_id}/mining/extractions/");
        let observers_url = format!("/v1/corporation/{corp_id}/mining/observers/");
        let observer_detail_url = format!("/v1/corporation/{corp_id}/mining/observers/{observer_id}/");
        let customs_url = format!("/v1/corporations/{corp_id}/customs_offices/");

        // Negative: zero-role & Security_Officer members receive 403 on all 6 endpoints despite valid SSO scopes
        for url in [
            &orders_url,
            &orders_hist_url,
            &extractions_url,
            &observers_url,
            &observer_detail_url,
            &customs_url,
        ] {
            assert_status(&router, url, Some(&tok_no_role), 403);
            assert_status(&router, url, Some(&tok_sec), 403);
        }

        // Orders & Order History: OpenAPI x-required-roles = ["Accountant", "Trader"]
        for tok in [&tok_acct, &tok_trader] {
            let ord = get_json(&router, &spec, &orders_url, Some(tok));
            assert_eq!(ord.as_array().unwrap().len(), 1);
            let hist = get_json(&router, &spec, &orders_hist_url, Some(tok));
            assert_eq!(hist.as_array().unwrap().len(), 1);
        }
        for tok in [&tok_jr_acct, &tok_station_mgr] {
            assert_status(&router, &orders_url, Some(tok), 403);
            assert_status(&router, &orders_hist_url, Some(tok), 403);
        }

        // Mining Extractions: OpenAPI x-required-roles = ["Station_Manager"]
        let ext = get_json(&router, &spec, &extractions_url, Some(&tok_station_mgr));
        assert_eq!(ext.as_array().unwrap().len(), 1);
        for tok in [&tok_acct, &tok_trader, &tok_jr_acct] {
            assert_status(&router, &extractions_url, Some(tok), 403);
        }

        // Mining Observers & Observer Ledger: OpenAPI x-required-roles = ["Accountant"]
        let obs = get_json(&router, &spec, &observers_url, Some(&tok_acct));
        assert_eq!(obs.as_array().unwrap().len(), 1);
        let obs_det = get_json(&router, &spec, &observer_detail_url, Some(&tok_acct));
        assert_eq!(obs_det.as_array().unwrap().len(), 1);
        for tok in [&tok_station_mgr, &tok_trader, &tok_jr_acct] {
            assert_status(&router, &observers_url, Some(tok), 403);
            assert_status(&router, &observer_detail_url, Some(tok), 403);
        }

        // Customs Offices: OpenAPI x-required-roles = ["Director"]
        let pocos = get_json(&router, &spec, &customs_url, Some(&tok_ceo));
        assert_eq!(pocos.as_array().unwrap().len(), 1);
        for tok in [&tok_station_mgr, &tok_acct, &tok_trader, &tok_jr_acct] {
            assert_status(&router, &customs_url, Some(tok), 403);
        }
    }
}

pub(crate) mod esi6_contracts_character_corporation_public {
    // `ESI-6` integration tests for Contracts and Planetary Interaction routes against live
    // `WorldRepository` and `SdeRegistry` state populated via `contractProxy`, `planetMgr`, `posMgr`,
    // and `planetOrbitalRegistryBroker` RPCs, validated against `OpenApiSpec::pinned()`.


    use crate::esi_market_industry_pi::{
        assert_status, get_json, issue_token_with_scopes, make_corp, make_item, rpc_call,
    };
    use eve_db::{
        corp_roles::{ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR, ROLE_STATION_MANAGER},
        InventoryFlag, IskCents, TrainedSkillRecord,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::{EsiRouter, OpenApiSpec};
    use eve_server::router::ServerContext;
    use serde_json::json;
    use std::sync::Arc;

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
        let char_id = 90_000_001_i64;
        let rival_char_id = 90_000_002_i64;
        let corp_id = 98_000_621_i64;
        let rival_corp_id = 98_000_699_i64;
        let jita_station = 60_003_760_i64;
        let escrow_item_id = 880_621_i64;
        let private_item_id = 880_622_i64;
        let personal_item_id = 880_623_i64;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.corporation_id = corp_id;
                ch.station_id = Some(jita_station);
                ch.solar_system_id = 30_000_142;
                ch.balance = IskCents::from_isk(250_000_000);
            }
            if let Some(ch2) = snap.characters.get_mut(&rival_char_id) {
                ch2.corporation_id = rival_corp_id;
                ch2.station_id = Some(jita_station);
                ch2.solar_system_id = 30_000_142;
                ch2.balance = IskCents::from_isk(250_000_000);
            }
            snap.corporations.insert(
                corp_id,
                make_corp(corp_id, "ESI6 Contracts Corp", "E6CT", char_id, None, 0.0),
            );
            snap.corporations.insert(
                rival_corp_id,
                make_corp(rival_corp_id, "Rival Contracts Corp", "RVCT", rival_char_id, None, 0.0),
            );
            snap.items.insert(
                escrow_item_id,
                make_item(escrow_item_id, 587, corp_id, jita_station, InventoryFlag::CorpSAG1, 1, true),
            );
            snap.items.insert(
                private_item_id,
                make_item(private_item_id, 587, rival_char_id, jita_station, InventoryFlag::Hangar, 1, true),
            );
            snap.items.insert(
                personal_item_id,
                make_item(personal_item_id, 587, char_id, jita_station, InventoryFlag::Hangar, 1, true),
            );
        });

        // 1. Create public corporation Auction contract + personal character Auction contract via contractProxy.CreateContract
        let mut issuer_sess = SessionState::new(9821);
        issuer_sess.user_id = Some(1);
        issuer_sess.character_id = Some(char_id);
        issuer_sess.corporation_id = Some(corp_id);
        issuer_sess.corp_role = ROLE_DIRECTOR | ROLE_CONTRACT_MANAGER;
        issuer_sess.station_id = Some(jita_station);
        issuer_sess.solar_system_id = Some(30_000_142);

        let pub_args = [
            PyRep::Integer(2), // Auction
            PyRep::Integer(0), // Public
            PyRep::Integer(0), // assigneeID
            PyRep::Integer(1440),
            PyRep::Integer(7),
            PyRep::Integer(jita_station),
            PyRep::Integer(jita_station),
            PyRep::Real(1_500_000.0),
            PyRep::Real(0.0),
            PyRep::Real(5_000_000.0), // buyout
            PyRep::String("Jita Rifter Auction".into()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(escrow_item_id), PyRep::Integer(1)])]),
            PyRep::Integer(4),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(1_000)])]),
            PyRep::Bool(true), // forCorp
        ];
        let pub_rep = rpc_call(&ctx, &mut issuer_sess, "contractProxy", "CreateContract", &pub_args).await;
        let public_contract_id = pub_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

        let personal_args = [
            PyRep::Integer(2), // Auction
            PyRep::Integer(0), // Public
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(7),
            PyRep::Integer(jita_station),
            PyRep::Integer(jita_station),
            PyRep::Real(1_500_000.0),
            PyRep::Real(0.0),
            PyRep::Real(5_000_000.0),
            PyRep::String("Personal Rifter Auction".into()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(personal_item_id), PyRep::Integer(1)])]),
            PyRep::Integer(4),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(1_000)])]),
            PyRep::Bool(false), // personal
        ];
        let personal_rep = rpc_call(&ctx, &mut issuer_sess, "contractProxy", "CreateContract", &personal_args).await;
        let personal_contract_id = personal_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

        // 2. Place two bids from rival_char_id via contractProxy.PlaceBid and create a private personal contract
        let mut rival_sess = SessionState::new(9822);
        rival_sess.user_id = Some(1);
        rival_sess.character_id = Some(rival_char_id);
        rival_sess.corporation_id = Some(rival_corp_id);
        rival_sess.station_id = Some(jita_station);
        rival_sess.solar_system_id = Some(30_000_142);

        let bid1 = [PyRep::Integer(public_contract_id), PyRep::Real(2_000_000.0)];
        rpc_call(&ctx, &mut rival_sess, "contractProxy", "PlaceBid", &bid1).await;
        let p_bid1 = [PyRep::Integer(personal_contract_id), PyRep::Real(2_000_000.0)];
        rpc_call(&ctx, &mut rival_sess, "contractProxy", "PlaceBid", &p_bid1).await;
        ctx.db.clock().advance_secs(600);
        let bid2 = [PyRep::Integer(public_contract_id), PyRep::Real(2_500_000.0)];
        rpc_call(&ctx, &mut rival_sess, "contractProxy", "PlaceBid", &bid2).await;
        let p_bid2 = [PyRep::Integer(personal_contract_id), PyRep::Real(2_500_000.0)];
        rpc_call(&ctx, &mut rival_sess, "contractProxy", "PlaceBid", &p_bid2).await;

        let priv_args = [
            PyRep::Integer(1), // ItemExchange
            PyRep::Integer(1), // Personal
            PyRep::Integer(rival_char_id),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(jita_station),
            PyRep::Integer(jita_station),
            PyRep::Real(100_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("Private Rival Contract".into()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(private_item_id), PyRep::Integer(1)])]),
            PyRep::Integer(4),
            PyRep::List(vec![]),
            PyRep::Bool(false),
        ];
        let priv_rep = rpc_call(&ctx, &mut rival_sess, "contractProxy", "CreateContract", &priv_args).await;
        let private_contract_id = priv_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

        let token = issue_token_with_scopes(
            &ctx,
            char_id,
            vec![
                "esi-contracts.read_character_contracts.v1",
                "esi-contracts.read_corporation_contracts.v1",
            ],
        );

        let char_contracts =
            get_json(&router, &spec, format!("/v1/characters/{char_id}/contracts/"), Some(&token));
        let char_contracts_arr = char_contracts.as_array().unwrap();
        assert_eq!(char_contracts_arr.len(), 1);
        assert_eq!(char_contracts_arr[0]["contract_id"], json!(personal_contract_id));
        assert_eq!(char_contracts_arr[0]["type"], json!("auction"));
        assert_eq!(char_contracts_arr[0]["buyout"], json!(5_000_000.0));

        let char_items = get_json(
            &router,
            &spec,
            format!("/v1/characters/{char_id}/contracts/{personal_contract_id}/items/"),
            Some(&token),
        );
        let char_items_arr = char_items.as_array().unwrap();
        assert_eq!(char_items_arr.len(), 2);
        assert!(char_items_arr.iter().any(|i| i["type_id"] == json!(587) && i["is_included"] == json!(true)));
        assert!(char_items_arr.iter().any(|i| i["type_id"] == json!(34) && i["is_included"] == json!(false)));

        assert_status(
            &router,
            format!("/v1/characters/{char_id}/contracts/{public_contract_id}/items/"),
            Some(&token),
            403,
        );
        assert_status(
            &router,
            format!("/v1/characters/{char_id}/contracts/{private_contract_id}/items/"),
            Some(&token),
            403,
        );

        let char_bids = get_json(
            &router,
            &spec,
            format!("/v1/characters/{char_id}/contracts/{personal_contract_id}/bids/"),
            Some(&token),
        );
        let char_bids_arr = char_bids.as_array().unwrap();
        assert_eq!(char_bids_arr.len(), 2);
        assert_eq!(char_bids_arr[0]["amount"], json!(2_500_000.0));
        assert_eq!(char_bids_arr[0]["bidder_id"], json!(rival_char_id));

        let pub_contracts = get_json(&router, &spec, "/v1/contracts/public/10000002/", None);
        assert!(pub_contracts
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["contract_id"] == json!(public_contract_id)));

        let pub_items =
            get_json(&router, &spec, format!("/v1/contracts/public/items/{public_contract_id}/"), None);
        assert_eq!(pub_items.as_array().unwrap().len(), 2);
        assert_status(&router, format!("/v1/contracts/public/items/{private_contract_id}/"), None, 404);

        let pub_bids =
            get_json(&router, &spec, format!("/v1/contracts/public/bids/{public_contract_id}/"), None);
        assert_eq!(pub_bids.as_array().unwrap().len(), 2);
        assert_eq!(pub_bids[0]["amount"], json!(2_500_000.0));

        let corp_contracts =
            get_json(&router, &spec, format!("/v1/corporations/{corp_id}/contracts/"), Some(&token));
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
    async fn test_esi_planetary_interaction_planets_and_customs_offices() {
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
        let char_id = 90_000_001_i64;
        let corp_id = 98_000_641_i64;
        let ship_id = 100_000_001_i64;
        let planet_id = 40_009_080_i64; // Jita III (Planet (Temperate))
        let lowsec_sys = 30_000_192_u32;
        let lowsec_planet_id = ctx.sde.solar_systems[&lowsec_sys].planet_ids[0];
        let cc_item_id = 890_001_i64;
        let gantry_item_id = 890_010_i64;
        let poco_upgrade_item_id = 890_011_i64;

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.corporation_id = corp_id;
                ch.station_id = None;
                ch.solar_system_id = 30_000_142;
                ch.active_ship_id = ship_id;
                ch.balance = IskCents::from_isk(250_000_000);
            }
            snap.corporations.insert(
                corp_id,
                make_corp(corp_id, "ESI6 Planetary Corp", "E6PI", char_id, None, 0.0),
            );
            let sk = snap.skills.entry(char_id).or_default();
            for tid in [2495u32, 2505u32] {
                sk.insert(
                    tid,
                    TrainedSkillRecord {
                        type_id: tid,
                        level: 4,
                        skill_points: 256_000.0,
                    },
                );
            }
            snap.items.insert(
                cc_item_id,
                make_item(cc_item_id, 2254, char_id, ship_id, InventoryFlag::Cargo, 1, false),
            );
            snap.items.insert(
                gantry_item_id,
                make_item(gantry_item_id, 3962, corp_id, ship_id, InventoryFlag::Cargo, 1, false),
            );
            snap.items.insert(
                poco_upgrade_item_id,
                make_item(poco_upgrade_item_id, 2233, corp_id, ship_id, InventoryFlag::Cargo, 1, false),
            );
        });

        // 1. Deploy Temperate Command Center, upgrade to level 4, place ECU + Factory, install schematic & extractor program,
        //    link pins, and route extracted Aqueous Liquids (2268) into the Command Center via planetMgr.UserUpdateNetwork.
        let mut pi_sess = SessionState::new(9830);
        pi_sess.user_id = Some(1);
        pi_sess.character_id = Some(char_id);
        pi_sess.corporation_id = Some(corp_id);
        pi_sess.corp_role = ROLE_DIRECTOR | ROLE_STATION_MANAGER;
        pi_sess.solar_system_id = Some(30_000_142);
        pi_sess.ship_id = Some(ship_id);

        let net_cmds = PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(101),
                PyRep::Integer(2254),
                PyRep::Real(0.45),
                PyRep::Real(1.15),
            ]),
            PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(4)]),
            PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(102),
                PyRep::Integer(3068),
                PyRep::Real(0.50),
                PyRep::Real(1.20),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(103),
                PyRep::Integer(2469),
                PyRep::Real(0.52),
                PyRep::Real(1.24),
            ]),
            PyRep::Tuple(vec![PyRep::Integer(8), PyRep::Integer(103), PyRep::Integer(121)]),
            PyRep::Tuple(vec![
                PyRep::Integer(10),
                PyRep::Integer(102),
                PyRep::Integer(2268),
                PyRep::Integer(300),
                PyRep::Integer(1),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(3),
                PyRep::Integer(102),
                PyRep::Integer(101),
                PyRep::Integer(2),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(6),
                PyRep::Integer(7),
                PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(101)]),
                PyRep::Integer(2268),
                PyRep::Integer(20),
            ]),
        ]);
        rpc_call(
            &ctx,
            &mut pi_sess,
            "planetMgr",
            "UserUpdateNetwork",
            &[PyRep::Integer(planet_id), net_cmds],
        )
        .await;

        // Advance simulated clock by 600s and call planetMgr.GetColony so extraction cycles populate pin contents
        ctx.db.clock().advance_secs(600);
        rpc_call(&ctx, &mut pi_sess, "planetMgr", "GetColony", &[PyRep::Integer(planet_id)]).await;

        // 2. Anchor Customs Office Gantry in lowsec 30_000_192, upgrade to POCO, and configure tax rates via RPCs
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = lowsec_sys;
                ch.standings.insert(500_026, 2.0);
            }
        });
        pi_sess.solar_system_id = Some(lowsec_sys);
        let lowsec_planet = ctx.sde.celestials_in_system(lowsec_sys).into_iter().find(|c| c.celestial_id == lowsec_planet_id).unwrap();
        let near_planet = lowsec_planet.position + glam::DVec3::new(lowsec_planet.radius + 5_000.0, 0.0, 0.0);
        ctx.systems.get_or_spawn(lowsec_sys).spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: near_planet,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        }).await.unwrap();
        rpc_call(
            &ctx,
            &mut pi_sess,
            "posMgr",
            "AnchorOrbital",
            &[PyRep::Integer(gantry_item_id), PyRep::Integer(lowsec_planet_id)],
        )
        .await;
        rpc_call(
            &ctx,
            &mut pi_sess,
            "posMgr",
            "OnlineOrbital",
            &[PyRep::Integer(gantry_item_id), PyRep::Integer(poco_upgrade_item_id)],
        )
        .await;
        let poco_args = [
            PyRep::Integer(lowsec_planet_id),
            PyRep::Integer(18),
            PyRep::keyval([
                ("corporation", PyRep::Real(0.04)),
                ("alliance", PyRep::Real(0.06)),
                ("standingHigh", PyRep::Real(0.05)),
                ("standingGood", PyRep::Real(0.08)),
                ("standingNeutral", PyRep::Real(0.10)),
                ("standingBad", PyRep::Real(0.15)),
                ("standingHorrible", PyRep::Real(0.20)),
            ]),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Bool(true),
        ];
        rpc_call(&ctx, &mut pi_sess, "planetOrbitalRegistryBroker", "UpdateSettings", &poco_args)
            .await;

        let token = issue_token_with_scopes(
            &ctx,
            char_id,
            vec![
                "esi-planets.manage_planets.v1",
                "esi-planets.read_customs_offices.v1",
            ],
        );

        let planets =
            get_json(&router, &spec, format!("/v1/characters/{char_id}/planets/"), Some(&token));
        let planets_arr = planets.as_array().unwrap();
        assert_eq!(planets_arr.len(), 1);
        assert_eq!(planets_arr[0]["planet_id"], json!(planet_id));
        assert_eq!(planets_arr[0]["upgrade_level"], json!(4));
        assert_eq!(planets_arr[0]["num_pins"], json!(3));
        assert_eq!(planets_arr[0]["planet_type"], json!("temperate"));

        let layout = get_json(
            &router,
            &spec,
            format!("/v3/characters/{char_id}/planets/{planet_id}/"),
            Some(&token),
        );
        let pins_arr = layout["pins"].as_array().unwrap();
        assert_eq!(pins_arr.len(), 3);
        let cc_pin = pins_arr.iter().find(|p| p["type_id"] == json!(2254)).expect("CC pin");
        assert_eq!(cc_pin["contents"][0]["type_id"], json!(2268));
        assert!(cc_pin["contents"][0]["amount"].as_i64().unwrap() > 0);
        let fac_pin = pins_arr.iter().find(|p| p["type_id"] == json!(2469)).expect("Factory pin");
        assert_eq!(fac_pin["schematic_id"], json!(121));
        assert_eq!(layout["links"][0]["link_level"], json!(2));
        assert_eq!(layout["routes"][0]["route_id"], json!(7));
        assert_eq!(layout["routes"][0]["quantity"], json!(20.0));
        assert_status(&router, format!("/v3/characters/{char_id}/planets/99999999/"), Some(&token), 404);

        let pocos = get_json(
            &router,
            &spec,
            format!("/v1/corporations/{corp_id}/customs_offices/"),
            Some(&token),
        );
        let pocos_arr = pocos.as_array().unwrap();
        assert_eq!(pocos_arr.len(), 1);
        assert_eq!(pocos_arr[0]["office_id"], json!(gantry_item_id));
        assert_eq!(pocos_arr[0]["system_id"], json!(lowsec_sys));
        assert_eq!(pocos_arr[0]["corporation_tax_rate"], json!(0.04));
        assert_eq!(pocos_arr[0]["alliance_tax_rate"], json!(0.06));
        assert_status(
            &router,
            format!("/v1/corporations/{corp_id}/customs_offices/?page=2"),
            Some(&token),
            404,
        );

        // Schematic lookup from real SDE planet_schematics
        let (&schem_id, schem_rec) = ctx.sde.planet_schematics.iter().next().expect("SDE schematics");
        let schem_json = get_json(&router, &spec, format!("/v1/universe/schematics/{schem_id}/"), None);
        assert_eq!(schem_json["schematic_name"], json!(schem_rec.name));
        assert_eq!(schem_json["cycle_time"], json!(schem_rec.cycle_time as i64));
        assert_status(&router, "/v1/universe/schematics/999999999/", None, 404);
    }
}

pub(crate) mod esi6_industry_mining_routes {
    // `ESI-6` integration tests for Industry & Mining routes against live `WorldRepository` and
    // `SdeRegistry` state populated via `industryManager`, `moonExtractions`, and `dogmaIM` RPCs,
    // validated against `OpenApiSpec::pinned()`.


    use crate::esi_market_industry_pi::{
        get_json, issue_token_with_scopes, make_corp, make_item, rpc_call,
    };
    use eve_db::{
        corp_roles::{ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER},
        InventoryFlag, IskCents, TrainedSkillRecord, UpwellStructureRecord,
    };
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::{EsiRouter, OpenApiSpec};
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_esi6_industry_and_mining_routes() {
        // parity: esi:GET /industry/facilities, esi:GET /industry/systems
        // parity: esi:GET /characters/{character_id}/mining
        // parity: esi:GET /corporation/{corporation_id}/mining/extractions
        // parity: esi:GET /corporation/{corporation_id}/mining/observers, esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}
        // parity: feat:esi.industry.facilities, feat:esi.industry.systems
        // parity: feat:esi.industry.char-mining
        // parity: feat:esi.industry.corp-extractions, feat:esi.industry.corp-observers
        eve_testkit::attest(&[
            "esi:GET /industry/facilities",
            "esi:GET /industry/systems",
            "esi:GET /characters/{character_id}/mining",
            "esi:GET /corporation/{corporation_id}/mining/extractions",
            "esi:GET /corporation/{corporation_id}/mining/observers",
            "esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}",
            "feat:esi.industry.facilities",
            "feat:esi.industry.systems",
            "feat:esi.industry.char-mining",
            "feat:esi.industry.corp-extractions",
            "feat:esi.industry.corp-observers",
        ]);

        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        let char_id = 90_000_001_i64;
        let other_char_id = 90_000_002_i64;
        let corp_id = 98_000_631_i64;
        let other_corp_id = 98_000_632_i64;
        let ship_id = 100_000_001_i64;
        let struct_id = 1_030_000_631_001_i64;
        let jita_stn = 60_003_760_i64;
        let dodixie_stn = ctx.sde.solar_systems[&30_002_659].stations[0].station_id;
        let amarr_stn = ctx.sde.solar_systems[&30_002_187].stations[0].station_id;
        let perim_moon = ctx
            .sde
            .celestials_in_system(30_000_144)
            .into_iter()
            .find(|c| c.group_id == 8)
            .expect("SDE moon in Perimeter");
        let perim_pos = [
            perim_moon.position.x + 2_000.0,
            perim_moon.position.y,
            perim_moon.position.z,
        ];
        let bp_mats: Vec<(u32, i32)> = ctx.sde.blueprints[&691]
            .materials
            .iter()
            .map(|m| (m.type_id, (m.quantity as i32) * 500))
            .collect();

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.corporation_id = corp_id;
                ch.solar_system_id = 30_000_144;
                ch.active_ship_id = ship_id;
                ch.balance = IskCents::from_isk(500_000_000);
            }
            if let Some(ch2) = snap.characters.get_mut(&other_char_id) {
                ch2.corporation_id = other_corp_id;
                ch2.solar_system_id = 30_000_142;
                ch2.balance = IskCents::from_isk(500_000_000);
            }
            snap.corporations.insert(
                corp_id,
                make_corp(corp_id, "ESI6 Industrial Corp", "E6IC", char_id, None, 0.08),
            );
            snap.corporations.insert(
                other_corp_id,
                make_corp(other_corp_id, "Other Industrial Corp", "OTHC", other_char_id, None, 0.05),
            );
            for cid in [char_id, other_char_id] {
                let sk = snap.skills.entry(cid).or_default();
                for &(sid, lvl) in &[(3380, 1), (3387, 5), (24625, 5), (24268, 5)] {
                    sk.insert(
                        sid,
                        TrainedSkillRecord {
                            type_id: sid,
                            level: lvl,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
            snap.structures.insert(
                struct_id,
                UpwellStructureRecord {
                    structure_id: struct_id,
                    type_id: 35835,
                    owner_corp_id: corp_id,
                    solar_system_id: 30_000_144,
                    name: "Perimeter Moon Refinery".into(),
                    position: perim_pos,
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 1_000,
                    shield_hp: 10000.0,
                    armor_hp: 10000.0,
                    hull_hp: 10000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            for (iid, tid, flag) in [
                (940_001, 35_878, InventoryFlag::ServiceSlot0),
                (940_002, 45_009, InventoryFlag::ServiceSlot1),
            ] {
                snap.items
                    .insert(iid, make_item(iid, tid, corp_id, struct_id, flag, 1, true));
                snap.module_online_states.insert(iid, true);
            }
            eve_server::services::industry_pi::industry_facilities_bps::save_facility_taxes(
                snap,
                struct_id,
                &std::collections::HashMap::from([(1_u8, 0.08)]),
            );
            let mut ext =
                eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
            ext.access_profiles.insert(
                struct_id,
                eve_server::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                    corp_only: false,
                    ..Default::default()
                },
            );
            eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
            let mut next_iid = 941_000_i64;
            for (owner, loc) in [
                (other_char_id, jita_stn),
                (char_id, struct_id),
                (char_id, jita_stn),
                (other_char_id, dodixie_stn),
                (char_id, amarr_stn),
            ] {
                next_iid += 1;
                snap.items.insert(
                    next_iid,
                    make_item(next_iid, 691, owner, loc, InventoryFlag::Hangar, 1, true),
                );
                for &(mat_tid, mat_qty) in &bp_mats {
                    next_iid += 1;
                    snap.items.insert(
                        next_iid,
                        make_item(next_iid, mat_tid, owner, loc, InventoryFlag::Hangar, mat_qty, false),
                    );
                }
            }
        });

        // 1. Start moon extraction #1 on struct_id, advance 6d, fracture chunk #1, and mine the fractured moon asteroid via dogmaIM.Mine
        let mut char_sess = SessionState::new(9841);
        char_sess.user_id = Some(1);
        char_sess.character_id = Some(char_id);
        char_sess.corporation_id = Some(corp_id);
        char_sess.corp_role =
            ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER | ROLE_ACCOUNTANT;
        char_sess.solar_system_id = Some(30_000_144);
        char_sess.ship_id = Some(ship_id);

        let ext_args = [PyRep::Integer(struct_id), PyRep::Integer(518_400)];
        rpc_call(&ctx, &mut char_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;
        ctx.db.clock().advance_secs(520_000);
        let frac1 = rpc_call(
            &ctx,
            &mut char_sess,
            "moonExtractions",
            "FractureChunkWithStructure",
            &[PyRep::Integer(struct_id)],
        )
        .await;
        let ast_id_1 = match frac1.dict_get("asteroidIDs") {
            Some(PyRep::List(v)) => v[0].as_i64().expect("first fractured asteroid ID"),
            other => panic!("unexpected asteroidIDs: {other:?}"),
        };
        let _ = ctx
            .systems
            .get_or_spawn(30_000_144)
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 606,
                position: DVec3::from_array(perim_pos),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        ctx.db.with_mut_snapshot(|snap| {
            if !snap.characters.contains_key(&char_id) {
                let mut ch = snap.characters.values().next().unwrap().clone();
                ch.character_id = char_id;
                ch.corporation_id = corp_id;
                ch.solar_system_id = 30_000_144;
                ch.active_ship_id = ship_id;
                snap.characters.insert(char_id, ch);
            }
            snap.skills.entry(char_id).or_default().insert(3386, eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 });
            snap.items.insert(ship_id, make_item(ship_id, 606, char_id, 30_000_144, InventoryFlag::None, 1, true));
            snap.items.insert(93_000_483, make_item(93_000_483, 483, char_id, ship_id, InventoryFlag::HiSlot0, 1, true));
            snap.module_online_states.insert(93_000_483, true);
        });
        ctx.invalidate_ship_fitting(ship_id);
        rpc_call(&ctx, &mut char_sess, "dogmaIM", "Mine", &[PyRep::Integer(ast_id_1)]).await;

        // 2. Install & deliver an old industry job in Jita via other_char_id, then advance 35 days so both the first mining ledger entry (>30d) and first Jita job (>28d) age out
        let mut other_sess = SessionState::new(9842);
        other_sess.user_id = Some(1);
        other_sess.character_id = Some(other_char_id);
        other_sess.corporation_id = Some(other_corp_id);
        other_sess.station_id = Some(jita_stn);
        other_sess.solar_system_id = Some(30_000_142);

        let old_job_dict = PyRep::Dict(vec![
            (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
            (PyRep::String("activityID".into()), PyRep::Integer(1)),
            (PyRep::String("runs".into()), PyRep::Integer(10)),
            (PyRep::String("facilityID".into()), PyRep::Integer(jita_stn)),
        ]);
        let old_job_rep =
            rpc_call(&ctx, &mut other_sess, "industryManager", "InstallJob", &[old_job_dict]).await;
        let old_job_id = old_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
        ctx.db.clock().advance_secs(100_000);
        rpc_call(&ctx, &mut other_sess, "industryManager", "CompleteJob", &[PyRep::Integer(old_job_id)])
            .await;
        ctx.db.clock().advance_secs(35 * 86_400);

        // 3. Start moon extraction #2, fracture chunk #2, mine a fresh moon asteroid via dogmaIM.Mine, and start active moon extraction #3
        rpc_call(&ctx, &mut char_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;
        ctx.db.clock().advance_secs(520_000);
        let frac2 = rpc_call(
            &ctx,
            &mut char_sess,
            "moonExtractions",
            "FractureChunkWithStructure",
            &[PyRep::Integer(struct_id)],
        )
        .await;
        let ast_id_2 = match frac2.dict_get("asteroidIDs") {
            Some(PyRep::List(v)) => v.get(1).or_else(|| v.first()).and_then(|x| x.as_i64()).unwrap(),
            other => panic!("unexpected asteroidIDs: {other:?}"),
        };
        let fresh_ore_tid = ctx.db.get_asteroid(ast_id_2).unwrap().type_id;
        rpc_call(&ctx, &mut char_sess, "dogmaIM", "Mine", &[PyRep::Integer(ast_id_2)]).await;
        rpc_call(&ctx, &mut char_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;

        // 4. Install & deliver Perimeter corp job (runs = 1), install active Jita corp job (runs = 1), and install active Dodixie job (runs = 8)
        char_sess.structure_id = Some(struct_id);
        char_sess.station_id = None;
        let perim_dict = PyRep::Dict(vec![
            (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
            (PyRep::String("activityID".into()), PyRep::Integer(1)),
            (PyRep::String("runs".into()), PyRep::Integer(1)),
            (PyRep::String("facilityID".into()), PyRep::Integer(struct_id)),
            (PyRep::String("useCorp".into()), PyRep::Bool(true)),
        ]);
        let perim_job_rep =
            rpc_call(&ctx, &mut char_sess, "industryManager", "InstallJob", &[perim_dict]).await;
        let perim_job_id = perim_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
        let perim_tax_isk = match perim_job_rep.dict_get("facilityTaxISK") {
            Some(PyRep::Real(v)) => *v,
            other => panic!("missing facilityTaxISK in perim_job_rep: {other:?}"),
        };
        ctx.db.clock().advance_secs(10_000);
        rpc_call(&ctx, &mut char_sess, "industryManager", "CompleteJob", &[PyRep::Integer(perim_job_id)])
            .await;

        char_sess.structure_id = None;
        char_sess.station_id = Some(jita_stn);
        char_sess.solar_system_id = Some(30_000_142);
        let jita_dict = PyRep::Dict(vec![
            (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
            (PyRep::String("activityID".into()), PyRep::Integer(1)),
            (PyRep::String("runs".into()), PyRep::Integer(1)),
            (PyRep::String("facilityID".into()), PyRep::Integer(jita_stn)),
            (PyRep::String("useCorp".into()), PyRep::Bool(false)),
        ]);
        let jita_job_rep =
            rpc_call(&ctx, &mut char_sess, "industryManager", "InstallJob", &[jita_dict]).await;
        let jita_job_id = jita_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
        let jita_tax_isk = match jita_job_rep.dict_get("facilityTaxISK") {
            Some(PyRep::Real(v)) => *v,
            other => panic!("missing facilityTaxISK in jita_job_rep: {other:?}"),
        };

        other_sess.station_id = Some(dodixie_stn);
        other_sess.solar_system_id = Some(30_002_659);
        let dodixie_dict = PyRep::Dict(vec![
            (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
            (PyRep::String("activityID".into()), PyRep::Integer(1)),
            (PyRep::String("runs".into()), PyRep::Integer(8)),
            (PyRep::String("facilityID".into()), PyRep::Integer(dodixie_stn)),
        ]);
        rpc_call(&ctx, &mut other_sess, "industryManager", "InstallJob", &[dodixie_dict]).await;

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
        let perim_fac = fac_arr
            .iter()
            .find(|f| f["facility_id"] == json!(struct_id))
            .expect("Perimeter structure in /industry/facilities");
        assert_eq!(perim_fac["owner_id"], json!(corp_id));
        assert_eq!(perim_fac["solar_system_id"], json!(30_000_144));
        // Upwell structure facility tax matches configured 0.08 and InstallJob
        let perim_reported_tax = perim_fac["tax"].as_f64().unwrap();
        assert!((perim_reported_tax - 0.08).abs() < 1e-9);
        let expected_eiv = jita_tax_isk / 0.0025;
        let expected_perim_tax_isk = ((expected_eiv * perim_reported_tax) * 100.0).round() / 100.0;
        assert!((perim_tax_isk - expected_perim_tax_isk).abs() < 1e-6);

        // Updating the structure's configured facility tax is reflected in /industry/facilities
        ctx.db.with_mut_snapshot(|snap| {
            let mut taxes = eve_server::services::industry_pi::industry_facilities_bps::load_facility_taxes(
                snap, struct_id,
            );
            taxes.insert(1, 0.035);
            eve_server::services::industry_pi::industry_facilities_bps::save_facility_taxes(
                snap, struct_id, &taxes,
            );
        });
        let facilities_updated = get_json(&router, &spec, "/v1/industry/facilities/", None);
        let perim_fac_updated = facilities_updated
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["facility_id"] == json!(struct_id))
            .unwrap();
        assert!((perim_fac_updated["tax"].as_f64().unwrap() - 0.035).abs() < 1e-9);

        assert!(fac_arr
            .iter()
            .any(|f| f["facility_id"] == json!(jita_stn) && f["solar_system_id"] == json!(30_000_142)));

        let systems = get_json(&router, &spec, "/v1/industry/systems/", None);
        let sys_arr = systems.as_array().unwrap();
        assert_eq!(sys_arr.len(), ctx.sde.solar_systems.len());
        let sci_for = |sid: u32, act: &str| -> f64 {
            sys_arr
                .iter()
                .find(|s| s["solar_system_id"] == json!(sid))
                .unwrap()["cost_indices"]
                .as_array()
                .unwrap()
                .iter()
                .find(|ci| ci["activity"] == json!(act))
                .unwrap()["cost_index"]
                .as_f64()
                .unwrap()
        };
        assert!((sci_for(30_000_142, "manufacturing") - 0.10_f64.sqrt()).abs() < 1e-6);
        assert!((sci_for(30_000_144, "manufacturing") - 0.10_f64.sqrt()).abs() < 1e-6);
        assert!((sci_for(30_002_659, "manufacturing") - 0.50).abs() < 1e-6);
        assert!((sci_for(30_002_187, "manufacturing") - 0.01).abs() < 1e-6);

        // Verify exact parity with `map.GetIndustryCostModifier` RPC across active, clamped, and idle systems:
        for sid in [30_000_142u32, 30_000_144, 30_002_659, 30_002_187] {
            let rep = rpc_call(
                &ctx,
                &mut char_sess,
                "map",
                "GetIndustryCostModifier",
                &[PyRep::Integer(i64::from(sid))],
            )
            .await;
            for (rpc_k, esi_k) in [
                ("manufacturing", "manufacturing"),
                ("research_te", "researching_time_efficiency"),
                ("research_me", "researching_material_efficiency"),
                ("copying", "copying"),
                ("invention", "invention"),
                ("reaction", "reaction"),
            ] {
                let Some(PyRep::Real(rpc_v)) = rep.dict_get(rpc_k) else {
                    panic!("missing {rpc_k} in {rep:?}")
                };
                assert!((sci_for(sid, esi_k) - rpc_v).abs() < 1e-9);
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
        assert_eq!(char_jobs_arr[0]["job_id"], json!(jita_job_id));
        assert_eq!(char_jobs_arr[0]["blueprint_type_id"], json!(691));
        assert_eq!(char_jobs_arr[0]["product_type_id"], json!(587));
        assert_eq!(char_jobs_arr[0]["runs"], json!(1));

        let char_mining =
            get_json(&router, &spec, format!("/v1/characters/{char_id}/mining/"), Some(&token));
        let char_mining_arr = char_mining.as_array().unwrap();
        assert_eq!(char_mining_arr.len(), 1, "Mining ledger entries >30d excluded");
        assert_eq!(char_mining_arr[0]["type_id"], json!(fresh_ore_tid));
        assert!(char_mining_arr[0]["quantity"].as_i64().unwrap() > 0);

        let corp_jobs = get_json(
            &router,
            &spec,
            format!("/v1/corporations/{corp_id}/industry/jobs/?include_completed=true"),
            Some(&token),
        );
        let corp_jobs_arr = corp_jobs.as_array().unwrap();
        assert_eq!(corp_jobs_arr.len(), 1);
        assert_eq!(corp_jobs_arr[0]["job_id"], json!(perim_job_id));

        let extractions = get_json(
            &router,
            &spec,
            format!("/v1/corporation/{corp_id}/mining/extractions/"),
            Some(&token),
        );
        assert_eq!(extractions[0]["structure_id"], json!(struct_id));
        assert_eq!(extractions[0]["moon_id"], json!(perim_moon.celestial_id));

        let observers = get_json(
            &router,
            &spec,
            format!("/v1/corporation/{corp_id}/mining/observers/"),
            Some(&token),
        );
        assert!(observers.as_array().unwrap().iter().any(|o| o["observer_id"] == json!(struct_id)));

        let obs_detail = get_json(
            &router,
            &spec,
            format!("/v1/corporation/{corp_id}/mining/observers/{struct_id}/"),
            Some(&token),
        );
        assert_eq!(obs_detail[0]["character_id"], json!(char_id));
        assert!(obs_detail[0]["quantity"].as_i64().unwrap() > 0);

        // IND-01 regression: cancelling an active job via `industryManager.CancelJob` must exclude its
        // `duration_seconds` from both `GET /v1/industry/systems/` and `map.GetIndustryCostModifier`.
        char_sess.station_id = Some(amarr_stn);
        char_sess.solar_system_id = Some(30_002_187);
        let amarr_dict = PyRep::Dict(vec![
            (PyRep::String("blueprintTypeID".into()), PyRep::Integer(691)),
            (PyRep::String("activityID".into()), PyRep::Integer(1)),
            (PyRep::String("runs".into()), PyRep::Integer(100)),
            (PyRep::String("facilityID".into()), PyRep::Integer(amarr_stn)),
        ]);
        let amarr_job_rep =
            rpc_call(&ctx, &mut char_sess, "industryManager", "InstallJob", &[amarr_dict]).await;
        let amarr_job_id = amarr_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

        let sys_active = get_json(&router, &spec, "/v1/industry/systems/", None);
        let sci_from = |arr: &serde_json::Value, sid: u32| -> f64 {
            arr.as_array()
                .unwrap()
                .iter()
                .find(|s| s["solar_system_id"] == json!(sid))
                .unwrap()["cost_indices"]
                .as_array()
                .unwrap()
                .iter()
                .find(|ci| ci["activity"] == json!("manufacturing"))
                .unwrap()["cost_index"]
                .as_f64()
                .unwrap()
        };
        assert!((sci_from(&sys_active, 30_002_187) - 0.50).abs() < 1e-6);
        rpc_call(
            &ctx,
            &mut char_sess,
            "industryManager",
            "CancelJob",
            &[PyRep::Integer(amarr_job_id)],
        )
        .await;
        let sys_cancelled = get_json(&router, &spec, "/v1/industry/systems/", None);
        assert!((sci_from(&sys_cancelled, 30_002_187) - 0.01).abs() < 1e-6);
        assert!((sci_from(&sys_cancelled, 30_000_142) - 0.10_f64.sqrt()).abs() < 1e-6);
        let amarr_rpc = rpc_call(
            &ctx,
            &mut char_sess,
            "map",
            "GetIndustryCostModifier",
            &[PyRep::Integer(30_002_187)],
        )
        .await;
        assert!((amarr_rpc.dict_get("manufacturing").and_then(|v| v.as_f64()).unwrap() - 0.01).abs() < 1e-9);
    }
}

pub(crate) mod esi_corp_role_gated_mining {
    // End-to-end integration tests for ESI corporation role-gated routes, market history/prices/structures,
    // and industry facilities/jobs (`esi:*` open-row closure).


    use crate::esi_market_industry_pi::{issue_token_with_scopes, make_item, make_structure, rpc_call};
    use eve_db::corp_roles::{
        ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER, ROLE_TRADER,
    };
    use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::sample_routes::is_operation_implemented;
    use eve_server::esi::{EsiHttpRequest, EsiRouter};
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use serde_json::{json, Value};
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn call_esi(router: &EsiRouter, path: &str, token: Option<&str>) -> (u16, Value) {
        let mut req = EsiHttpRequest::new("GET", path);
        if let Some(tok) = token {
            req = req.with_bearer_token(tok);
        }
        let resp = router.handle_request(&req);
        let body: Value = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);
        (resp.status_code, body)
    }

    #[tokio::test]
    async fn test_esi_corp_role_gated_mining_customs_and_orders() {
        // parity: esi:GET /corporation/{corporation_id}/mining/extractions, esi:GET /corporation/{corporation_id}/mining/observers, esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}, esi:GET /corporations/{corporation_id}/customs_offices, esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history
        eve_testkit::attest(&[
            "esi:GET /corporation/{corporation_id}/mining/extractions",
            "esi:GET /corporation/{corporation_id}/mining/observers",
            "esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}",
            "esi:GET /corporations/{corporation_id}/customs_offices",
            "esi:GET /corporations/{corporation_id}/orders",
            "esi:GET /corporations/{corporation_id}/orders/history",
        ]);
        assert!(is_operation_implemented("GET", "/corporation/{corporation_id}/mining/extractions"));
        assert!(is_operation_implemented("GET", "/corporation/{corporation_id}/mining/observers"));
        assert!(is_operation_implemented(
            "GET",
            "/corporation/{corporation_id}/mining/observers/{observer_id}"
        ));
        assert!(is_operation_implemented("GET", "/corporations/{corporation_id}/customs_offices"));

        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));

        let corp_id = 98_500_001_i64;
        let ceo_id = 90_000_001_i64;
        let station_mgr_id = 90_000_002_i64;
        let accountant_id = 90_000_003_i64;
        let trader_id = 90_000_004_i64;
        let plain_member_id = 90_000_005_i64;
        let observer_id = 1_030_000_000_001_i64;
        let ship_id = 100_000_001_i64;
        let jita_station = 60_003_760_i64;
        let lowsec_sys = 30_000_192_u32;
        let lowsec_planet_id = ctx.sde.solar_systems[&lowsec_sys].planet_ids[0];
        let perim_moon = ctx
            .sde
            .celestials_in_system(30_000_144)
            .into_iter()
            .find(|c| c.group_id == 8)
            .expect("SDE moon in Perimeter");
        let perim_pos = [
            perim_moon.position.x + 2_000.0,
            perim_moon.position.y,
            perim_moon.position.z,
        ];

        ctx.db
            .try_with_mut_snapshot(|snap| {
                let member_roles = HashMap::from([
                    (
                        ceo_id,
                        ROLE_DIRECTOR | ROLE_STATION_MANAGER | ROLE_ACCOUNTANT | ROLE_TRADER,
                    ),
                    (station_mgr_id, ROLE_STATION_MANAGER),
                    (accountant_id, ROLE_ACCOUNTANT),
                    (trader_id, ROLE_TRADER),
                    (plain_member_id, 0),
                ]);

                let template_char = snap.characters[&ceo_id].clone();
                for cid in [ceo_id, station_mgr_id, accountant_id, trader_id, plain_member_id] {
                    let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| {
                        let mut c = template_char.clone();
                        c.character_id = cid;
                        c.name = format!("Pilot {cid}");
                        c
                    });
                    ch.corporation_id = corp_id;
                    ch.station_id = None;
                    ch.structure_id = None;
                    ch.solar_system_id = 30_000_144;
                    ch.active_ship_id = ship_id;
                    ch.balance = IskCents::from_isk(250_000_000);
                    snap.characters.insert(cid, ch);
                }

                let mut corp = { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = corp_id;
                    __corp.name = "Role Gate Test Corp".into();
                    __corp.ticker = "RGTC".into();
                    __corp.ceo_id = ceo_id;
                    __corp.tax_rate = 0.05;
                    __corp.members = HashSet::from([
                        ceo_id,
                        station_mgr_id,
                        accountant_id,
                        trader_id,
                        plain_member_id,
                    ]);
                    __corp.member_roles = member_roles;
                    __corp.hq_station_id = Some(jita_station);
                    __corp.set_division_balance(1, IskCents::from_isk(250_000_000));
                    __corp };
                corp.ensure_wallet_divisions();
                for d in &mut corp.wallet_divisions {
                    d.balance = IskCents::from_isk(250_000_000);
                }
                snap.corporations.insert(corp_id, corp);

                let mut ref_st = make_structure(observer_id, 35835, corp_id, 30_000_144, "Corp Moon Refinery");
                ref_st.position = perim_pos;
                ref_st.fuel_blocks = 1_000;
                snap.structures.insert(observer_id, ref_st);
                snap.items.insert(
                    960_001,
                    make_item(960_001, 45_009, corp_id, observer_id, InventoryFlag::ServiceSlot0, 1, true),
                );
                snap.module_online_states.insert(960_001, true);
                Ok::<(), ()>(())
            })
            .unwrap();

        // Populate moon extraction & observer mining ledger via live RPCs
        let mut ceo_sess = SessionState::new(9861);
        ceo_sess.user_id = Some(1);
        ceo_sess.character_id = Some(ceo_id);
        ceo_sess.corporation_id = Some(corp_id);
        ceo_sess.corp_role = ROLE_DIRECTOR | ROLE_STATION_MANAGER | ROLE_ACCOUNTANT | ROLE_TRADER;
        ceo_sess.solar_system_id = Some(30_000_144);
        ceo_sess.ship_id = Some(ship_id);

        let ext_args = [PyRep::Integer(observer_id), PyRep::Integer(518_400)];
        rpc_call(&ctx, &mut ceo_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;
        ctx.db.clock().advance_secs(520_000);
        let frac = rpc_call(
            &ctx,
            &mut ceo_sess,
            "moonExtractions",
            "FractureChunkWithStructure",
            &[PyRep::Integer(observer_id)],
        )
        .await;
        let ast_id = match frac.dict_get("asteroidIDs") {
            Some(PyRep::List(v)) => v[0].as_i64().expect("fractured asteroid ID"),
            other => panic!("unexpected asteroidIDs: {other:?}"),
        };
        let _ = ctx
            .systems
            .get_or_spawn(30_000_144)
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 606,
                position: DVec3::from_array(perim_pos),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        ctx.db.with_mut_snapshot(|snap| {
            if !snap.characters.contains_key(&ceo_id) {
                let mut ch = snap.characters.values().next().unwrap().clone();
                ch.character_id = ceo_id;
                ch.corporation_id = corp_id;
                ch.solar_system_id = 30_000_144;
                ch.active_ship_id = ship_id;
                snap.characters.insert(ceo_id, ch);
            }
            snap.skills.entry(ceo_id).or_default().insert(3386, eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 });
            snap.items.insert(ship_id, make_item(ship_id, 606, ceo_id, 30_000_144, InventoryFlag::None, 1, true));
            snap.items.insert(92_000_483, make_item(92_000_483, 483, ceo_id, ship_id, InventoryFlag::HiSlot0, 1, true));
            snap.module_online_states.insert(92_000_483, true);
        });
        ctx.invalidate_ship_fitting(ship_id);
        rpc_call(&ctx, &mut ceo_sess, "dogmaIM", "Mine", &[PyRep::Integer(ast_id)]).await;
        rpc_call(&ctx, &mut ceo_sess, "moonExtractions", "StartNewExtraction", &ext_args).await;

        // Populate active & historical corporate orders via marketProxy.PlaceBuyOrder + CancelCharOrder
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&ceo_id) {
                ch.station_id = Some(jita_station);
                ch.solar_system_id = 30_000_142;
            }
        });
        ceo_sess.station_id = Some(jita_station);
        ceo_sess.solar_system_id = Some(30_000_142);
        ceo_sess.region_id = Some(10_000_002);
        let corp_buy_args = [
            PyRep::Integer(jita_station),
            PyRep::Integer(36),
            PyRep::Real(25.0),
            PyRep::Integer(1_000),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(true),
            PyRep::Integer(1000),
        ];
        let active_rep = rpc_call(&ctx, &mut ceo_sess, "marketProxy", "PlaceBuyOrder", &corp_buy_args).await;
        let active_oid = active_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
        let hist_rep = rpc_call(&ctx, &mut ceo_sess, "marketProxy", "PlaceBuyOrder", &corp_buy_args).await;
        let hist_oid = hist_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();
        rpc_call(&ctx, &mut ceo_sess, "marketProxy", "CancelCharOrder", &[PyRep::Integer(hist_oid)])
            .await;

        // Populate customs office via posMgr.AnchorOrbital + OnlineOrbital + planetOrbitalRegistryBroker.UpdateSettings
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&ceo_id) {
                ch.station_id = None;
                ch.solar_system_id = lowsec_sys;
                ch.standings.insert(500_026, 2.0);
            }
            snap.items
                .insert(960_002, make_item(960_002, 3962, corp_id, ship_id, InventoryFlag::Cargo, 1, false));
            snap.items
                .insert(960_003, make_item(960_003, 2233, corp_id, ship_id, InventoryFlag::Cargo, 1, false));
        });
        ceo_sess.station_id = None;
        ceo_sess.solar_system_id = Some(lowsec_sys);
        let lowsec_planet = ctx.sde.celestials_in_system(lowsec_sys).into_iter().find(|c| c.celestial_id == lowsec_planet_id).unwrap();
        let near_planet = lowsec_planet.position + glam::DVec3::new(lowsec_planet.radius + 5_000.0, 0.0, 0.0);
        ctx.systems.get_or_spawn(lowsec_sys).spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: near_planet,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        }).await.unwrap();
        rpc_call(
            &ctx,
            &mut ceo_sess,
            "posMgr",
            "AnchorOrbital",
            &[PyRep::Integer(960_002), PyRep::Integer(lowsec_planet_id)],
        )
        .await;
        rpc_call(
            &ctx,
            &mut ceo_sess,
            "posMgr",
            "OnlineOrbital",
            &[PyRep::Integer(960_002), PyRep::Integer(960_003)],
        )
        .await;
        let poco_args = [
            PyRep::Integer(lowsec_planet_id),
            PyRep::Integer(18),
            PyRep::keyval([
                ("corporation", PyRep::Real(0.01)),
                ("alliance", PyRep::Real(0.02)),
                ("standingHigh", PyRep::Real(0.05)),
                ("standingGood", PyRep::Real(0.08)),
                ("standingNeutral", PyRep::Real(0.10)),
                ("standingBad", PyRep::Real(0.15)),
                ("standingHorrible", PyRep::Real(0.20)),
            ]),
            PyRep::Real(0.0),
            PyRep::Bool(true),
            PyRep::Bool(false),
        ];
        rpc_call(&ctx, &mut ceo_sess, "planetOrbitalRegistryBroker", "UpdateSettings", &poco_args)
            .await;

        let all_scopes = vec![
            "esi-industry.read_corporation_mining.v1",
            "esi-planets.read_customs_offices.v1",
            "esi-markets.read_corporation_orders.v1",
        ];
        let sm_tok = issue_token_with_scopes(&ctx, station_mgr_id, all_scopes.clone());
        let acc_tok = issue_token_with_scopes(&ctx, accountant_id, all_scopes.clone());
        let trd_tok = issue_token_with_scopes(&ctx, trader_id, all_scopes.clone());
        let dir_tok = issue_token_with_scopes(&ctx, ceo_id, all_scopes.clone());
        let mem_tok = issue_token_with_scopes(&ctx, plain_member_id, all_scopes);

        // 1. Station_Manager allowed on mining/extractions; Accountant & plain member rejected with 403
        let (st, body) = call_esi(
            &router,
            &format!("/v1/corporation/{corp_id}/mining/extractions/"),
            Some(&sm_tok),
        );
        assert_eq!(st, 200);
        assert_eq!(body.as_array().unwrap().len(), 1);
        assert_eq!(
            call_esi(
                &router,
                &format!("/v1/corporation/{corp_id}/mining/extractions/"),
                Some(&acc_tok),
            )
            .0,
            403
        );

        // 2. Accountant allowed on mining/observers & mining/observers/{id}; Station_Manager rejected with 403
        let (st, obs) = call_esi(
            &router,
            &format!("/v1/corporation/{corp_id}/mining/observers/"),
            Some(&acc_tok),
        );
        assert_eq!(st, 200);
        assert_eq!(obs[0]["observer_id"], json!(observer_id));
        let (st, obs_detail) = call_esi(
            &router,
            &format!("/v1/corporation/{corp_id}/mining/observers/{observer_id}/"),
            Some(&acc_tok),
        );
        assert_eq!(st, 200);
        assert!(obs_detail[0]["quantity"].as_i64().unwrap() > 0);
        assert_eq!(
            call_esi(
                &router,
                &format!("/v1/corporation/{corp_id}/mining/observers/"),
                Some(&sm_tok),
            )
            .0,
            403
        );

        // 3. Accountant and Trader allowed on corporations/{id}/orders & orders/history; Station_Manager rejected
        for tok in [&acc_tok, &trd_tok] {
            let (st, orders) = call_esi(
                &router,
                &format!("/v1/corporations/{corp_id}/orders/"),
                Some(tok),
            );
            assert_eq!(st, 200);
            assert_eq!(orders[0]["order_id"], json!(active_oid));
            let (st_h, hist) = call_esi(
                &router,
                &format!("/v1/corporations/{corp_id}/orders/history/"),
                Some(tok),
            );
            assert_eq!(st_h, 200);
            assert_eq!(hist[0]["order_id"], json!(hist_oid));
        }
        assert_eq!(
            call_esi(&router, &format!("/v1/corporations/{corp_id}/orders/"), Some(&sm_tok)).0,
            403
        );

        // 4. Director allowed on customs_offices; plain member rejected with 403
        let (st, pocos) = call_esi(
            &router,
            &format!("/v1/corporations/{corp_id}/customs_offices/"),
            Some(&dir_tok),
        );
        assert_eq!(st, 200);
        assert_eq!(pocos[0]["system_id"], json!(lowsec_sys));
        assert_eq!(
            call_esi(
                &router,
                &format!("/v1/corporations/{corp_id}/customs_offices/"),
                Some(&mem_tok),
            )
            .0,
            403
        );
    }

    #[tokio::test]
    async fn test_esi_market_history_prices_structures_and_industry_jobs() {
        // parity: esi:GET /industry/facilities, esi:GET /markets/{region_id}/history
        // parity: feat:esi.industry.facilities, feat:esi.market.history
        eve_testkit::attest(&[
            "esi:GET /industry/facilities",
            "esi:GET /markets/{region_id}/history",
            "feat:esi.industry.facilities",
            "feat:esi.market.history",
        ]);

        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));

        let seller_id = 90_000_001_i64;
        let buyer_id = 90_000_002_i64;
        let corp_id = 98_500_002_i64;
        let jita_station = 60_003_760_i64;
        let jita_system = 30_000_142_u32;
        let forge_region = 10_000_002_u32;
        let structure_id = 1_050_000_000_001_i64;
        let trit_item_id = 99_910_001_i64;
        let bpc_item_id = 99_910_002_i64;

        // 1. GET /industry/facilities includes SDE NPC industry stations with 0.0025 tax even with 0 active jobs
        let (st, facs) = call_esi(&router, "/v1/industry/facilities/", None);
        assert_eq!(st, 200);
        let jita_fac = facs
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["facility_id"] == json!(jita_station))
            .expect("Jita 4-4 NPC factory facility");
        assert_eq!(jita_fac["tax"], json!(0.0025));

        // 2. Setup corp, structure without Market Hub, items, and wallets
        ctx.db
            .try_with_mut_snapshot(|snap| {
                let member_roles = HashMap::from([(seller_id, ROLE_DIRECTOR | ROLE_FACTORY_MANAGER)]);
                if let Some(ch) = snap.characters.get_mut(&seller_id) {
                    ch.corporation_id = corp_id;
                    ch.station_id = Some(jita_station);
                    ch.solar_system_id = jita_system;
                    ch.balance = IskCents::from_isk(100_000_000);
                }
                if let Some(ch) = snap.characters.get_mut(&buyer_id) {
                    ch.station_id = Some(jita_station);
                    ch.solar_system_id = jita_system;
                    ch.balance = IskCents::from_isk(100_000_000);
                }
                snap.corporations.insert(
                    corp_id,
                    { let mut __corp = CorporationRecord::default();
                        __corp.corporation_id = corp_id;
                        __corp.name = "Industry Market Corp".into();
                        __corp.ticker = "IMC".into();
                        __corp.ceo_id = seller_id;
                        __corp.tax_rate = 0.05;
                        __corp.members = HashSet::from([seller_id]);
                        __corp.member_roles = member_roles;
                        __corp.hq_station_id = Some(jita_station);
                        __corp.set_division_balance(1, IskCents::from_isk(100_000_000));
                        __corp },
                );
                snap.structures.insert(
                    structure_id,
                    make_structure(structure_id, 35833, corp_id, jita_system, "Jita Fortizar"),
                );
                snap.items.insert(
                    trit_item_id,
                    make_item(trit_item_id, 39, seller_id, jita_station, InventoryFlag::Hangar, 50_000, false),
                );
                snap.items.insert(
                    bpc_item_id,
                    InventoryItemRecord {
                        item_id: bpc_item_id,
                        type_id: 691, // Rifter Blueprint
                        owner_id: seller_id,
                        location_id: jita_station,
                        flag: InventoryFlag::Hangar,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        is_blueprint_copy: true,
                        blueprint_runs_remaining: Some(5),
                        ..Default::default()
                    },
                );
                for (idx, (tid, qty)) in [
                    (34_u32, 30_000_i32),
                    (35, 10_000),
                    (36, 3_000),
                    (37, 1_000),
                    (38, 5_000),
                    (39, 5_000),
                    (40, 5_000),
                    (57_478, 200),
                    (57_479, 10),
                    (57_486, 100),
                ]
                .into_iter()
                .enumerate()
                {
                    let iid = 99_910_010 + idx as i64;
                    snap.items.insert(
                        iid,
                        make_item(iid, tid, seller_id, jita_station, InventoryFlag::Hangar, qty, false),
                    );
                }
                Ok::<(), ()>(())
            })
            .unwrap();

        // 3. Bare structure without Standup Market Hub I must return 403 on GET /markets/structures/{id}
        let mkt_tok = issue_token_with_scopes(&ctx, seller_id, vec!["esi-markets.structure_markets.v1"]);
        assert_eq!(
            call_esi(
                &router,
                &format!("/v1/markets/structures/{structure_id}/"),
                Some(&mkt_tok),
            )
            .0,
            403
        );
        // Fit and online Standup Market Hub I (type_id 35892) -> 200 OK
        ctx.db
            .try_with_mut_snapshot(|snap| {
                snap.items.insert(
                    99_910_099,
                    make_item(99_910_099, 35892, corp_id, structure_id, InventoryFlag::ServiceSlot0, 1, true),
                );
                Ok::<(), ()>(())
            })
            .unwrap();
        assert_eq!(
            call_esi(
                &router,
                &format!("/v1/markets/structures/{structure_id}/"),
                Some(&mkt_tok),
            )
            .0,
            200
        );

        // 4. Execute real market orders via marketProxy and verify GET /markets/{region_id}/history & /markets/prices
        let mut seller_sess = SessionState::default();
        seller_sess.user_id = Some(1);
        seller_sess.role = eve_db::accounts::service_roles::ROLE_PLAYER_DEFAULT;
        seller_sess.character_id = Some(seller_id);
        seller_sess.corporation_id = Some(corp_id);
        seller_sess.station_id = Some(jita_station);
        seller_sess.solar_system_id = Some(jita_system);

        let mut buyer_sess = SessionState::default();
        buyer_sess.user_id = Some(1);
        buyer_sess.role = eve_db::accounts::service_roles::ROLE_PLAYER_DEFAULT;
        buyer_sess.character_id = Some(buyer_id);
        buyer_sess.station_id = Some(jita_station);
        buyer_sess.solar_system_id = Some(jita_system);

        let sell_order_arg = PyRep::List(vec![PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(trit_item_id)),
            (PyRep::String("typeID".into()), PyRep::Integer(39)),
            (PyRep::String("price".into()), PyRep::Real(7.5)),
            (PyRep::String("quantity".into()), PyRep::Integer(10_000)),
            (PyRep::String("stationID".into()), PyRep::Integer(jita_station)),
            (PyRep::String("duration".into()), PyRep::Integer(90)),
        ])]);
        rpc_call(
            &ctx,
            &mut seller_sess,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[sell_order_arg, PyRep::Bool(false)],
        )
        .await;
        let buy_args = [
            PyRep::Integer(jita_station),
            PyRep::Integer(39),
            PyRep::Real(7.5),
            PyRep::Integer(4_000),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ];
        rpc_call(&ctx, &mut buyer_sess, "marketProxy", "PlaceBuyOrder", &buy_args).await;

        let (st, hist) = call_esi(
            &router,
            &format!("/v1/markets/{forge_region}/history/?type_id=39"),
            None,
        );
        assert_eq!(st, 200);
        let hist_arr = hist.as_array().unwrap();
        assert!(!hist_arr.is_empty());
        assert_eq!(hist_arr[0]["volume"], json!(4_500));
        let hist_avg = hist_arr[0]["average"].as_f64().unwrap();
        assert!(hist_avg > 7.5);

        let (st, prices) = call_esi(&router, "/v1/markets/prices/", None);
        assert_eq!(st, 200);
        let trit_p = prices
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["type_id"] == json!(39))
            .expect("Zydrine in /markets/prices");
        assert_eq!(trit_p["average_price"], json!(hist_avg));
        assert_eq!(trit_p["adjusted_price"], json!(hist_avg));

        // 5. Install and deliver personal & corporation industry jobs via industryManager and verify isolation
        let personal_dict = PyRep::Dict(vec![
            (PyRep::String("blueprintID".into()), PyRep::Integer(bpc_item_id)),
            (PyRep::String("activityID".into()), PyRep::Integer(1)),
            (PyRep::String("runs".into()), PyRep::Integer(1)),
            (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
            (PyRep::String("useCorp".into()), PyRep::Bool(false)),
        ]);
        let personal_rep =
            rpc_call(&ctx, &mut seller_sess, "industryManager", "InstallJob", &[personal_dict]).await;
        let job_id = personal_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID");
        ctx.db.clock().advance_secs(10_000);
        rpc_call(&ctx, &mut seller_sess, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)]).await;

        let corp_dict = PyRep::Dict(vec![
            (PyRep::String("blueprintID".into()), PyRep::Integer(bpc_item_id)),
            (PyRep::String("activityID".into()), PyRep::Integer(1)),
            (PyRep::String("runs".into()), PyRep::Integer(1)),
            (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
            (PyRep::String("useCorp".into()), PyRep::Bool(true)),
        ]);
        let corp_rep =
            rpc_call(&ctx, &mut seller_sess, "industryManager", "InstallJob", &[corp_dict]).await;
        let corp_job_id = corp_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("corp jobID");
        ctx.db.clock().advance_secs(10_000);
        rpc_call(&ctx, &mut seller_sess, "industryManager", "CompleteJob", &[PyRep::Integer(corp_job_id)]).await;

        let ind_tok = issue_token_with_scopes(
            &ctx,
            seller_id,
            vec![
                "esi-industry.read_character_jobs.v1",
                "esi-industry.read_corporation_jobs.v1",
            ],
        );
        let (st, char_jobs) = call_esi(
            &router,
            &format!("/v1/characters/{seller_id}/industry/jobs/"),
            Some(&ind_tok),
        );
        assert_eq!(st, 200);
        let char_arr = char_jobs.as_array().unwrap();
        let cj = char_arr
            .iter()
            .find(|j| j["job_id"] == json!(job_id))
            .expect("delivered personal job in character industry/jobs");
        assert!(!char_arr.iter().any(|j| j["job_id"] == json!(corp_job_id)));
        assert_eq!(cj["status"], json!("delivered"));
        assert_eq!(cj["blueprint_location_id"], json!(jita_station));
        assert_eq!(cj["output_location_id"], json!(jita_station));
        assert!(cj["cost"].as_f64().unwrap_or(0.0) > 0.0);
        assert!(cj["completed_date"].as_str().is_some());
        assert_eq!(cj["completed_character_id"], json!(seller_id));

        let (st, corp_jobs) = call_esi(
            &router,
            &format!("/v1/corporations/{corp_id}/industry/jobs/"),
            Some(&ind_tok),
        );
        assert_eq!(st, 200);
        let corp_arr = corp_jobs.as_array().unwrap();
        let crj = corp_arr
            .iter()
            .find(|j| j["job_id"] == json!(corp_job_id))
            .expect("delivered corp job in corp industry/jobs");
        assert!(!corp_arr.iter().any(|j| j["job_id"] == json!(job_id)));
        assert_eq!(crj["location_id"], json!(jita_station));
        assert_eq!(crj["completed_character_id"], json!(seller_id));
    }
}

pub(crate) mod esi_corp_mining_customs_offices {
    // Live HTTP + MachoNet RPC + SQLite persistence integration tests for 12 ESI routes.


    use crate::common;
    use crate::esi_market_industry_pi::{issue_token_with_scopes, make_item, make_structure};
    use eve_db::{
        corp_roles::{ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_STATION_MANAGER, ROLE_TRADER},
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
        let accountant_id = 90_000_003_i64;
        let director_id = 90_000_004_i64;
        let corp_id = 98_000_811_i64;
        let athanor_id = 1_000_000_811_001_i64;

        ctx.db.with_mut_snapshot(|snap| {
            let mut corp = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "B2 Mining & Customs Corp".into();
                __corp.ticker = "B2MC".into();
                __corp.ceo_id = ceo_id;
                __corp };
            corp.member_roles.insert(ceo_id, ROLE_DIRECTOR);
            corp.member_roles.insert(miner_id, ROLE_STATION_MANAGER);
            corp.member_roles.insert(accountant_id, ROLE_ACCOUNTANT);
            corp.member_roles.insert(director_id, ROLE_DIRECTOR);
            snap.corporations.insert(corp_id, corp);
            for cid in [ceo_id, miner_id] {
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    ch.corporation_id = corp_id;
                }
            }
            let template_char = snap.characters.get(&ceo_id).unwrap().clone();
            for (cid, name) in [(accountant_id, "B2 Accountant"), (director_id, "B2 Non-CEO Director")] {
                let mut ch = template_char.clone();
                ch.character_id = cid;
                ch.user_id = cid as u64;
                ch.name = name.into();
                ch.corporation_id = corp_id;
                snap.characters.insert(cid, ch);
            }
            snap.structures.insert(
                athanor_id,
                make_structure(athanor_id, 35835, corp_id, JITA_SYSTEM_ID, "B2 Moon Athanor"),
            );
            snap.items.insert(
                athanor_id + 1,
                make_item(athanor_id + 1, 45_009, corp_id, athanor_id, InventoryFlag::ServiceSlot0, 1, true),
            );
            snap.module_online_states.insert(athanor_id + 1, true);
        });

        let mut ceo_sess = setup_pilot(&ctx, 11, "demo", "demo-secret", ceo_id).await;
        let mut miner_sess = setup_pilot(&ctx, 12, "other", "other-secret", miner_id).await;

        // 1. Start moon extraction via moonExtractions.StartNewExtraction
        let start_res = rpc(&ctx, &mut ceo_sess, "moonExtractions", "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(864_000)]).await;
        assert_eq!(start_res.dict_get("status").and_then(|v| v.as_str()), Some("Started"));

        // 2. Advance time, fracture chunk, and mine via real mining laser cycle
        ctx.advance_time_seconds(864_000);
        let frac_res = rpc(&ctx, &mut ceo_sess, "moonExtractions", "FractureChunkWithStructure", &[PyRep::Integer(athanor_id)]).await;
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
        let lowsec_planet = ctx
            .sde
            .celestials_in_system(lowsec_sys)
            .into_iter()
            .find(|c| c.group_id == 7)
            .expect("lowsec planet");
        let lowsec_planet_id = lowsec_planet.celestial_id;
        let ceo_undock = rpc(&ctx, &mut ceo_sess, "stationSvc", "Undock", &[]).await;
        let ceo_ship_id = ceo_undock.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
        let mut ceo_ball = jita.get_ball_snapshot(ceo_ship_id).await.unwrap().unwrap().1;
        let _ = jita.remove_ball(ceo_ship_id).await;
        let (gantry_id, poco_item_id) = (800_000_813_i64, 800_000_814_i64);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&ceo_id) {
                ch.station_id = None;
                ch.solar_system_id = lowsec_sys;
                ch.standings.insert(500_026, 2.0);
            }
            if let Some(ship) = snap.items.get_mut(&ceo_ship_id) {
                ship.location_id = i64::from(lowsec_sys);
            }
            snap.items.insert(gantry_id, make_item(gantry_id, 3962, corp_id, ceo_ship_id, InventoryFlag::Cargo, 1, false));
            snap.items.insert(poco_item_id, make_item(poco_item_id, 2233, corp_id, ceo_ship_id, InventoryFlag::Cargo, 1, false));
        });
        ceo_sess.station_id = None;
        ceo_sess.solar_system_id = Some(lowsec_sys);
        let kw = HashMap::new();
        let lowsec = ctx.systems.get_or_spawn(lowsec_sys);
        assert_eq!(
            ctx.dispatch_rpc(&mut ceo_sess, "posMgr", None, "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(lowsec_planet_id)], &kw, None).await.unwrap_err().code,
            "ShipNotInSpace"
        );
        let near_planet = lowsec_planet.position + DVec3::new(lowsec_planet.radius + 5_000.0, 0.0, 0.0);
        ceo_ball.position = lowsec_planet.position + DVec3::new(lowsec_planet.radius + 250_000_000.0, 0.0, 0.0);
        lowsec.spawn_ball(ceo_ball.clone()).await.expect("spawn far ball");
        assert_eq!(
            ctx.dispatch_rpc(&mut ceo_sess, "posMgr", None, "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(lowsec_planet_id)], &kw, None).await.unwrap_err().code,
            "OrbitalOutOfRange"
        );
        lowsec.set_ball_position(ceo_ship_id, near_planet).await;
        let anchor_res = rpc(&ctx, &mut ceo_sess, "posMgr", "AnchorOrbital", &[PyRep::Integer(gantry_id), PyRep::Integer(lowsec_planet_id)]).await;
        let orbital_id = anchor_res.dict_get("orbitalID").and_then(|v| v.as_i64()).unwrap();
        assert_eq!(orbital_id, gantry_id);

        let _ = lowsec.remove_ball(ceo_ship_id).await;
        assert_eq!(
            ctx.dispatch_rpc(&mut ceo_sess, "posMgr", None, "OnlineOrbital", &[PyRep::Integer(orbital_id), PyRep::Integer(poco_item_id)], &kw, None).await.unwrap_err().code,
            "ShipNotInSpace"
        );
        ceo_ball.position = near_planet + DVec3::new(25_000.0, 0.0, 0.0);
        lowsec.spawn_ball(ceo_ball).await.expect("spawn far online ball");
        assert_eq!(
            ctx.dispatch_rpc(&mut ceo_sess, "posMgr", None, "OnlineOrbital", &[PyRep::Integer(orbital_id), PyRep::Integer(poco_item_id)], &kw, None).await.unwrap_err().code,
            "OrbitalOutOfRange"
        );
        lowsec.set_ball_position(ceo_ship_id, near_planet).await;
        let _ = rpc(&ctx, &mut ceo_sess, "posMgr", "OnlineOrbital", &[PyRep::Integer(orbital_id), PyRep::Integer(poco_item_id)]).await;
        let tax_kv = PyRep::keyval([
            ("corporation", PyRep::Real(0.02)),
            ("alliance", PyRep::Real(0.05)),
            ("standingHigh", PyRep::Real(0.05)),
            ("standingGood", PyRep::Real(0.08)),
            ("standingNeutral", PyRep::Real(0.12)),
        ]);
        assert_eq!(
            ctx.dispatch_rpc(&mut ceo_sess, "planetOrbitalRegistryBroker", None, "UpdateSettings", &[PyRep::Integer(999_999_999)], &kw, None).await.unwrap_err().code,
            "CrpAccessDenied"
        );
        let _ = rpc(&ctx, &mut ceo_sess, "planetOrbitalRegistryBroker", "UpdateSettings", &[
            PyRep::Integer(orbital_id), PyRep::Integer(18), tax_kv, PyRep::Real(1.0), PyRep::Bool(true), PyRep::Bool(true), PyRep::None,
        ]).await;
        let info_rep = rpc(&ctx, &mut ceo_sess, "planetOrbitalRegistryBroker", "GetSettingsInfo", &[PyRep::Integer(orbital_id)]).await;
        let PyRep::Tuple(info_t) = &info_rep else { panic!("GetSettingsInfo 6-tuple") };
        assert_eq!(info_t.len(), 6);
        assert!(info_t[0].as_i64().is_some() && matches!(info_t[1], PyRep::KeyVal(_)) && info_t[2].as_f64() == Some(1.0));
        assert_eq!((info_t[3].as_bool(), info_t[4].as_bool(), &info_t[5]), (Some(true), Some(true), &PyRep::None));

        let (addr, _router, handle) = EsiServer::spawn(Arc::clone(&ctx), "127.0.0.1:0").await.expect("spawn ESI");
        let spec = OpenApiSpec::pinned();
        let scopes = vec!["esi-industry.read_corporation_mining.v1", "esi-planets.read_customs_offices.v1"];
        let tok_station_mgr = issue_token_with_scopes(&ctx, miner_id, scopes.clone());
        let tok_accountant = issue_token_with_scopes(&ctx, accountant_id, scopes.clone());
        let tok_non_ceo_director = issue_token_with_scopes(&ctx, director_id, scopes.clone());
        let tok_director = issue_token_with_scopes(&ctx, ceo_id, scopes);

        let ext_path = format!("/v1/corporation/{corp_id}/mining/extractions/");
        let obs_path = format!("/v1/corporation/{corp_id}/mining/observers/");
        let poco_path = format!("/v1/corporations/{corp_id}/customs_offices/");

        // Auth / role / 404 checks
        http_expect_status(addr, &ext_path, None, 401).await;
        http_expect_status(addr, &obs_path, Some(&tok_station_mgr), 403).await;
        http_expect_status(addr, &poco_path, Some(&tok_accountant), 403).await;
        http_expect_status(addr, "/v1/corporation/999999999/mining/extractions/", Some(&tok_director), 403).await;

        // Station Manager succeeds on extractions; non-CEO Director succeeds on customs_offices
        let exts = http_get_json(addr, &spec, &ext_path, Some(&tok_station_mgr)).await;
        assert_eq!(exts.as_array().unwrap().len(), 1);
        assert_eq!(exts[0]["structure_id"], athanor_id);

        http_expect_status(addr, &poco_path, Some(&tok_station_mgr), 403).await;
        let pocos = http_get_json(addr, &spec, &poco_path, Some(&tok_non_ceo_director)).await;
        assert_eq!(pocos.as_array().unwrap().len(), 1);
        assert_eq!(pocos[0]["office_id"], orbital_id);
        assert_eq!(pocos[0]["system_id"], lowsec_sys);

        // Accountant (non-CEO, ROLE_ACCOUNTANT only) & non-CEO Director succeed on mining observers
        let obs = http_get_json(addr, &spec, &obs_path, Some(&tok_accountant)).await;
        let obs_id = obs[0]["observer_id"].as_i64().unwrap();
        let obs_detail_path = format!("/v1/corporation/{corp_id}/mining/observers/{obs_id}/");
        http_expect_status(addr, &obs_detail_path, Some(&tok_station_mgr), 403).await;
        http_expect_status(addr, &format!("/v1/corporation/{corp_id}/mining/observers/999999999/"), Some(&tok_accountant), 404).await;
        let entries = http_get_json(addr, &spec, &obs_detail_path, Some(&tok_accountant)).await;
        assert_eq!(entries[0]["character_id"], miner_id);
        assert_eq!(entries[0]["quantity"], mine_res.units_delivered);
        let entries_dir = http_get_json(addr, &spec, &obs_detail_path, Some(&tok_non_ceo_director)).await;
        assert_eq!(entries_dir[0]["quantity"], mine_res.units_delivered);

        handle.abort();

        // Reload from SQLite and verify persistence over live HTTP
        let ctx2 = Arc::new(crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "esi_b2_corp_mining_poco"));
        let (addr2, _router2, handle2) = EsiServer::spawn(Arc::clone(&ctx2), "127.0.0.1:0").await.expect("spawn ESI 2");
        let tok_dir2 = issue_token_with_scopes(&ctx2, ceo_id, vec!["esi-industry.read_corporation_mining.v1", "esi-planets.read_customs_offices.v1"]);
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
        // parity: esi:GET /markets/{region_id}/history, esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history
        eve_testkit::attest(&[
            "esi:GET /markets/{region_id}/history",
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
            let mut corp = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "B2 Market & Citadel Corp".into();
                __corp.ticker = "B2MC".into();
                __corp.ceo_id = ceo_id;
                __corp.set_division_balance(1, IskCents(50_000_000_00));
                __corp };
            corp.member_roles.insert(ceo_id, ROLE_DIRECTOR);
            corp.member_roles
                .insert(trader_id, ROLE_TRADER | eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]);
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

        // Corporation orders & history accessible by Accountant | Trader (with Director/CEO always allowed)
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
}

pub(crate) mod esi_fw_stats_systems_leaderboards {
    // Live ESI HTTP integration tests for Batch 6 parity closure:
    // - Faction Warfare (`/fw/stats`, `/fw/systems`, `/fw/leaderboards`, `/fw/leaderboards/characters`,
    //   `/fw/leaderboards/corporations`, `/fw/wars`, `/characters/{character_id}/fw/stats`,
    //   `/corporations/{corporation_id}/fw/stats`)
    // - Incursions & Sovereignty (`/incursions`, `/sovereignty/campaigns`, `/sovereignty/structures`)
    // - Loyalty Store Offers (`/loyalty/stores/{corporation_id}/offers`)

    use std::collections::HashMap;
    use std::sync::Arc;

    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
    use eve_server::router::ServerContext;
    use eve_server::services::combat_crimewatch::process_ship_destruction_with_aggression;
    use eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed;
    use eve_server::services::pve_special_sites::{ensure_special_sites_spawned, with_pve_special_reg};
    use eve_server::services::sov_services::{
        claim_system_sovereignty, record_hub_hack, TYPE_SOVEREIGNTY_HUB,
    };
    use serde_json::Value;

    use crate::common;

    const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn issue_token_with_scopes(
        ctx: &ServerContext,
        uname: &str,
        character_id: i64,
        scopes: Vec<String>,
    ) -> String {
        let now = ctx.now_unix();
        let code = ctx
            .db
            .authorize_pkce(
                uname,
                "secret",
                character_id,
                "evomu-esi-b6-test",
                "https://localhost/callback",
                scopes,
                PKCE_CHALLENGE,
                "S256",
                now,
            )
            .expect("authorize_pkce");
        ctx.db
            .exchange_pkce_code(&code, "evomu-esi-b6-test", PKCE_VERIFIER, now)
            .expect("exchange_pkce_code")
            .access_token
    }

    fn validate_and_json(
        router: &EsiRouter,
        spec: &OpenApiSpec,
        expected_status: u16,
        req: &EsiHttpRequest,
    ) -> (Value, HashMap<String, String>) {
        let resp = router.handle_request(req);
        assert_eq!(resp.status_code, expected_status, "path={}", req.raw_target);
        let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
        spec.validate_response(&req.method, raw_path, resp.status_code, &resp.headers, &resp.body)
            .unwrap_or_else(|e| panic!("OpenAPI validation failed for {}: {e}", req.raw_target));
        let json_val = if resp.status_code == 304 || resp.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&resp.body).expect("Valid JSON response body")
        };
        (json_val, resp.headers)
    }

    async fn create_pilot_in_new_corp(
        ctx: &ServerContext,
        uname: &str,
        cname: &str,
        ticker: &str,
    ) -> (i64, i64, SessionState) {
        let uid = ctx.db.provision_player_account(uname, "secret");
        let char_id = ctx
            .db
            .create_character(uid, cname, 1, 1, 11, 17, &ctx.sde)
            .expect("create_character")
            .character_id;
        let mut sess = SessionState::new(char_id as u64);
        let kw = HashMap::new();
        ctx.dispatch_rpc(
            &mut sess,
            "authentication",
            None,
            "Login",
            &[PyRep::String(uname.into()), PyRep::String("secret".into())],
            &kw,
            None,
        )
        .await
        .expect("Login");
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
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.balance = eve_db::IskCents::from_isk(10_000_000);
            }
            snap.skills.entry(char_id).or_default().insert(
                3363,
                eve_db::TrainedSkillRecord {
                    type_id: 3363,
                    level: 1,
                    skill_points: 250.0,
                },
            );
        });
        ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(
            ctx.now_unix() + 15,
        )));
        let corp_res = ctx
            .dispatch_rpc(
                &mut sess,
                "corpRegistry",
                None,
                "AddCorporation",
                &[
                    PyRep::String(format!("{cname} Corp").into()),
                    PyRep::String(ticker.into()),
                    PyRep::String("Test Corp".into()),
                    PyRep::String("".into()),
                    PyRep::Real(0.05),
                    PyRep::Bool(false),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                ],
                &kw,
                None,
            )
            .await
            .expect("AddCorporation");
        let corp_id = match corp_res {
            PyRep::Integer(id) => id,
            _ => sess.corporation_id.unwrap_or(0),
        };
        (char_id, corp_id, sess)
    }

    #[tokio::test]
    async fn test_esi_b6_fw_stats_systems_leaderboards_and_wars_live_rpc_driven() {
        // parity: esi:GET /fw/wars
        eve_testkit::attest(&[
            "esi:GET /fw/wars",
        ]);

        let db_path = std::env::temp_dir().join(format!("esi_b6_fw_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&db_path);
        let ctx = Arc::new(ServerContext::from_sqlite(&db_path).expect("sqlite ctx"));
        common::pin_clock(&ctx);

        let (char_a, corp_a, mut sess_a) =
            create_pilot_in_new_corp(&ctx, "fw_pilot_a", "FW Pilot A", "FWPA").await;
        let (char_b, _corp_b, mut sess_b) =
            create_pilot_in_new_corp(&ctx, "fw_pilot_b", "FW Pilot B", "FWPB").await;
        let (char_u, corp_u, _sess_u) =
            create_pilot_in_new_corp(&ctx, "fw_unenlisted", "Unenlisted Pilot", "UNEN").await;

        let kw = HashMap::new();
        // Enlist Pilot A's corp in Caldari State (500001) and Pilot B in Gallente Federation (500004)
        ctx.dispatch_rpc(
            &mut sess_a,
            "facWarMgr",
            None,
            "JoinFactionAsCorporation",
            &[PyRep::Integer(500_001)],
            &kw,
            None,
        )
        .await
        .expect("JoinFactionAsCorporation");
        ctx.dispatch_rpc(
            &mut sess_b,
            "facWarMgr",
            None,
            "JoinFactionAsCharacter",
            &[PyRep::Integer(500_004)],
            &kw,
            None,
        )
        .await
        .expect("JoinFactionAsCharacter");

        // Pick a Gallente-occupied FW warzone system and donate LP as Caldari to advance victory points
        let fw_sys_id = ctx.db.with_snapshot(|snap| {
            snap.fw_system_occupations
                .values()
                .find(|o| o.occupier_faction_id == 500_004)
                .map(|o| o.solar_system_id)
                .expect("Gallente FW system")
        });
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_b) {
                ch.loyalty_points.insert(1_000_181, 5_000);
                ch.solar_system_id = fw_sys_id;
            }
        });
        sess_b.solar_system_id = Some(fw_sys_id);
        ctx.dispatch_rpc(
            &mut sess_b,
            "facWarMgr",
            None,
            "DonateLPsToSolarSystem",
            &[PyRep::Integer(fw_sys_id as i64), PyRep::Integer(1_000)],
            &kw,
            None,
        )
        .await
        .expect("DonateLPsToSolarSystem");
        eve_server::services::fw_services::add_victory_points(&ctx, fw_sys_id, 500);

        // Record a live PvP killmail between opposing FW pilots -> combat hook populates FW kills, VP, and LP
        process_ship_destruction_with_aggression(
            &ctx,
            char_b,
            880_000_001,
            587,
            char_a,
            2873,
            fw_sys_id,
            glam::DVec3::new(100.0, 200.0, 300.0),
            false,
        );

        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        // 1. /fw/wars
        let (wars, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/fw/wars/"));
        let wars_arr = wars.as_array().expect("wars array");
        assert!(
            wars_arr.iter().any(|w| w["faction_id"] == 500_001 && w["against_id"] == 500_004),
            "Caldari vs Gallente war pair must be present"
        );

        // 2. /fw/systems
        let (systems, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/fw/systems/"));
        let sys_entry = systems
            .as_array()
            .expect("systems array")
            .iter()
            .find(|s| s["solar_system_id"] == fw_sys_id as i64)
            .expect("fw system entry");
        assert_eq!(sys_entry["occupier_faction_id"], 500_004);
        assert!(sys_entry["victory_points"].as_i64().unwrap_or(0) > 0);
        assert_eq!(sys_entry["contested"], "contested");

        // 3. /fw/stats
        let (stats, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/fw/stats/"));
        let cal_stats = stats
            .as_array()
            .expect("stats array")
            .iter()
            .find(|s| s["faction_id"] == 500_001)
            .expect("Caldari stats");
        assert!(cal_stats["pilots"].as_i64().unwrap_or(0) >= 1);
        assert!(cal_stats["kills"]["total"].as_i64().unwrap_or(0) >= 1);

        // 4. /fw/leaderboards, /fw/leaderboards/characters, /fw/leaderboards/corporations
        let (lb, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/fw/leaderboards/"));
        assert!(lb["kills"]["active_total"].is_array());
        let (lb_chars, _) = validate_and_json(
            &router,
            &spec,
            200,
            &EsiHttpRequest::new("GET", "/v1/fw/leaderboards/characters/"),
        );
        assert!(lb_chars["kills"]["active_total"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["character_id"] == char_a));
        let (lb_corps, _) = validate_and_json(
            &router,
            &spec,
            200,
            &EsiHttpRequest::new("GET", "/v1/fw/leaderboards/corporations/"),
        );
        assert!(lb_corps["kills"]["active_total"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["corporation_id"] == corp_a));

        // 5. /characters/{character_id}/fw/stats: 401, 403, 200, 304, 404
        let char_token = issue_token_with_scopes(
            &ctx,
            "fw_pilot_a",
            char_a,
            vec![
                "esi-characters.read_fw_stats.v1".to_string(),
                "esi-corporations.read_fw_stats.v1".to_string(),
            ],
        );
        let no_scope_token = issue_token_with_scopes(&ctx, "fw_pilot_a", char_a, vec![]);

        let char_path = format!("/v1/characters/{char_a}/fw/stats/");
        validate_and_json(&router, &spec, 401, &EsiHttpRequest::new("GET", &char_path));
        validate_and_json(
            &router,
            &spec,
            403,
            &EsiHttpRequest::new("GET", &char_path).with_bearer_token(&no_scope_token),
        );
        let (char_fw, headers) = validate_and_json(
            &router,
            &spec,
            200,
            &EsiHttpRequest::new("GET", &char_path).with_bearer_token(&char_token),
        );
        assert_eq!(char_fw["faction_id"], 500_001);
        assert!(char_fw["enlisted_on"].is_string());
        assert!(char_fw["kills"]["total"].as_i64().unwrap_or(0) >= 1);

        let etag = headers.get("ETag").or_else(|| headers.get("etag")).cloned().expect("ETag");
        let mut cached_req = EsiHttpRequest::new("GET", &char_path).with_bearer_token(&char_token);
        cached_req.headers.insert("if-none-match".to_string(), etag);
        validate_and_json(&router, &spec, 304, &cached_req);

        validate_and_json(
            &router,
            &spec,
            403,
            &EsiHttpRequest::new("GET", "/v1/characters/999999999/fw/stats/").with_bearer_token(&char_token),
        );
        let unenlisted_token = issue_token_with_scopes(
            &ctx,
            "fw_unenlisted",
            char_u,
            vec![
                "esi-characters.read_fw_stats.v1".to_string(),
                "esi-corporations.read_fw_stats.v1".to_string(),
            ],
        );
        let (unenlisted_char_fw, _) = validate_and_json(
            &router,
            &spec,
            200,
            &EsiHttpRequest::new("GET", &format!("/v1/characters/{char_u}/fw/stats/"))
                .with_bearer_token(&unenlisted_token),
        );
        assert!(unenlisted_char_fw.get("faction_id").is_none());
        assert!(unenlisted_char_fw.get("enlisted_on").is_none());
        assert_eq!(unenlisted_char_fw["kills"]["total"], 0);
        assert_eq!(unenlisted_char_fw["victory_points"]["total"], 0);

        // 6. /corporations/{corporation_id}/fw/stats: 401, 403, 200, 404
        let corp_path = format!("/v1/corporations/{corp_a}/fw/stats/");
        validate_and_json(&router, &spec, 401, &EsiHttpRequest::new("GET", &corp_path));
        validate_and_json(
            &router,
            &spec,
            403,
            &EsiHttpRequest::new("GET", &corp_path).with_bearer_token(&no_scope_token),
        );
        let (corp_fw, _) = validate_and_json(
            &router,
            &spec,
            200,
            &EsiHttpRequest::new("GET", &corp_path).with_bearer_token(&char_token),
        );
        assert_eq!(corp_fw["faction_id"], 500_001);
        assert!(corp_fw["pilots"].as_i64().unwrap_or(0) >= 1);
        assert!(corp_fw["enlisted_on"].is_string());

        validate_and_json(
            &router,
            &spec,
            403,
            &EsiHttpRequest::new("GET", "/v1/corporations/999999999/fw/stats/").with_bearer_token(&char_token),
        );
        let (unenlisted_corp_fw, _) = validate_and_json(
            &router,
            &spec,
            200,
            &EsiHttpRequest::new("GET", &format!("/v1/corporations/{corp_u}/fw/stats/"))
                .with_bearer_token(&unenlisted_token),
        );
        assert!(unenlisted_corp_fw.get("faction_id").is_none());
        assert!(unenlisted_corp_fw.get("enlisted_on").is_none());
        assert_eq!(unenlisted_corp_fw["pilots"], 0);
        assert_eq!(unenlisted_corp_fw["kills"]["total"], 0);
        assert_eq!(unenlisted_corp_fw["victory_points"]["total"], 0);

        // 7. SQLite persistence across ServerContext reload
        drop(router);
        drop(ctx);
        let ctx_reloaded = Arc::new(ServerContext::from_sqlite(&db_path).expect("reload sqlite"));
        let reloaded_token = issue_token_with_scopes(
            &ctx_reloaded,
            "fw_pilot_a",
            char_a,
            vec!["esi-characters.read_fw_stats.v1".to_string()],
        );
        let router_reloaded = EsiRouter::new(ctx_reloaded);
        let (char_fw_after, _) = validate_and_json(
            &router_reloaded,
            &spec,
            200,
            &EsiHttpRequest::new("GET", &char_path).with_bearer_token(&reloaded_token),
        );
        assert_eq!(char_fw_after["faction_id"], 500_001);
        assert!(char_fw_after["kills"]["total"].as_i64().unwrap_or(0) >= 1);
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_esi_b6_incursions_and_sovereignty_campaigns_structures_live_state() {
        let db_path = std::env::temp_dir().join(format!("esi_b6_sov_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&db_path);
        let ctx = Arc::new(ServerContext::from_sqlite(&db_path).expect("sqlite ctx"));
        common::pin_clock(&ctx);

        // 1. Seed live SDE-topology incursions via ensure_special_sites_spawned
        ensure_special_sites_spawned(&ctx).await;
        let (cid, staging_sys, site_id, initial_inf) = with_pve_special_reg(&ctx, |reg| {
            let (&cid, inc) = reg.incursions.iter().next().expect("seeded incursion");
            let site = inc.sites.iter().find(|s| !s.locked_until_low_influence).expect("unlocked site");
            (cid, site.solar_system_id, site.site_id.clone(), inc.influence)
        });

        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        let (inc_list, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/incursions/"));
        let inc_arr = inc_list.as_array().expect("incursions array");
        assert_eq!(inc_arr.len(), 3);
        let target_inc = inc_arr
            .iter()
            .find(|i| i["constellation_id"] == cid as i64)
            .expect("target incursion");
        assert_eq!(target_inc["faction_id"], 500_019);
        assert_eq!(target_inc["type"], "Incursion");

        // Complete an incursion site and verify influence decreases via ESI
        let (pilot_id, _, _) = create_pilot_in_new_corp(&ctx, "inc_pilot", "Inc Pilot", "INCP").await;
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&pilot_id) {
                ch.solar_system_id = staging_sys;
            }
        });
        let boss_id = common::clear_incursion_site_escort_waves(&ctx, staging_sys, &site_id).await;
        let _ = ctx.systems.get_or_spawn(staging_sys).remove_ball(boss_id).await;
        on_incursion_boss_destroyed(&ctx, staging_sys, boss_id, Some(pilot_id)).await;

        let (inc_after, _) = validate_and_json(&router, &spec, 200, &EsiHttpRequest::new("GET", "/v1/incursions/"));
        let after_inf = inc_after
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["constellation_id"] == cid as i64)
            .map(|i| i["influence"].as_f64().unwrap())
            .unwrap_or(0.0);
        assert!(after_inf < initial_inf, "Incursion influence must decrease after site completion");

        // 2. Anchor a Sovereignty Hub in nullsec, claim sovereignty, and hack it to start a campaign
        let (_char_s, corp_s, _) =
            create_pilot_in_new_corp(&ctx, "sov_pilot", "Sov Pilot", "SOVP").await;
        let null_sys = ctx
            .sde
            .solar_systems
            .values()
            .find(|s| s.security_status <= -0.1)
            .map(|s| s.solar_system_id)
            .expect("nullsec system");
        let alliance_id = 99_000_601_i64;
        let hub_id = 77_000_601_i64;
        let now = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.system_states.entry(null_sys).or_default();
            if let Some(c) = snap.corporations.get_mut(&corp_s) {
                c.alliance_id = Some(alliance_id);
            }
            snap.structures.insert(
                hub_id,
                eve_db::UpwellStructureRecord {
                    structure_id: hub_id,
                    type_id: TYPE_SOVEREIGNTY_HUB,
                    owner_corp_id: corp_s,
                    solar_system_id: null_sys,
                    name: "B6 Sov Hub".into(),
                    position: [0.0, 0.0, 0.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 1_000,
                    shield_hp: 100_000.0,
                    armor_hp: 100_000.0,
                    hull_hp: 100_000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            snap.structure_vulnerability_windows.insert(hub_id, (now + 3600, now + 7200));
        });
        claim_system_sovereignty(&ctx, null_sys, alliance_id, hub_id, corp_s)
            .expect("claim_system_sovereignty");

        let (sov_structs, _) = validate_and_json(
            &router,
            &spec,
            200,
            &EsiHttpRequest::new("GET", "/v1/sovereignty/structures/"),
        );
        let hub_entry = sov_structs
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["structure_id"] == hub_id)
            .expect("sov hub in /sovereignty/structures");
        assert_eq!(hub_entry["alliance_id"], alliance_id);
        assert_eq!(hub_entry["solar_system_id"], null_sys as i64);

        // Hack the hub via live sov_services::record_hub_hack -> creates campaign & scores
        record_hub_hack(&ctx, null_sys, hub_id, 3600);
        let (campaigns, _) = validate_and_json(
            &router,
            &spec,
            200,
            &EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"),
        );
        let camp = campaigns
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["structure_id"] == hub_id)
            .expect("campaign in /sovereignty/campaigns");
        assert_eq!(camp["event_type"], "ihub_defense");
        assert_eq!(camp["defender_id"], alliance_id);
        assert_eq!(camp["attackers_score"], 0.35);
        assert_eq!(camp["defender_score"], 0.65);

        // 3. SQLite persistence across reload
        drop(router);
        drop(ctx);
        let ctx_reloaded = Arc::new(ServerContext::from_sqlite(&db_path).expect("reload sqlite"));
        let router_reloaded = EsiRouter::new(ctx_reloaded);
        let (campaigns_after, _) = validate_and_json(
            &router_reloaded,
            &spec,
            200,
            &EsiHttpRequest::new("GET", "/v1/sovereignty/campaigns/"),
        );
        assert!(campaigns_after
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["structure_id"] == hub_id));
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_esi_b6_loyalty_store_offers_sde_and_rpc_parity() {
        // parity: esi:GET /loyalty/stores/{corporation_id}/offers
        eve_testkit::attest(&["esi:GET /loyalty/stores/{corporation_id}/offers"]);

        let ctx = Arc::new(ServerContext::new());
        common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        // 1. Unseeded Paragon (1000419) & CONCORD (1000125) catalogs
        let (paragon_offers, _) = validate_and_json(
            &router,
            &spec,
            200,
            &EsiHttpRequest::new("GET", "/v1/loyalty/stores/1000419/offers/"),
        );
        let paragon_arr = paragon_offers.as_array().expect("Paragon offers array");
        assert!(!paragon_arr.is_empty(), "Paragon V24 SDE offers must be non-empty");
        assert!(paragon_arr[0]["offer_id"].as_i64().unwrap_or(0) > 0);
        assert!(paragon_arr[0]["type_id"].as_i64().unwrap_or(0) > 0);

        let (concord_offers, _) = validate_and_json(
            &router,
            &spec,
            200,
            &EsiHttpRequest::new("GET", "/v1/loyalty/stores/1000125/offers/"),
        );
        assert!(
            concord_offers.as_array().unwrap().len() >= 200,
            "CONCORD catalog must be non-empty"
        );

        // 2. Verify LPStoreMgr.GetAvailableOffersFromCorp RPC parity with ESI
        let (_char_lp, _corp_lp, mut sess) =
            create_pilot_in_new_corp(&ctx, "lp_pilot", "LP Pilot", "LPPP").await;
        let kw = HashMap::new();
        let rpc_res = ctx
            .dispatch_rpc(
                &mut sess,
                "LPStoreMgr",
                None,
                "GetAvailableOffersFromCorp",
                &[PyRep::Integer(1_000_419)],
                &kw,
                None,
            )
            .await
            .expect("LPStoreMgr.GetAvailableOffersFromCorp");
        let PyRep::List(rpc_offers) = rpc_res else {
            panic!("Expected PyRep::List from GetAvailableOffersFromCorp");
        };
        assert_eq!(rpc_offers.len(), paragon_arr.len());

        // 3. 404 on unknown corporation
        validate_and_json(
            &router,
            &spec,
            404,
            &EsiHttpRequest::new("GET", "/v1/loyalty/stores/999999999/offers/"),
        );
    }
}

pub(crate) mod esi_char_public_contracts_live {
    // `ESI-6` live HTTP + RPC + SQLite persistence integration tests for Contracts and Industry routes.


    use crate::esi_market_industry_pi::{assert_status, get_json, issue_token_with_scopes, validate_and_json};
    use eve_db::{
        corp_roles::{ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR, ROLE_FACTORY_MANAGER},
        CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
        UpwellStructureRecord, WorldRepository,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
    use eve_server::router::ServerContext;
    use eve_server::services::industry_pi::industry_facilities_bps::save_facility_taxes;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn temp_sqlite_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("evomu_p6_esi1_{tag}_{nanos}.sqlite"))
    }

    fn open_seeded_sqlite_ctx(tag: &str) -> (std::path::PathBuf, Arc<ServerContext>) {
        let db_path = temp_sqlite_path(tag);
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
        let ctx = Arc::new(ServerContext::with_repository(repo));
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        ctx.db
            .try_with_mut_snapshot(|snap| {
                *snap = demo;
                Ok::<_, std::convert::Infallible>(())
            })
            .unwrap();
        (db_path, ctx)
    }

    fn setup_session(ctx: &ServerContext, char_id: i64, corp_id: i64, station_id: i64, roles: u64) -> SessionState {
        let mut sess = SessionState::default();
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.station_id = Some(station_id);
        sess.solar_system_id = Some(30_000_142);
        sess.region_id = Some(10_000_002);
        sess.corp_role = roles;
        let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.corporation_id = corp_id;
                ch.station_id = Some(station_id);
                ch.solar_system_id = 30_000_142;
            }
            if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                corp.members.insert(char_id);
                corp.member_roles.insert(char_id, roles);
            }
            Ok(())
        });
        sess
    }

    fn seed_hangar_item(ctx: &ServerContext, owner_id: i64, station_id: i64, type_id: u32, qty: i32) -> i64 {
        ctx.db
            .try_with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let iid = snap.next_entity_id;
                snap.items.insert(
                    iid,
                    InventoryItemRecord {
                        item_id: iid,
                        type_id,
                        owner_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: qty,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
                Ok::<_, std::convert::Infallible>(iid)
            })
            .unwrap()
    }

    // parity: esi:GET /characters/{character_id}/contracts, esi:GET /characters/{character_id}/contracts/{contract_id}/bids, esi:GET /characters/{character_id}/contracts/{contract_id}/items, esi:GET /contracts/public/{region_id}, esi:GET /contracts/public/bids/{contract_id}, esi:GET /contracts/public/items/{contract_id}
    #[tokio::test]
    async fn test_esi_p6_char_and_public_contracts_live_rpc_and_sqlite_persistence() {
        let (db_path, ctx) = open_seeded_sqlite_ctx("char_public_contracts");
        let router = EsiRouter::new(ctx.clone());
        let spec = OpenApiSpec::pinned();
        let empty = HashMap::new();

        let station_id = 60_003_760_i64;
        let region_id = 10_000_002_i64;
        let (char1, corp1, char2, corp2) = ctx.db.with_snapshot(|snap| {
            let mut chars: Vec<_> = snap.characters.values().collect();
            chars.sort_by_key(|c| c.character_id);
            (
                chars[0].character_id,
                chars[0].corporation_id,
                chars[1].character_id,
                chars[1].corporation_id,
            )
        });

        let mut sess1 = setup_session(&ctx, char1, corp1, station_id, 0);
        let mut sess2 = setup_session(&ctx, char2, corp2, station_id, 0);
        let item_id = seed_hangar_item(&ctx, char1, station_id, 34, 500);

        let c_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "contractProxy",
                None,
                "CreateContract",
                &[
                    PyRep::Integer(2), // Auction
                    PyRep::Integer(0), // Public
                    PyRep::Integer(0),
                    PyRep::Integer(1440),
                    PyRep::Integer(1),
                    PyRep::Integer(station_id),
                    PyRep::Integer(station_id),
                    PyRep::Real(100_000.0),
                    PyRep::Real(0.0),
                    PyRep::Real(500_000.0),
                    PyRep::String("P6 Tritanium Auction".into()),
                    PyRep::String("Live RPC auction".into()),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(item_id),
                        PyRep::Integer(500),
                    ])]),
                ],
                &empty,
                None,
            )
            .await
            .expect("CreateContract");
        let PyRep::KeyVal(kv) = c_res else {
            panic!("expected KeyVal from CreateContract");
        };
        let cid = kv.get("contractID").unwrap().as_i64().unwrap();

        ctx.dispatch_rpc(
            &mut sess2,
            "contractProxy",
            None,
            "PlaceBid",
            &[PyRep::Integer(cid), PyRep::Real(125_000.0)],
            &empty,
            None,
        )
        .await
        .expect("PlaceBid");

        let tok1 = issue_token_with_scopes(&ctx, char1, vec!["esi-contracts.read_character_contracts.v1"]);
        let tok2 = issue_token_with_scopes(&ctx, char2, vec!["esi-contracts.read_character_contracts.v1"]);
        let bad_scope = issue_token_with_scopes(&ctx, char1, vec!["esi-wallet.read_character_wallet.v1"]);

        // Auth / 401 / 403 / 404 checks
        assert_status(&router, format!("/v1/characters/{char1}/contracts/"), None, 401);
        assert_status(&router, format!("/v1/characters/{char1}/contracts/"), Some(&bad_scope), 403);
        assert_status(&router, format!("/v1/characters/{char1}/contracts/"), Some(&tok2), 403);
        assert_status(&router, "/v1/characters/999999999/contracts/", Some(&tok1), 403);
        assert_status(&router, format!("/v1/characters/{char1}/contracts/999999999/items/"), Some(&tok1), 404);
        assert_status(&router, format!("/v1/characters/{char1}/contracts/999999999/bids/"), Some(&tok1), 404);
        assert_status(&router, "/v1/contracts/public/999999999/", None, 404);

        // Character contracts, items, bids
        let char_contracts = get_json(&router, &spec, format!("/v1/characters/{char1}/contracts/"), Some(&tok1));
        let found = char_contracts
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["contract_id"] == cid)
            .expect("contract in char list");
        assert_eq!(found["type"], "auction");
        assert_eq!(found["status"], "outstanding");
        assert_eq!(found["title"], "P6 Tritanium Auction");

        let items_req = EsiHttpRequest::new("GET", format!("/v1/characters/{char1}/contracts/{cid}/items/"))
            .with_bearer_token(&tok1);
        let items_resp = router.handle_request(&items_req);
        assert_eq!(items_resp.status_code, 200);
        assert!(items_resp.header("X-Pages").is_none());
        let char_items = validate_and_json(&router, &spec, &items_req, 200);
        assert_eq!(char_items.as_array().unwrap().len(), 1);
        assert_eq!(char_items[0]["type_id"], 34);
        assert_eq!(char_items[0]["quantity"], 500);

        let char_bids = get_json(&router, &spec, format!("/v1/characters/{char1}/contracts/{cid}/bids/"), Some(&tok1));
        assert_eq!(char_bids.as_array().unwrap().len(), 1);
        assert_eq!(char_bids[0]["amount"], 125_000.0);
        assert_eq!(char_bids[0]["bidder_id"], char2);

        // Public contracts, items, bids
        let pub_contracts = get_json(&router, &spec, format!("/v1/contracts/public/{region_id}/"), None);
        assert!(pub_contracts.as_array().unwrap().iter().any(|c| c["contract_id"] == cid));

        let pub_items = get_json(&router, &spec, format!("/v1/contracts/public/items/{cid}/"), None);
        assert_eq!(pub_items.as_array().unwrap().len(), 1);
        assert_eq!(pub_items[0]["type_id"], 34);

        let pub_bids = get_json(&router, &spec, format!("/v1/contracts/public/bids/{cid}/"), None);
        assert_eq!(pub_bids.as_array().unwrap().len(), 1);
        assert_eq!(pub_bids[0]["amount"], 125_000.0);

        // SQLite persistence check across repository reopen
        drop(router);
        drop(ctx);
        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        let router2 = EsiRouter::new(ctx2);
        let pub_bids2 = get_json(&router2, &spec, format!("/v1/contracts/public/bids/{cid}/"), None);
        assert_eq!(pub_bids2.as_array().unwrap().len(), 1);
        assert_eq!(pub_bids2[0]["amount"], 125_000.0);
        let _ = std::fs::remove_file(&db_path);
    }

    // parity: esi:GET /corporations/{corporation_id}/contracts, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items
    #[tokio::test]
    async fn test_esi_p6_corp_contracts_live_rpc_auth_and_sqlite_persistence() {
        let (db_path, ctx) = open_seeded_sqlite_ctx("corp_contracts");
        let router = EsiRouter::new(ctx.clone());
        let spec = OpenApiSpec::pinned();
        let empty = HashMap::new();

        let station_id = 60_003_760_i64;
        let corp1 = 98_000_001_i64;
        let member_char = 90_000_777_i64;
        let (char1, char2, corp2) = ctx.db.with_snapshot(|snap| {
            let mut chars: Vec<_> = snap.characters.values().collect();
            chars.sort_by_key(|c| c.character_id);
            (
                chars[0].character_id,
                chars[1].character_id,
                chars[1].corporation_id,
            )
        });
        let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
            let mut plain_member = snap.characters[&char1].clone();
            plain_member.character_id = member_char;
            plain_member.name = "P6 Ordinary Member".to_string();
            plain_member.corporation_id = corp1;
            snap.characters.insert(member_char, plain_member);

            let mut corp = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp1;
                __corp.name = "P6 Corp".to_string();
                __corp.ticker = "P6C".to_string();
                __corp.ceo_id = char1;
                __corp.set_division_balance(1, IskCents::from_isk(500_000_000));
                __corp };
            corp.ensure_wallet_divisions();
            corp.members.insert(member_char);
            corp.member_roles.insert(member_char, 0);
            snap.corporations.insert(corp1, corp);
            Ok(())
        });

        let mut sess1 = setup_session(
            &ctx,
            char1,
            corp1,
            station_id,
            ROLE_CONTRACT_MANAGER | ROLE_DIRECTOR,
        );
        let _sess_member = setup_session(&ctx, member_char, corp1, station_id, 0);
        let mut sess2 = setup_session(&ctx, char2, corp2, station_id, ROLE_DIRECTOR);
        let item_id = seed_hangar_item(&ctx, corp1, station_id, 35, 250);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&item_id) {
                it.flag = InventoryFlag::CorpSAG1;
            }
        });
        let personal_item_id = seed_hangar_item(&ctx, char1, station_id, 34, 100);

        let mut kwargs = HashMap::new();
        kwargs.insert("forCorp".to_string(), PyRep::Bool(true));
        let c_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "contractProxy",
                None,
                "CreateContract",
                &[
                    PyRep::Integer(2), // Auction
                    PyRep::Integer(0), // Public
                    PyRep::Integer(0),
                    PyRep::Integer(1440),
                    PyRep::Integer(1),
                    PyRep::Integer(station_id),
                    PyRep::Integer(station_id),
                    PyRep::Real(200_000.0),
                    PyRep::Real(0.0),
                    PyRep::Real(800_000.0),
                    PyRep::String("P6 Corp Pyerite Auction".into()),
                    PyRep::String("Corp auction".into()),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(item_id),
                        PyRep::Integer(250),
                    ])]),
                ],
                &kwargs,
                None,
            )
            .await
            .expect("CreateContract forCorp");
        let PyRep::KeyVal(kv) = c_res else {
            panic!("expected KeyVal");
        };
        let cid = kv.get("contractID").unwrap().as_i64().unwrap();

        // Also create a personal contract (`forCorp` omitted/false) by `char1` to verify character vs corp isolation.
        let p_res = ctx
            .dispatch_rpc(
                &mut sess1,
                "contractProxy",
                None,
                "CreateContract",
                &[
                    PyRep::Integer(1), // ItemExchange
                    PyRep::Integer(0), // Public
                    PyRep::Integer(0),
                    PyRep::Integer(1440),
                    PyRep::Integer(1),
                    PyRep::Integer(station_id),
                    PyRep::Integer(station_id),
                    PyRep::Real(50_000.0),
                    PyRep::Real(0.0),
                    PyRep::Real(0.0),
                    PyRep::String("P6 Personal Tritanium".into()),
                    PyRep::String("Personal exchange".into()),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(personal_item_id),
                        PyRep::Integer(100),
                    ])]),
                ],
                &empty,
                None,
            )
            .await
            .expect("CreateContract personal");
        let PyRep::KeyVal(p_kv) = p_res else {
            panic!("expected KeyVal");
        };
        let personal_cid = p_kv.get("contractID").unwrap().as_i64().unwrap();

        ctx.dispatch_rpc(
            &mut sess2,
            "contractProxy",
            None,
            "PlaceBid",
            &[PyRep::Integer(cid), PyRep::Real(250_000.0)],
            &empty,
            None,
        )
        .await
        .expect("PlaceBid on corp auction");

        let tok_member = issue_token_with_scopes(&ctx, member_char, vec!["esi-contracts.read_corporation_contracts.v1"]);
        let tok_char1 = issue_token_with_scopes(&ctx, char1, vec!["esi-contracts.read_character_contracts.v1"]);
        let tok_other = issue_token_with_scopes(&ctx, char2, vec!["esi-contracts.read_corporation_contracts.v1"]);

        // Cross-corp and unauthenticated negative tests on all 3 corporation contract endpoints
        assert_status(&router, format!("/v1/corporations/{corp1}/contracts/"), None, 401);
        assert_status(&router, format!("/v1/corporations/{corp1}/contracts/"), Some(&tok_other), 403);
        assert_status(&router, format!("/v1/corporations/{corp1}/contracts/{cid}/items/"), Some(&tok_other), 403);
        assert_status(&router, format!("/v1/corporations/{corp1}/contracts/{cid}/bids/"), Some(&tok_other), 403);
        assert_status(&router, format!("/v1/corporations/{corp1}/contracts/999999999/items/"), Some(&tok_member), 404);
        assert_status(&router, format!("/v1/corporations/{corp1}/contracts/999999999/bids/"), Some(&tok_member), 404);

        // Non-Director ordinary corporation member (`roles == 0`) gets 200 OK on all 3 corp contract routes
        let corp_contracts = get_json(&router, &spec, format!("/v1/corporations/{corp1}/contracts/"), Some(&tok_member));
        let corp_arr = corp_contracts.as_array().unwrap();
        let found = corp_arr
            .iter()
            .find(|c| c["contract_id"] == cid)
            .expect("corp contract in list for ordinary member");
        assert_eq!(found["for_corporation"], true);
        assert_eq!(found["issuer_corporation_id"], corp1);
        assert!(
            !corp_arr.iter().any(|c| c["contract_id"] == personal_cid),
            "personal character contract must not leak into corporation contracts"
        );

        // Character contracts for `char1` must include `personal_cid` and exclude `for_corp == true` `cid`
        let char1_contracts = get_json(&router, &spec, format!("/v1/characters/{char1}/contracts/"), Some(&tok_char1));
        let char1_arr = char1_contracts.as_array().unwrap();
        assert!(char1_arr.iter().any(|c| c["contract_id"] == personal_cid));
        assert!(
            !char1_arr.iter().any(|c| c["contract_id"] == cid),
            "for_corp contract must not appear in issuer character contracts"
        );

        let corp_items = get_json(
            &router,
            &spec,
            format!("/v1/corporations/{corp1}/contracts/{cid}/items/"),
            Some(&tok_member),
        );
        assert_eq!(corp_items.as_array().unwrap().len(), 1);
        assert_eq!(corp_items[0]["type_id"], 35);
        assert_eq!(corp_items[0]["quantity"], 250);

        let corp_bids = get_json(
            &router,
            &spec,
            format!("/v1/corporations/{corp1}/contracts/{cid}/bids/"),
            Some(&tok_member),
        );
        assert_eq!(corp_bids.as_array().unwrap().len(), 1);
        assert_eq!(corp_bids[0]["amount"], 250_000.0);

        // SQLite persistence check
        drop(router);
        drop(ctx);
        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        let router2 = EsiRouter::new(ctx2);
        let tok_member2 = issue_token_with_scopes(&router2.ctx, member_char, vec!["esi-contracts.read_corporation_contracts.v1"]);
        let corp_bids2 = get_json(
            &router2,
            &spec,
            format!("/v1/corporations/{corp1}/contracts/{cid}/bids/"),
            Some(&tok_member2),
        );
        assert_eq!(corp_bids2.as_array().unwrap().len(), 1);
        let _ = std::fs::remove_file(&db_path);
    }

    // parity: esi:GET /industry/facilities
    #[tokio::test]
    async fn test_esi_p6_industry_jobs_and_facilities_live_rpc_and_sqlite_persistence() {
        let (db_path, ctx) = open_seeded_sqlite_ctx("industry_jobs_facilities");
        let router = EsiRouter::new(ctx.clone());
        let spec = OpenApiSpec::pinned();
        let empty = HashMap::new();

        let station_id = 60_003_760_i64;
        let corp1 = 98_000_001_i64;
        let raitaru_id = 1_030_000_991_001_i64;
        let bare_astrahus_id = 1_030_000_991_002_i64;
        let (char1, char2, corp2) = ctx.db.with_snapshot(|snap| {
            let mut chars: Vec<_> = snap.characters.values().collect();
            chars.sort_by_key(|c| c.character_id);
            (
                chars[0].character_id,
                chars[1].character_id,
                chars[1].corporation_id,
            )
        });
        let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
            let mut corp = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp1;
                __corp.name = "P6 Corp".to_string();
                __corp.ticker = "P6C".to_string();
                __corp.ceo_id = char1;
                __corp.tax_rate = 0.11;
                __corp.set_division_balance(1, IskCents::from_isk(500_000_000));
                __corp };
            corp.ensure_wallet_divisions();
            snap.corporations.insert(corp1, corp);
            snap.skills.entry(char1).or_default().insert(
                3387,
                TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 },
            );

            for (sid, tid, name) in [
                (raitaru_id, 35_825, "P6 Perimeter Raitaru"),
                (bare_astrahus_id, 35_832, "P6 Bare Astrahus"),
            ] {
                snap.structures.insert(
                    sid,
                    UpwellStructureRecord {
                        structure_id: sid,
                        type_id: tid,
                        owner_corp_id: corp1,
                        solar_system_id: 30_000_144,
                        name: name.to_string(),
                        position: [1_000.0, 2_000.0, 3_000.0],
                        state: "ShieldVulnerable".to_string(),
                        quantum_core_installed: true,
                        fuel_blocks: 500,
                        shield_hp: 10_000.0,
                        armor_hp: 10_000.0,
                        hull_hp: 10_000.0,
                        reinforced_until_unix: 0,
                        last_damage_tick: 0,
                        last_damage_unix: 0,
                        self_destruct_at_unix: None,
                    },
                );
            }
            for (iid, tid, owner, loc, flag) in [
                (990_001, 35_878, corp1, raitaru_id, InventoryFlag::ServiceSlot0),
                (990_002, 691, char1, station_id, InventoryFlag::Hangar),
            ] {
                snap.items.insert(
                    iid,
                    InventoryItemRecord {
                        item_id: iid,
                        type_id: tid,
                        owner_id: owner,
                        location_id: loc,
                        flag,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
            save_facility_taxes(snap, raitaru_id, &HashMap::from([(1_u8, 0.035)]));
            let mut ext =
                eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
            for sid in [raitaru_id, bare_astrahus_id] {
                ext.access_profiles.insert(
                    sid,
                    eve_server::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                        corp_only: false,
                        ..Default::default()
                    },
                );
            }
            eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
            Ok(())
        });

        let mut sess1 = setup_session(&ctx, char1, corp1, station_id, ROLE_FACTORY_MANAGER | ROLE_DIRECTOR);
        let _sess2 = setup_session(&ctx, char2, corp2, station_id, 0);

        // Seed sufficient materials for 2 Rifter (691) manufacturing runs in Jita hangar
        let rifter_mats: Vec<(u32, i32)> = ctx.sde.blueprints[&691]
            .materials
            .iter()
            .map(|m| (m.type_id, (m.quantity as i32) * 10))
            .collect();
        for (tid, qty) in rifter_mats {
            seed_hangar_item(&ctx, char1, station_id, tid, qty);
        }

        let job_args = [PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)];
        let job_res = ctx
            .dispatch_rpc(&mut sess1, "industryManager", None, "InstallJob", &job_args, &empty, None)
            .await
            .expect("InstallJob");
        let PyRep::KeyVal(job_kv) = job_res else { panic!("expected KeyVal") };
        let job_id = job_kv.get("jobID").unwrap().as_i64().unwrap();

        let corp_kwargs = HashMap::from([("useCorp".to_string(), PyRep::Bool(true))]);
        let corp_job_res = ctx
            .dispatch_rpc(&mut sess1, "industryManager", None, "InstallJob", &job_args, &corp_kwargs, None)
            .await
            .expect("InstallJob useCorp");
        let PyRep::KeyVal(corp_job_kv) = corp_job_res else { panic!("expected KeyVal") };
        let corp_job_id = corp_job_kv.get("jobID").unwrap().as_i64().unwrap();

        // Public /industry/facilities endpoint: exact tax assertions + non-industry structure exclusion
        let facilities = get_json(&router, &spec, "/v1/industry/facilities/", None);
        let fac_arr = facilities.as_array().unwrap();
        let jita_fac = fac_arr.iter().find(|f| f["facility_id"] == station_id).expect("Jita facility");
        assert_eq!(jita_fac["tax"], 0.0025);
        let raitaru_fac = fac_arr.iter().find(|f| f["facility_id"] == raitaru_id).expect("Raitaru facility");
        assert_eq!(raitaru_fac["tax"], 0.035);
        assert!(!fac_arr.iter().any(|f| f["facility_id"] == bare_astrahus_id));

        let tok_char = issue_token_with_scopes(&ctx, char1, vec!["esi-industry.read_character_jobs.v1"]);
        let tok_corp = issue_token_with_scopes(&ctx, char1, vec!["esi-industry.read_corporation_jobs.v1"]);
        let tok_other = issue_token_with_scopes(&ctx, char2, vec!["esi-industry.read_character_jobs.v1"]);

        assert_status(&router, format!("/v1/characters/{char1}/industry/jobs/"), None, 401);
        assert_status(&router, format!("/v1/characters/{char1}/industry/jobs/"), Some(&tok_other), 403);
        assert_status(&router, format!("/v1/corporations/{corp1}/industry/jobs/"), None, 401);
        assert_status(&router, format!("/v1/corporations/{corp1}/industry/jobs/"), Some(&tok_other), 403);

        // Personal vs corporation job isolation while both jobs are active
        let char_jobs = get_json(&router, &spec, format!("/v1/characters/{char1}/industry/jobs/"), Some(&tok_char));
        let char_jobs_arr = char_jobs.as_array().unwrap();
        let j = char_jobs_arr.iter().find(|j| j["job_id"] == job_id).expect("personal job in character jobs");
        assert_eq!(j["status"], "active");
        assert_eq!(j["facility_id"], station_id);
        assert!(!char_jobs_arr.iter().any(|j| j["job_id"] == corp_job_id));

        let corp_jobs = get_json(&router, &spec, format!("/v1/corporations/{corp1}/industry/jobs/"), Some(&tok_corp));
        let corp_jobs_arr = corp_jobs.as_array().unwrap();
        assert!(corp_jobs_arr.iter().any(|j| j["job_id"] == corp_job_id));
        assert!(!corp_jobs_arr.iter().any(|j| j["job_id"] == job_id));

        // Complete personal job and verify include_completed=false vs true + continued corp isolation
        ctx.advance_virtual_clock(6_000);
        let tok_char = issue_token_with_scopes(&ctx, char1, vec!["esi-industry.read_character_jobs.v1"]);
        let tok_corp = issue_token_with_scopes(&ctx, char1, vec!["esi-industry.read_corporation_jobs.v1"]);
        ctx.dispatch_rpc(&mut sess1, "industryManager", None, "CompleteJob", &[PyRep::Integer(job_id)], &empty, None)
            .await
            .expect("CompleteJob");

        let active_only = get_json(&router, &spec, format!("/v1/characters/{char1}/industry/jobs/?include_completed=false"), Some(&tok_char));
        assert!(!active_only.as_array().unwrap().iter().any(|j| j["job_id"] == job_id));

        let all_jobs = get_json(&router, &spec, format!("/v1/characters/{char1}/industry/jobs/?include_completed=true"), Some(&tok_char));
        let all_char_arr = all_jobs.as_array().unwrap();
        let completed = all_char_arr.iter().find(|j| j["job_id"] == job_id).expect("completed job");
        assert_eq!(completed["status"], "delivered");
        assert_eq!(completed["completed_character_id"], char1);
        assert!(!all_char_arr.iter().any(|j| j["job_id"] == corp_job_id));

        let corp_all = get_json(&router, &spec, format!("/v1/corporations/{corp1}/industry/jobs/?include_completed=true"), Some(&tok_corp));
        let corp_all_arr = corp_all.as_array().unwrap();
        assert!(corp_all_arr.iter().any(|j| j["job_id"] == corp_job_id));
        assert!(!corp_all_arr.iter().any(|j| j["job_id"] == job_id));

        // SQLite persistence check
        drop(router);
        drop(ctx);
        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        let router2 = EsiRouter::new(ctx2);
        let tok_char2 = issue_token_with_scopes(&router2.ctx, char1, vec!["esi-industry.read_character_jobs.v1"]);
        let persisted = get_json(&router2, &spec, format!("/v1/characters/{char1}/industry/jobs/?include_completed=true"), Some(&tok_char2));
        assert!(persisted.as_array().unwrap().iter().any(|j| j["job_id"] == job_id));
        assert!(!persisted.as_array().unwrap().iter().any(|j| j["job_id"] == corp_job_id));
        let _ = std::fs::remove_file(&db_path);
    }

    #[tokio::test]
    async fn test_esi_corp_roles_personnel_manager_and_grantable_role_gate() {
        let (db_path, ctx) = open_seeded_sqlite_ctx("corp_roles_gate");
        let router = EsiRouter::new(ctx.clone());
        let spec = OpenApiSpec::pinned();
        let (corp1, ceo_id, plain_id, trader_id, pm_id, grantable_id) =
            (98_000_077_i64, 90_000_001_i64, 90_000_801_i64, 90_000_802_i64, 90_000_803_i64, 90_000_804_i64);

        let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
            let base = snap.characters[&ceo_id].clone();
            let mut corp = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp1;
                __corp.name = "Roles Gate Corp".into();
                __corp.ticker = "RGC".into();
                __corp.ceo_id = ceo_id;
                __corp };
            for (cid, roles) in [
                (ceo_id, ROLE_DIRECTOR),
                (plain_id, 0),
                (trader_id, eve_db::corp_roles::ROLE_TRADER),
                (pm_id, eve_db::corp_roles::ROLE_PERSONNEL_MANAGER),
                (grantable_id, 0),
            ] {
                let mut ch = base.clone();
                ch.character_id = cid;
                ch.corporation_id = corp1;
                snap.characters.insert(cid, ch);
                corp.members.insert(cid);
                corp.member_roles.insert(cid, roles);
            }
            snap.corporations.insert(corp1, corp);
            snap.corp_role_assignments.insert(
                (corp1, grantable_id),
                eve_db::CorpMemberRoleAssignment {
                    character_id: grantable_id,
                    grantable_roles: eve_db::corp_roles::ROLE_PERSONNEL_MANAGER,
                    ..Default::default()
                },
            );
            Ok(())
        });

        let tok_for = |cid: i64| issue_token_with_scopes(&ctx, cid, vec!["esi-corporations.read_corporation_membership.v1"]);
        let (tok_plain, tok_trader, tok_pm, tok_grantable, tok_ceo) =
            (tok_for(plain_id), tok_for(trader_id), tok_for(pm_id), tok_for(grantable_id), tok_for(ceo_id));

        assert_status(&router, format!("/v1/corporations/{corp1}/roles/"), Some(&tok_plain), 403);
        assert_status(&router, format!("/v1/corporations/{corp1}/roles/"), Some(&tok_trader), 403);
        assert_status(&router, format!("/v1/corporations/{corp1}/roles/history/"), Some(&tok_plain), 403);
        assert_status(&router, format!("/v1/corporations/{corp1}/roles/history/"), Some(&tok_pm), 403);
        assert_ne!(ceo_id, plain_id);
        assert_ne!(pm_id, grantable_id);

        for tok in [&tok_pm, &tok_grantable, &tok_ceo] {
            let arr = get_json(&router, &spec, format!("/v1/corporations/{corp1}/roles/"), Some(tok));
            assert!(arr.as_array().unwrap().iter().any(|r| r["character_id"] == grantable_id));
        }
        let _ = get_json(&router, &spec, format!("/v1/corporations/{corp1}/roles/history/"), Some(&tok_ceo));
        let _ = std::fs::remove_file(&db_path);
    }
}

pub(crate) mod esi_char_contract_party_isolation {
    // `ESI-6` live HTTP + RPC + SQLite persistence regression tests for contract party isolation,
    // public contract `204` lifecycle transitions, and 30-day contract list retention.


    use crate::esi_market_industry_pi::{assert_status, get_json, issue_token_with_scopes};
    use eve_db::{
        corp_roles::{ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR},
        CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, WorldRepository,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
    use eve_server::router::ServerContext;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn temp_sqlite_path(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("evomu_p6_esi1_pt2_{tag}_{nanos}.sqlite"))
    }

    fn open_seeded_sqlite_ctx(tag: &str) -> (std::path::PathBuf, Arc<ServerContext>) {
        let db_path = temp_sqlite_path(tag);
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
        let ctx = Arc::new(ServerContext::with_repository(repo));
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        ctx.db
            .try_with_mut_snapshot(|snap| {
                *snap = demo;
                Ok::<_, std::convert::Infallible>(())
            })
            .unwrap();
        crate::common::pin_clock(&ctx);
        (db_path, ctx)
    }

    fn setup_session(
        ctx: &ServerContext,
        char_id: i64,
        corp_id: i64,
        station_id: i64,
        roles: u64,
    ) -> SessionState {
        let mut sess = SessionState::default();
        sess.user_id = Some(1);
        sess.character_id = Some(char_id);
        sess.corporation_id = Some(corp_id);
        sess.station_id = Some(station_id);
        sess.solar_system_id = Some(30_000_142);
        sess.region_id = Some(10_000_002);
        sess.corp_role = roles;
        let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.corporation_id = corp_id;
                ch.station_id = Some(station_id);
                ch.solar_system_id = 30_000_142;
                ch.balance = IskCents::from_isk(500_000_000);
            }
            if let Some(corp) = snap.corporations.get_mut(&corp_id) {
                corp.members.insert(char_id);
                corp.member_roles.insert(char_id, roles);
            }
            Ok(())
        });
        sess
    }

    fn seed_hangar_item(
        ctx: &ServerContext,
        owner_id: i64,
        station_id: i64,
        type_id: u32,
        qty: i32,
    ) -> i64 {
        ctx.db
            .try_with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let iid = snap.next_entity_id;
                snap.items.insert(
                    iid,
                    InventoryItemRecord {
                        item_id: iid,
                        type_id,
                        owner_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: qty,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
                Ok::<_, std::convert::Infallible>(iid)
            })
            .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_contract_rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        ctype: i64,
        num_days: i64,
        start_station: i64,
        end_station: i64,
        price: f64,
        reward: f64,
        collateral_or_buyout: f64,
        title: &str,
        item_id: i64,
        qty: i64,
        for_corp: bool,
    ) -> i64 {
        let mut kw = HashMap::new();
        if for_corp {
            kw.insert("forCorp".to_string(), PyRep::Bool(true));
        }
        let PyRep::KeyVal(kv) = ctx
            .dispatch_rpc(
                sess,
                "contractProxy",
                None,
                "CreateContract",
                &[
                    PyRep::Integer(ctype),
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::Integer(1440),
                    PyRep::Integer(num_days),
                    PyRep::Integer(start_station),
                    PyRep::Integer(end_station),
                    PyRep::Real(price),
                    PyRep::Real(reward),
                    PyRep::Real(collateral_or_buyout),
                    PyRep::String(title.into()),
                    PyRep::String(String::new()),
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(item_id),
                        PyRep::Integer(qty),
                    ])]),
                ],
                &kw,
                None,
            )
            .await
            .expect("CreateContract")
        else {
            panic!("expected KeyVal from CreateContract");
        };
        kv.get("contractID").unwrap().as_i64().unwrap()
    }

    fn assert_204_valid(router: &EsiRouter, spec: &OpenApiSpec, target: impl Into<String>) {
        let req = EsiHttpRequest::new("GET", target);
        let resp = router.handle_request(&req);
        let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
        assert_eq!(
            resp.status_code,
            204,
            "Expected 204 for GET {}, got {} ({})",
            req.raw_target,
            resp.status_code,
            String::from_utf8_lossy(&resp.body)
        );
        assert!(resp.body.is_empty(), "204 response body must be empty");
        spec.validate_response("GET", raw_path, 204, &resp.headers, &resp.body)
            .unwrap_or_else(|e| panic!("OpenAPI 204 validation failed for {}: {e}", req.raw_target));
    }

    // parity: esi:GET /characters/{character_id}/contracts/{contract_id}/bids, esi:GET /characters/{character_id}/contracts/{contract_id}/items, esi:GET /contracts/public/bids/{contract_id}, esi:GET /contracts/public/items/{contract_id}, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/bids, esi:GET /corporations/{corporation_id}/contracts/{contract_id}/items
    #[tokio::test]
    async fn test_esi_p6_char_contract_party_isolation_and_bidder_privacy() {
        let (db_path, ctx) = open_seeded_sqlite_ctx("party_privacy");
        let router = EsiRouter::new(ctx.clone());
        let spec = OpenApiSpec::pinned();
        let empty = HashMap::new();

        let station_id = 60_003_760_i64;
        let corp1 = 98_000_501_i64;
        let char3 = 90_000_888_i64;
        let corp3 = 98_000_503_i64;
        let (char1, char2, corp2) = ctx.db.with_snapshot(|snap| {
            let mut chars: Vec<_> = snap.characters.values().collect();
            chars.sort_by_key(|c| c.character_id);
            (
                chars[0].character_id,
                chars[1].character_id,
                chars[1].corporation_id,
            )
        });
        let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
            let mut c1 = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp1;
                __corp.name = "Party Isolation Corp".into();
                __corp.ticker = "PIC1".into();
                __corp.ceo_id = char1;
                __corp.set_division_balance(1, IskCents::from_isk(500_000_000));
                __corp };
            c1.ensure_wallet_divisions();
            snap.corporations.insert(corp1, c1);

            let c3 = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp3;
                __corp.name = "Third Party Corp".into();
                __corp.ticker = "TPC3".into();
                __corp.ceo_id = char3;
                __corp.set_division_balance(1, IskCents::from_isk(100_000_000));
                __corp };
            snap.corporations.insert(corp3, c3);
            let mut bystander = snap.characters[&char1].clone();
            bystander.character_id = char3;
            bystander.name = "Unrelated Bystander".into();
            bystander.corporation_id = corp3;
            bystander.station_id = Some(station_id);
            bystander.balance = IskCents::from_isk(100_000_000);
            snap.characters.insert(char3, bystander);
            Ok(())
        });

        let mut sess1 = setup_session(
            &ctx,
            char1,
            corp1,
            station_id,
            ROLE_CONTRACT_MANAGER | ROLE_DIRECTOR,
        );
        let mut sess2 = setup_session(&ctx, char2, corp2, station_id, 0);
        let _sess3 = setup_session(&ctx, char3, corp3, station_id, 0);

        let personal_item = seed_hangar_item(&ctx, char1, station_id, 34, 400);
        let corp_item = seed_hangar_item(&ctx, corp1, station_id, 35, 300);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&corp_item) {
                it.flag = InventoryFlag::CorpSAG1;
            }
        });

        // 1. Personal public auction issued by char1 (`forCorp == false`) with bid by char2
        let personal_cid = create_contract_rpc(
            &ctx,
            &mut sess1,
            2,
            1,
            station_id,
            station_id,
            100_000.0,
            0.0,
            500_000.0,
            "Personal Public Auction",
            personal_item,
            400,
            false,
        )
        .await;

        ctx.dispatch_rpc(
            &mut sess2,
            "contractProxy",
            None,
            "PlaceBid",
            &[PyRep::Integer(personal_cid), PyRep::Real(150_000.0)],
            &empty,
            None,
        )
        .await
        .expect("PlaceBid personal auction");

        // 2. Corporate public auction issued by char1 (`forCorp == true`) with bid by char2
        let corp_cid = create_contract_rpc(
            &ctx,
            &mut sess1,
            2,
            1,
            station_id,
            station_id,
            200_000.0,
            0.0,
            900_000.0,
            "Corp Public Auction",
            corp_item,
            300,
            true,
        )
        .await;

        ctx.dispatch_rpc(
            &mut sess2,
            "contractProxy",
            None,
            "PlaceBid",
            &[PyRep::Integer(corp_cid), PyRep::Real(275_000.0)],
            &empty,
            None,
        )
        .await
        .expect("PlaceBid corp auction");

        let tok1 = issue_token_with_scopes(
            &ctx,
            char1,
            vec![
                "esi-contracts.read_character_contracts.v1",
                "esi-contracts.read_corporation_contracts.v1",
            ],
        );
        let tok3 = issue_token_with_scopes(&ctx, char3, vec!["esi-contracts.read_character_contracts.v1"]);

        // Unrelated character `char3` gets 403 on `/characters/{char3}/contracts/{cid}/{items,bids}` for BOTH public contracts
        for cid in [personal_cid, corp_cid] {
            assert_status(
                &router,
                format!("/v1/characters/{char3}/contracts/{cid}/items/"),
                Some(&tok3),
                403,
            );
            assert_status(
                &router,
                format!("/v1/characters/{char3}/contracts/{cid}/bids/"),
                Some(&tok3),
                403,
            );
        }

        // `for_corp == true` issuer `char1` gets 403 on `/characters/{char1}/contracts/{corp_cid}/{items,bids}`
        assert_status(
            &router,
            format!("/v1/characters/{char1}/contracts/{corp_cid}/items/"),
            Some(&tok1),
            403,
        );
        assert_status(
            &router,
            format!("/v1/characters/{char1}/contracts/{corp_cid}/bids/"),
            Some(&tok1),
            403,
        );

        // Public bids endpoint strips `bidder_id`, while personal issuer & corp routes preserve `bidder_id`
        let pub_bids = get_json(&router, &spec, format!("/v1/contracts/public/bids/{personal_cid}/"), None);
        assert_eq!(pub_bids[0]["amount"], 150_000.0);
        assert!(pub_bids[0].get("bidder_id").is_none(), "public bids must omit bidder_id");

        let issuer_bids = get_json(
            &router,
            &spec,
            format!("/v1/characters/{char1}/contracts/{personal_cid}/bids/"),
            Some(&tok1),
        );
        assert_eq!(issuer_bids[0]["bidder_id"], char2);

        let corp_bids = get_json(
            &router,
            &spec,
            format!("/v1/corporations/{corp1}/contracts/{corp_cid}/bids/"),
            Some(&tok1),
        );
        assert_eq!(corp_bids[0]["bidder_id"], char2);

        let _ = std::fs::remove_file(&db_path);
    }

    // parity: esi:GET /characters/{character_id}/contracts, esi:GET /contracts/public/{region_id}, esi:GET /contracts/public/bids/{contract_id}, esi:GET /contracts/public/items/{contract_id}, esi:GET /corporations/{corporation_id}/contracts
    #[tokio::test]
    async fn test_esi_p6_public_contract_204_on_accept_and_expiry_and_30d_retention() {
        let (db_path, ctx) = open_seeded_sqlite_ctx("lifecycle_retention");
        let router = EsiRouter::new(ctx.clone());
        let spec = OpenApiSpec::pinned();
        let empty = HashMap::new();

        let start_station = 60_003_760_i64;
        let dest_station = 60_003_757_i64;
        let region_id = 10_000_002_i64;
        let corp1 = 98_000_511_i64;
        let (char1, char2, corp2) = ctx.db.with_snapshot(|snap| {
            let mut chars: Vec<_> = snap.characters.values().collect();
            chars.sort_by_key(|c| c.character_id);
            (
                chars[0].character_id,
                chars[1].character_id,
                chars[1].corporation_id,
            )
        });
        let _: Result<(), ()> = ctx.db.try_with_mut_snapshot(|snap| {
            let mut c1 = { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp1;
                __corp.name = "Retention Corp".into();
                __corp.ticker = "RETC".into();
                __corp.ceo_id = char1;
                __corp.set_division_balance(1, IskCents::from_isk(500_000_000));
                __corp };
            c1.ensure_wallet_divisions();
            snap.corporations.insert(corp1, c1);
            Ok(())
        });

        let mut sess1 = setup_session(
            &ctx,
            char1,
            corp1,
            start_station,
            ROLE_CONTRACT_MANAGER | ROLE_DIRECTOR,
        );
        let mut sess2 = setup_session(&ctx, char2, corp2, start_station, 0);

        let exchange_item = seed_hangar_item(&ctx, char1, start_station, 34, 200);
        let expiring_item = seed_hangar_item(&ctx, char1, start_station, 35, 150);
        let courier_item = seed_hangar_item(&ctx, char1, start_station, 36, 50);
        let corp_exchange_item = seed_hangar_item(&ctx, corp1, start_station, 37, 100);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(it) = snap.items.get_mut(&corp_exchange_item) {
                it.flag = InventoryFlag::CorpSAG1;
            }
        });

        let accepted_cid = create_contract_rpc(
            &ctx,
            &mut sess1,
            1,
            1,
            start_station,
            start_station,
            50_000.0,
            0.0,
            0.0,
            "Accepted Exchange",
            exchange_item,
            200,
            false,
        )
        .await;
        let expiring_cid = create_contract_rpc(
            &ctx,
            &mut sess1,
            2,
            1,
            start_station,
            start_station,
            100_000.0,
            0.0,
            400_000.0,
            "Expiring Auction",
            expiring_item,
            150,
            false,
        )
        .await;
        let courier_cid = create_contract_rpc(
            &ctx,
            &mut sess1,
            3,
            28,
            start_station,
            dest_station,
            0.0,
            25_000.0,
            10_000.0,
            "Long InProgress Courier",
            courier_item,
            50,
            false,
        )
        .await;
        let corp_cid = create_contract_rpc(
            &ctx,
            &mut sess1,
            1,
            1,
            start_station,
            start_station,
            60_000.0,
            0.0,
            0.0,
            "Corp Exchange",
            corp_exchange_item,
            100,
            true,
        )
        .await;

        // While Outstanding & unexpired, public sub-routes return 200 OK
        assert_eq!(
            get_json(&router, &spec, format!("/v1/contracts/public/items/{accepted_cid}/"), None)
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            get_json(&router, &spec, format!("/v1/contracts/public/items/{expiring_cid}/"), None)
                .as_array()
                .unwrap()
                .len(),
            1
        );

        // Accept contracts A, C, and D via live `contractProxy.AcceptContract`
        for cid in [accepted_cid, courier_cid, corp_cid] {
            ctx.dispatch_rpc(
                &mut sess2,
                "contractProxy",
                None,
                "AcceptContract",
                &[PyRep::Integer(cid), PyRep::Bool(false)],
                &empty,
                None,
            )
            .await
            .expect("AcceptContract");
        }

        // Accepted contracts immediately return 204 No Content on `/contracts/public/{items,bids}/{cid}`
        assert_204_valid(&router, &spec, format!("/v1/contracts/public/items/{accepted_cid}/"));
        assert_204_valid(&router, &spec, format!("/v1/contracts/public/bids/{accepted_cid}/"));
        assert_204_valid(&router, &spec, format!("/v1/contracts/public/items/{courier_cid}/"));

        // Advance clock by 2 days -> `expiring_cid` expires; public region omits it and public items/bids return 204
        ctx.db.clock().advance_secs(2 * 86_400);
        let pub_after_2d = get_json(&router, &spec, format!("/v1/contracts/public/{region_id}/"), None);
        assert!(
            !pub_after_2d
                .as_array()
                .unwrap()
                .iter()
                .any(|c| c["contract_id"] == expiring_cid || c["contract_id"] == accepted_cid)
        );
        assert_204_valid(&router, &spec, format!("/v1/contracts/public/items/{expiring_cid}/"));
        assert_204_valid(&router, &spec, format!("/v1/contracts/public/bids/{expiring_cid}/"));

        // Within 30 days (`+2d`), finished/expired contracts are still retained in character & corporation lists
        let tok1 = issue_token_with_scopes(
            &ctx,
            char1,
            vec![
                "esi-contracts.read_character_contracts.v1",
                "esi-contracts.read_corporation_contracts.v1",
            ],
        );
        let char_2d = get_json(&router, &spec, format!("/v1/characters/{char1}/contracts/"), Some(&tok1));
        let char_2d_ids: Vec<i64> = char_2d
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["contract_id"].as_i64())
            .collect();
        assert!(char_2d_ids.contains(&accepted_cid));
        assert!(char_2d_ids.contains(&expiring_cid));
        assert!(char_2d_ids.contains(&courier_cid));

        let corp_2d = get_json(&router, &spec, format!("/v1/corporations/{corp1}/contracts/"), Some(&tok1));
        assert!(corp_2d.as_array().unwrap().iter().any(|c| c["contract_id"] == corp_cid));

        // Advance clock past 30 days (`+29d` -> total `+31d`): non-`in_progress` contracts older than 30 days are pruned,
        // while `in_progress` courier contract C remains visible.
        ctx.db.clock().advance_secs(29 * 86_400);
        let tok1_31d = issue_token_with_scopes(
            &ctx,
            char1,
            vec![
                "esi-contracts.read_character_contracts.v1",
                "esi-contracts.read_corporation_contracts.v1",
            ],
        );
        let char_31d = get_json(
            &router,
            &spec,
            format!("/v1/characters/{char1}/contracts/"),
            Some(&tok1_31d),
        );
        let char_31d_ids: Vec<i64> = char_31d
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["contract_id"].as_i64())
            .collect();
        assert!(
            !char_31d_ids.contains(&accepted_cid) && !char_31d_ids.contains(&expiring_cid),
            "finished/expired contracts > 30 days old must be excluded from character contracts"
        );
        assert!(
            char_31d_ids.contains(&courier_cid),
            "in_progress courier contract must remain visible past 30 days"
        );

        let corp_31d = get_json(
            &router,
            &spec,
            format!("/v1/corporations/{corp1}/contracts/"),
            Some(&tok1_31d),
        );
        assert!(
            !corp_31d.as_array().unwrap().iter().any(|c| c["contract_id"] == corp_cid),
            "completed corporation contract > 30 days old must be excluded from corporation contracts"
        );

        // Verify SQLite persistence across reopen
        drop(router);
        drop(ctx);
        let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        crate::common::pin_clock(&ctx2);
        ctx2.db.clock().advance_secs(31 * 86_400);
        let router2 = EsiRouter::new(ctx2);
        assert_204_valid(&router2, &spec, format!("/v1/contracts/public/items/{accepted_cid}/"));
        let tok1_reopen = issue_token_with_scopes(
            &router2.ctx,
            char1,
            vec!["esi-contracts.read_character_contracts.v1"],
        );
        let char_reopen = get_json(
            &router2,
            &spec,
            format!("/v1/characters/{char1}/contracts/"),
            Some(&tok1_reopen),
        );
        let reopen_ids: Vec<i64> = char_reopen
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["contract_id"].as_i64())
            .collect();
        assert!(reopen_ids.contains(&courier_cid));
        assert!(!reopen_ids.contains(&accepted_cid));
        let _ = std::fs::remove_file(&db_path);
    }
}

pub(crate) mod esi_calendar_rpc_http_persistence {
    // End-to-end behavioral verification integration tests for ESI social, calendar, contacts,
    // mailing lists, corporation governance, and sovereignty map (`esi:*` contract verification closure).


    use crate::esi_market_industry_pi::{issue_token_with_scopes, rpc_call};
    use eve_db::corp_roles::{
        ROLE_CONTRACT_MANAGER, ROLE_DIRECTOR, ROLE_PROJECT_MANAGER, ROLE_TRADER,
    };
    use eve_db::{AllianceRecord, CorporationRecord, IskCents, TrainedSkillRecord};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::esi::router::EsiRouter;
    use eve_server::esi::{EsiHttpRequest, OpenApiSpec};
    use eve_server::router::ServerContext;
    use eve_server::services::sov_services::claim_system_sovereignty;
    use serde_json::{json, Value};
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn esi_get_validated(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>) -> Value {
        let mut req = EsiHttpRequest::new("GET", path);
        if let Some(t) = token {
            req = req.with_bearer_token(t);
        }
        let resp = router.handle_request(&req);
        assert_eq!(resp.status_code, 200, "GET {path} -> {}: {}", resp.status_code, String::from_utf8_lossy(&resp.body));
        let raw_path = path.split('?').next().unwrap_or(path);
        spec.validate_response("GET", raw_path, resp.status_code, &resp.headers, &resp.body)
            .unwrap_or_else(|e| panic!("OpenAPI validation failed for GET {path}: {e}"));
        serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
    }

    fn esi_put_validated(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: &str, body: &Value) {
        let req = EsiHttpRequest::new("PUT", path)
            .with_bearer_token(token)
            .with_header("content-type", "application/json")
            .with_body(serde_json::to_vec(body).unwrap());
        let resp = router.handle_request(&req);
        assert_eq!(resp.status_code, 204, "PUT {path} -> {}: {}", resp.status_code, String::from_utf8_lossy(&resp.body));
        spec.validate_response("PUT", path, resp.status_code, &resp.headers, &resp.body)
            .unwrap_or_else(|e| panic!("OpenAPI validation failed for PUT {path}: {e}"));
    }

    #[tokio::test]
    async fn test_esi_calendar_rpc_and_http_persistence() {
        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        let ceo_id = 90_000_001_i64;
        let wingman_id = 90_000_002_i64;
        let corp_id = 98_600_001_i64;
        let jita_station = 60_003_760_i64;

        ctx.db.with_mut_snapshot(|snap| {
            let tpl = snap.characters[&ceo_id].clone();
            for (cid, name) in [(ceo_id, "Calendar CEO"), (wingman_id, "Calendar Wingman")] {
                let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
                ch.character_id = cid;
                ch.name = name.into();
                ch.corporation_id = corp_id;
                ch.station_id = Some(jita_station);
                ch.solar_system_id = 30_000_142;
                snap.characters.insert(cid, ch);
            }
            snap.corporations.insert(
                corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = corp_id;
                    __corp.name = "Calendar Corp".into();
                    __corp.ticker = "CALC".into();
                    __corp.ceo_id = ceo_id;
                    __corp.tax_rate = 0.05;
                    __corp.members = HashSet::from([ceo_id, wingman_id]);
                    __corp.member_roles = HashMap::from([(ceo_id, ROLE_DIRECTOR), (wingman_id, 0)]);
                    __corp },
            );
        });

        let mut ceo_sess = SessionState::new(70_101);
        ceo_sess.user_id = Some(1);
        ceo_sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
        ceo_sess.character_id = Some(ceo_id);
        ceo_sess.corporation_id = Some(corp_id);
        ceo_sess.station_id = Some(jita_station);
        ceo_sess.solar_system_id = Some(30_000_142);

        let mut wing_sess = SessionState::new(70_102);
        wing_sess.user_id = Some(1);
        wing_sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
        wing_sess.character_id = Some(wingman_id);
        wing_sess.corporation_id = Some(corp_id);
        wing_sess.station_id = Some(jita_station);
        wing_sess.solar_system_id = Some(30_000_142);

        let event_ft = ((ctx.now_unix() as i64 + 7_200) + 11_644_473_600) * 10_000_000;
        let create_rep = rpc_call(
            &ctx,
            &mut ceo_sess,
            "calendarMgr",
            "CreateCorporationEvent",
            &[
                PyRep::Integer(event_ft),
                PyRep::Integer(90),
                PyRep::String("Fleet Stratop".into()),
                PyRep::String("Form up in Jita 4-4".into()),
                PyRep::Integer(1),
            ],
        )
        .await;
        let event_id = create_rep
            .as_i64()
            .or_else(|| create_rep.dict_get("eventID").and_then(|v| v.as_i64()))
            .expect("created corporation event_id");
        assert!(event_id > 0);

        // Respond via MachoNet RPC as wingman (5 = tentative)
        rpc_call(
            &ctx,
            &mut wing_sess,
            "calendarMgr",
            "SendEventResponse",
            &[
                PyRep::Integer(event_id),
                PyRep::Integer(corp_id),
                PyRep::Integer(5),
            ],
        )
        .await;

        // Respond via ESI HTTP PUT as CEO ("accepted")
        let cal_tok = issue_token_with_scopes(
            &ctx,
            ceo_id,
            vec![
                "esi-calendar.read_calendar_events.v1",
                "esi-calendar.respond_calendar_events.v1",
            ],
        );
        esi_put_validated(
            &router,
            &spec,
            &format!("/v1/characters/{ceo_id}/calendar/{event_id}/"),
            &cal_tok,
            &json!({ "response": "accepted" }),
        );

        // Clear ephemeral esi_state to prove ESI reads directly from WorldStateSnapshot
        ctx.esi_state.calendar_events.write().unwrap().clear();
        ctx.esi_state.calendar_responses.write().unwrap().clear();

        let list = esi_get_validated(
            &router,
            &spec,
            &format!("/v1/characters/{ceo_id}/calendar/"),
            Some(&cal_tok),
        );
        let arr = list.as_array().expect("calendar list array");
        let item = arr.iter().find(|e| e["event_id"] == json!(event_id)).expect("event in calendar list");
        assert_eq!(item["title"], json!("Fleet Stratop"));
        assert_eq!(item["importance"], json!(1));
        assert_eq!(item["event_response"], json!("accepted"));

        let detail = esi_get_validated(
            &router,
            &spec,
            &format!("/v1/characters/{ceo_id}/calendar/{event_id}/"),
            Some(&cal_tok),
        );
        assert_eq!(detail["event_id"], json!(event_id));
        assert_eq!(detail["owner_id"], json!(corp_id));
        assert_eq!(detail["owner_type"], json!("corporation"));
        assert_eq!(detail["duration"], json!(90));
        assert_eq!(detail["text"], json!("Form up in Jita 4-4"));
        assert_eq!(detail["response"], json!("accepted"));

        let attendees = esi_get_validated(
            &router,
            &spec,
            &format!("/v1/characters/{ceo_id}/calendar/{event_id}/attendees/"),
            Some(&cal_tok),
        );
        let att_arr = attendees.as_array().expect("attendees array");
        assert!(att_arr.iter().any(|a| a["character_id"] == json!(ceo_id) && a["event_response"] == json!("accepted")));
        assert!(att_arr.iter().any(|a| a["character_id"] == json!(wingman_id) && a["event_response"] == json!("tentative")));
    }

    #[tokio::test]
    async fn test_esi_contacts_labels_and_mail_lists() {
        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        let ceo_id = 90_000_001_i64;
        let target_char_id = 90_000_002_i64;
        let corp_id = 98_600_002_i64;
        let alliance_id = 99_600_002_i64;

        ctx.db.with_mut_snapshot(|snap| {
            let tpl = snap.characters[&ceo_id].clone();
            for (cid, name) in [(ceo_id, "Diplo CEO"), (target_char_id, "Neutral Contact")] {
                let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
                ch.character_id = cid;
                ch.name = name.into();
                ch.corporation_id = if cid == ceo_id { corp_id } else { 1_000_009 };
                ch.alliance_id = if cid == ceo_id { Some(alliance_id) } else { None };
                ch.balance = IskCents::from_isk(50_000_000);
                snap.characters.insert(cid, ch);
            }
            snap.corporations.insert(
                corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = corp_id;
                    __corp.name = "Diplo Corp".into();
                    __corp.ticker = "DPLC".into();
                    __corp.ceo_id = ceo_id;
                    __corp.alliance_id = Some(alliance_id);
                    __corp.tax_rate = 0.05;
                    __corp.members = HashSet::from([ceo_id]);
                    __corp.member_roles = HashMap::from([(ceo_id, ROLE_DIRECTOR)]);
                    __corp },
            );
            snap.alliances.insert(
                alliance_id,
                AllianceRecord {
                    alliance_id,
                    name: "Diplo Alliance".into(),
                    short_name: "DPLA".into(),
                    executor_corp_id: corp_id,
                    member_corp_ids: HashSet::from([corp_id]),
                },
            );
        });

        let mut sess = SessionState::new(70_103);
        sess.user_id = Some(1);
        sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
        sess.character_id = Some(ceo_id);
        sess.corporation_id = Some(corp_id);
        sess.alliance_id = Some(alliance_id);
        sess.corp_role = ROLE_DIRECTOR;

        // 1. Character contact label via charMgr.CreateLabel
        rpc_call(
            &ctx,
            &mut sess,
            "charMgr",
            "CreateLabel",
            &[PyRep::String("Logi Anchors".into()), PyRep::Integer(1)],
        )
        .await;

        // 2. Corporation contact & label via corpRegistry
        rpc_call(
            &ctx,
            &mut sess,
            "corpRegistry",
            "CreateLabel",
            &[PyRep::String("Blues".into()), PyRep::Integer(2)],
        )
        .await;
        rpc_call(
            &ctx,
            &mut sess,
            "corpRegistry",
            "AddCorporateContact",
            &[PyRep::Integer(target_char_id), PyRep::Real(5.0), PyRep::Integer(1)],
        )
        .await;

        // 3. Alliance contact & label via allianceRegistry
        rpc_call(
            &ctx,
            &mut sess,
            "allianceRegistry",
            "CreateLabel",
            &[PyRep::String("Coalition".into()), PyRep::Integer(3)],
        )
        .await;
        rpc_call(
            &ctx,
            &mut sess,
            "allianceRegistry",
            "AddAllianceContact",
            &[PyRep::Integer(target_char_id), PyRep::Real(10.0), PyRep::Integer(1)],
        )
        .await;

        // 4. Mailing list via mailingListsMgr.Create
        rpc_call(
            &ctx,
            &mut sess,
            "mailingListsMgr",
            "Create",
            &[
                PyRep::String("FC Announcements".into()),
                PyRep::Integer(0),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
        )
        .await;

        // Clear ephemeral esi_state maps to prove SQLite/WorldStateSnapshot backing
        ctx.esi_state.char_contact_labels.write().unwrap().clear();
        ctx.esi_state.corp_contacts.write().unwrap().clear();
        ctx.esi_state.corp_contact_labels.write().unwrap().clear();
        ctx.esi_state.alliance_contacts.write().unwrap().clear();
        ctx.esi_state.alliance_contact_labels.write().unwrap().clear();
        ctx.esi_state.mail_lists.write().unwrap().clear();

        let tok = issue_token_with_scopes(
            &ctx,
            ceo_id,
            vec![
                "esi-characters.read_contacts.v1",
                "esi-corporations.read_contacts.v1",
                "esi-alliances.read_contacts.v1",
                "esi-mail.read_mail.v1",
            ],
        );

        let char_labels = esi_get_validated(&router, &spec, &format!("/v1/characters/{ceo_id}/contacts/labels/"), Some(&tok));
        assert!(char_labels.as_array().unwrap().iter().any(|l| l["label_name"] == json!("Logi Anchors")));

        let corp_contacts = esi_get_validated(&router, &spec, &format!("/v1/corporations/{corp_id}/contacts/"), Some(&tok));
        assert!(corp_contacts.as_array().unwrap().iter().any(|c| c["contact_id"] == json!(target_char_id) && c["standing"] == json!(5.0)));

        let corp_labels = esi_get_validated(&router, &spec, &format!("/v1/corporations/{corp_id}/contacts/labels/"), Some(&tok));
        assert!(corp_labels.as_array().unwrap().iter().any(|l| l["label_name"] == json!("Blues")));

        let ally_contacts = esi_get_validated(&router, &spec, &format!("/v1/alliances/{alliance_id}/contacts/"), Some(&tok));
        assert!(ally_contacts.as_array().unwrap().iter().any(|c| c["contact_id"] == json!(target_char_id) && c["standing"] == json!(10.0)));

        let ally_labels = esi_get_validated(&router, &spec, &format!("/v1/alliances/{alliance_id}/contacts/labels/"), Some(&tok));
        assert!(ally_labels.as_array().unwrap().iter().any(|l| l["label_name"] == json!("Coalition")));

        let mail_lists = esi_get_validated(&router, &spec, &format!("/v1/characters/{ceo_id}/mail/lists/"), Some(&tok));
        assert!(mail_lists.as_array().unwrap().iter().any(|m| m["name"] == json!("FC Announcements")));
    }

    #[tokio::test]
    async fn test_esi_corp_governance_roles_limit_and_sov_map() {
        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        let ceo_id = 90_000_001_i64;
        let member_id = 90_000_002_i64;
        let corp_id = 98_600_003_i64;
        let alliance_id = 99_600_003_i64;
        let null_sys = ctx
            .sde
            .solar_systems
            .values()
            .find(|s| s.security_status <= 0.0)
            .map(|s| s.solar_system_id)
            .expect("SDE nullsec solar system");

        ctx.db.with_mut_snapshot(|snap| {
            let tpl = snap.characters[&ceo_id].clone();
            for (cid, name) in [(ceo_id, "Gov CEO"), (member_id, "Gov Member")] {
                let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
                ch.character_id = cid;
                ch.name = name.into();
                ch.corporation_id = corp_id;
                ch.alliance_id = Some(alliance_id);
                snap.characters.insert(cid, ch);
            }
            snap.skills.entry(ceo_id).or_default().clear();
            snap.corporations.insert(
                corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = corp_id;
                    __corp.name = "Governance Corp".into();
                    __corp.ticker = "GOVC".into();
                    __corp.ceo_id = ceo_id;
                    __corp.alliance_id = Some(alliance_id);
                    __corp.tax_rate = 0.05;
                    __corp.members = HashSet::from([ceo_id, member_id]);
                    __corp.member_roles = HashMap::from([(ceo_id, ROLE_DIRECTOR), (member_id, 0)]);
                    __corp },
            );
            snap.alliances.insert(
                alliance_id,
                AllianceRecord {
                    alliance_id,
                    name: "Governance Alliance".into(),
                    short_name: "GOVA".into(),
                    executor_corp_id: corp_id,
                    member_corp_ids: HashSet::from([corp_id]),
                },
            );
        });

        let tok = issue_token_with_scopes(
            &ctx,
            ceo_id,
            vec![
                "esi-corporations.read_corporation_membership.v1",
                "esi-corporations.read_titles.v1",
                "esi-corporations.track_members.v1",
            ],
        );

        // 1. Untrained CEO defaults to base 10 member limit; training Corporation Management + UpdateCorporationAbilities updates it
        let base_limit = esi_get_validated(
            &router,
            &spec,
            &format!("/v1/corporations/{corp_id}/members/limit/"),
            Some(&tok),
        );
        assert_eq!(base_limit, json!(10));

        let sde_bonus = ctx
            .sde
            .types
            .get(&3363)
            .and_then(|t| t.default_attributes.get(&191))
            .copied()
            .expect("SDE attribute 191 on Corporation Management (3363)")
            .round() as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(ceo_id).or_default().insert(
                3363,
                TrainedSkillRecord {
                    type_id: 3363,
                    level: 3,
                    skill_points: 16_000.0,
                },
            );
        });

        let mut ceo_sess = SessionState::new(70_104);
        ceo_sess.user_id = Some(1);
        ceo_sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
        ceo_sess.character_id = Some(ceo_id);
        ceo_sess.corporation_id = Some(corp_id);
        ceo_sess.alliance_id = Some(alliance_id);
        ceo_sess.corp_role = ROLE_DIRECTOR;

        rpc_call(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateCorporationAbilities",
            &[],
        )
        .await;

        let updated_limit = esi_get_validated(
            &router,
            &spec,
            &format!("/v1/corporations/{corp_id}/members/limit/"),
            Some(&tok),
        );
        assert_eq!(updated_limit, json!(sde_bonus * 3));

        // 2. Live corpRegistry.UpdateMember grants Trader, Contract_Manager, Project_Manager -> reflected in /roles and /roles/history
        let granted_mask = (ROLE_TRADER | ROLE_CONTRACT_MANAGER | ROLE_PROJECT_MANAGER) as i64;
        rpc_call(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateMember",
            &[
                PyRep::Integer(member_id),
                PyRep::String(String::new()),
                PyRep::Integer(granted_mask),
                PyRep::Integer(0),
            ],
        )
        .await;
        ctx.advance_secs(86_405);
        rpc_call(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateMember",
            &[
                PyRep::Integer(member_id),
                PyRep::String(String::new()),
                PyRep::Integer(granted_mask),
                PyRep::Integer(0),
            ],
        )
        .await;
        let tok = issue_token_with_scopes(
            &ctx,
            ceo_id,
            vec![
                "esi-corporations.read_corporation_membership.v1",
                "esi-corporations.read_titles.v1",
                "esi-corporations.track_members.v1",
            ],
        );

        let roles_json = esi_get_validated(
            &router,
            &spec,
            &format!("/v1/corporations/{corp_id}/roles/"),
            Some(&tok),
        );
        let member_entry = roles_json
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["character_id"] == json!(member_id))
            .expect("member in /roles");
        let role_names: Vec<&str> = member_entry["roles"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(role_names.contains(&"Trader"));
        assert!(role_names.contains(&"Contract_Manager"));
        assert!(role_names.contains(&"Project_Manager"));

        let history_json = esi_get_validated(
            &router,
            &spec,
            &format!("/v1/corporations/{corp_id}/roles/history/"),
            Some(&tok),
        );
        let hist_entry = history_json
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["character_id"] == json!(member_id) && h["issuer_id"] == json!(ceo_id))
            .expect("role history entry");
        let new_roles: Vec<&str> = hist_entry["new_roles"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(new_roles.contains(&"Trader"));
        assert!(new_roles.contains(&"Contract_Manager"));

        // 3. Live sovereignty claim -> reflected in GET /sovereignty/map
        ctx.db.with_mut_snapshot(|snap| {
            snap.system_states.entry(null_sys).or_default();
        });
        claim_system_sovereignty(&ctx, null_sys, alliance_id, corp_id, 0)
            .expect("claim_system_sovereignty");
        let sov_map = esi_get_validated(&router, &spec, "/v1/sovereignty/map/", None);
        let sys_entry = sov_map
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["system_id"] == json!(null_sys as i64))
            .expect("claimed system in /sovereignty/map");
        assert_eq!(sys_entry["alliance_id"], json!(alliance_id));
        assert_eq!(sys_entry["corporation_id"], json!(corp_id));
    }
}

pub(crate) mod esi_corp_blueprints_structures_live {
    // End-to-end behavioral verification integration tests for ESI corp blueprints/structures
    // and Quasar title definitions, space CDN, freelance projects/item delivery,
    // and payment schedules/tokens/store (`esi:*` & `quasar:*` contract verification closure).


    use crate::esi_market_industry_pi::{issue_token_with_scopes, make_item, rpc_call};
    use crate::quasar_sov_equinox_hub::{drain, field, fields, id_req, idm, rpc, sess, subscribe};
    use eve_db::corp_roles::{ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER};
    use eve_db::{CorporationRecord, InventoryFlag, IskCents};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
    use eve_proto::PyRep;
    use eve_server::esi::router::EsiRouter;
    use eve_server::esi::{EsiHttpRequest, OpenApiSpec};
    use eve_server::quasar_gateway::{STATUS_OK, STATUS_UNAUTHORIZED};
    use eve_server::router::ServerContext;
    use eve_server::services::quasar_payment_store_types::{
        save_store_catalog, PaymentTokenRecord, StoreCatalog, StoreCategory,
        StoreOffer, StoreProduct,
    };
    use glam::DVec3;
    use serde_json::{json, Value};
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    fn esi_get_validated(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>) -> Value {
        let mut req = EsiHttpRequest::new("GET", path);
        if let Some(t) = token {
            req = req.with_bearer_token(t);
        }
        let resp = router.handle_request(&req);
        assert_eq!(resp.status_code, 200, "GET {path} -> {}: {}", resp.status_code, String::from_utf8_lossy(&resp.body));
        let raw_path = path.split('?').next().unwrap_or(path);
        spec.validate_response("GET", raw_path, resp.status_code, &resp.headers, &resp.body)
            .unwrap_or_else(|e| panic!("OpenAPI validation failed for GET {path}: {e}"));
        serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
    }

    #[tokio::test]
    async fn test_esi_corp_blueprints_and_structures_live_rpcs() {
        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        let ceo_id = 90_000_001_i64;
        let corp_id = 98_600_010_i64;
        let jita_station = 60_003_760_i64;
        let perim_sys = 30_000_144_u32;
        let ship_id = 80_600_001_i64;
        let bpo_id = 99_600_101_i64;
        let astrahus_item_id = 99_600_102_i64;
        let market_svc_id = 99_600_103_i64;

        let anchor_pos = ctx
            .sde
            .celestials_in_system(perim_sys)
            .into_iter()
            .find(|c| c.group_id == 7)
            .map(|p| [p.position.x + 25_000_000.0, p.position.y, p.position.z])
            .unwrap_or([100_000_000.0, 0.0, 0.0]);

        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = snap.characters[&ceo_id].clone();
            ch.corporation_id = corp_id;
            ch.station_id = Some(jita_station);
            ch.solar_system_id = 30_000_142;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(500_000_000);
            snap.characters.insert(ceo_id, ch);
            snap.corporations.insert(
                corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = corp_id;
                    __corp.name = "Industrial Structure Corp".into();
                    __corp.ticker = "INDS".into();
                    __corp.ceo_id = ceo_id;
                    __corp.tax_rate = 0.05;
                    __corp.members = HashSet::from([ceo_id]);
                    __corp.member_roles = HashMap::from([(
                        ceo_id,
                        ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER,
                    )]);
                    __corp.set_division_balance(1, IskCents::from_isk(100_000_000));
                    __corp },
            );
            // Seed corp BPO (Rifter Blueprint = 684) and Astrahus item (35832) + Standup Market Hub I (35892)
            snap.items.insert(
                bpo_id,
                make_item(bpo_id, 684, corp_id, jita_station, InventoryFlag::CorpSAG1, 1, true),
            );
            snap.items.insert(
                astrahus_item_id,
                make_item(astrahus_item_id, 35832, ceo_id, ship_id, InventoryFlag::Cargo, 1, false),
            );
            snap.items.insert(
                market_svc_id,
                make_item(market_svc_id, 35892, corp_id, jita_station, InventoryFlag::CorpSAG1, 1, false),
            );
        });

        let mut sess = SessionState::new(70_201);
        sess.user_id = Some(1);
        sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
        sess.character_id = Some(ceo_id);
        sess.corporation_id = Some(corp_id);
        sess.station_id = Some(jita_station);
        sess.solar_system_id = Some(30_000_142);
        sess.corp_role = ROLE_DIRECTOR | ROLE_FACTORY_MANAGER | ROLE_STATION_MANAGER;

        // 1. Live industryManager.InstallJob (ME Research = activityID 4) + CompleteJob on corp BPO
        let me_dict = PyRep::Dict(vec![
            (PyRep::String("blueprintID".into()), PyRep::Integer(bpo_id)),
            (PyRep::String("activityID".into()), PyRep::Integer(4)),
            (PyRep::String("runs".into()), PyRep::Integer(2)),
            (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
            (PyRep::String("useCorp".into()), PyRep::Bool(true)),
        ]);
        let job_rep = rpc_call(&ctx, &mut sess, "industryManager", "InstallJob", &[me_dict]).await;
        let job_id = job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("ME jobID");
        ctx.db.clock().advance_secs(50_000);
        rpc_call(&ctx, &mut sess, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)]).await;

        let tok = issue_token_with_scopes(
            &ctx,
            ceo_id,
            vec![
                "esi-corporations.read_blueprints.v1",
                "esi-corporations.read_structures.v1",
            ],
        );
        let bps = esi_get_validated(
            &router,
            &spec,
            &format!("/v1/corporations/{corp_id}/blueprints/"),
            Some(&tok),
        );
        let bp_entry = bps
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["item_id"] == json!(bpo_id))
            .expect("researched corp BPO in /corporations/{id}/blueprints");
        assert_eq!(bp_entry["material_efficiency"], json!(2));
        assert_eq!(bp_entry["runs"], json!(-1));

        // 2. Live structureDeployment.AnchorStructure + structureServices.SetServiceOnlineState
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&ceo_id) {
                ch.station_id = None;
                ch.solar_system_id = perim_sys;
            }
        });
        sess.station_id = None;
        sess.solar_system_id = Some(perim_sys);
        sess.ship_id = Some(ship_id);
        ctx.systems
            .get_or_spawn(perim_sys)
            .spawn_ball(Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::from_array(anchor_pos),
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 3.0,
                mass: 1_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await
            .unwrap();

        let anchor_rep = rpc_call(
            &ctx,
            &mut sess,
            "structureDeployment",
            "Anchor",
            &[
                PyRep::Integer(astrahus_item_id),
                PyRep::Tuple(vec![
                    PyRep::Real(anchor_pos[0] + 60_000.0),
                    PyRep::Real(anchor_pos[1]),
                    PyRep::Real(anchor_pos[2]),
                ]),
                PyRep::String("Perimeter Trade Citadel".into()),
                PyRep::Integer(1),
                PyRep::Integer(18),
            ],
        )
        .await;
        let structure_id = anchor_rep
            .as_i64()
            .or_else(|| anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()))
            .expect("anchored structure_id");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(st) = snap.structures.get_mut(&structure_id) {
                st.state = "ShieldVulnerable".into();
            }
            if let Some(svc) = snap.items.get_mut(&market_svc_id) {
                svc.location_id = structure_id;
                svc.flag = InventoryFlag::ServiceSlot0;
            }
        });
        rpc_call(
            &ctx,
            &mut sess,
            "structureDeployment",
            "RenameStructure",
            &[PyRep::Integer(structure_id), PyRep::String("Perimeter Trade Citadel".into())],
        )
        .await;

        let structs = esi_get_validated(
            &router,
            &spec,
            &format!("/v1/corporations/{corp_id}/structures/"),
            Some(&tok),
        );
        let st_entry = structs
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["structure_id"] == json!(structure_id))
            .expect("anchored structure in /corporations/{id}/structures");
        assert_eq!(st_entry["name"], json!("Perimeter Trade Citadel"));
        assert_eq!(st_entry["reinforce_hour"], json!(21));
        assert_eq!(
            st_entry["services"],
            json!([{ "name": "Standup Market Hub I", "state": "offline" }])
        );
    }

    #[tokio::test]
    async fn test_quasar_title_definitions_and_space_cdn() {
        let ctx = Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let mut s = sess(70_202, 90_000_001, 98_000_001, 30_000_142, 0);

        // 1. Character title definitions GetLatestRequest returns CDN URL; no school-synthesized titles
        let latest_rsp = rpc(
            &ctx,
            &mut s,
            "eve_public.cosmetic.character.title.definition.api.GetLatestRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(latest_rsp.status_code, STATUS_OK);
        let latest_val = latest_rsp.payload.expect("GetLatestResponse").value;
        let cp = field(&latest_val, 1).expect("checkpoint");
        let url_bytes = field(&cp, 1).expect("url");
        let url = String::from_utf8(url_bytes).expect("utf8 url");
        let path = url.strip_prefix("http://127.0.0.1:26004").expect("local ESI HTTP CDN URL");
        assert_eq!(path, "/cdn/cosmetics/character/title/definitions/v24.1.0/en-us.json");
        let cdn_http = router.handle_request(&EsiHttpRequest::new("GET", path));
        assert_eq!(cdn_http.status_code, 200);
        let parsed_defs: Value = serde_json::from_slice(&cdn_http.body).expect("title defs JSON");
        assert_eq!(parsed_defs, json!([]));

        // 3. Space CDN GetUpdateRequest returns configurable CDN URL; EsiRouter does not serve invented MAPT binary
        let mut no_local = Vec::new();
        encode_varint_field(2, 1, &mut no_local);
        let space_rsp = rpc(&ctx, &mut s, "eve_public.space.api.cdn.GetUpdateRequest", no_local).await;
        assert_eq!(space_rsp.status_code, STATUS_OK);
        let space_cp = field(&space_rsp.payload.unwrap().value, 1).expect("space checkpoint");
        let space_url = String::from_utf8(field(&space_cp, 1).unwrap()).unwrap();
        let space_path = space_url.strip_prefix("http://127.0.0.1:26004").expect("local ESI HTTP URL");
        assert_eq!(space_path, format!("/cdn/maptags/v24.1.{}/universe.fb", ctx.sde.solar_systems.len()));
        let map_http = router.handle_request(&EsiHttpRequest::new("GET", space_path));
        assert_eq!(map_http.status_code, 404);

        // 4. Achievement definition CDN GetLatestRequest returns local EsiRouter JSON bundle
        let ach_rsp = rpc(
            &ctx,
            &mut s,
            "eve_public.achievement.definition.api.cdn.GetLatestRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(ach_rsp.status_code, STATUS_OK);
        let ach_cp = field(&ach_rsp.payload.unwrap().value, 1).expect("ach checkpoint");
        let ach_url = String::from_utf8(field(&ach_cp, 1).unwrap()).unwrap();
        let ach_path = ach_url.strip_prefix("http://127.0.0.1:26004").expect("local ESI HTTP URL");
        let ach_mgr = &ctx.quasar_gateway.progression.achievements;
        assert_eq!(ach_path, format!("/cdn/achievements/v24.1.{}/{}.json", ach_mgr.sde_achievement_count, ach_mgr.sde_achievement_hash));
        let ach_http = router.handle_request(&EsiHttpRequest::new("GET", ach_path));
        assert_eq!(ach_http.status_code, 200);
        let ach_defs: Value = serde_json::from_slice(&ach_http.body).expect("ach defs JSON");
        assert_eq!(ach_defs.as_array().unwrap().len(), ach_mgr.sde_achievement_count);

        // 5. GlobalConfig includes ejabberd_client_address (#298) and V24.01 desktop flatbuffer CDN
        //    requests return 200 OK without triggering HTTPS handshake failures (#288, #304).
        let gcfg = eve_server::bug_reporting::build_client_global_config(&ctx, Some("127.0.0.1"));
        assert!(gcfg.contains_key("ejabberd_client_address"));
        let evetech_ach_req = eve_proto::quasar::EvePublicRequest {
            correlation_uuid: vec![1, 2, 3, 4],
            payload: Some(eve_proto::quasar::GoogleAny {
                type_url: "type.evetech.net/eve_public.achievement.definition.api.cdn.GetLatestRequest".to_string(),
                value: Vec::new(),
            }),
            ..Default::default()
        };
        let evetech_ach_rsp = ctx.quasar_gateway.handle_public_request(&s, &evetech_ach_req, None, false, 1_700_000_000);
        assert_eq!(evetech_ach_rsp.status_code, STATUS_OK);
        assert!(evetech_ach_rsp.payload.is_none());
    }

    #[tokio::test]
    async fn test_quasar_freelance_projects_and_item_delivery() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);

        let manager_id = 90_000_001_i64;
        let pilot_id = 90_000_002_i64;
        let corp_id = 98_600_020_i64;
        let jita_station = 60_003_760_i64;
        let ore_item_id = 99_600_201_i64;

        ctx.db.with_mut_snapshot(|snap| {
            let tpl = snap.characters[&manager_id].clone();
            for (cid, name, c_id) in [
                (manager_id, "Project Manager", corp_id),
                (pilot_id, "Contractor Pilot", 1_000_009),
            ] {
                let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| tpl.clone());
                ch.character_id = cid;
                ch.name = name.into();
                ch.corporation_id = c_id;
                ch.station_id = Some(jita_station);
                ch.solar_system_id = 30_000_142;
                ch.balance = IskCents::from_isk(10_000_000);
                snap.characters.insert(cid, ch);
            }
            snap.corporations.insert(
                corp_id,
                { let mut __corp = CorporationRecord::default();
                    __corp.corporation_id = corp_id;
                    __corp.name = "Freelance Sponsor Corp".into();
                    __corp.ticker = "FRSC".into();
                    __corp.ceo_id = manager_id;
                    __corp.tax_rate = 0.05;
                    __corp.members = HashSet::from([manager_id]);
                    __corp.member_roles = HashMap::from([(manager_id, ROLE_DIRECTOR)]);
                    __corp.set_division_balance(1, IskCents::from_isk(100_000_000));
                    __corp },
            );
            // Seed 500 Tritanium (type_id 34) in contractor's Jita hangar
            snap.items.insert(
                ore_item_id,
                make_item(ore_item_id, 34, pilot_id, jita_station, InventoryFlag::Hangar, 500, false),
            );
        });

        let mut mgr_sess = sess(70_203, manager_id, corp_id, 30_000_142, ROLE_DIRECTOR);
        let mut pilot_sess = sess(70_204, pilot_id, 1_000_009, 30_000_142, 0);
        let mut pilot_rx = subscribe(&ctx, &mut pilot_sess).await;

        // 1. CreateRequest with explicit 2,000,000 ISK budget (10,000 ISK/unit * 200), batch=100, type_id=34
        let (mut cfg_b, mut param_b, mut opts_b, mut mod_b, mut rew_b, mut isk_b) = (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        encode_string_field(1, "34", &mut opts_b);
        encode_bytes_field(3, &opts_b, &mut param_b);
        encode_bytes_field(3, &param_b, &mut cfg_b);
        encode_varint_field(4, 100, &mut mod_b);
        encode_varint_field(1, 10_000, &mut isk_b);
        encode_bytes_field(1, &isk_b, &mut rew_b);

        let mut create_req = Vec::new();
        encode_string_field(1, "Supply Jita Tritanium", &mut create_req);
        encode_string_field(2, "Deliver 200 Tritanium to Jita 4-4", &mut create_req);
        encode_varint_field(3, 200, &mut create_req);
        encode_bytes_field(6, &cfg_b, &mut create_req);
        encode_bytes_field(7, &mod_b, &mut create_req);
        encode_bytes_field(10, &rew_b, &mut create_req);
        let create_rsp = rpc(
            &ctx,
            &mut mgr_sess,
            "eve_public.freelance.project.api.CreateRequest",
            create_req.clone(),
        )
        .await;
        assert_eq!(create_rsp.status_code, STATUS_OK);
        let id_msg = field(&create_rsp.payload.unwrap().value, 1).expect("project Identifier");
        assert!(id_msg.len() >= 16);

        // 2. CommitRequest as contractor
        assert_eq!(
            rpc(&ctx, &mut pilot_sess, "eve_public.freelance.project.api.CommitRequest", id_msg.clone())
                .await
                .status_code,
            STATUS_OK
        );

        // 3. DeliverRequest for 200 units -> moves items to CorpDeliveries & emits ContributionSucceededNotice
        let mut deliver_req = id_msg.clone();
        encode_varint_field(4, 200, &mut deliver_req);
        let deliv_rsp = rpc(
            &ctx,
            &mut pilot_sess,
            "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest",
            deliver_req,
        )
        .await;
        assert_eq!(deliv_rsp.status_code, STATUS_OK);

        let notices = drain(&mut pilot_rx);
        assert!(
            notices.iter().any(|(t, _)| t.ends_with("ContributionSucceededNotice")),
            "DeliverRequest must emit ContributionSucceededNotice"
        );

        // Verify items moved from pilot Hangar (500 -> 300) into CorpDeliveries (200)
        ctx.db.with_snapshot(|snap| {
            let pilot_qty: i32 = snap
                .items
                .values()
                .filter(|i| i.owner_id == pilot_id && i.type_id == 34 && i.flag == InventoryFlag::Hangar)
                .map(|i| i.quantity)
                .sum();
            let corp_deliv_qty: i32 = snap
                .items
                .values()
                .filter(|i| i.owner_id == corp_id && i.type_id == 34 && i.flag == InventoryFlag::CorpDeliveries)
                .map(|i| i.quantity)
                .sum();
            assert_eq!(pilot_qty, 300);
            assert_eq!(corp_deliv_qty, 200);
        });

        // 4. RedeemRewardRequest credits contractor & emits ParticipantRewardRedeemedNotice
        let redeem_rsp = rpc(
            &ctx,
            &mut pilot_sess,
            "eve_public.freelance.project.api.RedeemRewardRequest",
            id_msg.clone(),
        )
        .await;
        assert_eq!(redeem_rsp.status_code, STATUS_OK);
        let redeem_notices = drain(&mut pilot_rx);
        assert!(
            redeem_notices.iter().any(|(t, _)| t.ends_with("ParticipantRewardRedeemedNotice")),
            "RedeemRewardRequest must emit ParticipantRewardRedeemedNotice"
        );

        // 5. Cancelled project DeliverRequest must fail without duplicating items into CorpDeliveries
        let create_rsp2 = rpc(&ctx, &mut mgr_sess, "eve_public.freelance.project.api.CreateRequest", create_req).await;
        assert_eq!(create_rsp2.status_code, STATUS_OK);
        let id_msg2 = field(&create_rsp2.payload.unwrap().value, 1).expect("project2 Identifier");
        assert_eq!(rpc(&ctx, &mut pilot_sess, "eve_public.freelance.project.api.CommitRequest", id_msg2.clone()).await.status_code, STATUS_OK);
        assert_eq!(rpc(&ctx, &mut mgr_sess, "eve_public.freelance.project.api.CloseRequest", id_msg2.clone()).await.status_code, STATUS_OK);
        let mut deliver_req2 = id_msg2;
        encode_varint_field(4, 100, &mut deliver_req2);
        assert_ne!(
            rpc(&ctx, &mut pilot_sess, "eve_public.freelance.contributionmethod.itemdelivery.api.DeliverRequest", deliver_req2)
                .await
                .status_code,
            STATUS_OK
        );
        ctx.db.with_snapshot(|snap| {
            let pilot_qty: i32 = snap.items.values().filter(|i| i.owner_id == pilot_id && i.type_id == 34 && i.flag == InventoryFlag::Hangar).map(|i| i.quantity).sum();
            let corp_deliv_qty: i32 = snap.items.values().filter(|i| i.owner_id == corp_id && i.type_id == 34 && i.flag == InventoryFlag::CorpDeliveries).map(|i| i.quantity).sum();
            assert_eq!(pilot_qty, 300);
            assert_eq!(corp_deliv_qty, 200);
        });
    }

    #[tokio::test]
    async fn test_quasar_payment_schedules_tokens_and_store() {
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);

        ctx.db.with_mut_snapshot(|snap| {
            save_store_catalog(
                snap,
                &StoreCatalog {
                    categories: vec![StoreCategory {
                        id: 1,
                        name: "Omega".into(),
                        description: "Omega Clone State".into(),
                    }],
                    products: vec![StoreProduct {
                        id: 101,
                        name: "30 Days Omega".into(),
                        description: "Omega time".into(),
                        type_id: 3966,
                        quantity: 1,
                    }],
                    offers: vec![StoreOffer {
                        id: 1001,
                        name: "30 Days Omega".into(),
                        description: "Monthly Omega".into(),
                        category_id: 1,
                        price_plex: 500,
                        product_ids: vec![101],
                        targeted: true,
                        price_cents: Some(1_999),
                        currency: Some("USD".into()),
                    }],
                },
            );
            let tokens = vec![
                PaymentTokenRecord {
                    id: 88,
                    user_id: 1,
                    token_str: "tok_live_88".into(),
                    is_quick_pay: true,
                    consumed: false,
                },
                PaymentTokenRecord {
                    id: 89,
                    user_id: 1,
                    token_str: "tok_live_89".into(),
                    is_quick_pay: true,
                    consumed: false,
                },
            ];
            snap.quasar_commerce.payment_tokens = tokens;
        });

        let mut s = sess(70_205, 90_000_001, 98_000_001, 30_000_142, 0);
        let mut rx = subscribe(&ctx, &mut s).await;

        // 1. Store category, product, offer, and targetaudience endpoints
        assert_eq!(rpc(&ctx, &mut s, "eve_public.store.category.api.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);
        assert_eq!(rpc(&ctx, &mut s, "eve_public.store.product.api.GetRequest", id_req(1, 101)).await.status_code, STATUS_OK);
        assert_eq!(rpc(&ctx, &mut s, "eve_public.store.offer.api.GetAllRequest", Vec::new()).await.status_code, STATUS_OK);
        let ta_rsp = rpc(
            &ctx,
            &mut s,
            "eve_public.store.targetaudience.offer.api.ListRequest",
            Vec::new(),
        )
        .await;
        assert_eq!(ta_rsp.status_code, STATUS_OK);
        assert!(!fields(&ta_rsp.payload.unwrap().value, 1).is_empty());

        // 2. Payment token GetQuickPayRequest + DisableAllRequest emits QuickPayRemovedNotice (and unauth -> 401)
        let mut unauth = SessionState::new(70_206);
        assert_eq!(
            rpc(&ctx, &mut unauth, "eve_public.payment.token.api.GetQuickPayRequest", Vec::new())
                .await
                .status_code,
            STATUS_UNAUTHORIZED
        );
        let qp_rsp = rpc(&ctx, &mut s, "eve_public.payment.token.api.GetQuickPayRequest", Vec::new()).await;
        assert_eq!(qp_rsp.status_code, STATUS_OK);
        assert!(!fields(&qp_rsp.payload.unwrap().value, 1).is_empty());

        // 3. Purchase Omega via TokenRequest -> creates PaymentScheduleRecord
        let mut purchase_req = Vec::new();
        let mut order = Vec::new();
        let mut off_ident = Vec::new();
        encode_string_field(1, "VEVC-1001", &mut off_ident);
        encode_bytes_field(1, &off_ident, &mut order);
        encode_varint_field(2, 1, &mut order);
        encode_bytes_field(1, &order, &mut purchase_req);
        encode_bytes_field(2, &idm(88), &mut purchase_req);
        assert_eq!(
            rpc(&ctx, &mut s, "eve_public.payment.purchase.api.TokenRequest", purchase_req)
                .await
                .status_code,
            STATUS_OK
        );

        // 4. Advance clock past 30-day billing cycle with quick-pay token -> GetActiveByType is pure read, tick renews & records receipt
        let sched_due: u64 = ctx.db.with_snapshot(|snap| {
            snap.quasar_commerce
                .payment_schedules
                .first()
                .map(|s| s.next_payment_unix)
                .unwrap()
        });
        let advance = sched_due.saturating_add(60).saturating_sub(ctx.now_unix() as u64);
        ctx.advance_secs(advance);
        let mut by_type = Vec::new();
        encode_varint_field(1, 1, &mut by_type);
        assert_eq!(
            rpc(&ctx, &mut s, "eve_public.payment.schedule.api.GetActiveByTypeRequest", by_type.clone())
                .await
                .status_code,
            STATUS_OK
        );
        let receipts_before_tick: usize = ctx.db.with_snapshot(|snap| {
            snap.quasar_commerce.payment_receipts.len()
        });
        assert_eq!(receipts_before_tick, 1);

        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let (renewed_due, receipts_after_tick) = ctx.db.with_snapshot(|snap| {
            let due = snap
                .quasar_commerce
                .payment_schedules
                .first()
                .map(|s| s.next_payment_unix)
                .unwrap();
            let rcpts = snap.quasar_commerce.payment_receipts.clone();
            (due, rcpts)
        });
        assert!(renewed_due > sched_due);
        assert_eq!(receipts_after_tick.len(), 2);
        assert_eq!(receipts_after_tick[1].amount_in_cents, 1_999);

        // 5. Disable quick-pay tokens, advance past renewed due date, and tick -> PaymentFailedNotice & CancelledNotice
        assert_eq!(
            rpc(&ctx, &mut s, "eve_public.payment.token.api.DisableAllRequest", Vec::new())
                .await
                .status_code,
            STATUS_OK
        );
        let tok_notices = drain(&mut rx);
        assert!(tok_notices.iter().any(|(t, _)| t.ends_with("QuickPayRemovedNotice")));

        let advance2 = renewed_due.saturating_add(60).saturating_sub(ctx.now_unix() as u64);
        ctx.advance_secs(advance2);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let sched_notices = drain(&mut rx);
        assert!(sched_notices.iter().any(|(t, _)| t.ends_with("PaymentFailedNotice")));
        assert!(sched_notices.iter().any(|(t, _)| t.ends_with("CancelledNotice")));
        assert_ne!(
            rpc(&ctx, &mut s, "eve_public.payment.schedule.api.GetActiveByTypeRequest", by_type)
                .await
                .status_code,
            STATUS_OK
        );
    }
}
