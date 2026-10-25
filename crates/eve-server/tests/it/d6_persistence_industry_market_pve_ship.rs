//! End-to-end SQLite restart durability tests for `plan:D-6` Batch 4:
//! - `feat:lifecycle.industry-pi-durable`
//! - `feat:lifecycle.market-wallet-history-durable`
//! - `feat:lifecycle.pve-progress-durable`
//! - `feat:lifecycle.abyssal-restart`
//! - `feat:lifecycle.ship-and-item-config-durable`
//! - `feat:lifecycle.account-progress-durable`

use eve_db::{corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, UpwellStructureRecord, VirtualServerClock, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX};
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::quasar_gateway::{STATUS_FORBIDDEN, STATUS_NOT_IMPLEMENTED, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::implant_plugging::TYPE_DAILY_ALPHA_INJECTOR;
use glam::DVec3;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const CORP_ID: i64 = 1_000_009;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;
const LOWSEC_SYS_ID: u32 = 30_000_012; // Asabona (Low-Sec 0.32 with moons)
const PLANET_ID: i64 = 40_009_082; // Jita IV (Gas)
const TRITANIUM: u32 = 34;
const MEXALLON: u32 = 36;
const RIFTER_BPO: u32 = 691;
const ALSC_CONTAINER_TYPE_ID: u32 = 17_366;

fn open_ctx_at(path: &Path, now_unix: u64) -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::open_sqlite(path).expect("open_sqlite"));
    let ctx = Arc::new(ServerContext::with_repository(db));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

fn issue_esi_token(ctx: &ServerContext, char_id: i64) -> String {
    let now = ctx.now_unix();
    let (user, pass) = if char_id == P2 { ("other", "other-secret") } else { ("demo", "demo-secret") };
    let scopes = vec![
        "esi-planets.manage_planets.v1".into(), "esi-planets.read_customs_offices.v1".into(),
        "esi-industry.read_character_mining.v1".into(), "esi-industry.read_corporation_mining.v1".into(),
        "esi-industry.read_character_jobs.v1".into(), "esi-characters.read_agents_research.v1".into(),
        "esi-markets.read_character_orders.v1".into(), "esi-wallet.read_character_wallet.v1".into(),
        "esi-wallet.read_corporation_wallets.v1".into(), "esi-skills.read_skillqueue.v1".into(),
        "esi-corporations.read_structures.v1".into(), "esi-corporations.read_starbases.v1".into(),
        "esi-corporations.read_container_logs.v1".into(), "esi-location.read_location.v1".into(),
        "esi-location.read_ship_type.v1".into(), "esi-killmails.read_killmails.v1".into(),
    ];
    let code = ctx.db.authorize_pkce(user, pass, char_id, "evomu-d6-batch4", "https://localhost/callback", scopes, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM", "S256", now).expect("authorize_pkce");
    ctx.db.exchange_pkce_code(&code, "evomu-d6-batch4", "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk", now).expect("exchange_pkce_code").access_token
}

fn esi_get(router: &EsiRouter, path: &str, token: Option<&str>) -> Value {
    let mut req = EsiHttpRequest::new("GET", path);
    if let Some(t) = token {
        req = req.with_bearer_token(t);
    }
    let resp = router.handle_request(&req);
    assert_eq!(resp.status_code, 200, "ESI GET {path} failed ({}): {}", resp.status_code, String::from_utf8_lossy(&resp.body));
    resp.json().expect("valid ESI JSON")
}

async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "authentication", None, "Login", &[PyRep::String(user.into()), PyRep::String(pass.into())], &kw, None).await.expect("Login");
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
    sess
}

async fn call(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn call_kw(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep], kw_pairs: &[(&str, PyRep)]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw: HashMap<String, PyRep> = kw_pairs.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect();
    ctx.dispatch_rpc(sess, svc, None, method, args, &kw, None).await
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

fn make_structure(structure_id: i64, type_id: u32, owner_corp_id: i64, solar_system_id: u32, name: &str, state: &str, fuel_blocks: u32) -> UpwellStructureRecord {
    UpwellStructureRecord {
        structure_id, type_id, owner_corp_id, solar_system_id, name: name.into(), position: [1_000.0, 2_000.0, 3_000.0],
        state: state.into(), quantum_core_installed: true, fuel_blocks, shield_hp: 100_000_000.0, armor_hp: 100_000_000.0,
        hull_hp: 100_000_000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0,
    }
}

async fn send_quasar_req(ctx: &ServerContext, sess: &mut SessionState, type_name: &str, payload_value: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }), correlation_uuid: b"corr-d6-batch4".to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value: payload_value }),
        external_origin: "eve-v24-client".into(), application_instance_uuid: b"app-d6-batch4".to_vec(), authoritative_context: None, trace_context: None,
    };
    let rep = call(ctx, sess, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(req.encode_proto())]).await.expect("publicGatewaySvc.SendRequest");
    let Some(PyRep::Bytes(b)) = rep.dict_get("response_proto") else { panic!("Expected response_proto") };
    EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse")
}

