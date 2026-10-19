//! Dogma Engine & Combat integration tests (DG-4, DG-5, C-1, C-2).
//!
//! Covers:
//! - DG-4: Module activation cycles, capacitor recharge ODE & stability, overheat & rack heat damage,
//!         shield booster & armor repairer cycle healing, and reactivation delay.
//! - DG-5: Combat target locking state machine, racial sensor strengths, ECM jam chance & lock break,
//!         sensor dampeners, target painters, tracking disruptors, stasis webifiers, warp disruptors & scramblers.
//! - C-1: Turret tracking formula, wrecking shots, and missile flight time & damage application.
//! - C-2: Dogma-initialized CombatState, 4-channel resonance HP deduction, and OnDamageStateChange notifications.

#![forbid(unsafe_code)]

mod common;

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{
    Ball, CombatState, DVec3, DamageVector, DestinyEvent, MovementMode, TargetLock,
};
use eve_dogma::*;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::formulas::*;
use eve_server::router::{NotificationTarget, ServerContext};
use std::collections::HashMap;

fn seed_test_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

fn grant_skills(ctx: &ServerContext, char_id: i64, skills: &[(u32, u8)]) {
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for &(skill_id, level) in skills {
            entry.insert(
                skill_id,
                eve_db::TrainedSkillRecord {
                    type_id: skill_id,
                    level,
                    skill_points: 256_000.0,
                },
            );
        }
    });
}

