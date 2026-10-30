//! Phase 5M (`MN-01`, `MN-02`, `MN-09`, `MN-17`, `MN-18`) and `OF-28` closure integration tests.

use std::collections::HashMap;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, Ballpark, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::living_world::{LivingWorldConvoy, LivingWorldDirector, NpcArchetype};
use eve_server::router::ServerContext;
use eve_server::services::mining_sites::ICE_BELT_SITES;
use eve_server::system_actor::{AsteroidState, populate_asteroid_belt_entities};
use glam::DVec3;

use crate::common::login_pilot;

// ============================================================================
// 1. MN-01: Mining static data in the SDE importer
// ============================================================================
#[tokio::test]
async fn test_mn01_sde_mining_static_data_and_distributions() {
    let ctx = ServerContext::new();
    let sde = &ctx.sde;

    // Veldspar (1230) -> Tritanium (34) only
    let veldspar_mats = sde.type_materials_for(1230);
    assert_eq!(veldspar_mats.len(), 1);
    assert_eq!(veldspar_mats[0].material_type_id, 34);

    // Scordite (1228) -> Tritanium (34) + Pyerite (35)
    let scordite_mats: Vec<u32> = sde.type_materials_for(1228).iter().map(|m| m.material_type_id).collect();
    assert_eq!(scordite_mats, vec![34, 35]);

    // Ice type (Clear Icicle 16262) yields its isotope and fuel materials (16272, 16273, 16274, 16275)
    let ice_mats: Vec<u32> = sde.type_materials_for(16262).iter().map(|m| m.material_type_id).collect();
    for expected in [16272u32, 16273, 16274, 16275] {
        assert!(ice_mats.contains(&expected), "Clear Icicle must yield {expected}");
    }

    // Ore variant yields differ from base ore (Concentrated Veldspar 17470 vs Veldspar 1230)
    let conc_veld_mats = sde.type_materials_for(17470);
    assert_eq!(conc_veld_mats.len(), 1);
    assert!(conc_veld_mats[0].quantity > veldspar_mats[0].quantity);

    // Moon ores (R4-R64: groups 1884, 1920..=1923), Gas (711), and Prismaticite (randomized_type_materials)
    assert!(sde.types.values().any(|t| (1920..=1923).contains(&t.group_id) && t.published));
    assert!(sde.types.values().any(|t| t.group_id == 711 && t.published));
    assert!(!sde.randomized_type_materials.is_empty(), "Prismaticite randomized materials must be loaded");

    // Compressed counterparts & portionSize (V24 Batch Compressed Veldspar = 62516)
    assert_eq!(sde.compressible_types.get(&1230).copied(), Some(62516));
    assert_eq!(sde.get_type(1230).unwrap().portion_size, 100);

    // Regional ice belts & belt celestials with positions (Perimeter 30000144)
    assert!(!ICE_BELT_SITES.is_empty());
    let perim_belts: Vec<_> = sde.celestials_in_system(30_000_144).into_iter().filter(|c| c.group_id == 9).collect();
    assert!(!perim_belts.is_empty());
    assert!(perim_belts.iter().all(|b| b.position.length() > 0.0));

    // Mining modules (Miner I 483), crystals, drones (Mining Drone I 10246), MLU I (22542), Venture (32880)
    let miner_i = sde.get_type(483).unwrap();
    assert!(miner_i.default_attributes.contains_key(&77)); // miningAmount
    assert!(miner_i.default_attributes.contains_key(&73)); // duration
    assert!(miner_i.default_attributes.contains_key(&54)); // maxRange
    let venture = sde.get_type(32880).unwrap();
    assert!(venture.default_attributes.contains_key(&1556)); // specialOreHoldCapacity
}

