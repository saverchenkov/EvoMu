//! E2E parity integration tests for Mining Market/Goals, Standings, Lookup, and Downtime (Part 2).

#![forbid(unsafe_code)]

use crate::open_feats_and_rpcs_b22::{rpc, select_pilot, PILOT_1, PILOT_2};
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::living_world::sde_ball_and_combat;
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::agents_standings_arcs::apply_faction_standing_with_derived;
use eve_server::sp6_lifecycle::tick_background_cluster;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;

const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;

#[tokio::test]
async fn test_parity_mining_market_and_mining_goals() {
    // parity: feat:mining.mining-market, feat:mining.mining-goals
    eve_testkit::attest(&["feat:mining.mining-market", "feat:mining.mining-goals"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let mut s1 = select_pilot(&ctx, 50_030, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_pilot(&ctx, 50_031, "other", "other-secret", PILOT_2).await;

    // 1. Negative for feat:mining.mining-goals: client-supplied event must not populate mining_ledger
    let _ = rpc(
        &ctx,
        &mut s1,
        "seasonManager",
        "process_client_event",
        &[PyRep::String("totalMined".into()), PyRep::Integer(99_999)],
    )
    .await;
    assert!(
        ctx.db.get_mining_ledger_with_waste(PILOT_1).is_empty(),
        "Client-supplied event must never populate mining_ledger"
    );

    // 2. Positive for feat:mining.mining-goals: real mining in space advances ledger & season challenges
    let ship_id = 850_030i64;
    let miner_id = 850_031i64;
    let ast_id = 850_032i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = JITA_SYS;
            c.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 32880,
                owner_id: PILOT_1,
                location_id: JITA_SYS as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            miner_id,
            InventoryItemRecord {
                item_id: miner_id,
                type_id: 483,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(miner_id, true);
        snap.skills.entry(PILOT_1).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386,
                level: 4,
                skill_points: 45_255.0,
            },
        );
    });
    s1.solar_system_id = Some(JITA_SYS);
    s1.station_id = None;
    s1.ship_id = Some(ship_id);
    ctx.invalidate_ship_fitting(ship_id);

    let jita_handle = ctx.systems.get_or_spawn(JITA_SYS);
    let (ship_ball, ship_combat) = sde_ball_and_combat(&ctx.sde, ship_id, 32880, DVec3::ZERO);
    jita_handle
        .spawn_ball_with_combat(ship_ball, ship_combat)
        .await
        .expect("spawn Venture");
    let (ast_ball, _) = sde_ball_and_combat(&ctx.sde, ast_id, 1230, DVec3::new(1_000.0, 0.0, 0.0));
    jita_handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ast_id,
                belt_id: 0,
                type_id: 1230,
                initial_units: 20_000,
                remaining_units: 20_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            ast_ball,
        )
        .await
        .expect("spawn asteroid");

    let _ = rpc(&ctx, &mut s1, "dogmaIM", "ShipOnlineModules", &[PyRep::Integer(ship_id)])
        .await
        .expect("ShipOnlineModules");
    let _ = rpc(&ctx, &mut s1, "dogmaIM", "AddTarget", &[PyRep::Integer(ast_id)])
        .await
        .expect("AddTarget");
    ctx.advance_secs(5);
    jita_handle.step_manual(5.0).await.expect("step lock");
    let _ = rpc(
        &ctx,
        &mut s1,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(miner_id), PyRep::Integer(ast_id)],
    )
    .await
    .expect("Activate Miner I");
    ctx.advance_secs(60);
    tick_background_cluster(&ctx).await;

    assert!(
        !ctx.db.get_mining_ledger_with_waste(PILOT_1).is_empty(),
        "Real mining must record to mining_ledger"
    );
    let progress = rpc(
        &ctx,
        &mut s1,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[],
    )
    .await
    .expect("get_character_progress_by_character_id");
    assert!(!matches!(progress, PyRep::None));

    // 3. feat:mining.mining-market: verify SDE volumes & trade raw ore (1230), compressed ore (62516), and Tritanium (34)
    let veld_vol = ctx.sde.types.get(&1230).map(|t| t.volume).unwrap_or(0.0);
    let comp_vol = ctx.sde.types.get(&62516).map(|t| t.volume).unwrap_or(0.0);
    let trit_vol = ctx.sde.types.get(&34).map(|t| t.volume).unwrap_or(0.0);
    assert!((veld_vol - 0.1).abs() < 1e-6, "Veldspar volume must be 0.1 m3");
    assert!(comp_vol < veld_vol, "Compressed Veldspar volume must be smaller than raw Veldspar");
    assert!((trit_vol - 0.01).abs() < 1e-6, "Tritanium volume must be 0.01 m3");

    let raw_ore_item = 850_033i64;
    let comp_ore_item = 850_034i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&PILOT_1) {
            c.station_id = Some(JITA_STATION);
        }
        if let Some(c2) = snap.characters.get_mut(&PILOT_2) {
            c2.station_id = Some(JITA_STATION);
            c2.balance = IskCents::from_isk(10_000_000);
        }
        snap.items.insert(
            raw_ore_item,
            InventoryItemRecord {
                item_id: raw_ore_item,
                type_id: 1230,
                owner_id: PILOT_1,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            comp_ore_item,
            InventoryItemRecord {
                item_id: comp_ore_item,
                type_id: 62516,
                owner_id: PILOT_1,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 200,
                singleton: false,
                ..Default::default()
            },
        );
    });
    s1.station_id = Some(JITA_STATION);
    s2.station_id = Some(JITA_STATION);

    let _ = rpc(
        &ctx,
        &mut s1,
        "reprocessingSvc",
        "Reprocess",
        &[
            PyRep::List(vec![PyRep::Integer(raw_ore_item)]),
            PyRep::Integer(JITA_STATION),
            PyRep::Integer(InventoryFlag::Hangar as i64),
            PyRep::Bool(false),
            PyRep::List(vec![]),
        ],
    )
    .await
    .expect("Reprocess Veldspar");

    let trit_item = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .find(|i| i.owner_id == PILOT_1 && i.location_id == JITA_STATION && i.type_id == 34 && i.quantity > 0)
            .map(|i| i.item_id)
            .expect("Tritanium produced")
    });

    let _ = rpc(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![
                PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION)),
                    ("typeID", PyRep::Integer(62516)),
                    ("itemID", PyRep::Integer(comp_ore_item)),
                    ("price", PyRep::Real(20.0)),
                    ("quantity", PyRep::Integer(50)),
                ]),
                PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION)),
                    ("typeID", PyRep::Integer(34)),
                    ("itemID", PyRep::Integer(trit_item)),
                    ("price", PyRep::Real(5.0)),
                    ("quantity", PyRep::Integer(100)),
                ]),
            ]),
            PyRep::Bool(false),
            PyRep::Integer(90),
        ],
    )
    .await
    .expect("PlaceMultiSellOrder");

    for tid in [62516i64, 34i64] {
        let orders = rpc(
            &ctx,
            &mut s2,
            "marketProxy",
            "GetOrders",
            &[PyRep::Integer(tid)],
        )
        .await
        .expect("marketProxy.GetOrders");
        assert!(
            !matches!(orders, PyRep::None),
            "Market orders for type {tid} must be listed"
        );
    }
}

