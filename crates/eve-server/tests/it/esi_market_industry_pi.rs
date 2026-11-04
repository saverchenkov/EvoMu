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
async fn test_esi6_industry_jobs_fields_and_completion() {
    // parity: esi:GET /characters/{character_id}/industry/jobs, esi:GET /corporations/{corporation_id}/industry/jobs
    // parity: feat:esi.industry.char-jobs, feat:esi.industry.corp-jobs
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/industry/jobs",
        "esi:GET /corporations/{corporation_id}/industry/jobs",
        "feat:esi.industry.char-jobs",
        "feat:esi.industry.corp-jobs",
    ]);

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
        for (idx, dc_tid) in [20424_u32, 20172, 20411, 20412, 20414, 20416].into_iter().enumerate() {
            snap.items.insert(
                925_200 + idx as i64,
                make_item(925_200 + idx as i64, dc_tid, char_id, jita_station, InventoryFlag::Hangar, 10, false),
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
    // parity: esi:GET /markets/groups/{market_group_id}, feat:esi.market.groups, esi:GET /markets/structures/{structure_id}, feat:esi.market.structure-orders
    eve_testkit::attest(&[
        "esi:GET /markets/groups/{market_group_id}",
        "feat:esi.market.groups",
        "esi:GET /markets/structures/{structure_id}",
        "feat:esi.market.structure-orders",
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
            CorporationRecord {
                corporation_id: corp_id,
                name: "Market Hub Test Corp".into(),
                ticker: "MHTC".into(),
                ceo_id: char_id,
                members: HashSet::from([char_id]),
                ..Default::default()
            },
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
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Role Gate Remediation Corp".into(),
            ticker: "RGRC".into(),
            ceo_id,
            members: HashSet::from([
                ceo_id,
                no_role_id,
                sec_officer_id,
                accountant_id,
                jr_accountant_id,
                trader_id,
                station_mgr_id,
            ]),
            member_roles: HashMap::from([
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
            ]),
            master_wallet: IskCents::from_isk(250_000_000),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        for d in &mut corp.wallet_divisions {
            d.balance = IskCents::from_isk(250_000_000);
        }
        snap.corporations.insert(corp_id, corp);

        // Refinery with online Standup Moon Drill I (35894) + Gantry/POCO items in cargo
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
            make_item(951_001, 35_894, corp_id, observer_id, InventoryFlag::ServiceSlot0, 1, true),
        );
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
            make_item(escrow_item_id, 587, char_id, jita_station, InventoryFlag::Hangar, 1, true),
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
            (940_002, 35_894, InventoryFlag::ServiceSlot1),
        ] {
            snap.items
                .insert(iid, make_item(iid, tid, corp_id, struct_id, flag, 1, true));
        }
        eve_server::services::industry_pi::industry_facilities_bps::save_facility_taxes(
            snap,
            struct_id,
            &std::collections::HashMap::from([(1_u8, 0.08)]),
        );
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