#[tokio::test]
async fn test_dg4_module_cycle_capacitor_and_shield_booster() {
    // parity: plan:DG-4, feat:dogma.capacitor-recharge, feat:dogma.capacitor-stable, feat:modules.shield-booster
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    grant_skills(&ctx, char_id, &[
        (3416, 5), // Shield Operation
        (3413, 5), // Shield Management
        (3429, 5), // Target Management
        (3430, 5), // Advanced Target Management
    ]);

    // 1. Verify EVE Capacitor Recharge ODE formula (feat:dogma.capacitor-recharge)
    // dC/dt = (10 / T) * (sqrt(C / C_max) - C / C_max) * C_max
    let cap_max = 1000.0;
    let recharge_time = 200.0; // seconds

    // Rate at 0% capacity is 0.0
    let rate_0 = capacitor_c_dot(0.0, cap_max, recharge_time);
    assert_eq!(rate_0, 0.0);

    // Rate at 100% capacity is 0.0
    let rate_100 = capacitor_c_dot(cap_max, cap_max, recharge_time);
    assert!((rate_100 - 0.0).abs() < 1e-6);

    // Rate peaks at exactly 25% capacity: sqrt(0.25) - 0.25 = 0.5 - 0.25 = 0.25
    // Peak rate = 10 / T * 0.25 * C_max = 2.5 * C_max / T = 12.5 GJ/s
    let rate_peak = capacitor_c_dot(0.25 * cap_max, cap_max, recharge_time);
    let expected_peak = peak_passive_recharge_per_sec(cap_max, recharge_time);
    assert!((rate_peak - expected_peak).abs() < 1e-6);
    assert_eq!(rate_peak, 12.5);

    // Verify rate at 10% and 50% are both strictly less than peak
    assert!(capacitor_c_dot(0.10 * cap_max, cap_max, recharge_time) < rate_peak);
    assert!(capacitor_c_dot(0.50 * cap_max, cap_max, recharge_time) < rate_peak);

    // Step capacitor recharge through ODE and compare with analytical passive recharge formula
    let c_start = 250.0;
    let dt = 1.0;
    let mut c_sim = c_start;
    for _ in 0..10 {
        c_sim = simulate_capacitor(c_sim, cap_max, recharge_time, dt);
    }
    let c_analytical = compute_passive_recharge(c_start, cap_max, recharge_time * 1000.0, 10.0 * 1000.0);
    assert!((c_sim - c_analytical).abs() < 2.0, "ODE step ({c_sim}) and analytical ({c_analytical}) must agree closely");

    // 2. Verify Capacitor Stability Accounting (feat:dogma.capacitor-stable)
    // A fit with 10.0 GJ/s draw is capacitor stable (10.0 <= peak 12.5 GJ/s)
    let stable_rep = evaluate_capacitor_stability(cap_max, recharge_time, 10.0);
    assert!(stable_rep.is_stable);
    assert!(stable_rep.stable_level_pct.is_some());
    assert!(stable_rep.stable_level_pct.unwrap() >= 25.0);
    assert!(stable_rep.depletion_time_s.is_none());

    // A fit with 15.0 GJ/s draw is capacitor UNSTABLE (15.0 > peak 12.5 GJ/s)
    let unstable_rep = evaluate_capacitor_stability(cap_max, recharge_time, 15.0);
    assert!(!unstable_rep.is_stable);
    assert!(unstable_rep.stable_level_pct.is_none());
    assert!(unstable_rep.depletion_time_s.is_some());

    // 3. Verify Local Shield Booster cycle healing (feat:modules.shield-booster)
    // Test Small Shield Booster I (type 399: duration 2.0s, capNeed 20.0, shieldBonus 26.0)
    let small_boost_item = DogmaItemState::from_sde(1, 399, &ctx.sde);
    let small_res = compute_local_repair_cycle(&small_boost_item, false, &ctx.sde);
    assert_eq!(small_res.shield_boost_amount, 26.0);
    assert_eq!(small_res.duration_s, 2.0);
    assert_eq!(small_res.capacitor_need, 20.0);

    // Overloaded booster gains 20% healing boost
    let small_res_ov = compute_local_repair_cycle(&small_boost_item, true, &ctx.sde);
    assert!((small_res_ov.shield_boost_amount - 26.0 * 1.20).abs() < 1e-6);

    // Test X-Large Shield Booster I (type 10840: shieldBonus 518.0, duration 5.0s, capNeed 400.0)
    let xl_booster_item = DogmaItemState::from_sde(2, 10840, &ctx.sde);
    let xl_res = compute_local_repair_cycle(&xl_booster_item, false, &ctx.sde);
    assert_eq!(xl_res.shield_boost_amount, 518.0);
    assert_eq!(xl_res.duration_s, 5.0);
    assert_eq!(xl_res.capacitor_need, 400.0);

    // Test CONCORD Capital Shield Booster (type 3542: shieldBonus 9750.0, duration 10.0s, capNeed 2400.0)
    let cap_booster_item = DogmaItemState::from_sde(3, 3542, &ctx.sde);
    let cap_res = compute_local_repair_cycle(&cap_booster_item, false, &ctx.sde);
    assert_eq!(cap_res.shield_boost_amount, 9750.0);
    assert_eq!(cap_res.duration_s, 10.0);
    assert_eq!(cap_res.capacitor_need, 2400.0);

    // Test Medium Armor Repairer I (type 3528: armorDamageAmount 276.0, duration 12.0s, capNeed 160.0)
    let med_rep_item = DogmaItemState::from_sde(4, 3528, &ctx.sde);
    let rep_res = compute_local_repair_cycle(&med_rep_item, false, &ctx.sde);
    assert_eq!(rep_res.armor_repair_amount, 276.0);
    assert_eq!(rep_res.duration_s, 12.0);
    assert_eq!(rep_res.capacitor_need, 160.0);

    // 4. In-space Shield Booster module activation and cycle execution
    // Fit Small Shield Booster I to MedSlot0
    let booster_item_id = 100_000_901i64;
    seed_test_item(&ctx, booster_item_id, 399, char_id, station_id, InventoryFlag::Hangar, 1);
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "MoveItem",
        &[
            PyRep::Integer(booster_item_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit Small Shield Booster I");

    // Undock into Jita space
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let sys = ctx.systems.get_or_spawn(30_000_142);

    // Damage ship shield: set shield to 200 / 450
    let mut initial_cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    initial_cs.shield_hp = 200.0;
    initial_cs.capacitor = 200.0;
    // Set recharge rates high so passive recharge doesn't mask booster effect in 2 seconds
    initial_cs.shield_recharge_time_ms = 1_000_000.0;
    initial_cs.capacitor_recharge_time_ms = 1_000_000.0;
    sys.set_combat_state(ship_id, initial_cs).await.unwrap();

    // Activate Small Shield Booster I
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(booster_item_id),
            PyRep::String("shieldBoosting".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Small Shield Booster I");

    // Step 2 seconds for cycle to complete (duration = 2.0s)
    for _ in 0..2 {
        sys.step_manual(1.0).await.expect("sys.step_manual booster cycle");
    }

    let cs_after_cycle = sys.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(
        cs_after_cycle.shield_hp > 200.0,
        "Shield HP must increase after booster cycle (expected ~226+, got {})",
        cs_after_cycle.shield_hp
    );

    // 5. Test insufficient capacitor blocks activation
    sys.set_capacitor(ship_id, 5.0).await.unwrap(); // Booster needs 20 GJ
    let no_cap_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(booster_item_id),
                PyRep::String("shieldBoosting".into()),
                PyRep::None,
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await;
    assert!(no_cap_err.is_err(), "Activation must fail when capacitor is below capacitorNeed");

    eve_testkit::attest(&[
        "plan:DG-4",
        "feat:dogma.capacitor-recharge",
        "feat:dogma.capacitor-stable",
        "feat:modules.shield-booster",
    ]);
}

#[tokio::test]
async fn test_dg4_overheat_heat_damage_and_reactivation_delay() {
    // parity: plan:DG-4, feat:fitting.overheat, feat:dogma.reactivation-delay
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4102);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // Grant skills: Thermodynamics (28164) at Level 3 (-15% heat damage),
    // Small Projectile Turret (3302), Gunnery (3300), Small Autocannon Spec (11084),
    // Warp Drive Operation (3435), Warp Core Operation (3455)
    grant_skills(&ctx, char_id, &[
        (28164, 3),
        (3302, 5),
        (3300, 5),
        (11084, 5),
        (3435, 5),
        (3455, 5),
    ]);

    // 1. Verify Overheat Thermodynamics & Rack Heat Propagation (feat:fitting.overheat)
    let mut fit = ShipFittingEngine::from_sde(ship_id, 587, &ctx.sde); // Rifter hull
    fit.character_skills.insert(28164, 3);
    fit.character_skills.insert(3302, 5);
    fit.character_skills.insert(3300, 5);
    fit.character_skills.insert(11084, 5);

    // Fit two 200mm AutoCannon II modules in adjacent high slots (HiSlot0=27, HiSlot1=28)
    let t1_id = 100_000_910i64;
    let t2_id = 100_000_911i64;
    fit.fit_module_with_state(t1_id, 2889, 27, true, &ctx.sde).expect("fit t1");
    fit.fit_module_with_state(t2_id, 2889, 28, true, &ctx.sde).expect("fit t2");

    // Initial state: rack heat is 0.0, both modules have 0.0 damage and are Online
    assert_eq!(fit.rack_heat[0], 0.0);
    assert_eq!(fit.modules.get(&t1_id).unwrap().damage_hp, 0.0);
    assert_eq!(fit.modules.get(&t1_id).unwrap().state, ModuleState::Online);

    // Activate t1 with overload
    fit.activate_module(t1_id, true, &ctx.sde).expect("activate overloaded");
    assert_eq!(fit.modules.get(&t1_id).unwrap().state, ModuleState::Overloaded);

    // Step heat cycle for t1 (heat_gen_rate = 0.05, dissipation = 0.01, duration = 3.75s, roll = 0.05 -> hits adjacent)
    let report = fit.apply_heat_cycle(t1_id, 0.05, 0.01, 3.75, 0.05, &ctx.sde).expect("apply_heat_cycle");
    assert!(report.rack_heat > 0.0, "High rack heat must increase after overloaded cycle");
    assert!(report.module_damage > 0.0, "Overloaded module must take heat damage");
    assert_eq!(fit.modules.get(&t1_id).unwrap().damage_hp, report.module_damage);

    // Verify Thermodynamics skill level 3 reduced heat damage by 15%
    let base_heat_dmg = fit.build_effective_module_state(t1_id, 2889, true, &ctx.sde).get_attribute(1211, &ctx.sde);
    let expected_dmg = base_heat_dmg * (1.0 - 0.05 * 3.0);
    assert!((report.module_damage - expected_dmg).abs() < 1e-4);

    // Verify adjacent module t2 in slot 28 also took heat damage
    assert!(!report.adjacent_damaged_modules.is_empty(), "Adjacent module in slot 28 must take heat damage");
    assert!(fit.modules.get(&t2_id).unwrap().damage_hp > 0.0);

    // Verify module burnout: when damage_hp reaches max_hp (40.0), module goes Offline
    if let Some(m) = fit.modules.get_mut(&t1_id) {
        m.damage_hp = 39.5;
    }
    let burn_report = fit.apply_heat_cycle(t1_id, 0.05, 0.01, 3.75, 0.99, &ctx.sde).expect("burn cycle");
    assert!(burn_report.module_burned);
    assert_eq!(fit.modules.get(&t1_id).unwrap().state, ModuleState::Offline, "Burned module must go Offline");

    // Attempting to activate burnt module returns error
    let burn_act_err = fit.activate_module(t1_id, false, &ctx.sde);
    assert!(burn_act_err.is_err(), "Cannot activate burned out module");

    // Verify Nanite Repair Paste cost formula (#34)
    let paste_needed = nanite_repair_paste_needed(40.0, 0.1);
    assert_eq!(paste_needed, 4);

    // 2. Verify Reactivation Delay (feat:dogma.reactivation-delay)
    // Fit Warp Core Stabilizer I (type 10998, has moduleReactivationDelay = 150,000 ms = 150 s)
    let wcs_item_id = 100_000_920i64;
    seed_test_item(&ctx, wcs_item_id, 10998, char_id, station_id, InventoryFlag::Hangar, 1);
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "MoveItem",
        &[
            PyRep::Integer(wcs_item_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::LoSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit Warp Core Stabilizer I");

    // Undock into space
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    // Activate Warp Core Stabilizer I
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(wcs_item_id),
            PyRep::String("warpCoreFitted".into()),
            PyRep::None,
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Warp Core Stabilizer I");

    // Deactivate Warp Core Stabilizer I -> records reactivation delay of 150 seconds
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(wcs_item_id),
            PyRep::String("warpCoreFitted".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Warp Core Stabilizer I");

    // Attempt immediate reactivation -> must fail with ModuleReactivationDelay
    let early_react_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(wcs_item_id),
                PyRep::String("warpCoreFitted".into()),
                PyRep::None,
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await;
    assert_eq!(
        early_react_err.expect_err("Must block early reactivation").code,
        "ModuleReactivationDelay"
    );

    // Advance virtual server clock by 151 seconds past the 150s reactivation delay
    ctx.advance_secs(151);

    // Reactivation after delay expiration must succeed!
    let delayed_react = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(wcs_item_id),
                PyRep::String("warpCoreFitted".into()),
                PyRep::None,
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await;
    assert!(delayed_react.is_ok(), "Reactivation must succeed after delay expiration");

    eve_testkit::attest(&[
        "plan:DG-4",
        "feat:fitting.overheat",
        "feat:dogma.reactivation-delay",
    ]);
}

#[tokio::test]
async fn test_dg5_targeting_state_machine_sensor_strength_and_ewar() {
    // parity: plan:DG-5, feat:dogma.targeting-ships-sensor-strength, feat:modules.ecm, feat:modules.sensor-dampener, feat:modules.tracking-disruptor, feat:modules.target-painter
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4103);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    let _char_id = 90_000_001i64;
    let _ship_id = 100_000_001i64;

    // 1. Verify Racial Sensor Strengths across all 4 factions (feat:dogma.targeting-ships-sensor-strength)
    // Caldari Merlin (603): Gravimetric (211) = 11.0
    let merlin_state = DogmaItemState::from_sde(1, 603, &ctx.sde);
    let (m_type, m_strength) = get_ship_sensor_strength(&merlin_state, &ctx.sde);
    assert_eq!(m_type, ShipSensorType::Gravimetric);
    assert_eq!(m_strength, 11.0);

    // Minmatar Rifter (587): Ladar (209) = 8.0
    let rifter_state = DogmaItemState::from_sde(2, 587, &ctx.sde);
    let (r_type, r_strength) = get_ship_sensor_strength(&rifter_state, &ctx.sde);
    assert_eq!(r_type, ShipSensorType::Ladar);
    assert_eq!(r_strength, 8.0);

    // Gallente Tristan (593): Magnetometric (210) = 9.0
    let tristan_state = DogmaItemState::from_sde(3, 593, &ctx.sde);
    let (t_type, t_strength) = get_ship_sensor_strength(&tristan_state, &ctx.sde);
    assert_eq!(t_type, ShipSensorType::Magnetometric);
    assert_eq!(t_strength, 9.0);

    // Amarr Executioner (589): Radar (208) = 8.0
    let exec_state = DogmaItemState::from_sde(4, 589, &ctx.sde);
    let (e_type, e_strength) = get_ship_sensor_strength(&exec_state, &ctx.sde);
    assert_eq!(e_type, ShipSensorType::Radar);
    assert_eq!(e_strength, 8.0);

    // 2. Verify ECM Jammer chance formula and lock breaking (feat:modules.ecm)
    // Multispectral ECM I (1957): scanGravimetricStrengthBonus = 2.2, optimal = 19,200m, falloff = 18,000m
    let ecm_item = DogmaItemState::from_sde(5, 1957, &ctx.sde);
    // Jam chance against Merlin at optimal: 2.2 / 11.0 = 0.20 (20%)
    let jam_opt = compute_ecm_jam_against_ship(&ecm_item, &merlin_state, 10_000.0, &ctx.sde);
    assert!((jam_opt - 0.20).abs() < 1e-4);

    // Jam chance at optimal + 1 falloff (37,200m): 0.20 * 0.5^1 = 0.10 (10%)
    let jam_falloff = compute_ecm_jam_against_ship(&ecm_item, &merlin_state, 19_200.0 + 18_000.0, &ctx.sde);
    assert!((jam_falloff - 0.10).abs() < 1e-4);

    // Test Ballpark ECM Jam & Break: when jammed, target loses locks and cannot lock for 20s
    let mut ballpark = eve_destiny::Ballpark::new(30_000_142);
    let victim_id = 990_000_030i64;
    ballpark.add_ball(Ball {
        id: victim_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.2,
        mass: 1_350_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    ballpark.combat_states.insert(victim_id, CombatState::default());
    ballpark.add_target_lock(victim_id, TargetLock {
        target_id: 990_000_031,
        lock_time_ms: 1000.0,
        remaining_lock_ms: 0.0,
        locked: true,
        max_range_m: Some(50_000.0),
    });
    assert_eq!(ballpark.get_target_locks(victim_id).len(), 1);

    // Apply ECM jam
    ballpark.apply_ecm_jam(victim_id, 20.0);
    assert_eq!(ballpark.get_target_locks(victim_id).len(), 0, "ECM jam must clear all target locks");
    assert_eq!(ballpark.combat_states.get(&victim_id).unwrap().ecm_jammed_remaining_s, 20.0);

    // Test ECM Burst AoE
    let burst_source = 990_000_032i64;
    ballpark.add_ball(Ball {
        id: burst_source,
        type_id: 603,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.2,
        mass: 1_350_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    ballpark.combat_states.insert(burst_source, CombatState::default());
    let jammed_by_burst = ballpark.apply_ecm_burst(burst_source, 10_000.0, |_| true);
    assert!(jammed_by_burst.contains(&victim_id), "ECM Burst AoE must jam targets in radius");

    // 3. Verify Remote Sensor Dampener & Scripts (feat:modules.sensor-dampener)
    // Remote Sensor Dampener I (1968): scanResolutionBonus = -13.7%, maxTargetRangeBonus = -13.7%
    let damp_item = DogmaItemState::from_sde(6, 1968, &ctx.sde);
    // Unscripted: reduces both scan res and target range
    let (damp_scan_un, damp_range_un) = eval_scripted_sensor_dampener(&damp_item, None, 10_000.0, &ctx.sde);
    assert!(damp_scan_un < 1.0 && damp_range_un < 1.0);
    assert!((damp_scan_un - (1.0 - 0.137)).abs() < 1e-4);
    assert!((damp_range_un - (1.0 - 0.137)).abs() < 1e-4);

    // Scripted with Scan Resolution Dampening Script (29013): 100% focused on scan res, range penalty is 0
    let (damp_scan_scr, damp_range_scr) = eval_scripted_sensor_dampener(&damp_item, Some(29013), 10_000.0, &ctx.sde);
    assert!(damp_scan_scr < damp_scan_un, "Scripted scan dampening must double scan resolution reduction");
    assert_eq!(damp_range_scr, 1.0, "Scan resolution script zeroes range reduction");

    // Scripted with Targeting Range Dampening Script (29015): 100% focused on range, scan res penalty is 0
    let (damp_scan_rng, damp_range_rng) = eval_scripted_sensor_dampener(&damp_item, Some(29015), 10_000.0, &ctx.sde);
    assert_eq!(damp_scan_rng, 1.0, "Targeting range script zeroes scan res reduction");
    assert!(damp_range_rng < damp_range_un, "Scripted range dampening must double range reduction");

    // 4. Verify Tracking Disruptor & Scripts (feat:modules.tracking-disruptor)
    // Tracking Disruptor I (2108): trackingSpeedBonus = -15.3%, maxRangeBonus = -15.3%, falloffBonus = -15.3%
    let td_item = DogmaItemState::from_sde(7, 2108, &ctx.sde);
    // Unscripted: reduces tracking, optimal, falloff
    let (t_mult, o_mult, f_mult) = eval_scripted_tracking_disruptor(&td_item, None, 10_000.0, &ctx.sde);
    assert!(t_mult < 1.0 && o_mult < 1.0 && f_mult < 1.0);

    // Scripted with Tracking Speed Disruption Script (29007): 100% focused on tracking
    let (t_scr, o_scr, f_scr) = eval_scripted_tracking_disruptor(&td_item, Some(29007), 10_000.0, &ctx.sde);
    assert!(t_scr < t_mult, "Scripted tracking disruption doubles tracking speed penalty");
    assert_eq!(o_scr, 1.0);
    assert_eq!(f_scr, 1.0);

    // Scripted with Optimal Range Disruption Script (29005): 100% focused on range
    let (t_rng, o_rng, f_rng) = eval_scripted_tracking_disruptor(&td_item, Some(29005), 10_000.0, &ctx.sde);
    assert_eq!(t_rng, 1.0);
    assert!(o_rng < o_mult);
    assert!(f_rng < f_mult);

    // 5. Verify Target Painter with Overload (feat:modules.target-painter)
    // Target Painter I (12709): signatureRadiusBonus = 25.0%
    let tp_item = DogmaItemState::from_sde(8, 12709, &ctx.sde);
    let tp_unloaded = eval_target_painter_with_overload(&tp_item, false, 10_000.0, &ctx.sde);
    assert!((tp_unloaded - 1.25).abs() < 1e-4, "Target painter must expand sig radius by 25%");

    // Overloaded painter gains +20% bonus (25% * 1.20 = 30% bonus -> 1.30 multiplier)
    let tp_overloaded = eval_target_painter_with_overload(&tp_item, true, 10_000.0, &ctx.sde);
    assert!((tp_overloaded - 1.30).abs() < 1e-4, "Overloaded target painter must expand sig radius by 30%");

    // 6. Verify Stasis Webifier & Warp Scrambler (DG-5)
    // Stasis Webifier I (526): speedFactor = -50.0%
    let web_item = DogmaItemState::from_sde(9, 526, &ctx.sde);
    let slowed_vel = eval_stasis_webifier_stacking(&[&web_item], &[5_000.0], 365.0, &ctx.sde);
    assert!((slowed_vel - 182.5).abs() < 1e-2, "Webifier must reduce velocity by 50%");

    // Warp Scrambler I (447): 2 warp scramble points and blocks MWD
    let scram_item = DogmaItemState::from_sde(10, 447, &ctx.sde);
    let scram_state = eval_warp_scrambler_effects(&[&scram_item], &[5_000.0], &ctx.sde);
    assert_eq!(scram_state.scramble_strength, 2);
    assert!(scram_state.blocks_mwd, "Warp scrambler must suppress MWD");

    // 7. Verify Target Lock Time & Max Locked Targets (DG-5)
    // Target lock time formula: t = 40000 / (scan_res * asinh(sig)^2)
    let t_lock_fast = compute_target_lock_time(660.0, 100.0); // cruiser size
    let t_lock_slow = compute_target_lock_time(660.0, 35.0);  // frigate size
    assert!(t_lock_slow > t_lock_fast, "Smaller signature radius takes longer to lock");

    // Max locked targets calculation with skills
    // Hull has 4 max targets, Target Management IV (+4) -> 2 + 4 = 6, capped by hull (4)
    let max_t1 = compute_effective_max_locked_targets(4, 4, 0);
    assert_eq!(max_t1, 4);
    // Hull has 10 max targets (Battleship), TM V (+5) -> 2 + 5 = 7
    let max_t2 = compute_effective_max_locked_targets(10, 5, 0);
    assert_eq!(max_t2, 7);
    // Battleship with TM V and Advanced TM III (+3) -> 2 + 5 + 3 = 10
    let max_t3 = compute_effective_max_locked_targets(10, 5, 3);
    assert_eq!(max_t3, 10);

    // Lock break on range: target moves beyond maxTargetRange
    let src_ball_id = 990_000_040i64;
    let tgt_ball_id = 990_000_041i64;
    ballpark.add_ball(Ball {
        id: src_ball_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.2,
        mass: 1_350_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    ballpark.combat_states.insert(src_ball_id, CombatState::default());
    ballpark.add_ball(Ball {
        id: tgt_ball_id,
        type_id: 587,
        position: DVec3::new(20_000.0, 0.0, 0.0), // 20 km
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.2,
        mass: 1_350_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    ballpark.combat_states.insert(tgt_ball_id, CombatState::default());
    ballpark.add_target_lock(src_ball_id, TargetLock {
        target_id: tgt_ball_id,
        lock_time_ms: 1000.0,
        remaining_lock_ms: 0.0,
        locked: true,
        max_range_m: Some(25_000.0), // 25 km max range
    });
    // Within range -> retained
    ballpark.step(1.0);
    assert_eq!(ballpark.get_target_locks(src_ball_id).len(), 1);

    // Target moves to 30 km (> 25 km max range) -> lock breaks!
    if let Some(tb) = ballpark.balls.get_mut(&tgt_ball_id) {
        tb.position = DVec3::new(30_000.0, 0.0, 0.0);
    }
    let step_events = ballpark.step(1.0);
    assert_eq!(ballpark.get_target_locks(src_ball_id).len(), 0, "Lock must break when target exceeds max range");
    assert!(step_events.iter().any(|e| matches!(e, DestinyEvent::OnTarget { mode, .. } if mode == "lost")));

    eve_testkit::attest(&[
        "plan:DG-5",
        "feat:dogma.targeting-ships-sensor-strength",
        "feat:modules.ecm",
        "feat:modules.sensor-dampener",
        "feat:modules.tracking-disruptor",
        "feat:modules.target-painter",
    ]);
}

#[tokio::test]
async fn test_c1_c2_turret_tracking_missiles_and_damage_pipeline() {
    // parity: plan:C-1, plan:C-2
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4101);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // 1. Turret Tracking Formula & Shot Resolution (plan:C-1)
    let att_pos = DVec3::ZERO;
    let att_vel = DVec3::ZERO;
    let tgt_pos = DVec3::new(5_000.0, 0.0, 0.0);
    let tgt_vel = DVec3::new(0.0, 200.0, 0.0); // moving transversely at 200 m/s

    let omega = turret_angular_velocity(att_pos, att_vel, tgt_pos, tgt_vel);
    assert!((omega - (200.0 / 5_000.0)).abs() < 1e-6); // 0.04 rad/s

    // Chance to hit with signature resolution
    let tracking = 0.10; // 0.10 rad/s
    let sig_res = 40_000.0;
    let sig_radius = 40_000.0;
    let optimal = 4_000.0;
    let falloff = 4_000.0;
    let distance = 5_000.0; // 1,000m into falloff
    let p_hit = turret_chance_to_hit_with_res(omega, tracking, sig_res, sig_radius, distance, optimal, falloff);
    assert!(p_hit > 0.50 && p_hit < 1.0, "Expected reasonable hit chance, got {p_hit}");

    // Shot resolution:
    let base_dmg = DamageVector::new(50.0, 50.0, 50.0, 50.0);
    // Roll u >= p_hit -> Miss
    let (miss_dmg, _) = turret_shot_damage(base_dmg, p_hit, 0.99);
    assert_eq!(miss_dmg.total(), 0.0);

    // Roll u < 0.01 -> Wrecking Shot (3.0x damage)
    let (wreck_dmg, is_wreck) = turret_shot_damage(base_dmg, p_hit, 0.005);
    assert!(is_wreck);
    assert_eq!(wreck_dmg.total(), 600.0); // 200 * 3.0 = 600

    // Roll 0.01 <= u < p_hit -> Normal hit with quality Q in [0.50, 1.49]
    let (normal_dmg, is_wreck_norm) = turret_shot_damage(base_dmg, p_hit, 0.50);
    assert!(!is_wreck_norm);
    assert!((normal_dmg.total() - 200.0 * 0.99).abs() < 1e-4);

    // 2. Missile Flight Time, Velocity & DRF Application (plan:C-1)
    // Heavy Missile: maxVelocity = 4,000 m/s, flightTime = 12.0s
    let v_missile = 4_000.0;
    let t_flight = 12.0;
    let max_missile_range = v_missile * t_flight;
    assert_eq!(max_missile_range, 48_000.0); // 48 km

    // Damage Reduction Factor (DRF)
    // Small fast target (sig = 35, vel = 1000) vs Cruiser missile (explosion_radius = 125, explosion_vel = 200, drf = 0.70)
    let drf_fast = missile_damage_factor(35.0, 1000.0, 125.0, 200.0, 0.70);
    // Large slow target (sig = 400, vel = 50)
    let drf_slow = missile_damage_factor(400.0, 50.0, 125.0, 200.0, 0.70);
    assert!(drf_fast < 0.20, "Fast small target takes heavily reduced missile damage");
    assert_eq!(drf_slow, 1.0, "Large slow target takes full missile damage");

    // 3. CombatState from Dogma & 4-Channel Layer Resonances (plan:C-2)
    // Build CombatState for Rifter (587)
    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let cs = ctx.build_ship_combat_state(char_id, ship_id, 587);

    // Verify HP and resistances initialized from Dogma
    assert_eq!(cs.max_shield_hp, 450.0);
    assert!((cs.max_armor_hp - 495.0).abs() < 1e-4);
    assert_eq!(cs.max_structure_hp, 350.0);

    // Minmatar shield has 0% EM resist (resonance = 1.0) and 50% Exp resist (resonance = 0.50)
    assert_eq!(cs.shield_resonance[0], 1.0);  // EM
    assert_eq!(cs.shield_resonance[3], 0.50); // Exp

    // Apply EM damage vs Explosive damage to shield:
    let mut cs_em = cs.clone();
    let (em_applied, _) = cs_em.apply_damage(DamageVector::new(100.0, 0.0, 0.0, 0.0));
    assert_eq!(em_applied, 100.0);
    assert_eq!(cs_em.shield_hp, 350.0); // 450 - 100 * 1.0 = 350

    let mut cs_exp = cs.clone();
    let (exp_applied, _) = cs_exp.apply_damage(DamageVector::new(0.0, 0.0, 0.0, 100.0));
    assert_eq!(exp_applied, 50.0);
    assert_eq!(cs_exp.shield_hp, 400.0); // 450 - 100 * 0.50 = 400 (50% resisted!)

    // Layer penetration: Shield -> Armor -> Structure
    let mut cs_heavy = cs.clone();
    // 500 EM damage: shield takes 450 EM (depletes shield to 0), remaining 50 EM hits Armor
    // Armor EM resonance on Minmatar is 0.40: 50 * 0.40 = 20 damage to armor
    let (heavy_applied, destroyed) = cs_heavy.apply_damage(DamageVector::new(500.0, 0.0, 0.0, 0.0));
    assert!(!destroyed);
    assert_eq!(cs_heavy.shield_hp, 0.0);
    assert!((cs_heavy.armor_hp - 475.0).abs() < 1e-4); // 495 - 20 = 475
    assert_eq!(heavy_applied, 470.0); // 450 (shield) + 20 (armor) = 470

    // 4. OnDamageStateChange Notification Emission (plan:C-2)
    let mut ballpark = eve_destiny::Ballpark::new(30_000_142);
    let target_ball_id = 990_000_050i64;
    ballpark.add_ball(Ball {
        id: target_ball_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.2,
        mass: 1_350_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    ballpark.combat_states.insert(target_ball_id, cs.clone());

    // Apply damage to ball via Ballpark -> emits DestinyEvent::OnDamageStateChange
    let _ = ballpark.apply_damage_to_ball(target_ball_id, DamageVector::new(100.0, 0.0, 0.0, 0.0));
    let events = ballpark.step(1.0);
    let dmg_event = events
        .iter()
        .find(|e| matches!(e, DestinyEvent::OnDamageStateChange { ball_id, .. } if *ball_id == target_ball_id));
    assert!(dmg_event.is_some(), "Ballpark must emit DestinyEvent::OnDamageStateChange when ball takes damage");

    if let Some(DestinyEvent::OnDamageStateChange { shield_ratio, armor_ratio, structure_ratio, .. }) = dmg_event {
        assert!(*shield_ratio < 1.0, "Shield ratio must be below 1.0");
        assert_eq!(*armor_ratio, 1.0);
        assert_eq!(*structure_ratio, 1.0);
    }

    // Verify ServerContext notify_damage_state_change
    let notifs = ctx.notify_damage_state_change(
        NotificationTarget::Character(char_id),
        target_ball_id,
        0.77,
        1.0,
        1.0,
    );
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0].event, "OnDamageStateChange");

    eve_testkit::attest(&[
        "plan:C-1",
        "plan:C-2",
    ]);
}
