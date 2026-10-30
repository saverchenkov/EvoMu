//! End-to-end integration tests for ESI corporation role-gated routes, market history/prices/structures,
//! and industry facilities/jobs (`esi:*` open-row closure).

#![forbid(unsafe_code)]

use eve_db::corp_roles::{
    ROLE_ACCOUNTANT, ROLE_DIRECTOR, ROLE_FACTORY_MANAGER, ROLE_STATION_MANAGER, ROLE_TRADER,
};
use eve_db::{
    CorporationRecord, CustomsOfficeRecord, InventoryFlag, InventoryItemRecord, IskCents,
    MarketOrderMeta, MarketOrderRecord, MoonExtractionRecord, ObserverMiningRecord,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::sample_routes::is_operation_implemented;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

fn issue_token(ctx: &ServerContext, character_id: i64, scopes: Vec<&str>) -> String {
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
            "evomu-esi-closure",
            "https://localhost/callback",
            scope_vec,
            PKCE_CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    let bundle = ctx
        .db
        .exchange_pkce_code(&code, "evomu-esi-closure", PKCE_VERIFIER, now)
        .expect("exchange_pkce_code");
    bundle.access_token
}

fn call_esi(router: &EsiRouter, path: &str, token: Option<&str>) -> (u16, Value) {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(tok) = token {
        req = req.with_bearer_token(tok);
    }
    let resp = router.handle_request(&req);
    let body: Value = serde_json::from_slice(&resp.body).unwrap_or(Value::Null);
    (resp.status_code, body)
}

async fn rpc_call(
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

// parity: esi:GET /corporation/{corporation_id}/mining/extractions, esi:GET /corporation/{corporation_id}/mining/observers, esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}, esi:GET /corporations/{corporation_id}/customs_offices, esi:GET /corporations/{corporation_id}/orders, esi:GET /corporations/{corporation_id}/orders/history
#[tokio::test]
async fn test_esi_corp_role_gated_mining_customs_and_orders() {
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
    let now = ctx.now_unix();

    let corp_id = 98_500_001_i64;
    let ceo_id = 90_000_001_i64;
    let station_mgr_id = 90_000_002_i64;
    let accountant_id = 90_000_003_i64;
    let trader_id = 90_000_004_i64;
    let plain_member_id = 90_000_005_i64;
    let observer_id = 1_030_000_000_001_i64;

    ctx.db
        .try_with_mut_snapshot(|snap| {
            let mut member_roles = HashMap::new();
            member_roles.insert(ceo_id, ROLE_DIRECTOR);
            member_roles.insert(station_mgr_id, ROLE_STATION_MANAGER);
            member_roles.insert(accountant_id, ROLE_ACCOUNTANT);
            member_roles.insert(trader_id, ROLE_TRADER);
            member_roles.insert(plain_member_id, 0);

            let template_char = snap.characters[&ceo_id].clone();
            for cid in [ceo_id, station_mgr_id, accountant_id, trader_id, plain_member_id] {
                let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| {
                    let mut c = template_char.clone();
                    c.character_id = cid;
                    c.name = format!("Pilot {cid}");
                    c
                });
                ch.corporation_id = corp_id;
                snap.characters.insert(cid, ch);
            }

            snap.corporations.insert(
                corp_id,
                CorporationRecord {
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
                    hq_station_id: Some(60_003_760),
                    ..Default::default()
                },
            );

            snap.structures.insert(
                observer_id,
                UpwellStructureRecord {
                    structure_id: observer_id,
                    type_id: 35835,
                    owner_corp_id: corp_id,
                    solar_system_id: 30_000_142,
                    name: "Corp Moon Refinery".into(),
                    position: [1000.0, 2000.0, 3000.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    shield_hp: 10000.0,
                    armor_hp: 10000.0,
                    hull_hp: 10000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            snap.moon_extractions.insert(
                observer_id,
                MoonExtractionRecord {
                    structure_id: observer_id,
                    moon_id: 40_009_081,
                    extraction_start_unix: now.saturating_sub(3600),
                    chunk_arrival_unix: now + 3600,
                    natural_decay_unix: now + 7200,
                    status: "Started".to_string(),
                    auto_fracture: false,
                    event_id: 1,
                    moon_ores: vec![(45490, 12_000.0)],
                    spawned_asteroid_ids: vec![],
                },
            );
            snap.observer_mining_ledger.insert(
                observer_id,
                vec![ObserverMiningRecord {
                    observer_id,
                    character_id: plain_member_id,
                    recorded_corporation_id: corp_id,
                    type_id: 45490,
                    quantity: 1200,
                    last_updated_date: "2025-12-16".to_string(),
                    timestamp_unix: now.saturating_sub(300),
                }],
            );
            snap.corp_customs_offices.insert(
                corp_id,
                vec![CustomsOfficeRecord {
                    office_id: 1_040_000_000_001,
                    system_id: 30_000_142,
                    reinforce_exit_start: 18,
                    reinforce_exit_end: 20,
                    allow_alliance_access: true,
                    allow_access_with_standings: false,
                    standing_level: None,
                    corporation_tax_rate: Some(0.01),
                    alliance_tax_rate: Some(0.02),
                    excellent_standing_tax_rate: None,
                    good_standing_tax_rate: None,
                    neutral_standing_tax_rate: Some(0.10),
                    bad_standing_tax_rate: None,
                    terrible_standing_tax_rate: None,
                }],
            );
            snap.market_orders.insert(
                8801,
                MarketOrderRecord {
                    order_id: 8801,
                    type_id: 34,
                    owner_id: corp_id,
                    station_id: 60_003_760,
                    region_id: 10_000_002,
                    price: IskCents::from_isk(5),
                    vol_entered: 1000,
                    vol_remaining: 1000,
                    min_volume: 1,
                    is_bid: false,
                    escrow: IskCents::ZERO,
                    is_npc: false,
                },
            );
            snap.market_order_meta.insert(
                8801,
                MarketOrderMeta {
                    issued_unix: now.saturating_sub(600),
                    duration_days: 90,
                    range: "station".to_string(),
                    issued_by: Some(ceo_id),
                    is_corporation: true,
                    wallet_division: Some(1000),
                    state: Some("active".into()),
                },
            );
            Ok::<(), ()>(())
        })
        .unwrap();

    let all_scopes = vec![
        "esi-industry.read_corporation_mining.v1",
        "esi-planets.read_customs_offices.v1",
        "esi-markets.read_corporation_orders.v1",
    ];
    let sm_tok = issue_token(&ctx, station_mgr_id, all_scopes.clone());
    let acc_tok = issue_token(&ctx, accountant_id, all_scopes.clone());
    let trd_tok = issue_token(&ctx, trader_id, all_scopes.clone());
    let dir_tok = issue_token(&ctx, ceo_id, all_scopes.clone());
    let mem_tok = issue_token(&ctx, plain_member_id, all_scopes);

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
    assert_eq!(obs_detail[0]["quantity"], json!(1200));
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
        assert_eq!(orders[0]["order_id"], json!(8801));
        let (st_h, _) = call_esi(
            &router,
            &format!("/v1/corporations/{corp_id}/orders/history/"),
            Some(tok),
        );
        assert_eq!(st_h, 200);
    }
    assert_eq!(
        call_esi(
            &router,
            &format!("/v1/corporations/{corp_id}/orders/"),
            Some(&sm_tok),
        )
        .0,
        403
    );

    // 4. Director allowed on customs_offices; plain member rejected with 403
    let (st, pocos) = call_esi(
        &router,
        &format!("/v1/corporations/{corp_id}/customs_offices/"),
        Some(&dir_tok),
    );
    assert_eq!(st, 200);
    assert_eq!(pocos[0]["system_id"], json!(30_000_142));
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

// parity: esi:GET /characters/{character_id}/industry/jobs, esi:GET /corporations/{corporation_id}/industry/jobs, esi:GET /industry/facilities, esi:GET /markets/prices, esi:GET /markets/structures/{structure_id}, esi:GET /markets/{region_id}/history
#[tokio::test]
async fn test_esi_market_history_prices_structures_and_industry_jobs() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let now = ctx.now_unix();

    let seller_id = 90_000_001_i64;
    let buyer_id = 90_000_002_i64;
    let corp_id = 98_500_002_i64;
    let jita_station = 60_003_760_i64;
    let jita_system = 30_000_142_u32;
    let forge_region = 10_000_002_u32;
    let structure_id = 1_050_000_000_001_i64;

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
    let mut trit_item_id = 0_i64;
    let mut bpc_item_id = 0_i64;
    ctx.db
        .try_with_mut_snapshot(|snap| {
            let mut member_roles = HashMap::new();
            member_roles.insert(seller_id, ROLE_DIRECTOR | ROLE_FACTORY_MANAGER);
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
                UpwellStructureRecord {
                    structure_id,
                    type_id: 35833,
                    owner_corp_id: corp_id,
                    solar_system_id: jita_system,
                    name: "Jita Fortizar".into(),
                    position: [1000.0, 2000.0, 3000.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    shield_hp: 10000.0,
                    armor_hp: 10000.0,
                    hull_hp: 10000.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            trit_item_id = 99_910_001;
            snap.items.insert(
                trit_item_id,
                InventoryItemRecord {
                    item_id: trit_item_id,
                    type_id: 39,
                    owner_id: seller_id,
                    location_id: jita_station,
                    flag: InventoryFlag::Hangar,
                    quantity: 50_000,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            bpc_item_id = 99_910_002;
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
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: true,
                    blueprint_runs_remaining: Some(5),
                },
            );
            for (idx, (tid, qty)) in [(34_u32, 30_000_i32), (35, 10_000), (36, 3_000), (37, 1_000), (38, 5_000), (39, 5_000), (40, 5_000), (57_478, 200), (57_479, 10), (57_486, 100)]
                .into_iter()
                .enumerate()
            {
                let iid = 99_910_010 + idx as i64;
                snap.items.insert(
                    iid,
                    InventoryItemRecord {
                        item_id: iid,
                        type_id: tid,
                        owner_id: seller_id,
                        location_id: jita_station,
                        flag: InventoryFlag::Hangar,
                        quantity: qty,
                        singleton: false,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
            }
            Ok::<(), ()>(())
        })
        .unwrap();

    // 3. Bare Astrahus without Standup Market Hub I must return 403 on GET /markets/structures/{id}
    let mkt_tok = issue_token(&ctx, seller_id, vec!["esi-markets.structure_markets.v1"]);
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
                InventoryItemRecord {
                    item_id: 99_910_099,
                    type_id: 35892,
                    owner_id: corp_id,
                    location_id: structure_id,
                    flag: InventoryFlag::ServiceSlot0,
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
    ctx.db
        .try_with_mut_snapshot(|snap| {
            snap.market_history.remove(&(forge_region, 35));
            Ok::<(), ()>(())
        })
        .unwrap();
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
    rpc_call(&ctx, &mut seller_sess, "marketProxy", "PlaceMultiSellOrder", &[sell_order_arg, PyRep::Bool(false)]).await;
    rpc_call(
        &ctx,
        &mut buyer_sess,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(jita_station),
            PyRep::Integer(39),
            PyRep::Real(7.5),
            PyRep::Integer(4_000),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await;

    let (st, hist) = call_esi(&router, &format!("/v1/markets/{forge_region}/history/?type_id=39"), None);
    assert_eq!(st, 200);
    let hist_arr = hist.as_array().unwrap();
    assert!(!hist_arr.is_empty());
    assert_eq!(hist_arr[0]["volume"], json!(4_500));
    let hist_avg = hist_arr[0]["average"].as_f64().unwrap();
    assert!(hist_avg > 7.5);

    let (st, prices) = call_esi(&router, "/v1/markets/prices/", None);
    assert_eq!(st, 200);
    let trit_p = prices.as_array().unwrap().iter().find(|p| p["type_id"] == json!(39)).expect("Zydrine in /markets/prices");
    assert_eq!(trit_p["average_price"], json!(hist_avg));
    assert_eq!(trit_p["adjusted_price"], json!(hist_avg));

    // 5. Install and deliver an industry job via industryJobMgr and verify character & corp ESI job fields
    let install_rep = rpc_call(
        &ctx,
        &mut seller_sess,
        "industryManager",
        "InstallJob",
        &[PyRep::Dict(vec![
            (PyRep::String("blueprintID".into()), PyRep::Integer(bpc_item_id)),
            (PyRep::String("activityID".into()), PyRep::Integer(1)),
            (PyRep::String("runs".into()), PyRep::Integer(1)),
            (PyRep::String("facilityID".into()), PyRep::Integer(jita_station)),
            (PyRep::String("useCorp".into()), PyRep::Bool(true)),
        ])],
    )
    .await;
    let job_id = match &install_rep {
        PyRep::KeyVal(kv) => kv
            .get("jobID")
            .and_then(|v| match v {
                PyRep::Integer(id) => Some(*id),
                _ => None,
            })
            .expect("jobID in InstallJob response"),
        _ => panic!("unexpected InstallJob response: {install_rep:?}"),
    };

    ctx.db
        .try_with_mut_snapshot(|snap| {
            if let Some(j) = snap.industry_jobs.get_mut(&job_id) {
                j.start_unix = now.saturating_sub(j.duration_seconds + 60);
            }
            Ok::<(), ()>(())
        })
        .unwrap();

    rpc_call(
        &ctx,
        &mut seller_sess,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(job_id)],
    )
    .await;

    let ind_tok = issue_token(
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
