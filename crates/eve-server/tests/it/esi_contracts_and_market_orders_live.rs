//! Authentic live-RPC end-to-end integration tests for ESI Contracts and Market Orders:
//! - `feat:esi.contracts.char-list`
//! - `feat:esi.contracts.char-items-bids`
//! - `feat:esi.contracts.corp`
//! - `feat:esi.contracts.public`
//! - `feat:esi.market.char-orders`
//! - `feat:esi.market.corp-orders`
//! - `feat:esi.market.prices`
//! - `feat:esi.market.structure-orders`

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use eve_server::router::ServerContext;
use eve_server::services::station_structure::structure_lifecycle as life;
use eve_server::world_seeder::seed_full_market_catalog;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const PERIMETER_SYS: u32 = 30_000_144;
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn login_and_select(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    ctx.advance_secs(12);
    sess
}

fn issue_sso_token(ctx: &ServerContext, user: &str, pass: &str, char_id: i64, scopes: &[&str]) -> String {
    let now = ctx.now_unix();
    let scope_vec: Vec<String> = scopes.iter().map(|s| (*s).to_string()).collect();
    let code = ctx
        .db
        .authorize_pkce(
            user,
            pass,
            char_id,
            "evomu-live-esi-it",
            "https://localhost/callback",
            scope_vec,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-live-esi-it", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

fn check_esi_200(router: &EsiRouter, spec: &OpenApiSpec, target: &str, token: Option<&str>) -> (Value, String, Option<String>) {
    let mut req = EsiHttpRequest::new("GET", target);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    let raw_path = req.raw_target.split('?').next().unwrap_or(&req.raw_target);
    assert_eq!(
        resp.status_code, 200,
        "Expected 200 for {target}, got {} ({})",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
    spec.validate_response(&req.method, raw_path, resp.status_code, &resp.headers, &resp.body)
        .unwrap_or_else(|e| panic!("OpenAPI validation failed for {target}: {e}"));
    let etag = resp.header("ETag").expect("ETag header required").to_string();
    assert!(resp.header("Cache-Control").is_some() || resp.header("Expires").is_some());
    let x_pages = resp.header("X-Pages").map(str::to_string);
    (resp.json().expect("valid JSON"), etag, x_pages)
}

fn assert_esi_status(router: &EsiRouter, target: &str, token: Option<&str>, expected: u16) {
    let mut req = EsiHttpRequest::new("GET", target);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    assert_eq!(
        resp.status_code, expected,
        "Expected {expected} for {target}, got {} ({})",
        resp.status_code,
        String::from_utf8_lossy(&resp.body)
    );
}

fn find_hangar_items(ctx: &ServerContext, owner_id: i64, loc_id: i64, type_id: u32) -> Vec<(i64, i32)> {
    let mut out: Vec<_> = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.owner_id == owner_id && i.location_id == loc_id && i.type_id == type_id)
            .map(|i| (i.item_id, i.quantity))
            .collect()
    });
    out.sort_by_key(|x| x.0);
    out
}

fn collect_skill_topo(sde: &eve_sde::SdeRegistry, sid: u32, seen: &mut Vec<u32>, out: &mut Vec<u32>) {
    if seen.contains(&sid) {
        return;
    }
    seen.push(sid);
    if let Some(t) = sde.types.get(&sid) {
        for attr in [182_u16, 183, 184, 1285, 1289, 1290] {
            if let Some(&req) = t.default_attributes.get(&attr) {
                if req > 0.0 {
                    collect_skill_topo(sde, req as u32, seen, out);
                }
            }
        }
        if t.category_id == 16 {
            out.push(sid);
        }
    }
}

async fn grant_skills_for_type(ctx: &ServerContext, sess: &mut SessionState, type_id: u32) {
    let (mut seen, mut ordered) = (Vec::new(), Vec::new());
    collect_skill_topo(&ctx.sde, type_id, &mut seen, &mut ordered);
    for sid in ordered {
        common::slash_cmd(ctx, sess, &format!("/giveskill {sid} 5")).await;
    }
}