// ============================================================================
// 2. MN-02: Asteroid belts as balls & security/region ore distribution
// ============================================================================
#[tokio::test]
async fn test_mn02_asteroid_belts_as_balls_highsec_and_nullsec() {
    // parity: feat:mining.belts-as-balls
    eve_testkit::attest(&["feat:mining.belts-as-balls"]);
    let ctx = ServerContext::new();
    let mut sess = SessionState::new(9102);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let _ = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .unwrap();

    // High-sec system (Perimeter 30000144, highsec 0.90): belts contain ONLY high-sec ores (volume <= 0.35 m3)
    let mut highsec_bp = Ballpark::new(30_000_144);
    let highsec_states = populate_asteroid_belt_entities(&mut highsec_bp, 30_000_144, &ctx.sde);
    assert!(!highsec_states.is_empty());
    for st in &highsec_states {
        let ore = ctx.sde.get_type(st.type_id).unwrap();
        assert!(ore.volume <= 0.35, "High-sec belt must contain only high-sec ores, found {}", ore.name);
        let ball = highsec_bp.balls.get(&st.asteroid_id).expect("Asteroid ball in ballpark");
        assert!(ball.radius >= 60.0);
    }

    // 0.0 / Null-sec system (30000208 LZ-6SU): has null-sec ores (volume > 3.0 m3)
    let mut null_bp = Ballpark::new(30_000_208);
    let null_states = populate_asteroid_belt_entities(&mut null_bp, 30_000_208, &ctx.sde);
    assert!(!null_states.is_empty());
    assert!(
        null_states.iter().all(|st| ctx.sde.get_type(st.type_id).unwrap().volume > 3.0),
        "0.0 system must spawn null-sec ores"
    );

    // Undock in Perimeter and warp to an asteroid belt -> OnWarpStarted fires (OnWarpToAsteroidBelt is client_local)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.solar_system_id = 30_000_144;
            ch.station_id = Some(60_003_766);
            if let Some(ship) = snap.items.get_mut(&ch.active_ship_id) {
                ship.location_id = 60_003_766;
            }
        }
    });
    sess.solar_system_id = Some(30_000_144);
    sess.station_id = Some(60_003_766);
    let _ = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap();
    let perim_belt_id = ctx.sde.celestials_in_system(30_000_144).into_iter().find(|c| c.group_id == 9).unwrap().celestial_id;
    let _ = ctx.drain_notifications_for_char(90_000_001);
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        None,
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(perim_belt_id), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("CmdWarpToStuff to Perimeter belt");
    let notifs = ctx.drain_notifications_for_char(90_000_001);
    assert!(notifs.iter().any(|n| n.event == "OnWarpStarted"));
    assert!(!notifs.iter().any(|n| n.event == "OnWarpToAsteroidBelt"));
}

// ============================================================================
// 3. OF-28: OnOreMined notification & characterMiningLedger RPC
// ============================================================================
#[tokio::test]
async fn test_of28_mining_laser_fired_local_and_canonical_ledger_rpc() {
    // parity: notify:OnOreMined, rpc:characterMiningLedger.GetCharacterLogs
    eve_testkit::attest(&["notify:OnOreMined", "rpc:characterMiningLedger.GetCharacterLogs"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let mut sess = SessionState::new(9109);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let _ = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();

    // Non-catalog `charMiningLedger` service must not be dispatched
    let legacy_err = ctx
        .dispatch_rpc(&mut sess, "charMiningLedger", None, "GetMiningLedger", &[], &kw, None)
        .await;
    assert!(legacy_err.is_err(), "Non-catalog charMiningLedger must be rejected");

    let laser_id = ctx.db.with_mut_snapshot(|snap| {
        let active_ship = snap.characters.get(&char_id).unwrap().active_ship_id;
        snap.items.get_mut(&active_ship).unwrap().type_id = 32880; // Venture
        snap.items.retain(|_, i| i.location_id != active_ship);
        snap.skills.entry(char_id).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386, // Mining
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 483, // Miner I
                owner_id: char_id,
                location_id: active_ship,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        id
    });

    let _ = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap();
    let ship_id = sess.ship_id.unwrap();
    ctx.invalidate_ship_fitting(ship_id);
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    let ast_id = 750_999_001i64;
    let ast_pos = ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0);
    ctx.db.spawn_asteroid(eve_db::AsteroidRecord {
        asteroid_id: ast_id,
        solar_system_id: 30_000_142,
        belt_id: 40_000_001,
        type_id: 1230,
        quantity: 10_000,
        initial_quantity: 10_000,
        position: (ast_pos.x, ast_pos.y, ast_pos.z),
        radius: 100.0,
    });
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: ast_id,
            belt_id: 40_000_001,
            type_id: 1230,
            initial_units: 10_000,
            remaining_units: 10_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: ast_id,
            type_id: 1230,
            position: ast_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e8,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    let _ = ctx.dispatch_rpc(&mut sess, "dogmaIM", None, "AddTarget", &[PyRep::Integer(ast_id)], &kw, None).await.unwrap();
    for _ in 0..10 {
        jita.step_manual(1.0).await.unwrap();
    }
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::String("miningLaser".into()), PyRep::Integer(ast_id)],
            &kw,
            None,
        )
        .await
        .unwrap();
    ctx.advance_secs(65);
    let _ = eve_server::services::mining_cycle::process_mining_cycles(&ctx).await;

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(notifs.iter().any(|n| n.event == "OnOreMined"), "OnOreMined must fire");

    let logs = ctx.dispatch_rpc(&mut sess, "characterMiningLedger", None, "GetCharacterLogs", &[], &kw, None).await.unwrap();
    let PyRep::List(rows) = logs else { panic!("Expected List from GetCharacterLogs") };
    assert!(!rows.is_empty());
    let kv_i64 = |rep: &PyRep, key: &str| -> Option<i64> {
        match rep {
            PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_i64()),
            PyRep::Dict(entries) => entries
                .iter()
                .find(|(k, _)| k.as_str() == Some(key))
                .and_then(|(_, v)| v.as_i64()),
            _ => None,
        }
    };
    let entry = rows
        .iter()
        .find(|r| kv_i64(r, "typeID") == Some(1230))
        .expect("GetCharacterLogs must contain Veldspar (1230) entry");
    assert_eq!(kv_i64(entry, "solarSystemID"), Some(30_000_142));
    assert!(kv_i64(entry, "quantity").unwrap_or(0) > 0, "GetCharacterLogs quantity must be > 0");
}

