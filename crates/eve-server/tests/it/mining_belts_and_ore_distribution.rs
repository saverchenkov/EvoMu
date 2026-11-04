//! Asteroid belts, ore distribution, mining market, and mining goals parity suite.
//!
//! Parity rows:
//! - `feat:mining.belts-as-balls`
//! - `feat:mining.belt-ore-distribution`
//! - `feat:mining.mining-market`
//! - `feat:mining.mining-goals`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::system_actor::populate_asteroid_belt_entities;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const PERIMETER_SYS: u32 = 30_000_144;
const AMAMAKE_SYS: u32 = 30_002_537;
const LZ6SU_SYS: u32 = 30_000_208;

async fn select_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

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

#[tokio::test]
async fn test_belts_as_balls_depletion_and_respawn() {
    // parity: feat:mining.belts-as-balls
    eve_testkit::attest(&["feat:mining.belts-as-balls"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // 1. Spawning asteroid belts as balls in space with ore type/quantity per security band
    let sys_handle = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let belt_asteroids = sys_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("populate belt asteroids");
    assert!(!belt_asteroids.is_empty(), "Perimeter must spawn belt asteroids");

    for ast in &belt_asteroids {
        let (_tick, ball) = sys_handle
            .get_ball_snapshot(ast.asteroid_id)
            .await
            .unwrap()
            .expect("Asteroid must exist as a Ball in ballpark");
        assert_eq!(ball.id as i64, ast.asteroid_id);
        assert!(ball.radius >= 60.0 && ball.radius <= 2_500.0);

        let bp = sys_handle.ballpark.read().unwrap();
        let meta = bp.ball_slim_meta.get(&ast.asteroid_id).expect("SlimItemMeta");
        assert_eq!(meta.category_id, 25, "Asteroid category must be 25");
        let sde_type = ctx.sde.types.get(&ast.type_id).expect("SDE type");
        assert_eq!(meta.group_id, sde_type.group_id);

        assert_eq!(ast.remaining_units, ast.initial_units);
        assert!(!ast.depleted);
        assert!((ast.unit_volume - sde_type.volume).abs() < 1e-4);

        let db_rec = ctx
            .db
            .with_snapshot(|s| s.asteroids.get(&ast.asteroid_id).cloned())
            .expect("DB asteroid");
        assert_eq!(db_rec.quantity, ast.initial_units as i64);
        assert_eq!(db_rec.initial_quantity, ast.initial_units as i64);
    }

    // 2. Mining depletion: decrementing quantity on cycle, and ball removal when depleted
    let target_ast = &belt_asteroids[0];
    let target_id = target_ast.asteroid_id;
    let target_belt = target_ast.belt_id;
    let init_qty = target_ast.initial_units;

    // Partial extraction
    let partial_units = init_qty / 3;
    let harvest_1 = sys_handle
        .mine_asteroid(target_id, partial_units, 0.0)
        .await
        .expect("partial mine");
    assert_eq!(harvest_1.mined_units, partial_units);
    assert!(!harvest_1.depleted);
    assert_eq!(harvest_1.remaining_units, init_qty - partial_units);
    assert!(sys_handle.get_ball_snapshot(target_id).await.unwrap().is_some());

    // Complete depletion of target asteroid
    let harvest_2 = sys_handle
        .mine_asteroid(target_id, harvest_1.remaining_units, 0.0)
        .await
        .expect("depleting mine");
    assert_eq!(harvest_2.remaining_units, 0);
    assert!(harvest_2.depleted);
    assert!(sys_handle.get_ball_snapshot(target_id).await.unwrap().is_none());

    let db_depleted_qty = ctx
        .db
        .with_snapshot(|s| s.asteroids.get(&target_id).unwrap().quantity);
    assert_eq!(db_depleted_qty, 0, "DB quantity must be 0 upon depletion");

    // Deplete remaining belt asteroids to trigger belt downtime respawn
    let other_ids: Vec<i64> = belt_asteroids
        .iter()
        .filter(|a| a.belt_id == target_belt && a.asteroid_id != target_id)
        .map(|a| a.asteroid_id)
        .collect();
    for oid in other_ids {
        if let Ok(Some(st)) = sys_handle.get_asteroid_state(oid).await {
            if st.remaining_units > 0 {
                let res = sys_handle.mine_asteroid(oid, st.remaining_units, 0.0).await.unwrap();
                assert!(res.depleted);
                assert!(sys_handle.get_ball_snapshot(oid).await.unwrap().is_none());
            }
        }
    }

    // 3. Daily downtime reset / respawn: repopulates depleted belt asteroids back to full quantity
    let restored_count = sys_handle.respawn_depleted_belts_on_downtime();
    assert!(restored_count > 0, "Depleted belt asteroids must be respawned");

    let (_restored_tick, restored_ball) = sys_handle
        .get_ball_snapshot(target_id)
        .await
        .unwrap()
        .expect("Respawned ball in ballpark");
    assert_eq!(restored_ball.id as i64, target_id);
    assert!(restored_ball.radius >= 60.0);

    let restored_state = sys_handle
        .get_asteroid_state(target_id)
        .await
        .unwrap()
        .expect("Restored asteroid state");
    assert_eq!(restored_state.remaining_units, init_qty);
    assert!(!restored_state.depleted);

    let db_restored_qty = ctx
        .db
        .with_snapshot(|s| s.asteroids.get(&target_id).unwrap().quantity);
    assert_eq!(db_restored_qty, init_qty as i64);
}

#[tokio::test]
async fn test_belt_ore_distribution_across_security_bands() {
    // parity: feat:mining.belt-ore-distribution
    eve_testkit::attest(&["feat:mining.belt-ore-distribution"]);

    let ctx = ServerContext::new();
    let ores_for = |sec: f64, reg: u32| -> Vec<u32> {
        ctx.sde.belt_ores_for_security_and_region(sec, reg).into_iter().map(|t| t.type_id).collect()
    };

    // 1. High-sec belts: contain Veldspar and Scordite (volume <= 0.35 m3)
    let hs_ores = ctx.sde.belt_ores_for_security_and_region(0.9, 10_000_002);
    let hs_ids: Vec<u32> = hs_ores.iter().map(|t| t.type_id).collect();
    assert_eq!(hs_ids, vec![1230, 1228], "0.9 high-sec must contain Veldspar and Scordite");
    assert!(hs_ores.iter().all(|t| t.volume <= 0.35));

    // Regional empire distributions in 0.7 high-sec:
    let amarr_ids = ores_for(0.7, 10_000_043);
    assert!(amarr_ids.contains(&1224) && !amarr_ids.contains(&18), "0.7 Amarr has Pyroxeres, no Plagioclase");

    let caldari_ids = ores_for(0.7, 10_000_002);
    assert!(caldari_ids.contains(&18) && !caldari_ids.contains(&1224), "0.7 Caldari has Plagioclase, no Pyroxeres");

    let gallente_ids = ores_for(0.7, 10_000_032);
    assert!(gallente_ids.contains(&18));

    let minmatar_ids = ores_for(0.7, 10_000_042);
    assert!(minmatar_ids.contains(&1224) && minmatar_ids.contains(&18), "0.7 Minmatar has both Pyroxeres & Plagioclase");

    // 2. Low-sec belts: Jaspet, Hemorphite, Hedbergite, Kernite, Omber (no Veldspar or Scordite)
    let ls_ids = ores_for(0.3, 10_000_030);
    assert!(ls_ids.contains(&1226) && ls_ids.contains(&1231) && ls_ids.contains(&21));
    assert!(ls_ids.contains(&20) && ls_ids.contains(&1227));
    assert!(!ls_ids.contains(&1230) && !ls_ids.contains(&1228));

    let mut low_bp = eve_destiny::Ballpark::new(AMAMAKE_SYS);
    let low_states = populate_asteroid_belt_entities(&mut low_bp, AMAMAKE_SYS, &ctx.sde);
    assert!(!low_states.is_empty(), "Amamake must generate belt asteroids");
    assert!(low_states.iter().all(|s| s.type_id != 1230 && s.type_id != 1228));
    assert!(low_states.iter().any(|s| matches!(s.type_id, 20 | 21 | 1226 | 1227 | 1231)));

    // 3. Null-sec belts: Bistot, Arkonor, Spodumain, Dark Ochre, Gneiss, Crokite, Mercoxit
    let deep_null = ctx.sde.belt_ores_for_security_and_region(-0.8, 10_000_050);
    let deep_ids: Vec<u32> = deep_null.iter().map(|t| t.type_id).collect();
    for null_ore in [1223, 22, 19, 1229, 1232, 1225, 11396] {
        assert!(deep_ids.contains(&null_ore), "Deep null-sec must contain {null_ore}");
    }
    assert!(!deep_ids.contains(&1230) && !deep_ids.contains(&1228));
    assert!(deep_null.iter().all(|t| t.volume > 3.0), "Null-sec ores have large volume");

    let high_ids = ores_for(-0.2, 10_000_014);
    assert!(high_ids.contains(&19) && !high_ids.contains(&1223) && !high_ids.contains(&22));

    let mut null_bp = eve_destiny::Ballpark::new(LZ6SU_SYS);
    let null_states = populate_asteroid_belt_entities(&mut null_bp, LZ6SU_SYS, &ctx.sde);
    assert!(!null_states.is_empty(), "LZ-6SU must generate belt asteroids");
    assert!(null_states.iter().all(|s| s.type_id != 1230 && s.type_id != 1228));

    // 4. Pochven: Bezdnacine, Rakovene, Talassonite
    let pochven = ctx.sde.belt_ores_for_security_and_region(-0.5, 10_000_070);
    assert!(!pochven.is_empty());
    assert!(pochven.iter().all(|t| matches!(t.group_id, 4029 | 4030 | 4031)));
}

#[tokio::test]
async fn test_mining_market_ore_mineral_compressed() {
    // parity: feat:mining.mining-market
    eve_testkit::attest(&["feat:mining.mining-market"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // 1. SDE volume and market group verification
    for (tid, expected_vol) in [
        (1230, 0.1), (1228, 0.15), (1224, 0.30), (18, 0.35),
        (19, 16.0), (1223, 16.0), (22, 16.0),
    ] {
        let t = ctx.sde.types.get(&tid).expect("Raw ore");
        assert!((t.volume - expected_vol).abs() < 1e-4);
        assert!(t.market_group_id.is_some());
    }

    for tid in [34u32, 35, 36, 37, 38, 39, 40, 11399] {
        let m = ctx.sde.types.get(&tid).expect("Mineral");
        assert!((m.volume - 0.01).abs() < 1e-4);
        assert!(m.market_group_id.is_some());
    }

    assert!(ctx.sde.compressible_types.contains_key(&1230));
    let comp_veldspar = ctx
        .sde
        .types
        .get(&62516)
        .or_else(|| ctx.sde.types.get(&28432))
        .expect("Compressed Veldspar");
    assert!(comp_veldspar.volume < ctx.sde.types[&1230].volume);
    assert!(comp_veldspar.market_group_id.is_some());

    let comp_tid = comp_veldspar.type_id;

    // 2. Live market trading: Seller (PILOT_1) and Buyer (PILOT_2) docked at Jita 4-4
    let mut s1 = select_pilot(&ctx, 60_001, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = select_pilot(&ctx, 60_002, "other", "other-secret", PILOT_2).await;

    let veld_item_id = 890_001_i64;
    let trit_item_id = 890_002_i64;
    let comp_item_id = 890_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT_1) {
            c1.station_id = Some(JITA_STATION);
            c1.solar_system_id = JITA_SYS;
            c1.balance = IskCents::from_isk(1_000_000);
        }
        if let Some(c2) = snap.characters.get_mut(&PILOT_2) {
            c2.station_id = Some(JITA_STATION);
            c2.solar_system_id = JITA_SYS;
            c2.balance = IskCents::from_isk(50_000_000);
        }
        for (id, tid, qty) in [
            (veld_item_id, 1230, 5_000),
            (trit_item_id, 34, 20_000),
            (comp_item_id, comp_tid, 200),
        ] {
            snap.items.insert(id, InventoryItemRecord {
                item_id: id,
                type_id: tid,
                owner_id: PILOT_1,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: qty,
                ..Default::default()
            });
        }
    });
    s1.station_id = Some(JITA_STATION);
    s2.station_id = Some(JITA_STATION);

    // Seller places multi sell orders for ore, mineral, and compressed ore
    let sell_orders = vec![
        PyRep::keyval([("stationID", PyRep::Integer(JITA_STATION)), ("typeID", PyRep::Integer(1230)), ("itemID", PyRep::Integer(veld_item_id)), ("price", PyRep::Real(10.0)), ("quantity", PyRep::Integer(1_000))]),
        PyRep::keyval([("stationID", PyRep::Integer(JITA_STATION)), ("typeID", PyRep::Integer(34)), ("itemID", PyRep::Integer(trit_item_id)), ("price", PyRep::Real(5.0)), ("quantity", PyRep::Integer(2_000))]),
        PyRep::keyval([("stationID", PyRep::Integer(JITA_STATION)), ("typeID", PyRep::Integer(comp_tid as i64)), ("itemID", PyRep::Integer(comp_item_id)), ("price", PyRep::Real(50.0)), ("quantity", PyRep::Integer(50))]),
    ];

    rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(sell_orders), PyRep::Bool(false), PyRep::Integer(90)])
        .await
        .expect("PlaceMultiSellOrder");

    // Buyer queries orders on marketProxy
    for tid in [1230u32, 34, comp_tid] {
        let orders_rep = rpc(&ctx, &mut s2, "marketProxy", "GetOrders", &[PyRep::Integer(tid as i64)])
            .await
            .expect("GetOrders");
        assert!(!matches!(orders_rep, PyRep::None), "Type {tid} must be listed on market");
    }

    // Buyer executes multi purchase across ore, mineral, and compressed ore
    let buyer_balance_before = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_2).unwrap().balance.to_f64_isk());
    let seller_balance_before = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.to_f64_isk());

    let buy_entries = vec![
        PyRep::Tuple(vec![PyRep::Integer(1230), PyRep::Integer(500), PyRep::Real(10.0)]),
        PyRep::Tuple(vec![PyRep::Integer(34), PyRep::Integer(1_000), PyRep::Real(5.0)]),
        PyRep::Tuple(vec![PyRep::Integer(comp_tid as i64), PyRep::Integer(20), PyRep::Real(50.0)]),
    ];

    let buy_res = rpc(&ctx, &mut s2, "marketProxy", "BuyMultipleItems", &[PyRep::List(buy_entries)])
        .await
        .expect("BuyMultipleItems");

    let PyRep::KeyVal(buy_kv) = buy_res else { panic!("Expected KeyVal") };
    assert_eq!(buy_kv.get("purchasedCount"), Some(&PyRep::Integer(3)));
    assert_eq!(buy_kv.get("totalQuantity"), Some(&PyRep::Integer(1_520)));

    let expected_cost = 500.0 * 10.0 + 1_000.0 * 5.0 + 20.0 * 50.0;
    assert_eq!(buy_kv.get("totalCostISK"), Some(&PyRep::Real(expected_cost)));

    let buyer_balance_after = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_2).unwrap().balance.to_f64_isk());
    let seller_balance_after = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_1).unwrap().balance.to_f64_isk());

    assert!((buyer_balance_before - buyer_balance_after - expected_cost).abs() < 1e-2);
    assert!(seller_balance_after > seller_balance_before);

    // Verify buyer received items in hangar
    ctx.db.with_snapshot(|snap| {
        for (tid, expected_qty) in [(1230, 500), (34, 1_000), (comp_tid, 20)] {
            let actual: i32 = snap
                .items
                .values()
                .filter(|it| it.owner_id == PILOT_2 && it.location_id == JITA_STATION && it.type_id == tid)
                .map(|it| it.quantity)
                .sum();
            assert_eq!(actual, expected_qty, "Buyer hangar must have {expected_qty} of type {tid}");
        }
    });
}