#[tokio::test]
async fn test_esi_live_contracts_character_corporation_and_public() {
    // parity: feat:esi.contracts.char-list, feat:esi.contracts.char-items-bids
    // parity: feat:esi.contracts.corp, feat:esi.contracts.public
    eve_testkit::attest(&[
        "feat:esi.contracts.char-list",
        "feat:esi.contracts.char-items-bids",
        "feat:esi.contracts.corp",
        "feat:esi.contracts.public",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    ctx.db.provision_gm_account("demo", "demo-secret", PILOT_1, "Aura GM");
    ctx.db.provision_gm_account("other", "other-secret", PILOT_2, "Rival GM");

    let mut s1 = login_and_select(&ctx, 8901, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = login_and_select(&ctx, 8902, "other", "other-secret", PILOT_2).await;

    common::slash_cmd(&ctx, &mut s1, "/giveskill 3363 5").await;
    common::slash_cmd(&ctx, &mut s1, "/giveisk 500000000").await;
    common::slash_cmd(&ctx, &mut s2, "/giveisk 500000000").await;
    ctx.advance_secs(31);

    let corp_rep = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Live Contract Corp".into()), PyRep::String("LCON".into()), PyRep::Real(0.05)],
    )
    .await
    .expect("AddCorporation");
    let corp_id = corp_rep.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corp_id");
    ctx.advance_virtual_clock(8 * 86_400);

    rpc(
        &ctx,
        &mut s1,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_id), PyRep::Real(50_000_000.0), PyRep::String("Fund corp".into())],
    )
    .await
    .expect("GiveCash to corp");

    common::slash_cmd(&ctx, &mut s1, "/giveitem 587 1").await;
    common::slash_cmd(&ctx, &mut s1, "/giveitem 587 1").await;
    common::slash_cmd(&ctx, &mut s1, "/giveitem 34 10000").await;
    ctx.advance_secs(12);

    let active_ship = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).map(|c| c.active_ship_id));
    let rifters: Vec<_> = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 587)
        .into_iter()
        .filter(|(id, _)| Some(*id) != active_ship)
        .collect();
    assert!(rifters.len() >= 2, "Expected at least 2 non-active Rifters in hangar");
    let rifter_auc_id = rifters[0].0;
    let rifter_corp_id = rifters[1].0;

    let trits = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 34);
    assert!(!trits.is_empty(), "Expected Tritanium in hangar");
    let trit_id = trits[0].0;

    // 1. Create Public Auction contract
    let auc_rep = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(2),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(JITA_STATION),
            PyRep::Real(1_000_000.0),
            PyRep::Real(0.0),
            PyRep::Real(10_000_000.0),
            PyRep::String("Live Public Auction Rifter".into()),
            PyRep::None,
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(rifter_auc_id), PyRep::Integer(1)])]),
            PyRep::None,
            PyRep::List(vec![]),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("Create Auction contract");
    let auc_id = auc_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    // Pilot 2 places bid on Auction
    rpc(
        &ctx,
        &mut s2,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(auc_id), PyRep::Real(2_000_000.0)],
    )
    .await
    .expect("Place bid on Auction");

    // 2. Create Public ItemExchange contract
    let ie_rep = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(JITA_STATION),
            PyRep::Real(50_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("Live Tritanium Exchange".into()),
            PyRep::None,
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(trit_id), PyRep::Integer(5000)])]),
            PyRep::None,
            PyRep::List(vec![]),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("Create ItemExchange contract");
    let ie_id = ie_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    // 3. Create Corporation contract
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&rifter_corp_id) {
            it.owner_id = corp_id;
            it.flag = eve_db::InventoryFlag::CorpSAG1;
        }
    });
    let corp_c_rep = rpc(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1),
            PyRep::Integer(2),
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(JITA_STATION),
            PyRep::Real(75_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String("Live Corp Fleet Rifter".into()),
            PyRep::None,
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(rifter_corp_id), PyRep::Integer(1)])]),
            PyRep::None,
            PyRep::List(vec![]),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("Create Corp contract");
    let corp_c_id = corp_c_rep.dict_get("contractID").and_then(|v| v.as_i64()).unwrap();

    let p1_char_tok = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-contracts.read_character_contracts.v1"]);
    let p1_corp_tok = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-contracts.read_corporation_contracts.v1"]);

    // ESI feat:esi.contracts.char-list
    assert_esi_status(&router, &format!("/v1/characters/{PILOT_1}/contracts/"), None, 401);
    let (char_c_json, char_c_etag, _) = check_esi_200(&router, &spec, &format!("/v1/characters/{PILOT_1}/contracts/"), Some(&p1_char_tok));
    let cond_c = EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_1}/contracts/")).with_bearer_token(&p1_char_tok).with_header("If-None-Match", &char_c_etag);
    assert_eq!(router.handle_request(&cond_c).status_code, 304);
    let c_arr = char_c_json.as_array().expect("contracts array");
    let found_auc = c_arr.iter().find(|c| c["contract_id"] == json!(auc_id)).expect("Auction in char contracts");
    assert_eq!(found_auc["issuer_id"], json!(PILOT_1));
    assert_eq!(found_auc["type"], json!("auction"));
    assert_eq!(found_auc["status"], json!("outstanding"));
    let found_ie = c_arr.iter().find(|c| c["contract_id"] == json!(ie_id)).expect("ItemExchange in char contracts");
    assert_eq!(found_ie["issuer_id"], json!(PILOT_1));
    assert_eq!(found_ie["type"], json!("item_exchange"));
    assert_eq!(found_ie["price"], json!(50_000.0));

    // ESI feat:esi.contracts.char-items-bids
    let (auc_items, _, _) = check_esi_200(&router, &spec, &format!("/v1/characters/{PILOT_1}/contracts/{auc_id}/items/"), Some(&p1_char_tok));
    let items_arr = auc_items.as_array().expect("items array");
    let rifter_item = items_arr.iter().find(|i| i["type_id"] == json!(587)).expect("Rifter in contract items");
    assert_eq!(rifter_item["quantity"], json!(1));

    let (auc_bids, _, _) = check_esi_200(&router, &spec, &format!("/v1/characters/{PILOT_1}/contracts/{auc_id}/bids/"), Some(&p1_char_tok));
    let bids_arr = auc_bids.as_array().expect("bids array");
    let bid_entry = bids_arr.iter().find(|b| b["bidder_id"] == json!(PILOT_2)).expect("Pilot 2 bid in auction bids");
    assert_eq!(bid_entry["amount"], json!(2_000_000.0));

    // ESI feat:esi.contracts.corp
    assert_esi_status(&router, &format!("/v1/corporations/{corp_id}/contracts/"), None, 401);
    let (corp_c_json, _, _) = check_esi_200(&router, &spec, &format!("/v1/corporations/{corp_id}/contracts/"), Some(&p1_corp_tok));
    let corp_arr = corp_c_json.as_array().expect("corp contracts array");
    let found_corp_c = corp_arr.iter().find(|c| c["contract_id"] == json!(corp_c_id)).expect("Corp contract found");
    assert_eq!(found_corp_c["for_corporation"], json!(true));
    assert_eq!(found_corp_c["price"], json!(75_000.0));

    let (corp_items, _, _) = check_esi_200(&router, &spec, &format!("/v1/corporations/{corp_id}/contracts/{corp_c_id}/items/"), Some(&p1_corp_tok));
    let c_items_arr = corp_items.as_array().expect("corp contract items array");
    assert!(c_items_arr.iter().any(|i| i["type_id"] == json!(587)));

    let (corp_bids, _, _) = check_esi_200(&router, &spec, &format!("/v1/corporations/{corp_id}/contracts/{corp_c_id}/bids/"), Some(&p1_corp_tok));
    assert!(corp_bids.as_array().is_some());

    // ESI feat:esi.contracts.public
    let forge_region = 10_000_002_u32;
    let (pub_c_json, pub_etag, _) = check_esi_200(&router, &spec, &format!("/v1/contracts/public/{forge_region}/"), None);
    let cond_pub = EsiHttpRequest::new("GET", &format!("/v1/contracts/public/{forge_region}/")).with_header("If-None-Match", &pub_etag);
    assert_eq!(router.handle_request(&cond_pub).status_code, 304);
    let pub_arr = pub_c_json.as_array().expect("public contracts array");
    assert!(pub_arr.iter().any(|c| c["contract_id"] == json!(auc_id)));
    assert!(pub_arr.iter().any(|c| c["contract_id"] == json!(ie_id)));

    let (pub_items, _, _) = check_esi_200(&router, &spec, &format!("/v1/contracts/public/items/{auc_id}/"), None);
    assert!(pub_items.as_array().unwrap().iter().any(|i| i["type_id"] == json!(587)));

    let (pub_bids, _, _) = check_esi_200(&router, &spec, &format!("/v1/contracts/public/bids/{auc_id}/"), None);
    assert!(pub_bids.as_array().unwrap().iter().any(|b| b["amount"] == json!(2_000_000.0)));
}