#[tokio::test]
async fn test_parity_standings_npc_npc_standings() {
    // parity: feat:standings.npc-npc-standings
    eve_testkit::attest(&["feat:standings.npc-npc-standings"]);

    let ctx = ServerContext::new();
    let mut unauth = SessionState::new(99_999);
    assert!(
        rpc(&ctx, &mut unauth, "standingMgr", "GetNPCNPCStandings", &[])
            .await
            .is_err(),
        "Unauthenticated GetNPCNPCStandings must fail"
    );

    let mut sess = select_pilot(&ctx, 50_040, "demo", "demo-secret", PILOT_1).await;

    let rep = rpc(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect("GetNPCNPCStandings");
    let PyRep::Rowset { rows, .. } = rep else {
        panic!("Expected Rowset from GetNPCNPCStandings");
    };
    assert!(!rows.is_empty(), "GetNPCNPCStandings must return inter-faction pairs");

    let mut found_amarr = false;
    let mut found_gallente = false;
    for r in &rows {
        if let (Some(from_id), Some(to_id), Some(val)) = (r[0].as_i64(), r[1].as_i64(), r[2].as_f64()) {
            if from_id == 500_001 && to_id == 500_003 {
                assert!((val - 5.0).abs() < 1e-6);
                found_amarr = true;
            }
            if from_id == 500_001 && to_id == 500_004 {
                assert!((val - (-5.0)).abs() < 1e-6);
                found_gallente = true;
            }
        }
    }
    assert!(found_amarr && found_gallente, "Must include Caldari-Amarr and Caldari-Gallente relationships");

    let mut changes = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        apply_faction_standing_with_derived(&ctx, snap, PILOT_1, 500_001, 20.0, true, &mut changes);
    });

    let (amarr_eff, gallente_eff) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&PILOT_1).unwrap();
        (
            ch.standings.get(&500_003).copied().unwrap_or(0.0),
            ch.standings.get(&500_004).copied().unwrap_or(0.0),
        )
    });
    assert!(amarr_eff > 0.0, "Amarr standing must derive positively from Caldari gain");
    assert!(gallente_eff < 0.0, "Gallente standing must derive negatively from Caldari gain");
}

