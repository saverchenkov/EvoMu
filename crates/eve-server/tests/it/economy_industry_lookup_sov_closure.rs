//! End-to-end integration tests for lookupSvc, sovMgr, ESI market history/prices,
//! and ESI industry facilities/jobs driven by real RPCs and gameplay state.

#![forbid(unsafe_code)]

use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use serde_json::Value;
use std::collections::HashMap;
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
            "evomu-w31-test",
            "https://localhost/callback",
            scope_vec,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-w31-test", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code");
    bundle.access_token
}

fn get_json(router: &EsiRouter, spec: &OpenApiSpec, path: &str, token: Option<&str>) -> Value {
    let mut headers = HashMap::new();
    if let Some(t) = token {
        headers.insert("authorization".to_string(), format!("Bearer {t}"));
    }
    let req = EsiHttpRequest {
        method: "GET".to_string(),
        raw_target: path.to_string(),
        headers,
        body: Vec::new(),
        peer_ip: "127.0.0.1".to_string(),
    };
    let resp = router.handle_request(&req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code, 200,
        "GET {path} failed with {}: {}",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    let val: Value = serde_json::from_slice(&resp.body).expect("valid JSON");
    spec.validate_response(&req.method, raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for {path}: {e}"));
    val
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        }),
        PyRep::KeyVal(entries) => entries.iter().find_map(|(k, v)| (k == key).then_some(v)),
        PyRep::Object { args, .. } => dict_get(args, key),
        _ => None,
    }
}

#[tokio::test]
async fn test_lookup_warable_corps_and_alliances_enforces_structure_eligibility() {
    // parity: rpc:lookupSvc.LookupWarableCorporationsOrAlliances, feat:ui.lookup
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(3101);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);
    sess.alliance_id = Some(99_000_001);
    sess.solar_system_id = Some(30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            98_000_110,
            CorporationRecord {
                corporation_id: 98_000_110,
                name: "Vanguard Structureless Corp".into(),
                ticker: "VSC".into(),
                ceo_id: 90_000_110,
                alliance_id: Some(99_000_110),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            99_000_110,
            AllianceRecord {
                alliance_id: 99_000_110,
                name: "Vanguard Structureless Alliance".into(),
                short_name: "VSA".into(),
                executor_corp_id: 98_000_110,
                ..Default::default()
            },
        );
        snap.corporations.insert(
            98_000_111,
            CorporationRecord {
                corporation_id: 98_000_111,
                name: "Vanguard Citadel Corp".into(),
                ticker: "VCC".into(),
                ceo_id: 90_000_111,
                alliance_id: Some(99_000_111),
                ..Default::default()
            },
        );
        snap.alliances.insert(
            99_000_111,
            AllianceRecord {
                alliance_id: 99_000_111,
                name: "Vanguard Citadel Alliance".into(),
                short_name: "VCA".into(),
                executor_corp_id: 98_000_111,
                ..Default::default()
            },
        );
        snap.structures.insert(
            1_030_000_111,
            UpwellStructureRecord {
                structure_id: 1_030_000_111,
                type_id: 35832, // Astrahus
                owner_corp_id: 98_000_111,
                solar_system_id: 30_000_142,
                name: "Vanguard Fort".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });

    let kw = HashMap::new();
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "lookupSvc",
            None,
            "LookupWarableCorporationsOrAlliances",
            &[PyRep::String("Vanguard".into()), PyRep::Bool(false)],
            &kw,
            None,
        )
        .await
        .expect("LookupWarableCorporationsOrAlliances");
    let PyRep::List(items) = res else {
        panic!("expected list from LookupWarableCorporationsOrAlliances");
    };
    assert!(
        !items.iter().any(|w| dict_get(w, "ownerID") == Some(&PyRep::Integer(98_000_110))),
        "Structureless corp must not be warable"
    );
    assert!(
        !items.iter().any(|w| dict_get(w, "ownerID") == Some(&PyRep::Integer(99_000_110))),
        "Alliance with no structure-owning corp must not be warable"
    );
    assert!(
        items.iter().any(|w| {
            dict_get(w, "ownerID") == Some(&PyRep::Integer(98_000_111))
                && dict_get(w, "groupID") == Some(&PyRep::Integer(2))
        }),
        "Structure-owning corp must be warable"
    );
    assert!(
        items.iter().any(|w| {
            dict_get(w, "ownerID") == Some(&PyRep::Integer(99_000_111))
                && dict_get(w, "groupID") == Some(&PyRep::Integer(32))
        }),
        "Alliance with structure-owning corp must be warable"
    );
}

