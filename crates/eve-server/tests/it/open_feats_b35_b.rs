//! Dedicated integration tests for open `feat:navigation.*` parity rows (Batch 35, Part 2).

use crate::modules_advanced_tactical::{build_ctx, seed_pilot, NULL_SYS};
use crate::open_feats_b35::{give_item, put_in_fleet, set_skill, spawn_ship_in_space, DEST_NULL_SYS};
use eve_db::InventoryFlag;
use eve_destiny::{Ball, DVec3, MovementMode};
use eve_proto::PyRep;
use std::collections::HashMap;

const ZARZAKH_SYS: u32 = 30100000;

#[tokio::test]
async fn test_b35_09_jump_drive_range_fuel() {
    // parity: feat:navigation.jump-drive-range-fuel
    eve_testkit::attest(&["feat:navigation.jump-drive-range-fuel"]);
    let ctx = build_ctx();
    let origin_sys = ctx.systems.get_or_spawn(NULL_SYS);

    let cyno_char = 953_001;
    let cyno_ship = 963_001;
    let mut cyno_sess = seed_pilot(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, false);
    spawn_ship_in_space(&ctx, cyno_char, cyno_ship, 11957, DEST_NULL_SYS, DVec3::new(10_000.0, 0.0, 0.0)).await;
    give_item(&ctx, 973_001, 21096, cyno_char, cyno_ship, InventoryFlag::HiSlot0, 1);
    give_item(&ctx, 973_002, 16273, cyno_char, cyno_ship, InventoryFlag::Cargo, 2_000);
    let cyno_res = ctx.dispatch_rpc(&mut cyno_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(973_001), PyRep::String("cynosuralGeneration".into()), PyRep::Integer(0), PyRep::Integer(1)], &HashMap::new(), None).await.expect("Cyno");
    let beacon_id = cyno_res.dict_get("beaconID").and_then(|v| v.as_i64()).unwrap();

    // Archon (23757): Helium Isotopes (16274), JDC (21611), JFC (21610), JDO (3456)
    let car_char = 953_002;
    let car_ship = 963_002;
    let mut car_sess = seed_pilot(&ctx, car_char, car_ship, 23757, NULL_SYS, false);
    spawn_ship_in_space(&ctx, car_char, car_ship, 23757, NULL_SYS, DVec3::ZERO).await;
    set_skill(&ctx, car_char, 21611, 5); // JDC V
    set_skill(&ctx, car_char, 21610, 5); // JFC V
    set_skill(&ctx, car_char, 3456, 5);  // JDO V
    put_in_fleet(&ctx, 880_010, car_char, &[(cyno_char, true), (car_char, true)]);

    // 1. Transactionality: failed jump (no fuel) must NOT deduct capacitor
    let cap_before_fail = origin_sys.get_combat_state(car_ship).await.unwrap().unwrap().capacitor;
    let fail_res = ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await;
    assert!(fail_res.is_err(), "Jump without fuel must fail");
    let cap_after_fail = origin_sys.get_combat_state(car_ship).await.unwrap().unwrap().capacitor;
    assert_eq!(cap_after_fail, cap_before_fail, "Failed jump must NOT deduct capacitor");

    // 2. Refused inside warp disruption bubble
    give_item(&ctx, 973_003, 16274, car_char, car_ship, InventoryFlag::SpecializedFuelBay, 100_000);
    let _ = origin_sys.add_warp_bubble(999_002, DVec3::ZERO, 20_000.0).await;
    assert!(ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await.is_err());

    // 3. Succeeds without alignment when unbubbled; consumes 95% - 5%*JDO(5) = 70% cap and JFC-scaled Helium Isotopes
    let _ = origin_sys.remove_warp_bubble(999_002).await;
    let mut cs_ok = origin_sys.get_combat_state(car_ship).await.unwrap().unwrap();
    cs_ok.capacitor = cs_ok.max_capacitor;
    let max_cap = cs_ok.max_capacitor;
    origin_sys.set_combat_state(car_ship, cs_ok).await.unwrap();

    let jump_ok = ctx.dispatch_rpc(&mut car_sess, "beyonce", None, "CmdJumpToFleetModuleBeacon", &[PyRep::Integer(cyno_char), PyRep::Integer(DEST_NULL_SYS as i64), PyRep::Integer(beacon_id)], &HashMap::new(), None).await.expect("Jump succeeds");
    assert_eq!(jump_ok.dict_get("fuelTypeID"), Some(&PyRep::Integer(16274)));
    let dest_sys = ctx.systems.get_or_spawn(DEST_NULL_SYS);
    let cap_after = dest_sys.get_combat_state(car_ship).await.unwrap().unwrap().capacitor;
    let expected_cap = max_cap * (1.0 - 0.95 * 0.75);
    assert!((cap_after - expected_cap).abs() < 1.0, "JDO V must reduce base 95% cap need by 25%, got {cap_after}/{max_cap}");
}

