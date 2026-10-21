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

use crate::common;

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

fn grant_all_required_skills_for_type(ctx: &ServerContext, char_id: i64, type_id: u32) {
    let Some(t) = ctx.sde.types.get(&type_id) else {
        return;
    };
    let pairs = [
        (eve_sde::attr_ids::REQUIRED_SKILL_1, eve_sde::attr_ids::REQUIRED_SKILL_1_LEVEL),
        (eve_sde::attr_ids::REQUIRED_SKILL_2, eve_sde::attr_ids::REQUIRED_SKILL_2_LEVEL),
        (eve_sde::attr_ids::REQUIRED_SKILL_3, eve_sde::attr_ids::REQUIRED_SKILL_3_LEVEL),
    ];
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.skills.entry(char_id).or_default();
        for (s_attr, l_attr) in pairs {
            if let Some(&sid_f) = t.default_attributes.get(&s_attr) {
                let sid = sid_f as u32;
                if sid > 0 {
                    let lvl = t
                        .default_attributes
                        .get(&l_attr)
                        .copied()
                        .unwrap_or(5.0)
                        .max(5.0) as u8;
                    entry.insert(
                        sid,
                        eve_db::TrainedSkillRecord {
                            type_id: sid,
                            level: lvl,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
        }
    });
}

fn extract_item_attr(info_rep: &PyRep, attr_id: i64) -> f64 {
    let attrs = info_rep.dict_get("attributes").expect("attributes dict in ItemGetInfo");
    if let PyRep::Dict(entries) = attrs {
        for (k, v) in entries {
            if k.as_i64() == Some(attr_id) {
                return v.as_f64().unwrap_or(0.0);
            }
        }
    }
    0.0
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
        "Add",
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
        "Add",
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

    let char_id = 90_000_001i64;
    let ship_id = 100_000_001i64;
    let station_id = 60_003_760i64;

    // Upgrade attacker hull to Scorpion (640, group 27 Battleship, 8 MedSlots) while docked
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_item) = snap.items.get_mut(&ship_id) {
            ship_item.type_id = 640;
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    for tid in [640u32, 1957, 580, 2108, 37543, 29005, 29007, 40334, 40335, 2889, 499, 210] {
        grant_all_required_skills_for_type(&ctx, char_id, tid);
    }
    grant_skills(&ctx, char_id, &[
        (3426, 5), // CPU Management
        (3413, 5), // Power Grid Management
        (3418, 5), // Capacitor Management
        (3417, 5), // Capacitor Systems Operation
        (3427, 5), // Electronic Warfare
        (3435, 5), // Weapon Disruption
    ]);

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

    // 2. Verify ECM Jammer & Burst Jammer fitting and activation via dogmaIM.Activate (feat:modules.ecm)
    let ecm_item = DogmaItemState::from_sde(5, 1957, &ctx.sde);
    let jam_opt = compute_ecm_jam_against_ship(&ecm_item, &merlin_state, 10_000.0, &ctx.sde);
    assert!((jam_opt - 0.20).abs() < 1e-4);
    let jam_falloff = compute_ecm_jam_against_ship(&ecm_item, &merlin_state, 19_200.0 + 18_000.0, &ctx.sde);
    assert!((jam_falloff - 0.10).abs() < 1e-4);

    // Fit Multispectral ECM I (1957), Burst Jammer I (580), Tracking Disruptor I (2108), and Guidance Disruptor I (37543) via invbroker.Add
    let ecm_mod_id = 100_000_931i64;
    let burst_mod_id = 100_000_932i64;
    let td_mod_id = 100_000_933i64;
    let gd_mod_id = 100_000_934i64;
    seed_test_item(&ctx, ecm_mod_id, 1957, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, burst_mod_id, 580, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, td_mod_id, 2108, char_id, station_id, InventoryFlag::Hangar, 1);
    seed_test_item(&ctx, gd_mod_id, 37543, char_id, station_id, InventoryFlag::Hangar, 1);

    let opt_script_id = 100_000_935i64;
    let trk_script_id = 100_000_936i64;
    let m_rng_script_id = 100_000_937i64;
    let m_prec_script_id = 100_000_938i64;
    seed_test_item(&ctx, opt_script_id, 29005, char_id, ship_id, InventoryFlag::Cargo, 1);
    seed_test_item(&ctx, trk_script_id, 29007, char_id, ship_id, InventoryFlag::Cargo, 1);
    seed_test_item(&ctx, m_rng_script_id, 40334, char_id, ship_id, InventoryFlag::Cargo, 1);
    seed_test_item(&ctx, m_prec_script_id, 40335, char_id, ship_id, InventoryFlag::Cargo, 1);

    for (mid, flag) in [
        (ecm_mod_id, InventoryFlag::MedSlot0),
        (burst_mod_id, InventoryFlag::MedSlot1),
        (td_mod_id, InventoryFlag::MedSlot2),
        (gd_mod_id, InventoryFlag::MedSlot3),
    ] {
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "Add",
            &[
                PyRep::Integer(mid),
                PyRep::Integer(ship_id),
                PyRep::Integer(flag as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("Fit EWAR module into MedSlot via invbroker.Add");
    }

    // Undock into solar system 30_000_142
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let sys_id = sess.solar_system_id.expect("solar_system_id after Undock");
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (_, ship_ball) = sys
        .get_ball_snapshot(ship_id)
        .await
        .unwrap()
        .expect("ship ball in space");

    // Seed target ship (victim_id) fitted with 200mm AutoCannon II (2889) and Light Missile Launcher I (499) + Scourge Light Missile (210)
    let target_char_id = 90_000_002i64;
    for tid in [587u32, 2889, 499, 210] {
        grant_all_required_skills_for_type(&ctx, target_char_id, tid);
    }
    grant_skills(&ctx, target_char_id, &[
        (3426, 5), // CPU Management
        (3413, 5), // Power Grid Management
    ]);
    let victim_id = 100_000_940i64;
    let target_turret_id = 100_000_941i64;
    let target_launcher_id = 100_000_942i64;
    let target_missile_id = 100_000_943i64;
    seed_test_item(&ctx, victim_id, 587, target_char_id, sys_id as i64, InventoryFlag::None, 1);
    seed_test_item(&ctx, target_turret_id, 2889, target_char_id, victim_id, InventoryFlag::HiSlot0, 1);
    seed_test_item(&ctx, target_launcher_id, 499, target_char_id, victim_id, InventoryFlag::HiSlot1, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            target_missile_id,
            InventoryItemRecord {
                item_id: target_missile_id,
                type_id: 210, // Scourge Light Missile
                owner_id: target_char_id,
                location_id: victim_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 40,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Spawn victim_id at 5 km (inside 10 km Burst Jammer range) and far_id at 25 km (outside 10 km Burst Jammer range)
    let far_id = 100_000_945i64;
    sys.spawn_ball(Ball {
        id: victim_id,
        type_id: 587,
        position: ship_ball.position + DVec3::new(5_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.2,
        mass: 1_350_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .expect("spawn victim ball");
    sys.spawn_ball(Ball {
        id: far_id,
        type_id: 587,
        position: ship_ball.position + DVec3::new(25_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.2,
        mass: 1_350_000.0,
        radius: 35.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    })
    .await
    .expect("spawn far ball");

    // Lock victim_id from attacker ship_id, and give victim_id a lock on ship_id
    sys.add_target_lock(
        ship_id,
        TargetLock {
            target_id: victim_id,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();
    sys.add_target_lock(
        victim_id,
        TargetLock {
            target_id: ship_id,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();
    assert_eq!(sys.get_target_locks(victim_id).await.unwrap().len(), 1);

    // Activate Multispectral ECM I (1957) on victim_id via dogmaIM.Activate until probabilistic CSPRNG jam roll lands
    let mut ecm_jammed = false;
    for _ in 0..100 {
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(ecm_mod_id),
                PyRep::String("targetHostile".into()),
                PyRep::Integer(victim_id),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Multispectral ECM I via dogmaIM.Activate");
        let jammed_s = sys
            .get_combat_state(victim_id)
            .await
            .unwrap()
            .unwrap()
            .ecm_jammed_remaining_s;
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Deactivate",
            &[
                PyRep::Integer(ecm_mod_id),
                PyRep::String("targetHostile".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Deactivate Multispectral ECM I");
        if jammed_s > 0.0 {
            ecm_jammed = true;
            break;
        }
    }
    assert!(
        ecm_jammed,
        "Victim must be ECM-jammed after Multispectral ECM I activation"
    );
    assert_eq!(
        sys.get_target_locks(victim_id).await.unwrap().len(),
        0,
        "Targeted Multispectral ECM I activation must break all target locks on victim"
    );

    // Clear victim's initial jam timer, then re-lock from both victim_id (5 km) and far_id (25 km)
    let _ = sys.apply_ecm_jam(victim_id, 0.0).await;
    sys.add_target_lock(
        victim_id,
        TargetLock {
            target_id: ship_id,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();
    sys.add_target_lock(
        far_id,
        TargetLock {
            target_id: ship_id,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        },
    )
    .await
    .unwrap();

    // Activate Burst Jammer I (580, ecmBurstRange = 10,000m) via dogmaIM.Activate until probabilistic CSPRNG jam roll lands on victim_id (5 km)
    let mut burst_jammed = false;
    for _ in 0..100 {
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(burst_mod_id),
                PyRep::String("ecmBurst".into()),
                PyRep::None,
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Burst Jammer I via dogmaIM.Activate");
        assert_eq!(
            sys.get_target_locks(far_id).await.unwrap().len(),
            1,
            "Burst Jammer I must NOT break locks on targets outside its 10km ecmBurstRange (far_id at 25km)"
        );
        assert_eq!(
            sys.get_combat_state(far_id)
                .await
                .unwrap()
                .unwrap()
                .ecm_jammed_remaining_s,
            0.0,
            "far_id at 25km must not be jammed by 10km Burst Jammer I"
        );
        let jammed_s = sys
            .get_combat_state(victim_id)
            .await
            .unwrap()
            .unwrap()
            .ecm_jammed_remaining_s;
        ctx.dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Deactivate",
            &[
                PyRep::Integer(burst_mod_id),
                PyRep::String("ecmBurst".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("Deactivate Burst Jammer I");
        if jammed_s > 0.0 {
            burst_jammed = true;
            break;
        }
    }
    assert!(
        burst_jammed,
        "Victim at 5km must be jammed by Burst Jammer I"
    );
    assert_eq!(
        sys.get_target_locks(victim_id).await.unwrap().len(),
        0,
        "Burst Jammer I must break locks on targets within its 10km ecmBurstRange (victim at 5km)"
    );

    // 3. Verify Remote Sensor Dampener & Scripts (feat:modules.sensor-dampener)
    let damp_item = DogmaItemState::from_sde(6, 1968, &ctx.sde);
    let (damp_scan_un, damp_range_un) = eval_scripted_sensor_dampener(&damp_item, None, 10_000.0, &ctx.sde);
    assert!(damp_scan_un < 1.0 && damp_range_un < 1.0);
    assert!((damp_scan_un - (1.0 - 0.137)).abs() < 1e-4);
    assert!((damp_range_un - (1.0 - 0.137)).abs() < 1e-4);

    let (damp_scan_scr, damp_range_scr) = eval_scripted_sensor_dampener(&damp_item, Some(29013), 10_000.0, &ctx.sde);
    assert!(damp_scan_scr < damp_scan_un, "Scripted scan dampening must double scan resolution reduction");
    assert_eq!(damp_range_scr, 1.0, "Scan resolution script zeroes range reduction");

    let (damp_scan_rng, damp_range_rng) = eval_scripted_sensor_dampener(&damp_item, Some(29015), 10_000.0, &ctx.sde);
    assert_eq!(damp_scan_rng, 1.0, "Targeting range script zeroes scan res reduction");
    assert!(damp_range_rng < damp_range_un, "Scripted range dampening must double range reduction");

    // 4. Verify Tracking Disruptor & Guidance Disruptor with Scripts via dogmaIM.Activate (feat:modules.tracking-disruptor)
    // Baseline target turret (2889 200mm AutoCannon II) and missile launcher (499 Light Missile Launcher I + 210 Scourge Light Missile) attributes
    let base_turret_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(target_turret_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo baseline turret");
    let base_optimal = extract_item_attr(&base_turret_info, 54);
    let base_falloff = extract_item_attr(&base_turret_info, 158);
    let base_tracking = extract_item_attr(&base_turret_info, 160);
    assert!(base_optimal > 0.0 && base_falloff > 0.0 && base_tracking > 0.0);

    let base_launcher_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(target_launcher_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo baseline launcher");
    let base_missile_vel = extract_item_attr(&base_launcher_info, 37);
    let base_flight_time = extract_item_attr(&base_launcher_info, 281);
    let base_exp_vel = extract_item_attr(&base_launcher_info, 653);
    let base_exp_radius = extract_item_attr(&base_launcher_info, 654);
    assert!(
        base_missile_vel > 0.0
            && base_flight_time > 0.0
            && base_exp_vel > 0.0
            && base_exp_radius > 0.0
    );

    // 4a. Unscripted Tracking Disruptor I (2108) -> reduces turret optimal (54), falloff (158), and trackingSpeed (160)
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(td_mod_id),
            PyRep::String("shipModuleTrackingDisruptor".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate unscripted Tracking Disruptor I");
    let td_unscripted_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(target_turret_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo under unscripted Tracking Disruptor I");
    let td_un_opt = extract_item_attr(&td_unscripted_info, 54);
    let td_un_fall = extract_item_attr(&td_unscripted_info, 158);
    let td_un_trk = extract_item_attr(&td_unscripted_info, 160);
    assert!(td_un_opt < base_optimal, "Unscripted TD must reduce target turret optimal range");
    assert!(td_un_fall < base_falloff, "Unscripted TD must reduce target turret falloff");
    assert!(td_un_trk < base_tracking, "Unscripted TD must reduce target turret tracking speed");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(td_mod_id),
            PyRep::String("shipModuleTrackingDisruptor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Tracking Disruptor I");

    // 4b. Tracking Disruptor I loaded with Optimal Range Disruption Script (29005) -> doubles range/falloff penalty, zeroes tracking penalty
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(opt_script_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot2 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Optimal Range Disruption Script into Tracking Disruptor I");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(td_mod_id),
            PyRep::String("shipModuleTrackingDisruptor".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Optimal Range scripted Tracking Disruptor I");
    let td_opt_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(target_turret_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo under Optimal Range scripted TD");
    let td_opt_opt = extract_item_attr(&td_opt_info, 54);
    let td_opt_fall = extract_item_attr(&td_opt_info, 158);
    let td_opt_trk = extract_item_attr(&td_opt_info, 160);
    assert!(td_opt_opt < td_un_opt, "Optimal Range Disruption Script must double optimal range reduction");
    assert!(td_opt_fall < td_un_fall, "Optimal Range Disruption Script must double falloff reduction");
    assert!((td_opt_trk - base_tracking).abs() < 1e-4, "Optimal Range Disruption Script must leave tracking speed unpenalized");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(td_mod_id),
            PyRep::String("shipModuleTrackingDisruptor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Optimal Range scripted TD");

    // 4c. Tracking Disruptor I loaded with Tracking Speed Disruption Script (29007) -> doubles tracking penalty, zeroes range/falloff penalty
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(trk_script_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot2 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Tracking Speed Disruption Script into Tracking Disruptor I");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(td_mod_id),
            PyRep::String("shipModuleTrackingDisruptor".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Tracking Speed scripted Tracking Disruptor I");
    let td_trk_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(target_turret_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo under Tracking Speed scripted TD");
    let td_trk_opt = extract_item_attr(&td_trk_info, 54);
    let td_trk_fall = extract_item_attr(&td_trk_info, 158);
    let td_trk_trk = extract_item_attr(&td_trk_info, 160);
    assert!((td_trk_opt - base_optimal).abs() < 1e-4, "Tracking Speed Disruption Script must leave optimal range unpenalized");
    assert!((td_trk_fall - base_falloff).abs() < 1e-4, "Tracking Speed Disruption Script must leave falloff unpenalized");
    assert!(td_trk_trk < td_un_trk, "Tracking Speed Disruption Script must double tracking speed reduction");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(td_mod_id),
            PyRep::String("shipModuleTrackingDisruptor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Tracking Speed scripted TD");

    // 4d. Unscripted Guidance Disruptor I (37543) -> reduces missile velocity (37), flight time (281), explosion velocity (653), and expands explosion radius (654)
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(gd_mod_id),
            PyRep::String("shipModuleGuidanceDisruptor".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate unscripted Guidance Disruptor I");
    let gd_un_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(target_launcher_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo under unscripted Guidance Disruptor I");
    let gd_un_vel = extract_item_attr(&gd_un_info, 37);
    let gd_un_time = extract_item_attr(&gd_un_info, 281);
    let gd_un_exp_vel = extract_item_attr(&gd_un_info, 653);
    let gd_un_exp_rad = extract_item_attr(&gd_un_info, 654);
    assert!(gd_un_vel < base_missile_vel, "Unscripted Guidance Disruptor must reduce target missile velocity");
    assert!(gd_un_time < base_flight_time, "Unscripted Guidance Disruptor must reduce target missile flight time");
    assert!(gd_un_exp_vel < base_exp_vel, "Unscripted Guidance Disruptor must reduce target missile explosion velocity");
    assert!(gd_un_exp_rad > base_exp_radius, "Unscripted Guidance Disruptor must increase target missile explosion radius");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(gd_mod_id),
            PyRep::String("shipModuleGuidanceDisruptor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate unscripted Guidance Disruptor I");

    // 4e. Guidance Disruptor I loaded with Missile Range Disruption Script (40334) -> doubles velocity/flight-time penalty, zeroes precision penalty
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(m_rng_script_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot3 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Missile Range Disruption Script into Guidance Disruptor I");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(gd_mod_id),
            PyRep::String("shipModuleGuidanceDisruptor".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Missile Range scripted Guidance Disruptor I");
    let gd_rng_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(target_launcher_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo under Missile Range scripted GD");
    let gd_rng_vel = extract_item_attr(&gd_rng_info, 37);
    let gd_rng_time = extract_item_attr(&gd_rng_info, 281);
    let gd_rng_exp_vel = extract_item_attr(&gd_rng_info, 653);
    let gd_rng_exp_rad = extract_item_attr(&gd_rng_info, 654);
    assert!(gd_rng_vel < gd_un_vel, "Missile Range Disruption Script must double missile velocity reduction");
    assert!(gd_rng_time < gd_un_time, "Missile Range Disruption Script must double missile flight time reduction");
    assert!((gd_rng_exp_vel - base_exp_vel).abs() < 1e-4, "Missile Range Disruption Script must leave explosion velocity unpenalized");
    assert!((gd_rng_exp_rad - base_exp_radius).abs() < 1e-4, "Missile Range Disruption Script must leave explosion radius unpenalized");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(gd_mod_id),
            PyRep::String("shipModuleGuidanceDisruptor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Missile Range scripted GD");

    // 4f. Guidance Disruptor I loaded with Missile Precision Disruption Script (40335) -> doubles explosion velocity/radius penalty, zeroes range penalty
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(m_prec_script_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::MedSlot3 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Load Missile Precision Disruption Script into Guidance Disruptor I");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(gd_mod_id),
            PyRep::String("shipModuleGuidanceDisruptor".into()),
            PyRep::Integer(victim_id),
            PyRep::Integer(1),
        ],
        &kw,
        None,
    )
    .await
    .expect("Activate Missile Precision scripted Guidance Disruptor I");
    let gd_prec_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(target_launcher_id)],
            &kw,
            None,
        )
        .await
        .expect("ItemGetInfo under Missile Precision scripted GD");
    let gd_prec_vel = extract_item_attr(&gd_prec_info, 37);
    let gd_prec_time = extract_item_attr(&gd_prec_info, 281);
    let gd_prec_exp_vel = extract_item_attr(&gd_prec_info, 653);
    let gd_prec_exp_rad = extract_item_attr(&gd_prec_info, 654);
    assert!((gd_prec_vel - base_missile_vel).abs() < 1e-4, "Missile Precision Disruption Script must leave missile velocity unpenalized");
    assert!((gd_prec_time - base_flight_time).abs() < 1e-4, "Missile Precision Disruption Script must leave missile flight time unpenalized");
    assert!(gd_prec_exp_vel < gd_un_exp_vel, "Missile Precision Disruption Script must double explosion velocity reduction");
    assert!(gd_prec_exp_rad > gd_un_exp_rad, "Missile Precision Disruption Script must double explosion radius expansion");
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "Deactivate",
        &[
            PyRep::Integer(gd_mod_id),
            PyRep::String("shipModuleGuidanceDisruptor".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Deactivate Missile Precision scripted GD");

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
    let mut ballpark = eve_destiny::Ballpark::new(30_000_142);
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
