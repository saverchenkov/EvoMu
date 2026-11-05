//! End-to-end live-RPC ESI integration tests for market prices, rolling EIV, ship insurance, and structure markets:
//! - `esi:GET /markets/prices`, `feat:esi.market.prices`
//! - `esi:GET /markets/structures/{structure_id}`, `feat:esi.market.structure-orders`

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
const JITA_SYS: u32 = 30_000_142;
const PERIMETER_SYS: u32 = 30_000_144;
const JITA_STATION: i64 = 60_003_760;
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
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

async fn fund_pilots_via_gm_transfer(ctx: &ServerContext, targets: &[(i64, f64)]) {
    ctx.db
        .provision_gm_account("isd_treasury", "isd-treasury-secret", 90_000_099, "ISD Treasury");
    let mut gm = login_and_select(ctx, 7910, "isd_treasury", "isd-treasury-secret", 90_000_099).await;
    rpc(ctx, &mut gm, "slash", "SlashCmd", &[PyRep::String("/giveisk 200000000000".into())])
        .await
        .expect("GM /giveisk");
    for &(cid, amount) in targets {
        rpc(
            ctx,
            &mut gm,
            "account",
            "GiveCash",
            &[PyRep::Integer(cid), PyRep::Real(amount), PyRep::String("test-grant".into())],
        )
        .await
        .expect("GM GiveCash");
    }
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

async fn train_skills_to_five(ctx: &ServerContext, sess: &mut SessionState, skill_ids: &[u32]) {
    let char_id = sess.character_id.unwrap();
    let (mut seen, mut ordered) = (Vec::new(), Vec::new());
    for &sid in skill_ids {
        collect_skill_topo(&ctx.sde, sid, &mut seen, &mut ordered);
    }
    for sid in ordered {
        let curr = ctx
            .db
            .with_snapshot(|s| s.skills.get(&char_id).and_then(|m| m.get(&sid)).map(|r| r.level).unwrap_or(0));
        if curr >= 5 {
            continue;
        }
        let _ = rpc(ctx, sess, "skillMgr2", "PurchaseSkills", &[PyRep::List(vec![PyRep::Integer(i64::from(sid))])]).await;
        let q: Vec<PyRep> = ((curr + 1)..=5)
            .map(|lvl| PyRep::Tuple(vec![PyRep::Integer(i64::from(sid)), PyRep::Integer(i64::from(lvl))]))
            .collect();
        rpc(ctx, sess, "skillMgr2", "SaveNewQueue", &[PyRep::List(q)])
            .await
            .unwrap_or_else(|e| panic!("SaveNewQueue skill {sid} failed: {e:?}"));
        ctx.advance_virtual_clock(90 * 86_400);
        let _ = rpc(ctx, sess, "skillMgr2", "GetSkillQueueAndFreePoints", &[]).await;
    }
}

async fn buy_jita(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, qty: i64, max_price: f64) {
    rpc(
        ctx,
        sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(i64::from(type_id)),
            PyRep::Real(max_price),
            PyRep::Integer(qty),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(0),
            PyRep::Bool(false),
        ],
    )
    .await
    .unwrap_or_else(|e| panic!("PlaceBuyOrder type {type_id} x{qty} failed: {e:?}"));
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

fn find_hangar_items(ctx: &ServerContext, owner_id: i64, loc_id: i64, type_id: u32) -> Vec<(i64, i32, bool, bool)> {
    let mut out: Vec<_> = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.owner_id == owner_id && i.location_id == loc_id && i.type_id == type_id)
            .map(|i| (i.item_id, i.quantity, i.singleton, i.is_blueprint_copy))
            .collect()
    });
    out.sort_by_key(|x| x.0);
    out
}