#[tokio::test]
async fn test_d6_industry_pi_and_market_wallet_history_durable_across_sqlite_restart() {
    // parity: feat:lifecycle.industry-pi-durable
    // parity: feat:lifecycle.market-wallet-history-durable
    eve_testkit::attest(&[
        "feat:lifecycle.industry-pi-durable",
        "feat:lifecycle.market-wallet-history-durable",
    ]);

    let path = std::env::temp_dir().join(format!("evomu_d6_ind_mkt_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    let base_time: u64 = 1_800_000_000;
    let athanor_id = 1_045_000_000_101_i64;
    let metenox_id = 1_045_000_000_102_i64;

    let (saved_clock_unix, mined_ore_type_id, installed_job_id, sell_order_id, bill_id, trade_id, raffle_id, old_hist_before, rnd_rp_before) = {
        let ctx = open_ctx_at(&path, base_time);
        let roles = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_STATION_MANAGER | corp_roles::ROLE_STARBASE_CONFIG;
        ctx.db.with_mut_snapshot(|snap| {
            *snap = demo;
            if let Some(ch) = snap.characters.get_mut(&P1) { ch.corporation_id = CORP_ID; ch.balance = IskCents::from_isk(500_000_000); ch.standings.insert(1_000_084, 6.0); }
            if let Some(ch) = snap.characters.get_mut(&P2) { ch.balance = IskCents::from_isk(500_000_000); }
            let mut corp = CorporationRecord {
                corporation_id: CORP_ID, name: "Durable Corp".into(), ticker: "DURB".into(), ceo_id: P1, alliance_id: None,
                tax_rate: 0.05, master_wallet: IskCents::from_isk(200_000_000), wallet_divisions: Vec::new(), description: String::new(),
                url: String::new(), hq_station_id: Some(JITA_STATION_ID), members: [P1].into_iter().collect(),
                member_roles: HashMap::from([(P1, roles)]), applications: HashMap::new(),
            };
            corp.ensure_wallet_divisions();
            corp.wallet_divisions[0].balance = IskCents::from_isk(200_000_000);
            snap.corporations.insert(CORP_ID, corp);
            for &(tid, lvl) in &[(2403, 5), (2406, 5), (2495, 5), (2505, 5), (3380, 5), (3386, 5), (3402, 5), (11448, 4), (13279, 5), (46152, 5)] {
                snap.skills.entry(P1).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
            }
            for (iid, tid, qty, sing) in [(910_002, RIFTER_BPO, 1, true), (910_003, TRITANIUM, 5_000_000, false), (910_004, 35, 1_000_000, false), (910_005, MEXALLON, 500_000, false), (910_006, 37, 500_000, false), (910_007, 44992, 100, false), (910_008, 52_568, 500, false), (910_009, 587, 1, false)] {
                snap.items.insert(iid, make_item(iid, tid, P1, JITA_STATION_ID, InventoryFlag::Hangar, qty, sing));
            }
            snap.structures.insert(athanor_id, make_structure(athanor_id, 35835, CORP_ID, JITA_SYSTEM_ID, "Durable Athanor", "ShieldVulnerable", 500));
            snap.structures.insert(metenox_id, make_structure(metenox_id, 81826, CORP_ID, LOWSEC_SYS_ID, "Durable Metenox", "ShieldVulnerable", 200));
            snap.items.insert(910_020, make_item(910_020, 81143, CORP_ID, metenox_id, InventoryFlag::Hangar, 500, false));
            eve_server::services::industry_pi::save_orbital_settings(snap, PLANET_ID, &eve_server::services::industry_pi::OrbitalCustomsSettings { planet_id: PLANET_ID, owner_corp_id: CORP_ID, ..Default::default() });
        });

        let mut sess_p1 = login_select(&ctx, 8101, "demo", "demo-secret", P1).await;
        let mut sess_p2 = login_select(&ctx, 8102, "other", "other-secret", P2).await;
        sess_p1.corp_role = roles;

        // 1. Start Moon Extraction & Metenox, advance to chunk arrival, fracture & mine moon asteroid, deploy PI network while undocked
        call(&ctx, &mut sess_p1, "moonExtractions", "StartNewExtraction", &[PyRep::Integer(athanor_id), PyRep::Integer(518_400)]).await.expect("StartNewExtraction");
        call(&ctx, &mut sess_p1, "autoMoonMining", "RequestMiningDetails", &[PyRep::Integer(metenox_id)]).await.expect("RequestMiningDetails");
        ctx.advance_time_seconds(518_400);
        call(&ctx, &mut sess_p1, "autoMoonMining", "RequestMiningCycleOutput", &[PyRep::Integer(metenox_id), PyRep::Integer(2)]).await.expect("RequestMiningCycleOutput");
        let frac = call(&ctx, &mut sess_p1, "moonExtractions", "FractureChunkWithStructure", &[PyRep::Integer(athanor_id)]).await.expect("FractureChunkWithStructure");
        let PyRep::List(ast_ids) = frac.dict_get("asteroidIDs").unwrap() else { panic!("Expected asteroidIDs") };
        let first_ast_id = ast_ids[0].as_i64().unwrap();

        let undock = call(&ctx, &mut sess_p1, "stationSvc", "Undock", &[]).await.expect("Undock");
        let ship_id = undock.dict_get("shipID").and_then(|v| v.as_i64()).unwrap();
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ship) = s.items.get_mut(&ship_id) {
                ship.type_id = 32880;
            }
            s.items.retain(|_, item| item.location_id != ship_id);
            s.items.insert(910_030, make_item(910_030, 483, P1, ship_id, InventoryFlag::HiSlot0, 1, true));
            s.items.insert(910_031, make_item(910_031, 2534, P1, ship_id, InventoryFlag::Cargo, 1, false));
        });
        ctx.invalidate_ship_fitting(ship_id);

        // Deploy PI colony & network on Jita IV while undocked with Gas Command Center (2534) in ship cargo
        let res_info = call(&ctx, &mut sess_p1, "planetMgr", "GetPlanetResourceInfo", &[PyRep::Integer(PLANET_ID)]).await.expect("GetPlanetResourceInfo");
        let PyRep::List(res_list) = res_info.dict_get("resources").unwrap() else { panic!() };
        let first_p0_tid = res_list[0].dict_get("resourceTypeID").and_then(|v| v.as_i64()).unwrap();
        call(
            &ctx, &mut sess_p1, "planetMgr", "UserUpdateNetwork",
            &[
                PyRep::Integer(PLANET_ID),
                PyRep::List(vec![
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(101), PyRep::Integer(2534), PyRep::Real(0.10), PyRep::Real(0.10)]),
                    PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(101), PyRep::Integer(3)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(102), PyRep::Integer(2848), PyRep::Real(0.11), PyRep::Real(0.11)]),
                    PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(103), PyRep::Integer(2492), PyRep::Real(0.12), PyRep::Real(0.12)]),
                    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(101), PyRep::Integer(102), PyRep::Integer(0)]),
                    PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(102), PyRep::Integer(103), PyRep::Integer(0)]),
                    PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(102), PyRep::Integer(103), PyRep::Integer(1)]),
                    PyRep::Tuple(vec![PyRep::Integer(7), PyRep::Integer(103), PyRep::Integer(65)]),
                    PyRep::Tuple(vec![PyRep::Integer(6), PyRep::Integer(501), PyRep::List(vec![PyRep::Integer(102), PyRep::Integer(103)]), PyRep::Integer(first_p0_tid), PyRep::Integer(3_000)]),
                    PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(102), PyRep::Integer(first_p0_tid), PyRep::Real(0.02), PyRep::Integer(3)]),
                ]),
            ],
        ).await.expect("UserUpdateNetwork");

        let jita = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
        let undock_pos = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.position;
        let ast_pos = jita.get_ball_snapshot(first_ast_id).await.unwrap().unwrap().1.position;
        if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await { sb.position = ast_pos + DVec3::new(500.0, 0.0, 0.0); let _ = jita.spawn_ball(sb).await; }
        let mine_res = crate::mining_cycle_engine::mine_once(&ctx, &mut sess_p1, 910_030, first_ast_id).await.expect("mine_once on moon asteroid");
        if let Ok(Some((_, mut sb))) = jita.get_ball_snapshot(ship_id).await { sb.position = undock_pos; let _ = jita.spawn_ball(sb).await; }
        call(&ctx, &mut sess_p1, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION_ID)]).await.expect("Dock");

        call_kw(&ctx, &mut sess_p1, "planetOrbitalRegistryBroker", "UpdateSettings", &[PyRep::Integer(PLANET_ID)], &[("taxRateValues", PyRep::keyval([("corporation", PyRep::Real(0.075)), ("alliance", PyRep::Real(0.095))])), ("allowAlliance", PyRep::Bool(true)), ("allowStandings", PyRep::Bool(true)), ("minimumStanding", PyRep::Real(-2.0))]).await.expect("UpdateSettings");
        call(&ctx, &mut sess_p1, "posMgr", "ChangeStructureProvisionType", &[PyRep::Integer(athanor_id), PyRep::Integer(athanor_id), PyRep::Integer(16634)]).await.expect("ChangeStructureProvisionType");

        let rnd_agent_id = ctx.sde.agents.values().filter(|a| a.agent_type_id == 4 || a.division_id == 18).map(|a| a.agent_id).min().expect("R&D agent");
        ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&P1) { ch.standings.insert(rnd_agent_id, 9.0); } });
        call(&ctx, &mut sess_p1, "agentMgr", "DoAction", &[PyRep::Integer(12), PyRep::Integer(rnd_agent_id), PyRep::Integer(11448)]).await.expect("StartResearch");
        let PyRep::Tuple(j_parts) = call(&ctx, &mut sess_p1, "agentMgr", "GetMyJournalDetails", &[]).await.expect("GetMyJournalDetails") else { panic!() };
        let PyRep::List(rnd_list_before) = &j_parts[1] else { panic!() };
        let rnd_rp_before = rnd_list_before[0].dict_get("points").and_then(|v| v.as_f64()).unwrap_or(0.0);

        let inst = call(&ctx, &mut sess_p1, "industryManager", "InstallJob", &[PyRep::Integer(RIFTER_BPO as i64), PyRep::Integer(1), PyRep::Integer(2)]).await.expect("InstallJob");
        let job_id = inst.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

        let sell_res = call(&ctx, &mut sess_p1, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![PyRep::Dict(vec![(PyRep::String("itemID".into()), PyRep::Integer(910_003)), (PyRep::String("typeID".into()), PyRep::Integer(TRITANIUM as i64)), (PyRep::String("price".into()), PyRep::Real(4.50)), (PyRep::String("quantity".into()), PyRep::Integer(1_000)), (PyRep::String("stationID".into()), PyRep::Integer(JITA_STATION_ID)), (PyRep::String("duration".into()), PyRep::Integer(90))])]), PyRep::Bool(false)]).await.expect("PlaceMultiSellOrder");
        let sell_oid = sell_res.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

        call(&ctx, &mut sess_p2, "marketProxy", "PlaceBuyOrder", &[PyRep::Integer(JITA_STATION_ID), PyRep::Integer(TRITANIUM as i64), PyRep::Real(4.50), PyRep::Integer(400), PyRep::Integer(32767), PyRep::Integer(1), PyRep::Integer(90), PyRep::Bool(false)]).await.expect("PlaceBuyOrder");
        let old_hist = call(&ctx, &mut sess_p1, "marketProxy", "GetOldPriceHistory", &[PyRep::Integer(TRITANIUM as i64)]).await.expect("GetOldPriceHistory");
        call_kw(&ctx, &mut sess_p1, "account", "GiveCashFromCorpAccount", &[PyRep::Integer(P2), PyRep::Real(50_000.0), PyRep::Integer(1000)], &[("reason", PyRep::String("Corp dividend".into()))]).await.expect("GiveCashFromCorpAccount");

        let bill_id = eve_server::services::wallet_serverops::create_bill(&ctx, 2, CORP_ID, 1_000_035, 250_000.0, ctx.now_unix() + 86_400, 0.0, JITA_STATION_ID, 0);
        let trade_id = call(&ctx, &mut sess_p1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await.expect("InitiateTrade").dict_get("tradeContainerID").and_then(|v| v.as_i64()).unwrap();
        call(&ctx, &mut sess_p1, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id), PyRep::Real(125_000.0)]).await.expect("OfferMoney");
        let raffle_id = call(&ctx, &mut sess_p1, "raffleProxy", "CreateRaffle", &[PyRep::Dict(vec![(PyRep::String("item_id".into()), PyRep::Integer(910_009)), (PyRep::String("ticket_count".into()), PyRep::Integer(8)), (PyRep::String("ticket_price".into()), PyRep::Real(100_000.0))])]).await.expect("CreateRaffle").dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();

        (ctx.now_unix(), mine_res.ore_type_id, job_id, sell_oid, bill_id, trade_id, raffle_id, old_hist, rnd_rp_before)
    };

    // === RESTART SERVER FROM SQLITE (+3,600s elapsed across downtime) ===
    {
        let ctx = open_ctx_at(&path, saved_clock_unix + 3_600);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let token = issue_esi_token(&ctx, P1);
        let mut sess_p1 = login_select(&ctx, 8103, "demo", "demo-secret", P1).await;
        sess_p1.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_STATION_MANAGER | corp_roles::ROLE_STARBASE_CONFIG;

        // 1. Verify PI colony, customs office, moon extraction, Metenox, mining ledgers, R&D agent, industry job via RPC + ESI
        let col = call(&ctx, &mut sess_p1, "planetMgr", "GetColony", &[PyRep::Integer(PLANET_ID)]).await.expect("GetColony");
        assert!(col.dict_get("simulatedExtractedQuantity").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
        let esi_planet = esi_get(&router, &format!("/v3/characters/{P1}/planets/{PLANET_ID}/"), Some(&token));
        assert_eq!(esi_planet["pins"].as_array().unwrap().len(), 3);
        assert_eq!(esi_planet["routes"].as_array().unwrap().len(), 1);
        let tax = call(&ctx, &mut sess_p1, "planetOrbitalRegistryBroker", "GetTaxRate", &[PyRep::Integer(PLANET_ID)]).await.expect("GetTaxRate");
        assert_eq!(tax.as_f64(), Some(0.075));
        let esi_pocos = esi_get(&router, &format!("/v1/corporations/{CORP_ID}/customs_offices/"), Some(&token));
        assert_eq!(esi_pocos.as_array().unwrap()[0]["corporation_tax_rate"], 0.075);
        let prov = call(&ctx, &mut sess_p1, "posMgr", "GetMoonProcessInfoForTower", &[PyRep::Integer(athanor_id)]).await.expect("GetMoonProcessInfoForTower");
        assert_eq!(prov.as_i64(), Some(16634));
        let ext_info = call(&ctx, &mut sess_p1, "moonExtractions", "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(athanor_id)]).await.expect("GetExtractionAndEventIDFromStructureID");
        assert_eq!(ext_info.dict_get("status").and_then(|v| v.as_str()), Some("Fractured"));
        let esi_exts = esi_get(&router, &format!("/v1/corporation/{CORP_ID}/mining/extractions/"), Some(&token));
        assert_eq!(esi_exts.as_array().unwrap()[0]["structure_id"], athanor_id);
        let met_det = call(&ctx, &mut sess_p1, "autoMoonMining", "RequestMiningDetails", &[PyRep::Integer(metenox_id)]).await.expect("RequestMiningDetails after restart");
        assert_eq!(met_det.dict_get("status").and_then(|v| v.as_str()), Some("Active"));
        assert_eq!(met_det.dict_get("fuelRemaining").and_then(|v| v.as_i64()), Some(190));
        let obs_led = call(&ctx, &mut sess_p1, "corpMiningLedger", "GetObserverLedger", &[PyRep::Integer(athanor_id)]).await.expect("GetObserverLedger");
        assert!(format!("{obs_led:?}").contains(&P1.to_string()));
        let esi_obs = esi_get(&router, &format!("/v1/corporation/{CORP_ID}/mining/observers/{athanor_id}/"), Some(&token));
        assert_eq!(esi_obs.as_array().unwrap()[0]["character_id"], P1);

        let char_led = call(&ctx, &mut sess_p1, "characterMiningLedger", "GetCharacterLogs", &[]).await.expect("GetCharacterLogs");
        assert!(format!("{char_led:?}").contains(&mined_ore_type_id.to_string()));
        let esi_mining = esi_get(&router, &format!("/v1/characters/{P1}/mining/"), Some(&token));
        assert_eq!(esi_mining.as_array().unwrap()[0]["type_id"], mined_ore_type_id as i64);

        let PyRep::Tuple(j_parts) = call(&ctx, &mut sess_p1, "agentMgr", "GetMyJournalDetails", &[]).await.expect("GetMyJournalDetails") else { panic!() };
        let PyRep::List(rnd_list_after) = &j_parts[1] else { panic!() };
        assert!(rnd_list_after[0].dict_get("points").and_then(|v| v.as_f64()).unwrap_or(0.0) > rnd_rp_before);
        let esi_rnd = esi_get(&router, &format!("/v2/characters/{P1}/agents_research/"), Some(&token));
        assert_eq!(esi_rnd.as_array().unwrap()[0]["skill_type_id"], 11448);

        let jobs_rep = call(&ctx, &mut sess_p1, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)]).await.expect("GetJobsByOwner");
        assert!(format!("{jobs_rep:?}").contains(&installed_job_id.to_string()));
        let esi_jobs = esi_get(&router, &format!("/v1/characters/{P1}/industry/jobs/"), Some(&token));
        assert!(esi_jobs.as_array().unwrap().iter().any(|j| j["job_id"] == installed_job_id));

        // 2. Verify Market Orders, Price History, Wallet Journals/Transactions, Bill, Trade, Raffle via RPC + ESI
        let char_orders = call(&ctx, &mut sess_p1, "marketProxy", "GetCharOrders", &[]).await.expect("GetCharOrders");
        assert!(format!("{char_orders:?}").contains(&sell_order_id.to_string()));
        let esi_orders = esi_get(&router, &format!("/v2/characters/{P1}/orders/"), Some(&token));
        let ord_json = esi_orders.as_array().unwrap().iter().find(|o| o["order_id"] == sell_order_id).expect("sell order in ESI");
        assert_eq!(ord_json["volume_remain"], 600);
        assert_eq!(ord_json["duration"], 90);

        let old_hist_after = call(&ctx, &mut sess_p1, "marketProxy", "GetOldPriceHistory", &[PyRep::Integer(TRITANIUM as i64)]).await.expect("GetOldPriceHistory");
        assert_eq!(old_hist_after, old_hist_before);
        let esi_hist = esi_get(&router, "/v1/markets/10000002/history/?type_id=34", None);
        assert_eq!(esi_hist.as_array().unwrap()[0]["volume"], 400);
        let esi_w_journal = esi_get(&router, &format!("/v6/characters/{P1}/wallet/journal/"), Some(&token));
        assert!(!esi_w_journal.as_array().unwrap()[0]["date"].as_str().unwrap().starts_with("1970-"));
        let esi_w_tx = esi_get(&router, &format!("/v1/characters/{P1}/wallet/transactions/"), Some(&token));
        assert_eq!(esi_w_tx.as_array().unwrap()[0]["quantity"], 400);
        let esi_corp_j = esi_get(&router, &format!("/v4/corporations/{CORP_ID}/wallets/1/journal/"), Some(&token));
        assert!(esi_corp_j.as_array().unwrap().iter().any(|e| e["description"] == "Corp dividend"));
        let bills = call(&ctx, &mut sess_p1, "billMgr", "GetCorporationBills", &[]).await.expect("GetCorporationBills");
        assert!(format!("{bills:?}").contains(&bill_id.to_string()));
        let tr = call(&ctx, &mut sess_p1, "trademgr", "List", &[PyRep::Integer(trade_id)]).await.expect("trademgr.List");
        assert_eq!(tr.dict_get("myMoney").and_then(|v| v.as_f64()), Some(125_000.0));
        let raf = call(&ctx, &mut sess_p1, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)]).await.expect("GetRaffle");
        assert!(format!("{raf:?}").contains(&raffle_id.to_string()));
    }
    let _ = std::fs::remove_file(&path);
}