#[tokio::test]
async fn test_esi_live_market_orders_prices_and_structures() {
    // parity: feat:esi.market.char-orders, feat:esi.market.corp-orders
    // parity: feat:esi.market.prices, feat:esi.market.structure-orders
    eve_testkit::attest(&[
        "feat:esi.market.char-orders",
        "feat:esi.market.corp-orders",
        "feat:esi.market.prices",
        "feat:esi.market.structure-orders",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    ctx.db.provision_gm_account("demo", "demo-secret", PILOT_1, "Aura GM");
    let mut s1 = login_and_select(&ctx, 8911, "demo", "demo-secret", PILOT_1).await;

    grant_skills_for_type(&ctx, &mut s1, 3363).await;
    grant_skills_for_type(&ctx, &mut s1, 20185).await;
    common::slash_cmd(&ctx, &mut s1, "/giveisk 10000000000").await;
    ctx.advance_secs(31);

    let corp_rep = rpc(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Live Market Corp".into()), PyRep::String("LMKT".into()), PyRep::Real(0.02)],
    )
    .await
    .expect("AddCorporation");
    let corp_id = corp_rep.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corp_id");
    ctx.advance_virtual_clock(8 * 86_400);

    rpc(
        &ctx,
        &mut s1,
        "account",
        "GiveCash",
        &[PyRep::Integer(corp_id), PyRep::Real(1_000_000_000.0), PyRep::String("Corp funding".into())],
    )
    .await
    .expect("GiveCash to corp");

    // 1. Character orders (`feat:esi.market.char-orders`)
    let o1_rep = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(34),
            PyRep::Real(0.01),
            PyRep::Integer(1000),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(30),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder char 1");
    let char_oid_1 = o1_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    let o2_rep = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(34),
            PyRep::Real(0.01),
            PyRep::Integer(500),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(30),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder char 2");
    let char_oid_2 = o2_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(char_oid_2)])
        .await
        .expect("CancelCharOrder 2");

    let p1_mkt_tok = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-markets.read_character_orders.v1"]);
    assert_esi_status(&router, &format!("/v1/characters/{PILOT_1}/orders/"), None, 401);
    let (char_orders, char_o_etag, _) = check_esi_200(&router, &spec, &format!("/v1/characters/{PILOT_1}/orders/"), Some(&p1_mkt_tok));
    let cond_o = EsiHttpRequest::new("GET", &format!("/v1/characters/{PILOT_1}/orders/")).with_bearer_token(&p1_mkt_tok).with_header("If-None-Match", &char_o_etag);
    assert_eq!(router.handle_request(&cond_o).status_code, 304);
    let arr_co = char_orders.as_array().expect("char orders array");
    assert!(arr_co.iter().any(|o| o["order_id"] == json!(char_oid_1) && o["volume_remain"] == json!(1000)));
    assert!(!arr_co.iter().any(|o| o["order_id"] == json!(char_oid_2)));

    let (char_hist, _, _) = check_esi_200(&router, &spec, &format!("/v1/characters/{PILOT_1}/orders/history/"), Some(&p1_mkt_tok));
    let arr_ch = char_hist.as_array().expect("char history array");
    let hist_order = arr_ch.iter().find(|o| o["order_id"] == json!(char_oid_2)).expect("cancelled order in history");
    assert_eq!(hist_order["state"], json!("cancelled"));

    // 2. Corporation orders (`feat:esi.market.corp-orders`)
    let co1_rep = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(34),
            PyRep::Real(0.01),
            PyRep::Integer(2000),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(30),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("PlaceBuyOrder corp 1");
    let corp_oid_1 = co1_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    let co2_rep = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(34),
            PyRep::Real(0.01),
            PyRep::Integer(1000),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(30),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("PlaceBuyOrder corp 2");
    let corp_oid_2 = co2_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(corp_oid_2)])
        .await
        .expect("Cancel corp order 2");

    let p1_corp_mkt_tok = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-markets.read_corporation_orders.v1"]);
    assert_esi_status(&router, &format!("/v1/corporations/{corp_id}/orders/"), None, 401);
    let (corp_orders, _, _) = check_esi_200(&router, &spec, &format!("/v1/corporations/{corp_id}/orders/"), Some(&p1_corp_mkt_tok));
    let arr_corp_o = corp_orders.as_array().expect("corp orders array");
    assert!(arr_corp_o.iter().any(|o| o["order_id"] == json!(corp_oid_1) && o["volume_remain"] == json!(2000)));
    assert!(!arr_corp_o.iter().any(|o| o["order_id"] == json!(corp_oid_2)));

    let (corp_hist, _, _) = check_esi_200(&router, &spec, &format!("/v1/corporations/{corp_id}/orders/history/"), Some(&p1_corp_mkt_tok));
    let arr_corp_h = corp_hist.as_array().expect("corp history array");
    let corp_hist_order = arr_corp_h.iter().find(|o| o["order_id"] == json!(corp_oid_2)).expect("cancelled corp order in history");
    assert_eq!(corp_hist_order["state"], json!("cancelled"));

    // 3. Market prices (`feat:esi.market.prices`)
    let (prices, prices_etag, _) = check_esi_200(&router, &spec, "/v1/markets/prices/", None);
    let cond_p = EsiHttpRequest::new("GET", "/v1/markets/prices/").with_header("If-None-Match", &prices_etag);
    assert_eq!(router.handle_request(&cond_p).status_code, 304);
    let arr_prices = prices.as_array().expect("prices array");
    let trit_price = arr_prices.iter().find(|p| p["type_id"] == json!(34)).expect("Tritanium price row");
    assert!(trit_price["adjusted_price"].as_f64().unwrap() > 0.0);

    // 4. Structure market orders (`feat:esi.market.structure-orders`)
    let fortizar_tid = 35833_u32;
    let core_tid = ctx.sde.types[&fortizar_tid].default_attributes[&life::ATTR_REQUIRES_DEED_TYPE] as u32;
    common::slash_cmd(&ctx, &mut s1, "/createitem 20185 1").await;
    common::slash_cmd(&ctx, &mut s1, &format!("/createitem {fortizar_tid} 1")).await;
    common::slash_cmd(&ctx, &mut s1, &format!("/createitem {core_tid} 1")).await;
    common::slash_cmd(&ctx, &mut s1, "/createitem 35892 1").await;
    common::slash_cmd(&ctx, &mut s1, "/createitem 4051 3000").await;
    ctx.advance_secs(12);

    let freighter_id = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 20185)[0].0;
    rpc(&ctx, &mut s1, "ship", "AssembleShip", &[PyRep::Integer(freighter_id)]).await.expect("AssembleShip Charon");
    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(freighter_id)]).await.expect("Board Charon");

    for tid in [fortizar_tid, core_tid, 35892, 4051] {
        let iid = find_hangar_items(&ctx, PILOT_1, JITA_STATION, tid)[0].0;
        let qty = if tid == 4051 { 3000 } else { 1 };
        rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(iid), PyRep::Integer(freighter_id), PyRep::Integer(5), PyRep::Integer(qty)])
            .await
            .expect("Load cargo into Charon");
    }

    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.expect("Undock Charon");
    common::slash_cmd(&ctx, &mut s1, "/tr 30000144").await;
    ctx.advance_secs(12);

    let anchor_rep = rpc(
        &ctx,
        &mut s1,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(fortizar_tid as i64), PyRep::String("Live Perimeter Market Hub".into())],
    )
    .await
    .expect("Anchor Fortizar");
    let structure_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).expect("structureID");

    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS + 10);
    let st_pos = ctx.db.with_snapshot(|s| glam::DVec3::from_array(s.structures[&structure_id].position));
    ctx.systems.get_or_spawn(PERIMETER_SYS).set_ball_position(freighter_id, st_pos).await;
    rpc(&ctx, &mut s1, "structureDocking", "Dock", &[PyRep::Integer(structure_id)]).await.expect("Dock in Fortizar");

    let core_item = find_hangar_items(&ctx, PILOT_1, freighter_id, core_tid)[0].0;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(structure_id), PyRep::Integer(172)])
        .await
        .expect("Install Quantum Core");

    let fuel_item = find_hangar_items(&ctx, PILOT_1, freighter_id, 4051)[0].0;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(fuel_item), PyRep::Integer(structure_id), PyRep::Integer(133), PyRep::Integer(2900)])
        .await
        .expect("Load Fuel Bay");

    ctx.advance_virtual_clock(life::ONLINING_SECS + 15);
    let hub_item = find_hangar_items(&ctx, PILOT_1, freighter_id, 35892)[0].0;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(hub_item), PyRep::Integer(structure_id), PyRep::Integer(164)])
        .await
        .expect("Fit Standup Market Hub");

    let st_buy_rep = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(structure_id),
            PyRep::Integer(34),
            PyRep::Real(10.0),
            PyRep::Integer(500),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(14),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder at Fortizar");
    let st_buy_oid = st_buy_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    let p1_struct_tok = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-markets.structure_markets.v1"]);
    let st_url = format!("/v1/markets/structures/{structure_id}/");
    assert_esi_status(&router, &st_url, None, 401);
    let (st_orders, st_etag, _) = check_esi_200(&router, &spec, &st_url, Some(&p1_struct_tok));
    let cond_st = EsiHttpRequest::new("GET", &st_url).with_bearer_token(&p1_struct_tok).with_header("If-None-Match", &st_etag);
    assert_eq!(router.handle_request(&cond_st).status_code, 304);
    let arr_st = st_orders.as_array().expect("structure orders array");
    let st_o = arr_st.iter().find(|o| o["order_id"] == json!(st_buy_oid)).expect("structure order found");
    assert_eq!(st_o["location_id"], json!(structure_id));
    assert_eq!(st_o["type_id"], json!(34));
    assert_eq!(st_o["price"], json!(10.0));
}