#[tokio::test]
async fn test_sov_structures_info_for_solar_system_filters_to_sov_structures() {
    // parity: rpc:sovMgr.GetSovStructuresInfoForSolarSystem
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();

    let nullsec_sys = 30_000_201u32;
    let hub_id = 1_040_000_201i64;
    let astrahus_id = 1_040_000_202i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32458, // Sovereignty Hub
                owner_corp_id: 98_000_001,
                solar_system_id: nullsec_sys,
                name: "Sov Hub".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            astrahus_id,
            UpwellStructureRecord {
                structure_id: astrahus_id,
                type_id: 35832, // Astrahus (non-sov Upwell structure)
                owner_corp_id: 98_000_001,
                solar_system_id: nullsec_sys,
                name: "Regular Astrahus".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(3102);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);
    sess.solar_system_id = Some(nullsec_sys);

    let kw = HashMap::new();
    let res = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "GetSovStructuresInfoForSolarSystem",
            &[PyRep::Integer(i64::from(nullsec_sys))],
            &kw,
            None,
        )
        .await
        .expect("GetSovStructuresInfoForSolarSystem");

    let PyRep::List(structs) = &res else {
        panic!("expected top-level structures list in GetSovStructuresInfoForSolarSystem");
    };
    assert!(
        structs.iter().any(|s| dict_get(s, "structureID") == Some(&PyRep::Integer(hub_id))),
        "Sovereignty Hub must be included in GetSovStructuresInfoForSolarSystem"
    );
    assert!(
        !structs.iter().any(|s| dict_get(s, "structureID") == Some(&PyRep::Integer(astrahus_id))),
        "Non-sovereignty Upwell Astrahus must be excluded from GetSovStructuresInfoForSolarSystem"
    );
}