async fn unspawn_balls(ctx: &ServerContext, sess: &mut SessionState, balls: &[i64]) {
    sess.role |= eve_machonet::service_roles::ROLE_GML;
    for &bid in balls {
        call(ctx, sess, "slash", "SlashCmd", &[PyRep::String(format!("/unspawn {bid}"))]).await.expect("slash /unspawn");
    }
}

#[tokio::test]
async fn test_d6_pve_progress_and_abyssal_restart_durable_across_sqlite_restart() {
    // parity: feat:lifecycle.pve-progress-durable
    // parity: feat:lifecycle.abyssal-restart
    eve_testkit::attest(&[
        "feat:lifecycle.pve-progress-durable",
        "feat:lifecycle.abyssal-restart",
    ]);

    let path = std::env::temp_dir().join(format!("evomu_d6_pve_abyss_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    let base_time: u64 = 1_760_100_000;
    let hack_sig_id = format!("DAT-{JITA_SYSTEM_ID}");
    let dat_pos = [-32_000_000.0, 28_000_000.0, -10_000_000.0];
    let trace_origin = [145_000.0, -75_000.0, 42_000.0];

    let (s2_id, r1_balls): (i64, Vec<i64>) = {
        let ctx = open_ctx_at(&path, base_time);
        let s2 = ctx.db.with_mut_snapshot(|snap| {
            *snap = demo;
            let s1 = snap.characters[&P1].active_ship_id;
            let s2 = snap.characters[&P2].active_ship_id;
            snap.items.insert(920_001, make_item(920_001, 47763, P1, s1, InventoryFlag::Cargo, 5, false));
            snap.items.insert(920_002, make_item(920_002, 47763, P2, s2, InventoryFlag::Cargo, 5, false));
            s2
        });

        let (mut sess_p1, mut sess_p2) = (login_select(&ctx, 8201, "demo", "demo-secret", P1).await, login_select(&ctx, 8202, "other", "other-secret", P2).await);
        eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| eve_server::services::pve_incursion_sites::seed_incursion_for_constellation(&ctx, reg, 20_000_020));

        let any_agent = *ctx.sde.agents.keys().next().unwrap();
        call(&ctx, &mut sess_p1, "agentMgr", "DoAction", &[PyRep::Integer(18), PyRep::Integer(any_agent), PyRep::String("Amarr".into())]).await.expect("StartEpicArc");
        call(&ctx, &mut sess_p1, "agentMgr", "DoAction", &[PyRep::Integer(2)]).await.expect("OfferMission");
        let dec1 = call(&ctx, &mut sess_p1, "agentMgr", "DoAction", &[PyRep::Integer(9)]).await.expect("DeclineMission 1");
        assert_eq!(dec1.dict_get("standingPenaltyApplied").and_then(|v| v.as_bool()), Some(false));

        call(&ctx, &mut sess_p1, "stationSvc", "Undock", &[]).await.expect("Undock P1");
        call(&ctx, &mut sess_p2, "stationSvc", "Undock", &[]).await.expect("Undock P2");
        ctx.db.with_mut_snapshot(|snap| { snap.item_positions.insert(s2, trace_origin); });

        call(&ctx, &mut sess_p1, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[]).await.expect("GetCombatAnomalyInstances");
        call(&ctx, &mut sess_p1, "dungeonInstanceCacheMgr", "GetSignatureInstances", &[]).await.expect("GetSignatureInstances");
        let tracker = call(&ctx, &mut sess_p1, "scanMgr", "SignalTrackerRegister", &[]).await.expect("SignalTrackerRegister");
        assert_eq!(tracker.dict_get("anomalyCount").and_then(|v| v.as_i64()), Some(2));

        let probes: Vec<PyRep> = (0..8).map(|_| PyRep::List(vec![PyRep::Real(dat_pos[0]), PyRep::Real(dat_pos[1]), PyRep::Real(dat_pos[2])])).collect();
        let scan_rep = call(&ctx, &mut sess_p1, "scanMgr", "RequestScans", &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)]).await.expect("RequestScans");
        assert!(format!("{scan_rep:?}").contains("Digital Plexus"));
        call(&ctx, &mut sess_p1, "bookmark", "BookmarkScanResult", &[PyRep::Integer(JITA_SYSTEM_ID as i64), PyRep::String("Data Site Bookmark".into()), PyRep::String("scanned".into()), PyRep::String(hack_sig_id.clone()), PyRep::Integer(0)]).await.expect("BookmarkScanResult");

        let w1_balls = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system.get(&JITA_SYSTEM_ID)
                .and_then(|l| l.iter().find(|s| s.site_id == "PVE-ANOM-30000142-1"))
                .map(|s| s.active_npc_ball_ids.clone()).unwrap_or_default()
        });
        unspawn_balls(&ctx, &mut sess_p1, &w1_balls).await;
        let w2 = call(&ctx, &mut sess_p1, "keeper", "EnterNextDungeonRoom", &[PyRep::String("PVE-ANOM-30000142-1".into())]).await.expect("Wave 1 -> 2");
        let w2_balls: Vec<i64> = match w2.dict_get("activeNpcBallIDs") { Some(PyRep::List(l)) => l.iter().filter_map(|b| b.as_i64()).collect(), _ => vec![] };
        unspawn_balls(&ctx, &mut sess_p1, &w2_balls).await;
        let w_done = call(&ctx, &mut sess_p1, "keeper", "EnterNextDungeonRoom", &[PyRep::String("PVE-ANOM-30000142-1".into())]).await.expect("Complete anomaly");
        assert_eq!(w_done.dict_get("escalated").and_then(|v| v.as_bool()), Some(true));

        call(&ctx, &mut sess_p1, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(1_870_014_200)]).await.expect("StartNewGameInstance");
        call(&ctx, &mut sess_p1, "map", "GetIncursionGlobalReport", &[]).await.expect("GetIncursionGlobalReport");
        let (vg_sys, vg_bid, vg_pos) = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            let s = reg.incursions[&20_000_020].sites.iter().find(|s| s.tier == "Vanguard").unwrap();
            (s.solar_system_id, s.boss_npc_ball_id, s.position)
        });
        sess_p1.solar_system_id = Some(vg_sys);
        let s1 = ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&P1).unwrap().solar_system_id = vg_sys; s.characters[&P1].active_ship_id });
        let _ = ctx.systems.get_or_spawn(vg_sys).spawn_ball(eve_server::services::pve_incursion_sites::make_site_npc_ball(s1, 587, vg_pos)).await;
        unspawn_balls(&ctx, &mut sess_p1, &[vg_bid]).await;
        let inc_step = call(&ctx, &mut sess_p1, "keeper", "EnterNextDungeonRoom", &[PyRep::String("INC-VG-5165".into())]).await.expect("Complete Incursion VG");
        sess_p1.solar_system_id = Some(JITA_SYSTEM_ID);
        ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&P1).unwrap().solar_system_id = JITA_SYSTEM_ID; });
        assert_eq!(inc_step.dict_get("influence").and_then(|v| v.as_f64()), Some(0.95));

        let act2 = call(&ctx, &mut sess_p2, "abyssal_content_manager", "ActivateAbyssalKey", &[PyRep::Integer(47763)]).await.expect("ActivateAbyssalKey P2");
        let r1_balls: Vec<i64> = match act2.dict_get("npcBallIDs") { Some(PyRep::List(l)) => l.iter().filter_map(|b| b.as_i64()).collect(), _ => vec![] };
        assert!(!r1_balls.is_empty());
        (s2, r1_balls)
    };

    // === RESTART 1: +300s (within 20m Abyssal window; P2 resumes in intact pocket with NPC balls restored, then downtime evacuates P2 near trace origin) ===
    {
        let ctx = open_ctx_at(&path, base_time + 300);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let token_p2 = issue_esi_token(&ctx, P2);
        let mut sess_p1 = login_select(&ctx, 8203, "demo", "demo-secret", P1).await;
        let sess_p2_in_abyss = login_select(&ctx, 8204, "other", "other-secret", P2).await;
        assert_eq!(sess_p2_in_abyss.solar_system_id, Some(32_000_001));
        assert_eq!(esi_get(&router, &format!("/v1/characters/{P2}/location/"), Some(&token_p2))["solar_system_id"], 32_000_001);
        let abyss_sys = ctx.systems.get_or_spawn(32_000_001);
        assert!(abyss_sys.get_ball_snapshot(r1_balls[0]).await.unwrap().is_some(), "Room 1 NPC balls must be restored in Abyssal pocket on restart login");

        sess_p1.role |= eve_machonet::service_roles::ROLE_GML;
        call(&ctx, &mut sess_p1, "slash", "SlashCmd", &[PyRep::String("/downtime".into())]).await.expect("slash /downtime");
        let mut sess_p2 = login_select(&ctx, 8207, "other", "other-secret", P2).await;
        assert_eq!(sess_p2.solar_system_id, Some(JITA_SYSTEM_ID));
        assert_eq!(sess_p2.ship_id, Some(s2_id));
        assert_eq!(esi_get(&router, &format!("/v1/characters/{P2}/location/"), Some(&token_p2))["solar_system_id"], JITA_SYSTEM_ID);
        let p2_pos = ctx.db.with_snapshot(|snap| snap.item_positions[&s2_id]);
        for i in 0..3 {
            let d = (p2_pos[i] - trace_origin[i]).abs();
            assert!((500.0..=5_000.0).contains(&d), "Downtime evacuation offset axis {i} must be 500..=5000m from trace origin, got {d}");
        }
        call(&ctx, &mut sess_p2, "abyssal_content_manager", "ActivateAbyssalKey", &[PyRep::Integer(47763)]).await.expect("ActivateAbyssalKey P2 #2");

        let passive_probe = PyRep::List(vec![PyRep::List(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)])]);
        let scan_after = call(&ctx, &mut sess_p1, "scanMgr", "RequestScans", &[PyRep::Real(1_000.0), PyRep::Real(1.0), passive_probe]).await.expect("RequestScans after restart");
        let PyRep::Rowset { rows: sig_rows, .. } = &scan_after else { panic!("Expected Rowset") };
        let dat_row = sig_rows.iter().find(|r| r[0].as_str() == Some(hack_sig_id.as_str())).expect("DAT signature row");
        assert_eq!(dat_row[2].as_str(), Some("Digital Plexus"));
        assert_eq!(dat_row[3].as_f64(), Some(1.0));
        assert_eq!(dat_row[4].as_bool(), Some(true));
        assert_eq!([dat_row[5].as_f64().unwrap(), dat_row[6].as_f64().unwrap(), dat_row[7].as_f64().unwrap()], dat_pos);

        let tracker_after = call(&ctx, &mut sess_p1, "scanMgr", "SignalTrackerRegister", &[]).await.expect("SignalTrackerRegister after restart");
        assert_eq!(tracker_after.dict_get("anomalyCount").and_then(|v| v.as_i64()), Some(1));
        let Some(PyRep::Rowset { rows: anom_rows, .. }) = tracker_after.dict_get("anomalies") else { panic!("Expected anomalies rowset") };
        assert_eq!(anom_rows.len(), 1);
        assert_eq!(anom_rows[0][0].as_str(), Some("PVE-ANOM-30000142-2"));
        assert_eq!([anom_rows[0][4].as_f64().unwrap(), anom_rows[0][5].as_f64().unwrap(), anom_rows[0][6].as_f64().unwrap()], [30_000_000.0, 12_000_000.0, -8_000_000.0]);
        let anoms_cache = call(&ctx, &mut sess_p1, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[]).await.expect("GetCombatAnomalyInstances after restart");
        assert!(!format!("{anoms_cache:?}").contains("2134") && format!("{anoms_cache:?}").contains("1215"));

        let esc_list = call(&ctx, &mut sess_p1, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await.expect("GetMyEscalatingPathDetails");
        let PyRep::List(escs) = &esc_list else { panic!("Expected List") };
        assert_eq!(escs.len(), 1);
        assert_eq!(escs[0].dict_get("sourceDungeonID").and_then(|v| v.as_i64()), Some(2134));
        assert_eq!(escs[0].dict_get("dungeonID").and_then(|v| v.as_i64()), Some(2403));
        let bms = call(&ctx, &mut sess_p1, "bookmark", "GetBookmarks", &[]).await.expect("GetBookmarks");
        let bm_dbg = format!("{bms:?}");
        assert!(bm_dbg.contains("Data Site Bookmark") && bm_dbg.contains("-32000000.0") && bm_dbg.contains("Escalation:"));

        let hack = call(&ctx, &mut sess_p1, "hackingMgr", "GetMyHackProgress", &[PyRep::Integer(1_870_014_200)]).await.expect("GetMyHackProgress after restart");
        assert_eq!(hack.dict_get("status").and_then(|v| v.as_str()), Some("InProgress"));

        let tales = call(&ctx, &mut sess_p1, "map", "GetIncursionGlobalReport", &[]).await.expect("GetIncursionGlobalReport after restart");
        let PyRep::List(tale_list) = &tales else { panic!("Expected List") };
        let kimo_tale = tale_list.iter().find(|t| t.dict_get("constellationID").and_then(|v| v.as_i64()) == Some(20_000_020)).unwrap();
        assert_eq!(kimo_tale.dict_get("influence").and_then(|v| v.as_f64()), Some(0.95));
        let esi_inc = esi_get(&router, "/v1/incursions/", None);
        let kimo_esi = esi_inc.as_array().unwrap().iter().find(|i| i["constellation_id"] == 20_000_020).unwrap();
        assert_eq!(kimo_esi["influence"], 0.95);

        let arcs = call(&ctx, &mut sess_p1, "agentMgr", "GetMyEpicArcStatus", &[]).await.expect("GetMyEpicArcStatus");
        assert!(format!("{arcs:?}").contains("Amarr"));
        call(&ctx, &mut sess_p1, "agentMgr", "DoAction", &[PyRep::Integer(2)]).await.expect("OfferMission 2");
        let dec2 = call(&ctx, &mut sess_p1, "agentMgr", "DoAction", &[PyRep::Integer(9)]).await.expect("DeclineMission 2");
        assert_eq!(dec2.dict_get("standingPenaltyApplied").and_then(|v| v.as_bool()), Some(true));
    }

    // === RESTART 2: +2,000s (> 1,500s collapse deadline; P2's ship & pod destroyed, respawned at home station in fresh Capsule + killmail) ===
    {
        let ctx = open_ctx_at(&path, base_time + 2_000);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let token_p2 = issue_esi_token(&ctx, P2);
        let sess_p2 = login_select(&ctx, 8205, "other", "other-secret", P2).await;
        assert_eq!(sess_p2.solar_system_id, Some(JITA_SYSTEM_ID));
        assert_eq!(sess_p2.station_id, Some(JITA_STATION_ID));
        let new_pod_id = sess_p2.ship_id.expect("respawned pod ship_id");
        assert_ne!(new_pod_id, s2_id, "Old ship must be destroyed on Abyssal collapse");
        assert_eq!(esi_get(&router, &format!("/v1/characters/{P2}/location/"), Some(&token_p2))["station_id"], JITA_STATION_ID);
        assert_eq!(esi_get(&router, &format!("/v1/characters/{P2}/ship/"), Some(&token_p2))["ship_type_id"], 670);
        let kms = esi_get(&router, &format!("/v1/characters/{P2}/killmails/recent/"), Some(&token_p2));
        assert!(!kms.as_array().unwrap().is_empty(), "Abyssal collapse must record a killmail visible via ESI");
        ctx.db.with_snapshot(|snap| {
            assert!(!snap.items.contains_key(&s2_id), "Destroyed Abyssal ship must be removed from inventory");
            assert_eq!(snap.items[&new_pod_id].type_id, 670, "Pilot must respawn in a fresh Capsule (type 670)");
            assert!(snap.killmails.values().any(|k| k.victim_character_id == P2 && k.solar_system_id == 32_000_001), "Abyssal collapse must record a killmail");
        });
    }

    // === RESTART 3: +90,000s (> 86,400s 24h lifetime; signatures, anomalies, and escalations naturally despawn) ===
    {
        let ctx = open_ctx_at(&path, base_time + 90_000);
        let mut sess_p1 = login_select(&ctx, 8206, "demo", "demo-secret", P1).await;
        let tracker_exp = call(&ctx, &mut sess_p1, "scanMgr", "SignalTrackerRegister", &[]).await.expect("SignalTrackerRegister at +90,000s");
        assert_eq!(tracker_exp.dict_get("anomalyCount").and_then(|v| v.as_i64()), Some(0), "Expired anomalies must despawn after 24h");
        let sigs_exp = call(&ctx, &mut sess_p1, "scanMgr", "GetSignatures", &[]).await.expect("GetSignatures at +90,000s");
        let PyRep::Rowset { rows: exp_sig_rows, .. } = &sigs_exp else { panic!("Expected Rowset") };
        assert!(exp_sig_rows.is_empty(), "Expired cosmic signatures must despawn after 24h");
        let esc_exp = call(&ctx, &mut sess_p1, "dungeonExplorationMgr", "GetMyEscalatingPathDetails", &[]).await.expect("GetMyEscalatingPathDetails at +90,000s");
        assert_eq!(esc_exp, PyRep::List(vec![]), "Expired escalations must despawn after 24h");
    }
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn test_d6_ship_item_config_and_account_progress_durable_across_sqlite_restart() {
    // parity: feat:lifecycle.ship-and-item-config-durable
    // parity: feat:lifecycle.account-progress-durable
    eve_testkit::attest(&[
        "feat:lifecycle.ship-and-item-config-durable",
        "feat:lifecycle.account-progress-durable",
    ]);

    let path = std::env::temp_dir().join(format!("evomu_d6_ship_acct_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    let base_time: u64 = 1_760_200_000;
    let (struct_id, tower_id, mod1_id, mod2_id, ammo_id, svc_mod_id, svc_mod2_id) = (1_045_000_000_301_i64, 555_001_i64, 100_000_005_i64, 400_002_i64, 400_003_i64, 400_004_i64, 400_005_i64);

    let (ship_id, alsc_id, locked_item_id, sp_before, lic_initial, lic_bytes_before, code_payload) = {
        let ctx = open_ctx_at(&path, base_time);
        let (ship_id, alsc_id, locked_item_id, injector_id) = ctx.db.with_mut_snapshot(|snap| {
            *snap = demo;
            let ship_id = snap.characters[&P1].active_ship_id;
            if let Some(ch) = snap.characters.get_mut(&P1) {
                ch.corporation_id = CORP_ID;
                ch.clone_grade = 1;
                ch.skill_points = 1_000_000.0;
            }
            snap.corporations.insert(CORP_ID, CorporationRecord {
                corporation_id: CORP_ID, name: "Durable Corp".into(), ticker: "DURB".into(), ceo_id: P1, alliance_id: None,
                tax_rate: 0.05, master_wallet: IskCents::from_isk(50_000_000), wallet_divisions: Vec::new(), description: String::new(),
                url: String::new(), hq_station_id: Some(JITA_STATION_ID), members: [P1].into_iter().collect(),
                member_roles: HashMap::from([(P1, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STARBASE_CONFIG)]), applications: HashMap::new(),
            });
            snap.skills.entry(P1).or_default().insert(3300, TrainedSkillRecord { type_id: 3300, level: 3, skill_points: 8_000.0 });
            snap.items.insert(mod2_id, make_item(mod2_id, 2889, P1, ship_id, InventoryFlag::HiSlot1, 1, true));
            snap.items.insert(ammo_id, make_item(ammo_id, 183, P1, ship_id, InventoryFlag::Cargo, 500, false));
            snap.structures.insert(struct_id, make_structure(struct_id, 35832, CORP_ID, JITA_SYSTEM_ID, "Durable Astrahus", "ShieldVulnerable", 5_000));
            snap.items.insert(svc_mod_id, make_item(svc_mod_id, 35894, P1, struct_id, InventoryFlag::Hangar, 1, true));
            snap.items.insert(svc_mod2_id, make_item(svc_mod2_id, 35894, P1, struct_id, InventoryFlag::Hangar, 1, true));
            snap.structures.insert(tower_id, make_structure(tower_id, 12235, CORP_ID, JITA_SYSTEM_ID, "Durable Control Tower", "Online", 500));
            snap.items.insert(tower_id, make_item(tower_id, 12235, CORP_ID, JITA_SYSTEM_ID as i64, InventoryFlag::Hangar, 1, true));
            let (alsc_id, locked_item_id, injector_id) = (777_100_i64, 777_101_i64, 777_102_i64);
            snap.items.insert(alsc_id, make_item(alsc_id, ALSC_CONTAINER_TYPE_ID, CORP_ID, JITA_STATION_ID, InventoryFlag::CorpSAG1, 1, true));
            snap.items.insert(locked_item_id, make_item(locked_item_id, TRITANIUM, P1, alsc_id, InventoryFlag::Hangar, 500, false));
            snap.items.insert(injector_id, make_item(injector_id, TYPE_DAILY_ALPHA_INJECTOR, P1, JITA_STATION_ID, InventoryFlag::Hangar, 2, false));
            (ship_id, alsc_id, locked_item_id, injector_id)
        });
        ctx.invalidate_ship_fitting(ship_id);

        let mut sess = login_select(&ctx, 8301, "demo", "demo-secret", P1).await;
        sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STARBASE_CONFIG;

        call(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(ship_id), PyRep::Integer(mod1_id)]).await.expect("SetModuleOnline mod1");
        call(&ctx, &mut sess, "dogmaIM", "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(mod1_id), PyRep::Integer(mod2_id)]).await.expect("LinkWeapons");
        call(&ctx, &mut sess, "dogmaIM", "TakeModuleOffline", &[PyRep::Integer(ship_id), PyRep::Integer(mod2_id)]).await.expect("TakeModuleOffline mod2");
        call(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(svc_mod_id), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::ServiceSlot0 as i64)]).await.expect("Fit structure service 0 via invbroker.Add");
        call(&ctx, &mut sess, "dogmaIM", "TakeModuleOffline", &[PyRep::Integer(struct_id), PyRep::Integer(svc_mod_id)]).await.expect("Take structure service 0 offline via dogmaIM.TakeModuleOffline");
        call(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(svc_mod2_id), PyRep::Integer(struct_id), PyRep::Integer(InventoryFlag::ServiceSlot1 as i64)]).await.expect("Fit structure service 1 via invbroker.Add");
        call(&ctx, &mut sess, "dogmaIM", "SetModuleOnline", &[PyRep::Integer(struct_id), PyRep::Integer(svc_mod2_id)]).await.expect("Set structure service 1 online via dogmaIM.SetModuleOnline");
        call(&ctx, &mut sess, "posMgr", "SetTowerSentrySettings", &[PyRep::Integer(tower_id), PyRep::Real(5.0), PyRep::Real(0.0), PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(false)]).await.expect("SetTowerSentrySettings");

        call(&ctx, &mut sess, "skillMgr2", "SaveSkillQueue", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(3300), PyRep::Integer(4)])])]).await.expect("SaveSkillQueue");
        call(&ctx, &mut sess, "skillMgr2", "AbortTraining", &[]).await.expect("AbortTraining");

        call(&ctx, &mut sess, "invbroker", "SetPassword", &[PyRep::Integer(alsc_id), PyRep::String("vault-pw-99".into())]).await.expect("SetPassword");
        call_kw(&ctx, &mut sess, "invbroker", "ALSCLockItems", &[PyRep::Integer(alsc_id), PyRep::List(vec![PyRep::Integer(locked_item_id)])], &[("password", PyRep::String("vault-pw-99".into()))]).await.expect("ALSCLockItems");
        call(&ctx, &mut sess, "ship", "ConfigureShip", &[PyRep::Integer(ship_id), PyRep::Dict(vec![(PyRep::String("SMB_AllowFleetAccess".into()), PyRep::Bool(false)), (PyRep::String("FleetHangar_AllowCorpAccess".into()), PyRep::Bool(false))])]).await.expect("ConfigureShip");

        call(&ctx, &mut sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(injector_id)]).await.expect("InjectSkillpoints");
        let sp_before = call(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPoints", &[]).await.expect("GetFreeSkillPoints").as_i64().unwrap();

        let err_client = call(&ctx, &mut sess, "seasonManager", "process_client_event", &[PyRep::Integer(1002), PyRep::Integer(5)]).await.expect_err("process_client_event rejects server-driven challenge");
        assert_eq!(err_client.code, "InvalidChallengeEvent"); // rpc-lint: allow error code assertion
        call(&ctx, &mut sess, "seasonManager", "fill_up_and_return_challenges_for_character", &[]).await.expect("fill_up_and_return_challenges");
        ctx.db.with_mut_snapshot(|s| {
            for i in 1..=5 {
                s.killmails.insert(900_000 + i, eve_db::KillmailRecord { kill_id: 900_000 + i, victim_character_id: 0, attacker_character_id: P1, ..Default::default() });
            }
        });
        call(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(1002)]).await.expect("get_challenge");
        call(&ctx, &mut sess, "seasonManager", "claim_challenge_rewards", &[PyRep::Integer(1002)]).await.expect("claim_challenge_rewards");
        for _ in 0..4 { call(&ctx, &mut sess, "ProjectDiscovery", "increase_tutorial_level", &[]).await.expect("increase_tutorial_level"); }
        assert_eq!(call(&ctx, &mut sess, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.expect("give_tutorial_rewards"), PyRep::Bool(true));
        call(&ctx, &mut sess, "ProjectDiscovery", "get_new_task", &[]).await.expect("get_new_task");
        call(&ctx, &mut sess, "ProjectDiscovery", "post_classification", &[PyRep::String("task_1".into()), PyRep::String("consensus".into())]).await.expect("post_classification");

        let lic_initial = send_quasar_req(&ctx, &mut sess, "eve_public.user.license.api.GetRequest", Vec::new()).await.payload.unwrap().value;
        assert_eq!(send_quasar_req(&ctx, &mut sess, "eve.user.legal.agreement.api.AcceptRequest", Vec::new()).await.status_code, STATUS_OK);

        let mut ent_payload = Vec::new(); encode_string_field(3, "omega_veteran_reward", &mut ent_payload);
        let mut ent_outer = Vec::new(); encode_bytes_field(1, &ent_payload, &mut ent_outer);
        // Unprivileged user grant must fail with 403 Forbidden, while GM grant succeeds with 200 OK.
        assert_eq!(send_quasar_req(&ctx, &mut sess, "eve.entitlement.user.api.GrantRequest", ent_outer.clone()).await.status_code, STATUS_FORBIDDEN);
        let saved_role = sess.role;
        sess.role |= eve_machonet::service_roles::ROLE_GM_DEFAULT;
        assert_eq!(send_quasar_req(&ctx, &mut sess, "eve.entitlement.user.api.GrantRequest", ent_outer).await.status_code, STATUS_OK);
        sess.role = saved_role;

        let mut code_inner = Vec::new(); encode_string_field(1, "EVE-OMEGA-30D-PROMO", &mut code_inner);
        let mut code_payload = Vec::new(); encode_bytes_field(1, &code_inner, &mut code_payload);
        assert_eq!(send_quasar_req(&ctx, &mut sess, "eve_public.payment.code.api.RedeemRequest", code_payload.clone()).await.status_code, STATUS_NOT_IMPLEMENTED);
        eve_server::services::quasar_auth_user::with_identity_store(&ctx.db, |st| { st.omega_subscriptions.insert(1, ctx.now_unix() + 60 * 86_400); });
        let lic_bytes_before = send_quasar_req(&ctx, &mut sess, "eve_public.user.license.api.GetRequest", Vec::new()).await.payload.unwrap().value;
        assert_ne!(lic_bytes_before, lic_initial, "Direct Omega license extension must extend license expiry before restart");

        (ship_id, alsc_id, locked_item_id, sp_before, lic_initial, lic_bytes_before, code_payload)
    };

    // === RESTART SERVER FROM SQLITE (+1,800s elapsed) ===
    {
        let ctx = open_ctx_at(&path, base_time + 1_800);
        let router = EsiRouter::new(Arc::clone(&ctx));
        let token = issue_esi_token(&ctx, P1);
        let mut sess = login_select(&ctx, 8302, "demo", "demo-secret", P1).await;
        sess.corp_role = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STARBASE_CONFIG;

        let sp_after = call(&ctx, &mut sess, "skillMgr2", "GetFreeSkillPoints", &[]).await.expect("GetFreeSkillPoints after restart");
        assert_eq!(sp_after.as_i64(), Some(sp_before));
        let esi_sq = esi_get(&router, &format!("/v2/characters/{P1}/skillqueue/"), Some(&token));
        assert!(esi_sq.as_array().unwrap()[0].get("finish_date").is_none(), "Paused skill queue must omit finish_date in ESI");

        let dogma_info = call(&ctx, &mut sess, "dogmaIM", "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)]).await.expect("dogmaIM.GetAllInfo after restart");
        let Some(PyRep::Dict(mod_attrs)) = dogma_info.dict_get("moduleAttributes") else { panic!("Expected moduleAttributes dict") };
        let mod_online = |mid: i64| -> Option<f64> {
            let PyRep::Dict(attrs) = &mod_attrs.iter().find(|(k, _)| k.as_i64() == Some(mid))?.1 else { return None };
            attrs.iter().find(|(k, _)| k.as_i64() == Some(2))?.1.as_f64()
        };
        assert_eq!(mod_online(mod1_id), Some(1.0), "Online module must report isOnline (attr 2) == 1.0 after restart");
        assert_eq!(mod_online(mod2_id), Some(0.0), "Offline module must report isOnline (attr 2) == 0.0 after restart");

        let load_rep = call(&ctx, &mut sess, "dogmaIM", "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod1_id)]), PyRep::List(vec![PyRep::Integer(ammo_id)])]).await.expect("LoadAmmo on linked weapon bank");
        assert_eq!(load_rep.dict_get("totalLoaded").and_then(|v| v.as_i64()), Some(240), "Linked slave weapon must also receive ammo after restart");

        let esi_structs = esi_get(&router, &format!("/v4/corporations/{CORP_ID}/structures/"), Some(&token));
        let st_json = esi_structs.as_array().unwrap().iter().find(|s| s["structure_id"] == struct_id).expect("structure in ESI");
        let svcs = st_json["services"].as_array().expect("services array");
        assert!(svcs.iter().any(|s| s["state"] == "offline") && svcs.iter().any(|s| s["state"] == "online"));

        let sentry = call(&ctx, &mut sess, "posMgr", "GetTowerSentrySettings", &[PyRep::Integer(tower_id)]).await.expect("GetTowerSentrySettings");
        assert!(format!("{sentry:?}").contains("5.0"));
        assert_eq!(esi_get(&router, &format!("/v1/corporations/{CORP_ID}/starbases/{tower_id}/?system_id={JITA_SYSTEM_ID}"), Some(&token))["attack_standing_threshold"], 5.0);

        let cfg_get = call(&ctx, &mut sess, "invbroker", "ALSCConfigGet", &[PyRep::Integer(alsc_id)]).await.expect("ALSCConfigGet");
        assert_eq!(cfg_get.dict_get("hasPassword").and_then(|v| v.as_bool()), Some(true));
        let locked_dict = call(&ctx, &mut sess, "itemLocking", "GetItemsByLocation", &[PyRep::Integer(alsc_id)]).await.expect("GetItemsByLocation");
        assert!(format!("{locked_dict:?}").contains(&locked_item_id.to_string()));
        let log_rep = call_kw(&ctx, &mut sess, "invbroker", "ALSCLogGet", &[PyRep::Integer(alsc_id)], &[("password", PyRep::String("vault-pw-99".into()))]).await.expect("ALSCLogGet");
        assert!(log_rep.dict_get("logCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 2);
        assert!(esi_get(&router, &format!("/v3/corporations/{CORP_ID}/containers/logs/"), Some(&token)).as_array().unwrap().iter().any(|l| l["container_id"] == alsc_id && l["action"] == "lock"));

        let ship_cfg = call(&ctx, &mut sess, "ship", "GetShipConfiguration", &[PyRep::Integer(ship_id)]).await.expect("GetShipConfiguration");
        assert_eq!((ship_cfg.dict_get("SMB_AllowFleetAccess").and_then(|v| v.as_bool()), ship_cfg.dict_get("FleetHangar_AllowCorpAccess").and_then(|v| v.as_bool())), (Some(false), Some(false)));

        let err_cd = call(&ctx, &mut sess, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(777_102)]).await.expect_err("Daily Alpha Injector cooldown must persist across restart");
        assert_eq!(err_cd.code, "DailyAlphaInjectorCooldownActive");

        let ch_rep = call(&ctx, &mut sess, "seasonManager", "get_challenge", &[PyRep::Integer(1002)]).await.expect("get_challenge");
        assert_eq!(ch_rep.dict_get("claimed").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(ch_rep.dict_get("seasonPoints").and_then(|v| v.as_i64()), Some(50));
        let pd_stats = call(&ctx, &mut sess, "ProjectDiscovery", "get_player_statistics", &[]).await.expect("get_player_statistics");
        assert_eq!((pd_stats.dict_get("rank").and_then(|v| v.as_i64()), pd_stats.dict_get("tasks_submitted").and_then(|v| v.as_i64())), (Some(1), Some(1)));
        assert_eq!(pd_stats.dict_get("experience").and_then(|v| v.as_f64()), Some(400.0));
        let pd_ak = call(&ctx, &mut sess, "ProjectDiscovery", "get_player_analysis_kredits", &[]).await.expect("get_player_analysis_kredits");
        assert_eq!(pd_ak.as_i64(), Some(100));

        let lic_after = send_quasar_req(&ctx, &mut sess, "eve_public.user.license.api.GetRequest", Vec::new()).await.payload.unwrap().value;
        assert_eq!(lic_after, lic_bytes_before);
        assert_ne!(lic_after, lic_initial, "Mutated Omega license expiry must persist across SQLite restart");
        let agr_after = send_quasar_req(&ctx, &mut sess, "eve.legal.agreement.api.GetLatestRequest", Vec::new()).await;
        assert!(String::from_utf8_lossy(&agr_after.payload.unwrap().value).contains("eula_v24"));
        let ent_after = send_quasar_req(&ctx, &mut sess, "eve_public.entitlement.user.api.GetRequest", Vec::new()).await;
        assert!(String::from_utf8_lossy(&ent_after.payload.unwrap().value).contains("omega_veteran_reward"));
        assert_eq!(send_quasar_req(&ctx, &mut sess, "eve_public.payment.code.api.RedeemRequest", code_payload).await.status_code, STATUS_NOT_IMPLEMENTED);
    }
    let _ = std::fs::remove_file(&path);
}