#[tokio::test]
async fn test_b35_10_align_server_tick_rounding() {
    // parity: feat:navigation.align-server-tick-rounding
    eve_testkit::attest(&["feat:navigation.align-server-tick-rounding"]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);

    let c_align = 953_101;
    let s_align = 963_101;
    seed_pilot(&ctx, c_align, s_align, 587, NULL_SYS, false);
    let align_ball = Ball {
        id: s_align,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 2.23614, // ln(4) * 2.23614 * 1e6 / 1e6 = 3.1000s -> ceil = 4s
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Warp {
            destination: DVec3::new(1_000_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            phase: eve_destiny::WarpPhase::Aligning,
            elapsed_warp_time: 0.0,
            origin: DVec3::ZERO,
            total_distance: 1_000_000_000.0,
        },
    };
    let _ = sys.spawn_ball(align_ball).await;
    for _ in 0..3 {
        let _ = sys.step_manual(1.0).await;
    }
    let (_, b3) = sys.get_ball_snapshot(s_align).await.unwrap().unwrap();
    if let MovementMode::Warp { phase, .. } = b3.mode {
        assert_eq!(phase, eve_destiny::WarpPhase::Aligning, "Must still be aligning at tick 3");
        assert!(b3.velocity.length() < 400.0, "Subwarp velocity at tick 3: {}", b3.velocity.length());
    } else {
        panic!("Expected Warp mode at tick 3");
    }
    let _ = sys.step_manual(1.0).await;
    let (_, b4) = sys.get_ball_snapshot(s_align).await.unwrap().unwrap();
    if let MovementMode::Warp { phase, .. } = b4.mode {
        assert_eq!(phase, eve_destiny::WarpPhase::Accelerating, "Must enter warp on tick 4");
        assert!(b4.velocity.length() > 300.0, "Warp velocity at tick 4: {}", b4.velocity.length());
    } else {
        panic!("Expected Warp mode at tick 4");
    }
}

#[tokio::test]
async fn test_b35_11_cloak_velocity_penalty() {
    // parity: feat:navigation.cloak-velocity-penalty
    eve_testkit::attest(&["feat:navigation.cloak-velocity-penalty"]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);

    let c_clk = 953_102;
    let s_clk = 963_102;
    let mut sess_clk = seed_pilot(&ctx, c_clk, s_clk, 641, NULL_SYS, false);
    spawn_ship_in_space(&ctx, c_clk, s_clk, 641, NULL_SYS, DVec3::new(50_000.0, 0.0, 0.0)).await;
    let (_, b_init) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
    let base_vmax = b_init.max_velocity;

    // 1. Prototype Cloaking Device I (11370): -90% velocity
    let proto_clk = 973_101;
    give_item(&ctx, proto_clk, 11370, c_clk, s_clk, InventoryFlag::HiSlot0, 1);
    ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(proto_clk), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &HashMap::new(), None).await.expect("Prototype Cloak");
    let (_, b_proto) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
    assert!((b_proto.max_velocity - base_vmax * 0.10).abs() < 1e-3, "Prototype cloak must reduce max velocity by 90%");
    let _ = sys.remove_module_cycle(s_clk, proto_clk).await;
    let _ = sys.step_manual(1.0).await;
    let (_, b_unclk) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
    assert!((b_unclk.max_velocity - base_vmax).abs() < 1e-3, "Deactivating cloak must restore max velocity");

    // 2. Improved Cloaking Device II (11577): -75% velocity
    let imp_clk = 973_102;
    give_item(&ctx, imp_clk, 11577, c_clk, s_clk, InventoryFlag::HiSlot1, 1);
    ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(imp_clk), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &HashMap::new(), None).await.expect("Improved Cloak");
    let (_, b_imp) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
    assert!((b_imp.max_velocity - base_vmax * 0.25).abs() < 1e-3, "Improved cloak must reduce max velocity by 75%");
    let _ = sys.remove_module_cycle(s_clk, imp_clk).await;
    let _ = sys.step_manual(1.0).await;

    // 3. Covert Ops Cloaking Device II (11578): 0% velocity penalty
    let cov_clk = 973_103;
    give_item(&ctx, cov_clk, 11578, c_clk, s_clk, InventoryFlag::HiSlot2, 1);
    ctx.dispatch_rpc(&mut sess_clk, "dogmaIM", None, "Activate", &[PyRep::Integer(cov_clk), PyRep::String("cloaking".into()), PyRep::None, PyRep::Integer(1)], &HashMap::new(), None).await.expect("Covert Ops Cloak");
    let (_, b_cov) = sys.get_ball_snapshot(s_clk).await.unwrap().unwrap();
    assert!((b_cov.max_velocity - base_vmax).abs() < 1e-3, "Covert Ops cloak must incur 0% velocity penalty");
}