#[tokio::test]
async fn test_esi_market_history_and_prices_driven_by_real_market_orders() {
    // parity: esi:GET /markets/{region_id}/history, feat:esi.market.history, esi:GET /markets/prices, feat:esi.market.prices
    let ctx = Arc::new(ServerContext::new());
    ctx.seed_demo_world_fixtures();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let seller_id = 90_000_001i64;
    let buyer_id = 90_000_002i64;
    let jita_station = 60_003_760i64;
    let jita_sys = 30_000_142u32;
    let trit_type = 34u32;

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.clear();
        snap.market_history.clear();
        if let Some(ch) = snap.characters.get_mut(&buyer_id) {
            ch.balance = IskCents(50_000_000);
        }
        snap.items.insert(
            7_700_000_001,
            InventoryItemRecord {
                item_id: 7_700_000_001,
                type_id: trit_type,
                owner_id: seller_id,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut seller_sess = SessionState::new(3103);
    seller_sess.user_id = Some(1);
    seller_sess.character_id = Some(seller_id);
    seller_sess.corporation_id = Some(98_000_001);
    seller_sess.station_id = Some(jita_station);
    seller_sess.solar_system_id = Some(jita_sys);
    seller_sess.region_id = Some(10_000_002);

    let mut buyer_sess = SessionState::new(3104);
    buyer_sess.user_id = Some(1);
    buyer_sess.character_id = Some(buyer_id);
    buyer_sess.corporation_id = Some(98_000_001);
    buyer_sess.station_id = Some(jita_station);
    buyer_sess.solar_system_id = Some(jita_sys);
    buyer_sess.region_id = Some(10_000_002);

    let kw = HashMap::new();
    // Place two sell orders at 10.0 ISK and 20.0 ISK, then buy 100 units at each price via PlaceMultiSellOrder + PlaceBuyOrder
    for price in [10.0_f64, 20.0_f64] {
        ctx.dispatch_rpc(
            &mut seller_sess,
            "marketProxy",
            None,
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(jita_station)),
                    ("typeID", PyRep::Integer(i64::from(trit_type))),
                    ("itemID", PyRep::Integer(7_700_000_001)),
                    ("price", PyRep::Real(price)),
                    ("quantity", PyRep::Integer(100)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("sell PlaceMultiSellOrder");

        ctx.dispatch_rpc(
            &mut buyer_sess,
            "marketProxy",
            None,
            "PlaceBuyOrder",
            &[
                PyRep::Integer(jita_station),
                PyRep::Integer(i64::from(trit_type)),
                PyRep::Real(price),
                PyRep::Integer(100),
                PyRep::Integer(-1), // range station
                PyRep::Integer(1), // minVolume
                PyRep::Integer(90),
            ],
            &kw,
            None,
        )
        .await
        .expect("buy PlaceBuyOrder");
    }

    let hist = get_json(
        &router,
        &spec,
        "/v1/markets/10000002/history/?type_id=34",
        None,
    );
    let hist_arr = hist.as_array().expect("history array");
    assert!(!hist_arr.is_empty(), "Market history must be populated by filled orders");
    let today = hist_arr.last().unwrap();
    assert_eq!(today["volume"], serde_json::json!(200));
    assert!((today["lowest"].as_f64().unwrap() - 10.0).abs() < 1e-6);
    assert!((today["highest"].as_f64().unwrap() - 20.0).abs() < 1e-6);
    assert!((today["average"].as_f64().unwrap() - 15.0).abs() < 1e-6);

    let prices = get_json(&router, &spec, "/v1/markets/prices/", None);
    let prices_arr = prices.as_array().expect("prices array");
    let trit_price = prices_arr
        .iter()
        .find(|p| p["type_id"] == serde_json::json!(34))
        .expect("Tritanium in /markets/prices");
    assert!((trit_price["average_price"].as_f64().unwrap() - 15.0).abs() < 1e-6);
    assert!((trit_price["adjusted_price"].as_f64().unwrap() - 15.0).abs() < 1e-6);
}

#[tokio::test]
async fn test_esi_industry_facilities_and_jobs_driven_by_real_industry_rpcs() {
    // parity: esi:GET /industry/facilities, feat:esi.industry.facilities, esi:GET /characters/{character_id}/industry/jobs, feat:esi.industry.char-jobs, esi:GET /corporations/{corporation_id}/industry/jobs, feat:esi.industry.corp-jobs
    let ctx = Arc::new(ServerContext::new());
    ctx.seed_demo_world_fixtures();
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    // 1. GET /industry/facilities must include SDE NPC industry stations with 0.0025 tax even before any jobs exist
    let facilities = get_json(&router, &spec, "/v1/industry/facilities/", None);
    let fac_arr = facilities.as_array().expect("facilities array");
    let jita_fac = fac_arr
        .iter()
        .find(|f| f["facility_id"] == serde_json::json!(60_003_760))
        .expect("Jita 4-4 NPC industry station in /industry/facilities");
    assert_eq!(jita_fac["tax"], serde_json::json!(0.0025));

    // 2. Install and complete a real manufacturing job via industryManager RPCs
    let char_id = 90_000_001i64;
    let corp_id = 98_000_001i64;
    let station_id = 60_003_760i64;
    let bp_item_id = 7_700_000_010i64;
    let bp_type_id = 681u32; // Bantam Blueprint

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(char_id, u64::MAX);
        }
        snap.items.insert(
            bp_item_id,
            InventoryItemRecord {
                item_id: bp_item_id,
                type_id: bp_type_id,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Seed ample minerals for manufacturing
        for (idx, (mat_tid, qty)) in [
            (34u32, 50_000i32),
            (35, 20_000),
            (36, 10_000),
            (37, 5_000),
            (38, 5_000),
        ]
        .into_iter()
        .enumerate()
        {
            snap.items.insert(
                7_700_000_020 + idx as i64,
                InventoryItemRecord {
                    item_id: 7_700_000_020 + idx as i64,
                    type_id: mat_tid,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mut sess = SessionState::new(3105);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.solar_system_id = Some(30_000_142);

    let mut kw = HashMap::new();
    kw.insert("blueprintItemID".to_string(), PyRep::Integer(bp_item_id));
    kw.insert("useCorp".to_string(), PyRep::Bool(true));
    let install_res = ctx
        .dispatch_rpc(
            &mut sess,
            "industryManager",
            None,
            "InstallJob",
            &[
                PyRep::Integer(i64::from(bp_type_id)),
                PyRep::Integer(1), // Manufacturing
                PyRep::Integer(1), // 1 run
            ],
            &kw,
            None,
        )
        .await
        .expect("InstallJob");
    let job_id = dict_get(&install_res, "jobID")
        .and_then(PyRep::as_i64)
        .expect("jobID");

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
        &format!("/v1/characters/{char_id}/industry/jobs/"),
        Some(&token),
    );
    let cj_arr = char_jobs.as_array().expect("char jobs array");
    let job_obj = cj_arr
        .iter()
        .find(|j| j["job_id"] == serde_json::json!(job_id))
        .expect("installed job in /characters/{id}/industry/jobs");
    assert_eq!(job_obj["blueprint_location_id"], serde_json::json!(station_id));
    assert_eq!(job_obj["output_location_id"], serde_json::json!(station_id));
    assert!(job_obj["cost"].as_f64().unwrap_or(0.0) > 0.0);

    // Advance job start_unix so it is ready to complete, then call CompleteJob
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(j) = snap.industry_jobs.get_mut(&job_id) {
            j.start_unix = j.start_unix.saturating_sub(j.duration_seconds + 10);
        }
    });
    ctx.dispatch_rpc(
        &mut sess,
        "industryManager",
        None,
        "CompleteJob",
        &[PyRep::Integer(job_id)],
        &kw,
        None,
    )
    .await
    .expect("CompleteJob");

    let char_jobs_after = get_json(
        &router,
        &spec,
        &format!("/v1/characters/{char_id}/industry/jobs/?include_completed=true"),
        Some(&token),
    );
    let done_obj = char_jobs_after
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["job_id"] == serde_json::json!(job_id))
        .expect("completed job");
    assert_eq!(done_obj["status"], serde_json::json!("delivered"));
    assert!(done_obj.get("completed_date").is_some());
    assert_eq!(done_obj["completed_character_id"], serde_json::json!(char_id));

    let corp_jobs = get_json(
        &router,
        &spec,
        &format!("/v1/corporations/{corp_id}/industry/jobs/?include_completed=true"),
        Some(&token),
    );
    assert!(corp_jobs
        .as_array()
        .unwrap()
        .iter()
        .any(|j| j["job_id"] == serde_json::json!(job_id) && j["status"] == serde_json::json!("delivered")));
}