// ============================================================================
// 4. MN-09: Mining drones
// ============================================================================
#[tokio::test]
async fn test_mn09_mining_drones_ore_hold_delivery_and_range_check() {
    // parity: plan:MN-09, feat:drones.mining-drones, rpc:entity.CmdMineRepeatedly
    eve_testkit::attest(&["plan:MN-09", "feat:drones.mining-drones", "rpc:entity.CmdMineRepeatedly"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001i64;
    let mut sess = SessionState::new(9109);
    login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    let _ = ctx
        .dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap();

    let (ship_id, drone_a, drone_b) = ctx.db.with_mut_snapshot(|snap| {
        let active_ship = snap.characters.get(&char_id).unwrap().active_ship_id;
        snap.items.get_mut(&active_ship).unwrap().type_id = 32880; // Venture (5,000 m3 SpecializedOreHold, 10 mbit drone bandwidth)
        snap.items.retain(|_, i| !(i.location_id == active_ship && (i.flag == InventoryFlag::SpecializedOreHold || i.flag == InventoryFlag::DroneBay)));
        snap.next_entity_id += 1;
        let prefill_id = snap.next_entity_id;
        snap.items.insert(
            prefill_id,
            InventoryItemRecord {
                item_id: prefill_id,
                type_id: 1230, // Veldspar (0.1 m3 -> 49,500 units = 4,950 m3, leaving 50 m3 = 500 units)
                owner_id: char_id,
                location_id: active_ship,
                flag: InventoryFlag::SpecializedOreHold,
                quantity: 49_500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let da = snap.next_entity_id;
        snap.items.insert(
            da,
            InventoryItemRecord {
                item_id: da,
                type_id: 10246, // Mining Drone I
                owner_id: char_id,
                location_id: active_ship,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let db_id = snap.next_entity_id;
        snap.items.insert(
            db_id,
            InventoryItemRecord {
                item_id: db_id,
                type_id: 10246, // Mining Drone I
                owner_id: char_id,
                location_id: active_ship,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        (active_ship, da, db_id)
    });

    let _ = ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None).await.unwrap();
    ctx.invalidate_ship_fitting(ship_id);
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let (_, ship_ball) = jita.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Launch 2 Mining Drone Is from the ship's drone bay
    let drone_pair = PyRep::List(vec![PyRep::Integer(drone_a), PyRep::Integer(drone_b)]);
    ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchDrones", std::slice::from_ref(&drone_pair), &kw, None)
        .await
        .expect("ship.LaunchDrones 2 Mining Drone Is");

    // Out-of-range asteroid at 25 km (> 20 km default drone control range, < 35 km legacy limit) must be rejected
    let far_ast_id = 750_999_002i64;
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: far_ast_id,
            belt_id: 40_000_001,
            type_id: 1230,
            initial_units: 5_000,
            remaining_units: 5_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: far_ast_id,
            type_id: 1230,
            position: ship_ball.position + DVec3::new(25_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e8,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();
    let far_err = ctx
        .dispatch_rpc(
            &mut sess,
            "entity",
            None,
            "CmdMineRepeatedly",
            &[drone_pair.clone(), PyRep::Integer(far_ast_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        far_err.expect_err("Out-of-range asteroid must be rejected for mining drones").code,
        "OutOfRange"
    );

    // Spawn in-range asteroid and run entity.CmdMineRepeatedly
    let near_ast_id = 750_999_003i64;
    jita.spawn_asteroid(
        AsteroidState {
            asteroid_id: near_ast_id,
            belt_id: 40_000_001,
            type_id: 1230,
            initial_units: 10_000,
            remaining_units: 10_000,
            unit_volume: 0.1,
            depleted: false,
            respawn_at_tick: None,
        },
        Ball {
            id: near_ast_id,
            type_id: 1230,
            position: ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e8,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    )
    .await
    .unwrap();

    ctx.dispatch_rpc(
        &mut sess,
        "entity",
        None,
        "CmdMineRepeatedly",
        &[drone_pair, PyRep::Integer(near_ast_id)],
        &kw,
        None,
    )
    .await
    .expect("entity.CmdMineRepeatedly 2 Mining Drone Is");

    let c1_dur = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.active_drone_miners.len(), 2, "Both Mining Drone Is must be actively mining");
        rt.active_drone_miners.values().next().unwrap().duration_secs
    });

    // Cycle 1: 2 Mining Drone Is deposit Veldspar into SpecializedOreHold
    ctx.advance_secs(c1_dur);
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, ctx.now_unix()).await;
    let hold_after_c1: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::SpecializedOreHold && i.type_id == 1230)
            .map(|i| i.quantity)
            .sum()
    });
    assert!(hold_after_c1 > 49_500, "Ore hold quantity ({hold_after_c1}) must increase after drone mining cycle");

    // Cycle 2: Ore hold is now full for another full drone cycle -> drones stop mining
    ctx.advance_secs(c1_dur);
    eve_server::sp6_lifecycle::step_repeating_drone_mining(&ctx, ctx.now_unix()).await;
    let active_after_full = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
        rt.active_drone_miners.len()
    });
    assert_eq!(active_after_full, 0, "Mining drones must stop mining when ore hold is full");
}

// ============================================================================
// 5. DERIVED NPC mining op hauler replenishment
// ============================================================================
#[tokio::test]
async fn test_npc_mining_op_derived_hauler_replenishment() {
    let ctx = ServerContext::new();
    let director = LivingWorldDirector::new();

    let convoy_id = 800_000_199i64;
    let system_id = 30_000_144u32;
    let dest_station_id = 60_003_766i64;
    let ore_type_id = 1230u32;

    let handle = ctx.systems.get_or_spawn(system_id);
    let ast_id = 750_000_271i64;
    handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ast_id,
                belt_id: 40_000_001,
                type_id: ore_type_id,
                initial_units: 60_000,
                remaining_units: 60_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ast_id,
                type_id: ore_type_id,
                position: DVec3::new(5_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e8,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await
        .unwrap();

    director.register_convoy(LivingWorldConvoy {
        convoy_id,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880,
        origin_system_id: system_id,
        current_system_id: system_id,
        destination_system_id: system_id,
        destination_station_id: dest_station_id,
        cargo_type_id: ore_type_id,
        cargo_quantity: 48_800,
        lod_level: 1,
        destroyed: false,
    });

    let vol_before: i32 = ctx.db.with_snapshot(|snap| {
        snap.market_orders
            .values()
            .filter(|o| !o.is_bid && o.is_npc && o.station_id == dest_station_id && o.type_id == ore_type_id)
            .map(|o| o.vol_remaining)
            .sum()
    });
    let _ = director.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.unwrap();
    ctx.advance_secs(60);
    let step = director.step_npc_mining_op(convoy_id, &ctx.systems, &ctx.db, &ctx.sde).await.unwrap();
    assert!(step.mined_units > 0, "NPC mining op must deplete asteroid quantity over ticks");
    assert!(step.remaining_asteroid_units < 60_000);
    assert!(step.hauled_to_station);
    let vol_after: i32 = ctx.db.with_snapshot(|snap| {
        snap.market_orders
            .values()
            .filter(|o| !o.is_bid && o.is_npc && o.station_id == dest_station_id && o.type_id == ore_type_id)
            .map(|o| o.vol_remaining)
            .sum()
    });
    assert!(
        vol_after > vol_before,
        "NPC hauler delivery must increase NPC sell-order vol_remaining ({vol_before} -> {vol_after})"
    );
}