#[tokio::test]
async fn test_mining_goals_from_ledger_events() {
    // parity: feat:mining.mining-goals
    eve_testkit::attest(&["feat:mining.mining-goals"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut sess = select_pilot(&ctx, 60_003, "demo", "demo-secret", PILOT_1).await;

    // 1. Negative: Client-supplied event must never populate mining_ledger or advance mining progress
    let _ = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "process_client_event",
        &[PyRep::String("totalMined".into()), PyRep::Integer(99_999)],
    )
    .await;

    assert!(
        ctx.db.get_mining_ledger_with_waste(PILOT_1).is_empty(),
        "Client event must never insert records into mining_ledger"
    );

    let prog_rep = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("get_character_progress_by_character_id");

    let PyRep::KeyVal(prog_kv) = prog_rep else { panic!("Expected KeyVal") };
    assert_eq!(
        prog_kv.get("currentProgress"),
        Some(&PyRep::Integer(0)),
        "Mining goal progress must remain 0 when ledger is empty"
    );

    // Negative claim: attempt to claim career goal without ledger progress
    let claim_err = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_goal_reward",
        &[PyRep::Integer(2)],
    )
    .await
    .expect_err("Claiming unearned career goal must fail");
    assert_eq!(claim_err.code, "GoalProgressIncomplete"); // rpc-lint: allow goal reward error assertion

    // 2. Positive: Genuine mining events in space record to mining_ledger and drive goal progress
    let now = ctx.now_unix();
    let _ = ctx.db.record_mining_ledger_entry_with_waste(PILOT_1, PERIMETER_SYS, 1230, 25_000, 500, now);

    let ledger = ctx.db.get_mining_ledger_with_waste(PILOT_1);
    assert_eq!(ledger.len(), 1);
    assert_eq!(ledger[0].0.quantity, 25_000);
    assert_eq!(ledger[0].1, 500);

    let prog_after = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "get_character_progress_by_character_id",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("get_character_progress_by_character_id after ledger event");

    let PyRep::KeyVal(after_kv) = prog_after else { panic!("Expected KeyVal") };
    assert_eq!(
        after_kv.get("currentProgress"),
        Some(&PyRep::Integer(25_000)),
        "Mining goal progress must match genuine ledger quantity"
    );
    assert_eq!(
        after_kv.get("verifiedFromLedger"),
        Some(&PyRep::Bool(true)),
        "Mining goal must be verified from ledger"
    );

    // Record additional mining harvest to reach career milestone target (100,000 units)
    let _ = ctx.db.record_mining_ledger_entry_with_waste(PILOT_1, PERIMETER_SYS, 1230, 75_000, 0, now + 10);

    let total_ledger: i64 = ctx
        .db
        .get_mining_ledger_with_waste(PILOT_1)
        .iter()
        .map(|r| r.0.quantity)
        .sum();
    assert_eq!(total_ledger, 100_000);

    // Successfully claim career mining milestone
    let claim_res = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_goal_reward",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("claim_goal_reward must succeed with sufficient ledger volume");

    let PyRep::KeyVal(claim_kv) = claim_res else { panic!("Expected KeyVal") };
    assert_eq!(
        claim_kv.get("spReward"),
        Some(&PyRep::Real(50_000.0)),
        "Career mining milestone must award 50,000 SP"
    );

    // Idempotency: duplicate claim must be rejected
    let dup_claim_err = rpc(
        &ctx,
        &mut sess,
        "seasonManager",
        "claim_goal_reward",
        &[PyRep::Integer(2)],
    )
    .await
    .expect_err("Duplicate claim must be rejected");
    assert_eq!(dup_claim_err.code, "GoalAlreadyClaimed"); // rpc-lint: allow goal reward error assertion
}