#[tokio::test]
async fn test_esi_live_market_prices_eiv_insurance_and_structure_orders() {
    // parity: esi:GET /markets/prices, feat:esi.market.prices
    // parity: esi:GET /markets/structures/{structure_id}, feat:esi.market.structure-orders
    eve_testkit::attest(&[
        "esi:GET /markets/prices",
        "feat:esi.market.prices",
        "esi:GET /markets/structures/{structure_id}",
        "feat:esi.market.structure-orders",
    ]);

    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots_via_gm_transfer(&ctx, &[(PILOT_1, 80_000_000_000.0), (PILOT_2, 5_000_000_000.0)]).await;

    let router = EsiRouter::new(Arc::clone(&ctx));
    let spec = OpenApiSpec::pinned();

    let mut s1 = login_and_select(&ctx, 7911, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = login_and_select(&ctx, 7912, "other", "other-secret", PILOT_2).await;

    train_skills_to_five(
        &ctx,
        &mut s1,
        &[3363, 3380, 3387, 3402, 3406, 3409, 3443, 3444, 20185, 35833, 35892],
    )
    .await;

    // 1. Baseline public GET /v1/markets/prices/, Rifter EIV, and Rifter ship insurance
    let (prices_t0, prices_etag0, _) = check_esi_200(&router, &spec, "/v1/markets/prices/", None);
    let cond_p0 = EsiHttpRequest::new("GET", "/v1/markets/prices/").with_header("If-None-Match", &prices_etag0);
    assert_eq!(router.handle_request(&cond_p0).status_code, 304);

    let trit_adj_0 = prices_t0
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["type_id"] == json!(34))
        .and_then(|r| r["adjusted_price"].as_f64())
        .expect("baseline Tritanium adjusted_price");

    let rifter_bp_tid = ctx.db.with_snapshot(|s| s.items[&100_000_007].type_id);
    buy_jita(&ctx, &mut s1, rifter_bp_tid, 1, 50_000_000.0).await;
    buy_jita(&ctx, &mut s1, rifter_bp_tid, 1, 50_000_000.0).await;
    let rifter_bpos: Vec<i64> = find_hangar_items(&ctx, PILOT_1, JITA_STATION, rifter_bp_tid).into_iter().map(|x| x.0).collect();
    let (bpo_base, bpo_me_test) = (rifter_bpos[0], rifter_bpos[1]);

    let j_base = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_base)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("baseline Rifter InstallJob");
    let eiv_0 = j_base.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();

    let (ins_esi_0, _, _) = check_esi_200(&router, &spec, "/v1/insurance/prices/", None);
    let rifter_plat_0 = ins_esi_0
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["type_id"] == json!(587))
        .and_then(|r| r["levels"].as_array())
        .and_then(|lvls| lvls.last())
        .and_then(|l| l["payout"].as_f64())
        .expect("baseline Rifter ESI Platinum payout");
    let ins_rpc_0 = rpc(&ctx, &mut s1, "insuranceSvc", "GetInsurancePrice", &[PyRep::Integer(100_000_001)])
        .await
        .unwrap()
        .as_f64()
        .unwrap();

    for _ in 0..2 {
        let rj = rpc(
            &ctx,
            &mut s1,
            "industryManager",
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(bpo_me_test)),
                ("facilityID", PyRep::Integer(JITA_STATION)),
                ("activityID", PyRep::Integer(4)),
                ("runs", PyRep::Integer(1)),
            ])],
        )
        .await
        .expect("research ME");
        let rjid = rj.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
        let rdur = rj.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap() as u64;
        ctx.advance_virtual_clock(rdur + 2);
        rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(rjid)]).await.unwrap();
    }

    // 2. Execute multi-day live Tritanium (34) trades across 6 UTC days + place extreme unfilled orders
    let avail_trit: i64 = ctx.db.with_snapshot(|s| {
        s.market_orders
            .values()
            .filter(|o| o.station_id == JITA_STATION && !o.is_bid && o.type_id == 34)
            .map(|o| i64::from(o.vol_remaining))
            .sum()
    });
    if avail_trit > 0 {
        buy_jita(&ctx, &mut s1, 34, avail_trit, 100.0).await;
    }
    ctx.advance_virtual_clock(86_400);

    for day in 0..6 {
        let exec_price = 12.0 + f64::from(day);
        let trit_stack = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 34)[0].0;
        let sell_items = PyRep::List(vec![PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(trit_stack)),
            (PyRep::String("typeID".into()), PyRep::Integer(34)),
            (PyRep::String("price".into()), PyRep::Real(exec_price)),
            (PyRep::String("quantity".into()), PyRep::Integer(10_000)),
            (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION)),
            (PyRep::String("duration".into()), PyRep::Integer(30)),
        ])]);
        rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[sell_items, PyRep::Bool(false), PyRep::Integer(30)])
            .await
            .expect("PlaceMultiSellOrder Tritanium");
        buy_jita(&ctx, &mut s2, 34, 10_000, exec_price).await;
        ctx.advance_virtual_clock(86_400);
    }

    rpc(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(34),
            PyRep::Real(0.01),
            PyRep::Integer(50_000),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(30),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("Unfilled lowball buy order");
    let trit_rem = find_hangar_items(&ctx, PILOT_1, JITA_STATION, 34)[0].0;
    let extreme_ask = PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(trit_rem)),
        (PyRep::String("typeID".into()), PyRep::Integer(34)),
        (PyRep::String("price".into()), PyRep::Real(9_999_000.0)),
        (PyRep::String("quantity".into()), PyRep::Integer(1_000)),
        (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION)),
        (PyRep::String("duration".into()), PyRep::Integer(30)),
    ])]);
    rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[extreme_ask, PyRep::Bool(false), PyRep::Integer(30)])
        .await
        .expect("Unfilled extreme sell order");

    let (prices_t1, _, _) = check_esi_200(&router, &spec, "/v1/markets/prices/", None);
    let trit_row_1 = prices_t1.as_array().unwrap().iter().find(|r| r["type_id"] == json!(34)).unwrap();
    let trit_avg_1 = trit_row_1["average_price"].as_f64().unwrap();
    let trit_adj_1 = trit_row_1["adjusted_price"].as_f64().unwrap();
    assert!(trit_avg_1 > 5.0 && trit_avg_1 < 20.0, "average_price ({trit_avg_1}) must ignore unfilled orders");
    assert!(trit_adj_1 > trit_adj_0 && trit_adj_1 < 20.0, "adjusted_price ({trit_adj_1} vs {trit_adj_0}) must follow clamped 28d EMA");

    buy_jita(&ctx, &mut s1, rifter_bp_tid, 1, 50_000_000.0).await;
    let bpo_me0_after = find_hangar_items(&ctx, PILOT_1, JITA_STATION, rifter_bp_tid).last().unwrap().0;
    for &(tid, q) in ctx.sde.blueprints[&rifter_bp_tid].materials.iter().map(|m| (m.type_id, m.quantity)).collect::<Vec<_>>().iter() {
        let have: i64 = find_hangar_items(&ctx, PILOT_1, JITA_STATION, tid).iter().map(|x| i64::from(x.1)).sum();
        let need = (q as i64 * 3).saturating_sub(have);
        if tid != 34 && need > 0 {
            buy_jita(&ctx, &mut s1, tid, need, 500_000.0).await;
        }
    }
    let j_me0 = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_me0_after)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("post-trade ME0 InstallJob");
    let j_me2 = rpc(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintID", PyRep::Integer(bpo_me_test)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("post-trade ME2 InstallJob");
    let eiv_me0 = j_me0.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    let eiv_me2 = j_me2.dict_get("estimatedItemValue").and_then(|v| v.as_f64()).unwrap();
    assert!(eiv_me0 > eiv_0, "EIV ({eiv_me0}) must rise above baseline ({eiv_0})");
    assert!((eiv_me0 - eiv_me2).abs() < 1e-4, "EIV must be strictly invariant to blueprint ME");

    let (ins_esi_1, _, _) = check_esi_200(&router, &spec, "/v1/insurance/prices/", None);
    let rifter_plat_1 = ins_esi_1
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["type_id"] == json!(587))
        .and_then(|r| r["levels"].as_array())
        .and_then(|lvls| lvls.last())
        .and_then(|l| l["payout"].as_f64())
        .unwrap();
    let ins_rpc_1 = rpc(&ctx, &mut s1, "insuranceSvc", "GetInsurancePrice", &[PyRep::Integer(100_000_001)])
        .await
        .unwrap()
        .as_f64()
        .unwrap();
    assert!(rifter_plat_1 > rifter_plat_0);
    assert!(ins_rpc_1 > ins_rpc_0);

    // 3. Live Upwell Fortizar (35833) deployment in Perimeter + Standup Market Hub I (35892) + GET /v1/markets/structures/{id}/
    rpc(&ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String("Perimeter Market Corp".into()), PyRep::String("PMKT".into()), PyRep::Real(0.02)])
        .await
        .expect("AddCorporation PMKT");
    ctx.advance_virtual_clock(8 * 86_400);

    let fortizar_tid = 35833_u32;
    let core_tid = ctx.sde.types[&fortizar_tid].default_attributes[&life::ATTR_REQUIRES_DEED_TYPE] as u32;
    let market_hub_tid = 35892_u32;
    let fuel_tid = 4051_u32;
    let freighter_tid = 20185_u32;

    buy_jita(&ctx, &mut s1, freighter_tid, 1, 5_000_000_000.0).await;
    buy_jita(&ctx, &mut s1, fortizar_tid, 1, 30_000_000_000.0).await;
    buy_jita(&ctx, &mut s1, core_tid, 1, 15_000_000_000.0).await;
    buy_jita(&ctx, &mut s1, market_hub_tid, 1, 2_000_000_000.0).await;
    buy_jita(&ctx, &mut s1, fuel_tid, 850, 100_000.0).await;
    // Market Hub onlining needs 2,880 blocks; top up with the other racial fuel blocks (1,000 per Jita seller).
    let extra_fuel = [4246_u32, 4247, 4312];
    for tid in extra_fuel {
        buy_jita(&ctx, &mut s1, tid, 1_000, 100_000.0).await;
    }

    let freighter_id = find_hangar_items(&ctx, PILOT_1, JITA_STATION, freighter_tid)[0].0;
    rpc(&ctx, &mut s1, "ship", "AssembleShip", &[PyRep::Integer(freighter_id)]).await.expect("AssembleShip Charon");
    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(freighter_id)]).await.expect("Board Charon");

    for (tid, qty) in [(fortizar_tid, 1), (core_tid, 1), (market_hub_tid, 1), (fuel_tid, 850), (4246, 1_000), (4247, 1_000), (4312, 1_000)] {
        let iid = find_hangar_items(&ctx, PILOT_1, JITA_STATION, tid)[0].0;
        rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(iid), PyRep::Integer(freighter_id), PyRep::Integer(5), PyRep::Integer(qty)])
            .await
            .expect("Load cargo into Charon");
    }

    ctx.advance_secs(12);
    rpc(&ctx, &mut s1, "stationSvc", "Undock", &[]).await.expect("Undock Jita");
    let jita_to_perim_gate = *ctx
        .sde
        .jumps
        .iter()
        .find(|(_, j)| j.from_solar_system_id == JITA_SYS && j.to_solar_system_id == PERIMETER_SYS)
        .expect("Jita -> Perimeter stargate")
        .0;
    rpc(&ctx, &mut s1, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(jita_to_perim_gate), PyRep::Real(0.0)])
        .await
        .expect("Warp to Perimeter gate");
    let jita_sim = ctx.systems.get_or_spawn(JITA_SYS);
    for _ in 0..300 {
        let _ = jita_sim.step_manual(1.0).await;
    }
    ctx.advance_secs(15);
    rpc(&ctx, &mut s1, "beyonce", "CmdStargateJump", &[PyRep::Integer(jita_to_perim_gate)])
        .await
        .expect("Jump into Perimeter");

    let perim_sun = ctx.sde.celestials_in_system(PERIMETER_SYS).into_iter().find(|c| c.group_id == 6).unwrap().celestial_id;
    ctx.advance_secs(15);
    rpc(&ctx, &mut s1, "beyonce", "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(perim_sun), PyRep::Real(100_000.0)])
        .await
        .expect("Warp to Perimeter sun");
    let perim_sim = ctx.systems.get_or_spawn(PERIMETER_SYS);
    for _ in 0..360 {
        let _ = perim_sim.step_manual(1.0).await;
    }

    let anchor_rep = rpc(
        &ctx,
        &mut s1,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(i64::from(fortizar_tid)), PyRep::String("Perimeter Trade Citadel".into())],
    )
    .await
    .expect("Anchor Fortizar in Perimeter");
    let structure_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).expect("structureID");

    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS + 10);
    let _ = rpc(&ctx, &mut s1, "structureGuests", "GetGuests", &[PyRep::Integer(structure_id)]).await;
    let perim_bound = crate::npc_spawns_pve::bind_beyonce(&ctx, &mut s1, PERIMETER_SYS).await;
    ctx.dispatch_rpc(&mut s1, "beyonce", Some(perim_bound), "CmdFollowBall", &[PyRep::Integer(structure_id), PyRep::Real(0.0)], &HashMap::new(), None)
        .await
        .expect("Approach anchored Fortizar");
    for _ in 0..240 {
        let _ = perim_sim.step_manual(1.0).await;
    }
    rpc(&ctx, &mut s1, "structureDocking", "Dock", &[PyRep::Integer(structure_id)]).await.expect("Dock in Fortizar");

    let core_item = find_hangar_items(&ctx, PILOT_1, freighter_id, core_tid)[0].0;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(core_item), PyRep::Integer(structure_id), PyRep::Integer(172)])
        .await
        .expect("Install Quantum Core");
    let fuel_item = find_hangar_items(&ctx, PILOT_1, freighter_id, fuel_tid)[0].0;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(fuel_item), PyRep::Integer(structure_id), PyRep::Integer(133), PyRep::Integer(800)])
        .await
        .expect("Load Fuel Bay");
    for tid in extra_fuel {
        let extra_item = find_hangar_items(&ctx, PILOT_1, freighter_id, tid)[0].0;
        rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(extra_item), PyRep::Integer(structure_id), PyRep::Integer(133), PyRep::Integer(1_000)])
            .await
            .expect("Load racial fuel blocks into Fuel Bay");
    }
    let sell_fuel = find_hangar_items(&ctx, PILOT_1, freighter_id, fuel_tid)[0].0;
    rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(sell_fuel), PyRep::Integer(structure_id), PyRep::Integer(4), PyRep::Integer(50)])
        .await
        .expect("Unload 50 fuel blocks into structure hangar");

    rpc(&ctx, &mut s1, "structureDocking", "Undock", &[]).await.expect("Undock for onlining timer");
    ctx.advance_virtual_clock(life::ONLINING_SECS + 15);
    rpc(&ctx, &mut s1, "structureDocking", "Dock", &[PyRep::Integer(structure_id)]).await.expect("Re-dock in online Fortizar");

    let p1_mkt_tok = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &["esi-markets.structure_markets.v1"]);
    let p1_no_mkt = issue_sso_token(&ctx, "demo", "demo-secret", PILOT_1, &[]);
    let p2_mkt_tok = issue_sso_token(&ctx, "other", "other-secret", PILOT_2, &["esi-markets.structure_markets.v1"]);
    let st_mkt_url = format!("/v1/markets/structures/{structure_id}/");

    assert_esi_status(&router, &st_mkt_url, Some(&p1_mkt_tok), 403); // No Standup Market Hub I yet

    let mhub_item = find_hangar_items(&ctx, PILOT_1, freighter_id, market_hub_tid)[0].0;
    let fit_rep = rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(mhub_item), PyRep::Integer(structure_id), PyRep::Integer(164)])
        .await
        .expect("Fit & online Standup Market Hub I");
    let fitted_mhub = fit_rep.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();

    let prof_id = rpc(&ctx, &mut s1, "structureProfiles", "CreateProfile", &[PyRep::String("Corp Only Market".into()), PyRep::String("".into())])
        .await
        .unwrap()
        .as_i64()
        .unwrap();
    rpc(
        &ctx,
        &mut s1,
        "structureProfiles",
        "SaveProfileSettings",
        &[
            PyRep::Integer(prof_id),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Bool(false), PyRep::Integer(0)])]),
        ],
    )
    .await
    .expect("SaveProfileSettings corp_only");
    rpc(&ctx, &mut s1, "structureProfiles", "ChangeProfiles", &[PyRep::List(vec![PyRep::Integer(structure_id)]), PyRep::Integer(prof_id)])
        .await
        .expect("ChangeProfiles");

    let buy_rep = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(structure_id),
            PyRep::Integer(587),
            PyRep::Real(650_000.0),
            PyRep::Integer(4),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(14),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder at Fortizar");
    let st_buy_oid = buy_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    let st_hangar_fuel = find_hangar_items(&ctx, PILOT_1, structure_id, fuel_tid)[0].0;
    let st_sell_items = PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(st_hangar_fuel)),
        (PyRep::String("typeID".into()), PyRep::Integer(i64::from(fuel_tid))),
        (PyRep::String("price".into()), PyRep::Real(22_500.0)),
        (PyRep::String("quantity".into()), PyRep::Integer(50)),
        (PyRep::String("stationID".into()), PyRep::Integer(structure_id)),
        (PyRep::String("duration".into()), PyRep::Integer(30)),
    ])]);
    rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[st_sell_items, PyRep::Bool(false), PyRep::Integer(30)])
        .await
        .expect("PlaceMultiSellOrder at Fortizar");

    assert_esi_status(&router, &st_mkt_url, None, 401);
    assert_esi_status(&router, &st_mkt_url, Some(&p1_no_mkt), 403);
    assert_esi_status(&router, &st_mkt_url, Some(&p2_mkt_tok), 403);
    assert_esi_status(&router, "/v1/markets/structures/1999999999999/", Some(&p1_mkt_tok), 404);

    let (st_orders, st_etag, st_pages) = check_esi_200(&router, &spec, &st_mkt_url, Some(&p1_mkt_tok));
    assert_eq!(st_pages.as_deref(), Some("1"));
    let cond_st = EsiHttpRequest::new("GET", &st_mkt_url).with_bearer_token(&p1_mkt_tok).with_header("If-None-Match", &st_etag);
    assert_eq!(router.handle_request(&cond_st).status_code, 304);

    let st_arr = st_orders.as_array().unwrap();
    assert_eq!(st_arr.len(), 2, "Expected both buy and sell structure orders");
    let buy_row = st_arr.iter().find(|o| o["order_id"] == json!(st_buy_oid)).expect("buy order row");
    assert_eq!(buy_row["location_id"], json!(structure_id));
    assert_eq!(buy_row["type_id"], json!(587));
    assert_eq!(buy_row["price"], json!(650_000.0));
    assert_eq!(buy_row["is_buy_order"], json!(true));
    assert_eq!(buy_row["range"], json!("station"));
    let sell_row = st_arr.iter().find(|o| o["is_buy_order"] == json!(false)).expect("sell order row");
    assert_eq!(sell_row["type_id"], json!(fuel_tid));
    assert_eq!(sell_row["price"], json!(22_500.0));
    assert_eq!(sell_row["volume_remain"], json!(50));

    // Offlining Standup Market Hub I (35892) disables the structure market on ESI (403)
    rpc(&ctx, &mut s1, "dogmaIM", "TakeModuleOffline", &[PyRep::Integer(structure_id), PyRep::Integer(fitted_mhub)])
        .await
        .expect("Offline Standup Market Hub I");
    assert_esi_status(&router, &st_mkt_url, Some(&p1_mkt_tok), 403);
}