#[tokio::test]
async fn test_b35_12_collision_bumping() {
    // parity: feat:navigation.collision-bumping
    eve_testkit::attest(&["feat:navigation.collision-bumping"]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);

    // Heavy Battleship (Megathron 641) vs light Frigate (Rifter 587) overlapping at origin
    let c_heavy = 953_201;
    let s_heavy = 963_201;
    seed_pilot(&ctx, c_heavy, s_heavy, 641, NULL_SYS, false);
    let m_heavy = ctx.sde.types.get(&641).map(|t| t.mass).unwrap_or(100_000_000.0);
    let r_heavy = ctx.sde.types.get(&641).map(|t| t.radius).unwrap_or(300.0);

    let c_light = 953_202;
    let s_light = 963_202;
    seed_pilot(&ctx, c_light, s_light, 587, NULL_SYS, false);
    let m_light = ctx.sde.types.get(&587).map(|t| t.mass).unwrap_or(1_000_000.0);
    let r_light = ctx.sde.types.get(&587).map(|t| t.radius).unwrap_or(40.0);

    let _ = sys.spawn_ball(Ball {
        id: s_heavy, type_id: 641, position: DVec3::ZERO, velocity: DVec3::new(100.0, 0.0, 0.0),
        max_velocity: 100.0, inertia: 1_000_000.0, mass: m_heavy, radius: r_heavy, speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    let _ = sys.spawn_ball(Ball {
        id: s_light, type_id: 587, position: DVec3::new(200.0, 0.0, 0.0), velocity: DVec3::ZERO,
        max_velocity: 300.0, inertia: 1_000_000.0, mass: m_light, radius: r_light, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    let _ = sys.step_manual(1.0).await;
    let (_, b_h) = sys.get_ball_snapshot(s_heavy).await.unwrap().unwrap();
    let (_, b_l) = sys.get_ball_snapshot(s_light).await.unwrap().unwrap();
    assert!(b_h.position.distance(b_l.position) >= r_heavy + r_light - 1.0, "Colliding ships must separate with no interpenetration");
    assert!(b_l.velocity.length() > (b_h.velocity - DVec3::new(100.0, 0.0, 0.0)).length(), "Lighter ship must receive larger velocity impulse than heavier ship");
}

#[tokio::test]
async fn test_b35_13_drifter_wormhole_systems() {
    // parity: feat:navigation.drifter-wormhole-systems
    eve_testkit::attest(&["feat:navigation.drifter-wormhole-systems"]);
    let ctx = build_ctx();

    // All 5 Drifter systems (31000001..=31000004, 31000006): 6 C1-C6 statics + 56 K-space connections
    for sys_id in [31_000_001, 31_000_002, 31_000_003, 31_000_004, 31_000_006] {
        let (sigs, _) = eve_server::services::space_destiny::space_docking_cloaking::ensure_system_signatures_and_anomalies(&ctx, sys_id).await;
        let wh_sigs: Vec<_> = sigs.iter().filter(|s| s.wormhole_type_id.is_some()).collect();
        assert_eq!(wh_sigs.len(), sigs.len(), "Every signature in Drifter system {sys_id} must be a wormhole");
        assert!(wh_sigs.len() >= 62, "Drifter system {sys_id} must have 6 statics + 56 K-space connections, got {}", wh_sigs.len());
    }
}

#[tokio::test]
async fn test_b35_14_gravity_hazard_safe_zones() {
    // parity: feat:navigation.gravity-hazard-safe-zones
    eve_testkit::attest(&["feat:navigation.gravity-hazard-safe-zones"]);
    let ctx = build_ctx();
    let z_sys = ctx.systems.get_or_spawn(ZARZAKH_SYS);

    let c_safe = 953_103;
    let s_safe = 963_103;
    seed_pilot(&ctx, c_safe, s_safe, 641, ZARZAKH_SYS, false);
    let fulcrum_pos = ctx.sde.stations.get(&60015187).map(|s| s.position).unwrap_or(DVec3::ZERO);
    spawn_ship_in_space(&ctx, c_safe, s_safe, 641, ZARZAKH_SYS, fulcrum_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;

    let c_haz = 953_104;
    let s_haz = 963_104;
    seed_pilot(&ctx, c_haz, s_haz, 641, ZARZAKH_SYS, false);
    spawn_ship_in_space(&ctx, c_haz, s_haz, 641, ZARZAKH_SYS, DVec3::new(500_000_000.0, 0.0, 0.0)).await;

    let safe_shield_before = z_sys.get_combat_state(s_safe).await.unwrap().unwrap().shield_hp;
    let haz_shield_before = z_sys.get_combat_state(s_haz).await.unwrap().unwrap().shield_hp;
    let _ = z_sys.step_manual(1.0).await;
    let safe_shield_after = z_sys.get_combat_state(s_safe).await.unwrap().unwrap().shield_hp;
    let haz_shield_after = z_sys.get_combat_state(s_haz).await.unwrap().unwrap().shield_hp;
    assert_eq!(safe_shield_after, safe_shield_before, "Ship inside Zarzakh safe zone must take 0 hazard damage");
    assert!(haz_shield_after < haz_shield_before, "Ship outside Zarzakh safe zones must take periodic damage via damage pipeline");
}

#[tokio::test]
async fn test_b35_15_interdiction_nullification_module() {
    // parity: feat:navigation.interdiction-nullification-module
    eve_testkit::attest(&["feat:navigation.interdiction-nullification-module"]);
    let ctx = build_ctx();
    let sys = ctx.systems.get_or_spawn(NULL_SYS);

    let c_base = 953_106;
    let s_base = 963_106;
    seed_pilot(&ctx, c_base, s_base, 11176, NULL_SYS, false);
    let fit_before = ctx.get_or_rebuild_ship_fitting(c_base, s_base).unwrap();
    let ev_before = fit_before.evaluated_context(&ctx.sde);
    let scan_res_before = ev_before.ship.get_attribute(564, &ctx.sde); // scanResolution
    let target_range_before = ev_before.ship.get_attribute(76, &ctx.sde); // maxTargetRange

    let c_null = 953_105;
    let s_null = 963_105;
    let mut sess_null = seed_pilot(&ctx, c_null, s_null, 11176, NULL_SYS, false); // Crow
    spawn_ship_in_space(&ctx, c_null, s_null, 11176, NULL_SYS, DVec3::new(200_000.0, 0.0, 0.0)).await;
    let null_mod = 973_104;
    give_item(&ctx, null_mod, 58919, c_null, s_null, InventoryFlag::HiSlot0, 1); // Interdiction Nullifier I
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(null_mod, true);
    });
    ctx.invalidate_ship_fitting(s_null);
    let fit_after = ctx.get_or_rebuild_ship_fitting(c_null, s_null).unwrap();
    let ev_after = fit_after.evaluated_context(&ctx.sde);
    let scan_res_after = ev_after.ship.get_attribute(564, &ctx.sde);
    let target_range_after = ev_after.ship.get_attribute(76, &ctx.sde);
    assert!(scan_res_after < scan_res_before, "Fitted Interdiction Nullifier must incur passive scan resolution penalty");
    assert!(target_range_after < target_range_before, "Fitted Interdiction Nullifier must incur passive targeting range penalty");

    ctx.dispatch_rpc(&mut sess_null, "dogmaIM", None, "Activate", &[PyRep::Integer(null_mod), PyRep::String("defaultEffect".into()), PyRep::None, PyRep::Integer(1)], &HashMap::new(), None).await.expect("Activate Interdiction Nullifier");
    let cs_null = sys.get_combat_state(s_null).await.unwrap().unwrap();
    assert!(cs_null.is_interdiction_nullified, "Interdiction Nullifier must grant bubble immunity");
}
