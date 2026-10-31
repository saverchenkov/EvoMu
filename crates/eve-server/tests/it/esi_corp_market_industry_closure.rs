//! End-to-end integration tests for ESI corporation role-gated routes, market history/prices/structures,
//! and industry facilities/jobs (`esi:*` open-row closure).

#![forbid(unsafe_code)]

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

            let mut corp = CorporationRecord {
                corporation_id: corp_id,
                name: "Role Gate Test Corp".into(),
                ticker: "RGTC".into(),
                ceo_id,
                tax_rate: 0.05,
                members: HashSet::from([
                    ceo_id,
                    station_mgr_id,
                    accountant_id,
                    trader_id,
                    plain_member_id,
                ]),
                member_roles,
                hq_station_id: Some(jita_station),
                master_wallet: IskCents::from_isk(250_000_000),
                ..Default::default()
            };
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
                make_item(960_001, 35_894, corp_id, observer_id, InventoryFlag::ServiceSlot0, 1, true),
            );
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
    // parity: esi:GET /characters/{character_id}/industry/jobs, esi:GET /corporations/{corporation_id}/industry/jobs, esi:GET /industry/facilities, esi:GET /markets/prices, esi:GET /markets/structures/{structure_id}, esi:GET /markets/{region_id}/history
    // parity: feat:esi.industry.facilities, feat:esi.industry.char-jobs, feat:esi.industry.corp-jobs, feat:esi.market.history, feat:esi.market.prices
    eve_testkit::attest(&[
        "esi:GET /characters/{character_id}/industry/jobs",
        "esi:GET /corporations/{corporation_id}/industry/jobs",
        "esi:GET /industry/facilities",
        "esi:GET /markets/prices",
        "esi:GET /markets/structures/{structure_id}",
        "esi:GET /markets/{region_id}/history",
        "feat:esi.industry.facilities",
        "feat:esi.industry.char-jobs",
        "feat:esi.industry.corp-jobs",
        "feat:esi.market.history",
        "feat:esi.market.prices",
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
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "Industry Market Corp".into(),
                    ticker: "IMC".into(),
                    ceo_id: seller_id,
                    tax_rate: 0.05,
                    members: HashSet::from([seller_id]),
                    member_roles,
                    hq_station_id: Some(jita_station),
                    master_wallet: IskCents::from_isk(100_000_000),
                    ..Default::default()
                },
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

    // 5. Install and deliver an industry job via industryManager and verify character & corp ESI job fields
    let install_dict = PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(bpc_item_id)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(1)),
        (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
        (PyRep::String("useCorp".into()), PyRep::Bool(true)),
    ]);
    let install_rep =
        rpc_call(&ctx, &mut seller_sess, "industryManager", "InstallJob", &[install_dict]).await;
    let job_id = install_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID");

    ctx.db.clock().advance_secs(10_000);
    rpc_call(&ctx, &mut seller_sess, "industryManager", "CompleteJob", &[PyRep::Integer(job_id)])
        .await;

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
    let cj = char_jobs
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["job_id"] == json!(job_id))
        .expect("delivered job in character industry/jobs");
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
    let crj = corp_jobs
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["job_id"] == json!(job_id))
        .expect("delivered job in corp industry/jobs");
    assert_eq!(crj["location_id"], json!(jita_station));
    assert_eq!(crj["completed_character_id"], json!(seller_id));
}