#[tokio::test]
async fn test_parity_ui_lookup_all_methods() {
    // parity: feat:ui.lookup
    eve_testkit::attest(&["feat:ui.lookup"]);

    let ctx = ServerContext::new();
    let corp_id = 98_050_001i64;
    let mut sess = select_pilot(&ctx, 50_050, "demo", "demo-secret", PILOT_1).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Astraeus Mining Guild".to_string(),
                ticker: "AMG".to_string(),
                ceo_id: PILOT_1,
                ..Default::default()
            },
        );
    });

    let kv_get = |rep: &PyRep, key: &str| -> Option<PyRep> {
        if let PyRep::KeyVal(entries) = rep {
            entries.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone())
        } else {
            None
        }
    };

    let chars = rpc(&ctx, &mut sess, "lookupSvc", "LookupCharacters", &[PyRep::String("".into()), PyRep::Bool(false)])
        .await
        .expect("LookupCharacters");
    let PyRep::List(char_list) = chars else { panic!("Expected List") };
    assert!(char_list.iter().any(|item| kv_get(item, "characterID").and_then(|v| v.as_i64()) == Some(PILOT_1)));

    let corps = rpc(&ctx, &mut sess, "lookupSvc", "LookupCorporations", &[PyRep::String("Astraeus".into()), PyRep::Bool(false)])
        .await
        .expect("LookupCorporations");
    let PyRep::List(corp_list) = corps else { panic!("Expected List") };
    assert_eq!(corp_list.len(), 1);
    assert_eq!(kv_get(&corp_list[0], "corporationID").and_then(|v| v.as_i64()), Some(corp_id));
    assert_eq!(kv_get(&corp_list[0], "corporationName").as_ref().and_then(|v| v.as_str()), Some("Astraeus Mining Guild"));

    let owners = rpc(&ctx, &mut sess, "lookupSvc", "LookupOwners", &[PyRep::String("Astraeus".into()), PyRep::Bool(false)])
        .await
        .expect("LookupOwners");
    let PyRep::List(owner_list) = owners else { panic!("Expected List") };
    assert!(owner_list.iter().any(|item| kv_get(item, "ownerID").and_then(|v| v.as_i64()) == Some(corp_id)));

    let locs = rpc(&ctx, &mut sess, "lookupSvc", "LookupLocationsByGroup", &[PyRep::Integer(5), PyRep::String("Jita".into())])
        .await
        .expect("LookupLocationsByGroup");
    let PyRep::List(loc_list) = locs else { panic!("Expected List") };
    assert!(loc_list.iter().any(|item| kv_get(item, "itemID").and_then(|v| v.as_i64()) == Some(JITA_SYS as i64)));

    let warable = rpc(&ctx, &mut sess, "lookupSvc", "LookupWarableCorporationsOrAlliances", &[PyRep::String("Astraeus".into())])
        .await
        .expect("LookupWarableCorporationsOrAlliances");
    let PyRep::List(warable_list) = warable else { panic!("Expected List") };
    assert!(warable_list.is_empty(), "Corp without structures must not be war-eligible");
}

#[tokio::test]
async fn test_parity_serverops_daily_downtime() {
    // parity: feat:serverops.downtime
    eve_testkit::attest(&["feat:serverops.downtime"]);

    let ctx = ServerContext::new();
    let base_1055 = 1_767_264_900u64;
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        base_1055,
    )));

    let _sess = select_pilot(&ctx, 50_060, "demo", "demo-secret", PILOT_1).await;
    assert_eq!(ctx.db.online_session_count(), 1);

    tick_background_cluster(&ctx).await;
    assert!(!ctx.is_shutting_down());
    assert_eq!(ctx.db.online_session_count(), 1);

    ctx.advance_secs(300);
    tick_background_cluster(&ctx).await;
    assert!(ctx.is_shutting_down(), "Cluster must be in downtime at 11:00 UTC");
    assert_eq!(ctx.db.online_session_count(), 0, "Non-GM sessions must be disconnected at 11:00 UTC");

    ctx.advance_secs(900);
    tick_background_cluster(&ctx).await;
    assert!(!ctx.is_shutting_down(), "Cluster must complete restart by 11:15 UTC");
}
