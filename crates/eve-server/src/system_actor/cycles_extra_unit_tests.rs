//! Deep branch and mutation-killing unit tests for `system_actor/cycles.rs`.

#![forbid(unsafe_code)]

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, Ballpark, CombatState, DamageVector, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

fn make_ship(id: BallId, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_500_000.0,
        radius: 50.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

fn make_ball(id: BallId, type_id: u32, pos: DVec3) -> Ball {
    make_ship(id, type_id, pos)
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && matches!(flag, InventoryFlag::LoSlot0 | InventoryFlag::HiSlot0 | InventoryFlag::HiSlot1 | InventoryFlag::MedSlot0 | InventoryFlag::MedSlot1 | InventoryFlag::MedSlot2),
        ..Default::default()
    }
}

fn make_extra_sde() -> Arc<SdeRegistry> {
    Arc::new(SdeRegistry::with_canonical_seed())
}

#[test]
fn step_rah_resonances_and_is_capital_remote_repair_module_cover_all_branches() {
    // 1. Zero damage fallback -> [1.0, 0.0, 0.0, 0.0] decreases EM resonance by 0.06 and adds 0.02 to each of the other 3
    let res = step_rah_resonances([0.85, 0.85, 0.85, 0.85], [0.0, 0.0, 0.0, 0.0], 6.0);
    assert!((res[0] - 0.79).abs() < 1e-9);
    assert!((res[1] - 0.87).abs() < 1e-9);
    assert!((res[2] - 0.87).abs() < 1e-9);
    assert!((res[3] - 0.87).abs() < 1e-9);

    // 2. Single-type kinetic damage when EM/Therm are near floor (1.0) clamps at 1.0 and only shifts available pool
    let near_floor = step_rah_resonances([0.99, 0.99, 0.50, 0.92], [0.0, 0.0, 500.0, 0.0], 6.0);
    assert!((near_floor[0] - 1.0).abs() < 1e-9);
    assert!((near_floor[1] - 1.0).abs() < 1e-9);
    assert!((near_floor[2] - 0.46).abs() < 1e-9);
    assert!((near_floor[3] - 0.94).abs() < 1e-9);

    // 3. Multi-type equal EM + Thermal damage shifts from Kin + Exp donors (note dmg is [em, therm, kin, exp] -> [em, exp, kin, therm])
    let multi = step_rah_resonances([0.85, 0.85, 0.85, 0.85], [100.0, 100.0, 0.0, 0.0], 6.0);
    assert!((multi[0] - 0.82).abs() < 1e-9);
    assert!((multi[1] - 0.88).abs() < 1e-9);
    assert!((multi[2] - 0.88).abs() < 1e-9);
    assert!((multi[3] - 0.82).abs() < 1e-9);

    // 4. is_capital_remote_repair_module checks skill reqs (24568, 24571, 27936) and powergrid >= 10,000
    let mut sde = SdeRegistry::with_canonical_seed();
    let base = sde.types.values().next().unwrap().clone();

    let mut cap_skill_mod = base.clone();
    cap_skill_mod.type_id = 940_001;
    cap_skill_mod.default_attributes.clear();
    cap_skill_mod.default_attributes.insert(eve_sde::attr_ids::REQUIRED_SKILL_1, 24568.0);
    sde.types.insert(940_001, cap_skill_mod);

    let mut cap_pg_mod = base.clone();
    cap_pg_mod.type_id = 940_002;
    cap_pg_mod.default_attributes.clear();
    cap_pg_mod.default_attributes.insert(eve_sde::attr_ids::POWER, 12_000.0);
    sde.types.insert(940_002, cap_pg_mod);

    let mut subcap_mod = base;
    subcap_mod.type_id = 940_003;
    subcap_mod.default_attributes.clear();
    subcap_mod.default_attributes.insert(eve_sde::attr_ids::POWER, 50.0);
    sde.types.insert(940_003, subcap_mod);

    let sde_opt = Some(Arc::new(sde));
    assert!(is_capital_remote_repair_module(940_001, &sde_opt));
    assert!(is_capital_remote_repair_module(940_002, &sde_opt));
    assert!(!is_capital_remote_repair_module(940_003, &sde_opt));
    assert!(!is_capital_remote_repair_module(940_001, &None));
}

#[test]
fn tethered_heat_repair_nanite_paste_anchoring_disruptor_and_capital_rr_impedance() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let mut cap_rr = sde.types.values().next().unwrap().clone();
    cap_rr.type_id = 940_010;
    cap_rr.group_id = 41; // Remote Shield Booster
    cap_rr.default_attributes.clear();
    cap_rr.default_attributes.insert(eve_sde::attr_ids::POWER, 15_000.0);
    sde.types.insert(940_010, cap_rr);
    let sde_opt = Some(Arc::new(sde));

    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ship_tethered = 3001_i64;
    let ship_nanite = 3002_i64;
    let ship_rr_2 = 3003_i64;
    let ship_target = 3004_i64;
    let mod_tethered = 8001_i64;
    let mod_nanite = 8002_i64;
    let paste_id = 8003_i64;
    let bub_id = 8004_i64;

    db.with_mut_snapshot(|snap| {
        // Damaged module on tethered ship (1.5 damage -> repaired in 1.0s tick at 2.0 HP/s)
        let mut m1 = eve_db::InventoryItemRecord {
            item_id: mod_tethered,
            type_id: 587,
            owner_id: 90_000_001,
            location_id: ship_tethered,
            flag: eve_db::InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        m1.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 1.5);
        snap.items.insert(mod_tethered, m1);

        // Damaged module on nanite ship + 1 nanite paste in cargo
        let mut m2 = eve_db::InventoryItemRecord {
            item_id: mod_nanite,
            type_id: 587,
            owner_id: 90_000_001,
            location_id: ship_nanite,
            flag: eve_db::InventoryFlag::HiSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        m2.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 10.0);
        snap.items.insert(mod_nanite, m2);
        snap.items.insert(
            paste_id,
            eve_db::InventoryItemRecord {
                item_id: paste_id,
                type_id: 28_668,
                owner_id: 90_000_001,
                location_id: ship_nanite,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut bp = Ballpark::new(30_000_142);
    for id in [ship_tethered, ship_nanite, ship_rr_2, ship_target, bub_id] {
        bp.add_ball(make_ship(id, 587, DVec3::ZERO));
        bp.combat_states.insert(
            id,
            CombatState {
                capacitor: 1_000.0,
                max_capacitor: 1_000.0,
                shield_hp: 100.0,
                max_shield_hp: 1_000.0,
                armor_hp: 100.0,
                max_armor_hp: 1_000.0,
                structure_hp: 100.0,
                max_structure_hp: 1_000.0,
                is_tethered: id == ship_tethered,
                ..CombatState::default()
            },
        );
    }

    let mut nanite_repairs = vec![super::types::ActiveNaniteRepair {
        char_id: 90_000_001,
        ship_id: ship_nanite,
        module_id: mod_nanite,
        initial_damage_hp: 10.0,
        paste_total_needed: 1,
        paste_consumed_so_far: 0,
        duration_s: 1.0,
        elapsed_s: 0.0,
    }];
    let mut anchoring_disruptors = vec![super::types::PendingAnchoringDisruptor {
        id: bub_id,
        type_id: 587,
        position: DVec3::ZERO,
        radius: 20_000.0,
        remaining_s: 0.5,
    }];

    // Two capital remote armor repairers hitting ship_target simultaneously -> rank 0 gets 1.0x (100), rank 1 gets 1/(1+0.25) = 0.8x (80)!
    let mut cycles = vec![
        ActiveModuleCycle {
            ship_id: ship_nanite,
            module_id: 9001,
            module_type_id: 940_010,
            target_id: Some(ship_target),
            remote_armor_amount: 100.0,
            duration_s: 1.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: ship_rr_2,
            module_id: 9002,
            module_type_id: 940_010,
            target_id: Some(ship_target),
            remote_armor_amount: 100.0,
            duration_s: 1.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
    ];

    let mut pending_actions = Vec::new();
    step_cycles_and_capacitor(
        &mut bp,
        &mut cycles,
        &mut anchoring_disruptors,
        &mut nanite_repairs,
        &mut HashMap::new(),
        &mut HashMap::new(),
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &Some(Arc::clone(&db)),
        &None,
        &Arc::new(RwLock::new(Default::default())),
        &mut pending_actions,
        1.0,
        None,
    );

    // Verify tethered heat damage was completely removed, nanite repair completed & consumed paste, bubble activated, and capital RR impedance applied (100 + 100 + 80 = 280)
    db.with_snapshot(|snap| {
        assert!(!snap.items[&mod_tethered].dynamic_attributes.contains_key(&eve_sde::attr_ids::DAMAGE));
        assert!(!snap.items[&mod_nanite].dynamic_attributes.contains_key(&eve_sde::attr_ids::DAMAGE));
        assert!(!snap.items.contains_key(&paste_id));
    });
    assert!(nanite_repairs.is_empty());
    assert!(anchoring_disruptors.is_empty());
    assert_eq!(bp.balls[&bub_id].mode, MovementMode::Field);
    assert!((bp.combat_states[&ship_target].armor_hp - 280.0).abs() < 1e-6);
}

#[test]
fn rah_mutadaptive_nosferatu_override_ehe_burnout_asb_aar_and_salvager_cycles() {
    let mut sde = SdeRegistry::with_canonical_seed();
    let base = sde.types.values().next().unwrap().clone();
    // 940_020: Sleeper NPC target (group 959)
    let mut sleeper_t = base.clone();
    sleeper_t.type_id = 940_020;
    sleeper_t.group_id = 959;
    sde.types.insert(940_020, sleeper_t);
    // 940_021: Salvager (group 1122)
    let mut salv_t = base;
    salv_t.type_id = 940_021;
    salv_t.group_id = 1122;
    sde.types.insert(940_021, salv_t);
    let sde_opt = Some(Arc::new(sde));

    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ship_id = 4001_i64;
    let target_ship = 4002_i64;
    let sleeper_ball = 4003_i64;
    let ehe_mod_id = 9101_i64;
    let rah_mod_id = 9102_i64;
    let muta_mod_id = 9103_i64;
    let asb_mod_id = 9104_i64;
    let asb_charge_id = 9105_i64;
    let aar_mod_id = 9106_i64;
    let aar_paste_id = 9107_i64;

    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ehe_mod_id,
            eve_db::InventoryItemRecord {
                item_id: ehe_mod_id,
                type_id: 40_714,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            asb_mod_id,
            eve_db::InventoryItemRecord {
                item_id: asb_mod_id,
                type_id: 32_772,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            asb_charge_id,
            eve_db::InventoryItemRecord {
                item_id: asb_charge_id,
                type_id: 32_014,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::MedSlot0,
                quantity: 2,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            aar_mod_id,
            eve_db::InventoryItemRecord {
                item_id: aar_mod_id,
                type_id: 33_101,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::LoSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            aar_paste_id,
            eve_db::InventoryItemRecord {
                item_id: aar_paste_id,
                type_id: 28_668,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::LoSlot1,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(make_ship(ship_id, 587, DVec3::ZERO));
    bp.add_ball(make_ship(target_ship, 587, DVec3::new(2_000.0, 0.0, 0.0)));
    bp.add_ball(make_ship(sleeper_ball, 940_020, DVec3::new(3_000.0, 0.0, 0.0)));
    for id in [ship_id, target_ship, sleeper_ball] {
        bp.combat_states.insert(
            id,
            CombatState {
                capacitor: if id == ship_id { 500.0 } else { 300.0 },
                max_capacitor: 1_000.0,
                shield_hp: 100.0,
                max_shield_hp: 1_000.0,
                armor_hp: 100.0,
                max_armor_hp: 1_000.0,
                structure_hp: 100.0,
                max_structure_hp: 1_000.0,
                ..CombatState::default()
            },
        );
    }

    let mut rah_accum = HashMap::from([(ship_id, [200.0, 0.0, 0.0, 0.0])]);
    let mut cycles = vec![
        // 1. EHE (burns out after 1 cycle!)
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(90_000_001),
            module_id: ehe_mod_id,
            module_type_id: 40_714,
            ehe_hull_resists: Some([0.9, 0.9, 0.9, 0.9]),
            duration_s: 1.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
        // 2. RAH (adapts armor_resonance on cycle completion)
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(90_000_001),
            module_id: rah_mod_id,
            is_rah: true,
            rah_shift_amount: 6.0,
            duration_s: 1.0,
            repeat: 1,
            ..ActiveModuleCycle::default()
        },
        // 3. Mutadaptive Remote Armor Repairer + Blood Raider Nosferatu override on target_ship
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(90_000_001),
            module_id: muta_mod_id,
            target_id: Some(target_ship),
            remote_armor_amount: 100.0,
            mutadaptive_spool_step: 0.10,
            mutadaptive_spool_max: 1.50,
            current_spool_cycles: 1,
            cap_transfer_target: 50.0,
            nosferatu_blood_raider_override: true,
            duration_s: 1.0,
            repeat: 1,
            ..ActiveModuleCycle::default()
        },
        // 4. Nosferatu on Sleeper NPC (grants cap to ship_id without draining sleeper_ball)
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(90_000_001),
            module_id: 9108,
            target_id: Some(sleeper_ball),
            cap_transfer_target: 40.0,
            nosferatu_blood_raider_override: true,
            duration_s: 1.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
        // 5. ASB repeating cycle (consumes 1 cap booster charge from MedSlot0)
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(90_000_001),
            module_id: asb_mod_id,
            is_asb: true,
            shield_boost_amount: 80.0,
            duration_s: 1.0,
            repeat: 1,
            ..ActiveModuleCycle::default()
        },
        // 6. AAR repeating cycle (consumes nanite paste from LoSlot1 for 3x armor repair)
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(90_000_001),
            module_id: aar_mod_id,
            module_type_id: 33_101,
            is_aar: true,
            aar_paste_rate: 4,
            aar_base_armor_repair: 50.0,
            armor_repair_amount: 150.0,
            duration_s: 1.0,
            repeat: 1,
            ..ActiveModuleCycle::default()
        },
        // 7. Salvager completing cycle -> emits PendingCycleAction::SalvageEnd
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(90_000_001),
            module_id: 9109,
            module_type_id: 940_021,
            target_id: Some(target_ship),
            duration_s: 1.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
    ];

    let mut pending_actions = Vec::new();
    step_cycles_and_capacitor(
        &mut bp,
        &mut cycles,
        &mut Vec::new(),
        &mut Vec::new(),
        &mut HashMap::new(),
        &mut rah_accum,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &Some(Arc::clone(&db)),
        &None,
        &Arc::new(RwLock::new(Default::default())),
        &mut pending_actions,
        1.0,
        None,
    );

    db.with_snapshot(|snap| {
        // EHE burned out and offlined
        assert_eq!(snap.module_online_states.get(&ehe_mod_id).copied(), Some(false));
        assert!(snap.items[&ehe_mod_id].dynamic_attributes[&eve_sde::attr_ids::DAMAGE] >= 40.0);
        // RAH updated transient attribute 267 (EM resonance = 0.79)
        assert!((snap.transient_item_attributes[&rah_mod_id][&267] - 0.79).abs() < 1e-6);
        // Mutadaptive spooled attr 84 (100 * 1.10 = 110.0)
        assert!((snap.transient_item_attributes[&muta_mod_id][&84] - 110.0).abs() < 1e-6);
        // ASB consumed 1 charge (2 -> 1), AAR consumed 4 paste (10 -> 6)
        assert_eq!(snap.items[&asb_charge_id].quantity, 1);
        assert_eq!(snap.items[&aar_paste_id].quantity, 6);
    });
    // Blood Raider Nosferatu drained target_ship even though ship_id had higher cap (300 -> 250)
    assert!((bp.combat_states[&target_ship].capacitor - 250.0).abs() < 1e-6);
    // Sleeper NPC kept its 300 capacitor while granting cap to ship_id
    assert!((bp.combat_states[&sleeper_ball].capacitor - 300.0).abs() < 1e-6);
    assert!(pending_actions
        .iter()
        .any(|a| matches!(a, PendingCycleAction::SalvageEnd { module_id: 9109, .. })));
}

#[test]
fn mjd_disabled_guard_cleanup_removed_cycle_and_overload_transition_boundaries() {
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let mut sde = eve_sde::SdeRegistry::with_canonical_seed();

    // Register a category-5 overload effect + custom module types for apply_overload_transition_to_cycle
    let base_type = sde.types[&587].clone();
    let cat5_eid = 59_901_u16;
    let mut cat5_eff = sde.effects.values().next().unwrap().clone();
    cat5_eff.effect_id = cat5_eid;
    cat5_eff.effect_category = 5;
    sde.effects.insert(cat5_eid, cat5_eff);

    // 999_801: MWD with overloadSpeedFactorBonus (1223 = 50%), 1206 = -15%
    let mut t_mwd = base_type.clone();
    t_mwd.type_id = 999_801;
    t_mwd.default_attributes.clear();
    t_mwd.default_attributes.insert(1223, 50.0);
    t_mwd.default_attributes.insert(1206, -15.0);
    sde.types.insert(999_801, t_mwd);

    // 999_802: Repairer WITHOUT cat5 effect and WITHOUT 1230 -> hits 1.20 fallback
    let mut t_rep_nocat5 = base_type.clone();
    t_rep_nocat5.type_id = 999_802;
    t_rep_nocat5.default_attributes.clear();
    t_rep_nocat5.effect_ids.clear();
    sde.types.insert(999_802, t_rep_nocat5);

    // 999_803: Repairer WITH cat5 effect and WITHOUT 1230 -> must NOT hit 1.20 fallback (uses 1.0)
    let mut t_rep_cat5 = base_type.clone();
    t_rep_cat5.type_id = 999_803;
    t_rep_cat5.default_attributes.clear();
    t_rep_cat5.effect_ids = vec![cat5_eid];
    sde.types.insert(999_803, t_rep_cat5);

    // 999_804: Grappler with 1222 = 300% (4.0x range)
    let mut t_grappler = base_type;
    t_grappler.type_id = 999_804;
    t_grappler.default_attributes.clear();
    t_grappler.default_attributes.insert(1222, 300.0);
    sde.types.insert(999_804, t_grappler);

    let sde_opt = Some(Arc::new(sde));
    let db_opt = Some(Arc::clone(&db));
    let tact = RwLock::new(crate::services::tactical_modules::TacticalModulesState::default());

    let ship_id = 88_100_001_i64;
    let tgt_id = 88_100_002_i64;
    let mut bp = Ballpark::new(30_000_142);

    let mut ship = make_ship(ship_id, 587, DVec3::new(0.0, 0.0, 0.0));
    ship.velocity = DVec3::new(100.0, 0.0, 0.0);
    bp.add_ball(ship);
    bp.add_ball(make_ship(tgt_id, 587, DVec3::new(3_000.0, 0.0, 0.0)));

    let mut cs = CombatState {
        capacitor: 1_000.0,
        max_capacitor: 1_000.0,
        prop_velocity_multiplier: 3.0,
        is_mwd_mjd_disabled: true,
        decloak_targeting_delay_remaining_s: 12.0,
        cloak_configured_targeting_delay_s: 5.0,
        cloak_velocity_multiplier: Some(0.1),
        adc_active: true,
        ehe_hull_resists: Some([0.1, 0.1, 0.1, 0.1]),
        rah_armor_resonance: Some([0.85, 0.85, 0.85, 0.85]),
        module_immobilized: true,
        ..Default::default()
    };
    cs.is_interdiction_nullified = true;
    bp.combat_states.insert(ship_id, cs);

    // 1. MJD cycle with is_mwd_mjd_disabled == true completes cycle WITHOUT jumping
    let mut cycles = vec![ActiveModuleCycle {
        ship_id,
        module_id: 90_101,
        module_type_id: 4383,
        effect_name: "microJumpDrive".into(),
        duration_s: 1.0,
        repeat: 0,
        is_mwd_or_mjd: true,
        mjd_jump_distance_m: Some(100_000.0),
        ..Default::default()
    }];
    let mut pad = Vec::new();
    let mut nan = Vec::new();
    let mut heat = HashMap::new();
    let mut rah_dmg = HashMap::new();
    let mut pending = Vec::new();
    cycles::step_cycles_and_capacitor(
        &mut bp,
        &mut cycles,
        &mut pad,
        &mut nan,
        &mut heat,
        &mut rah_dmg,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        &None,
        &tact,
        &mut pending,
        1.0,
        None,
    );
    assert!(bp.balls[&ship_id].position.length() < 1.0, "MJD blocked by scram/disabled flag");

    // With is_mwd_mjd_disabled == false, MJD jumps 100 km
    bp.combat_states.get_mut(&ship_id).unwrap().is_mwd_mjd_disabled = false;
    cycles.push(ActiveModuleCycle {
        ship_id,
        module_id: 90_102,
        module_type_id: 4383,
        effect_name: "microJumpDrive".into(),
        duration_s: 1.0,
        repeat: 0,
        is_mwd_or_mjd: true,
        mjd_jump_distance_m: Some(100_000.0),
        ..Default::default()
    });
    cycles::step_cycles_and_capacitor(
        &mut bp,
        &mut cycles,
        &mut pad,
        &mut nan,
        &mut heat,
        &mut rah_dmg,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        &None,
        &tact,
        &mut pending,
        1.0,
        None,
    );
    assert!((bp.balls[&ship_id].position.length() - 100_000.0).abs() < 10.0, "MJD executes 100 km jump");
    bp.balls.get_mut(&ship_id).unwrap().position = DVec3::ZERO;

    // 2. cleanup_removed_module_cycle: max() on decloak delay, transient cleanup, HIC bubble, nullification, DB sets
    let cloak_mod = 90_201_i64;
    let combined_mod = 90_202_i64;
    bp.add_warp_bubble(combined_mod, DVec3::ZERO, 20_000.0);
    db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(cloak_mod);
        snap.module_active_charges.insert(cloak_mod, 1);
        snap.module_active_states.insert(combined_mod);
        snap.module_active_charges.insert(combined_mod, 2);
        let ship_tr = snap.transient_item_attributes.entry(ship_id).or_default();
        for aid in [267_u16, 274, 109, 113, 999] {
            ship_tr.insert(aid, 0.5);
        }
        let mod_tr = snap.transient_item_attributes.entry(combined_mod).or_default();
        mod_tr.insert(84, 2.0);
        mod_tr.insert(268, 0.4);
        mod_tr.insert(999, 1.0);
    });

    cycles::cleanup_removed_module_cycle(
        &mut bp,
        &ActiveModuleCycle {
            ship_id,
            module_id: cloak_mod,
            effect_name: "cloakingWarpSafe".into(),
            ..Default::default()
        },
        &db_opt,
    );
    {
        let cs_now = &bp.combat_states[&ship_id];
        assert_eq!(cs_now.cloak_velocity_multiplier, None);
        // Existing 12.0 > configured 5.0 must be preserved by .max()
        assert!((cs_now.decloak_targeting_delay_remaining_s - 12.0).abs() < 1e-9);
    }
    // Now set delay < configured 5.0 -> updates to 5.0
    bp.combat_states.get_mut(&ship_id).unwrap().decloak_targeting_delay_remaining_s = 1.0;
    cycles::cleanup_removed_module_cycle(
        &mut bp,
        &ActiveModuleCycle {
            ship_id,
            module_id: cloak_mod,
            effect_name: "cloaking".into(),
            ..Default::default()
        },
        &db_opt,
    );
    assert!((bp.combat_states[&ship_id].decloak_targeting_delay_remaining_s - 5.0).abs() < 1e-9);

    cycles::cleanup_removed_module_cycle(
        &mut bp,
        &ActiveModuleCycle {
            ship_id,
            module_id: combined_mod,
            effect_name: "module".into(),
            is_adc: true,
            ehe_hull_resists: Some([0.1, 0.1, 0.1, 0.1]),
            mutadaptive_spool_step: 0.1,
            is_rah: true,
            immobilize_self: true,
            hic_bubble_radius_m: Some(20_000.0),
            grants_interdiction_nullification: true,
            ..Default::default()
        },
        &db_opt,
    );
    {
        let cs_now = &bp.combat_states[&ship_id];
        assert!(!cs_now.adc_active);
        assert!(cs_now.ehe_hull_resists.is_none());
        assert!(cs_now.rah_armor_resonance.is_none());
        assert!(!cs_now.module_immobilized);
        assert!(!cs_now.is_interdiction_nullified);
        assert!(!bp.warp_bubble_mgr.bubbles.contains_key(&combined_mod));
    }
    db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&combined_mod));
        assert!(!snap.module_active_charges.contains_key(&combined_mod));
        let ship_tr = &snap.transient_item_attributes[&ship_id];
        assert!(!ship_tr.contains_key(&267));
        assert!(!ship_tr.contains_key(&109));
        assert_eq!(ship_tr.get(&999).copied(), Some(0.5));
        let mod_tr = &snap.transient_item_attributes[&combined_mod];
        assert!(!mod_tr.contains_key(&84));
        assert!(!mod_tr.contains_key(&268));
        assert_eq!(mod_tr.get(&999).copied(), Some(1.0));
    });

    // 3. apply_overload_transition_to_cycle boundaries
    bp.combat_states.get_mut(&ship_id).unwrap().prop_velocity_multiplier = 3.0;
    let mut c_mwd = ActiveModuleCycle {
        ship_id,
        module_id: 90_301,
        module_type_id: 999_801,
        effect_name: "moduleBonusMicrowarpdrive".into(),
        speed_boost_factor: 2.0,
        duration_s: 10.0,
        ..Default::default()
    };
    cycles_tackle::apply_overload_transition_to_cycle(&mut c_mwd, false, &mut bp, &sde_opt); // same state noop
    assert!((c_mwd.speed_boost_factor - 2.0).abs() < 1e-9);
    cycles_tackle::apply_overload_transition_to_cycle(&mut c_mwd, true, &mut bp, &sde_opt);
    assert!(c_mwd.is_overloaded);
    assert!((c_mwd.speed_boost_factor - 3.0).abs() < 1e-9);
    assert!((c_mwd.duration_s - 8.5).abs() < 1e-9);
    assert!((bp.combat_states[&ship_id].prop_velocity_multiplier - 4.0).abs() < 1e-9);
    cycles_tackle::apply_overload_transition_to_cycle(&mut c_mwd, false, &mut bp, &sde_opt);
    assert!(!c_mwd.is_overloaded);
    assert!((c_mwd.speed_boost_factor - 2.0).abs() < 1e-9);
    assert!((c_mwd.duration_s - 10.0).abs() < 1e-9);
    assert!((bp.combat_states[&ship_id].prop_velocity_multiplier - 3.0).abs() < 1e-9);

    // cat5 guard on 1.20 repairer fallback
    let mut c_nocat5 = ActiveModuleCycle {
        ship_id,
        module_id: 90_302,
        module_type_id: 999_802,
        armor_repair_amount: 100.0,
        shield_boost_amount: 200.0,
        hull_repair_amount: 50.0,
        ..Default::default()
    };
    cycles_tackle::apply_overload_transition_to_cycle(&mut c_nocat5, true, &mut bp, &sde_opt);
    assert!((c_nocat5.armor_repair_amount - 120.0).abs() < 1e-6);
    assert!((c_nocat5.shield_boost_amount - 240.0).abs() < 1e-6);
    assert!((c_nocat5.hull_repair_amount - 60.0).abs() < 1e-6);

    let mut c_cat5 = ActiveModuleCycle {
        ship_id,
        module_id: 90_303,
        module_type_id: 999_803,
        armor_repair_amount: 100.0,
        shield_boost_amount: 200.0,
        hull_repair_amount: 50.0,
        ..Default::default()
    };
    cycles_tackle::apply_overload_transition_to_cycle(&mut c_cat5, true, &mut bp, &sde_opt);
    assert!((c_cat5.armor_repair_amount - 100.0).abs() < 1e-6);
    assert!((c_cat5.shield_boost_amount - 200.0).abs() < 1e-6);

    // 1222 Grappler overload + un-overload restores optimal/max_effective_range_m + remote_optimal
    let mut c_grap = ActiveModuleCycle {
        ship_id,
        module_id: 90_304,
        module_type_id: 999_804,
        target_id: Some(tgt_id),
        grappler_optimal_m: 1_000.0,
        grappler_falloff_m: 2_000.0,
        grappler_speed_factor_pct: -80.0,
        remote_optimal: 1_000.0,
        max_effective_range_m: Some(7_000.0),
        ..Default::default()
    };
    cycles_tackle::apply_overload_transition_to_cycle(&mut c_grap, true, &mut bp, &sde_opt);
    assert!((c_grap.grappler_optimal_m - 4_000.0).abs() < 1e-6);
    assert!((c_grap.remote_optimal - 4_000.0).abs() < 1e-6);
    assert!((c_grap.max_effective_range_m.unwrap() - 10_000.0).abs() < 1e-6);
    assert!(c_grap.target_velocity_multiplier.is_some());
    cycles_tackle::apply_overload_transition_to_cycle(&mut c_grap, false, &mut bp, &sde_opt);
    assert!((c_grap.grappler_optimal_m - 1_000.0).abs() < 1e-6);
    assert!((c_grap.remote_optimal - 1_000.0).abs() < 1e-6);
    assert!((c_grap.max_effective_range_m.unwrap() - 7_000.0).abs() < 1e-6);
}

#[test]
fn cycles_nanite_exhaustion_crystal_reload_asb_aar_cap_booster_overload_and_lance() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mut sde = (*make_extra_sde()).clone();
    let base = sde.types.values().next().unwrap().clone();
    // Add Smartbomb (group 72) for RepeatCycleStart and Projectile Turret (group 55) + Ammo
    let mut sb_ty = base.clone();
    sb_ty.type_id = 940_072;
    sb_ty.group_id = 72;
    sb_ty.category_id = 7;
    sb_ty.name = "Test Smartbomb".into();
    sb_ty.default_attributes.clear();
    sde.types.insert(940_072, sb_ty);

    let mut ac_ty = base.clone();
    ac_ty.type_id = 940_055;
    ac_ty.group_id = 55;
    ac_ty.category_id = 7;
    ac_ty.name = "Test Autocannon".into();
    ac_ty.capacity = 0.25;
    ac_ty.default_attributes = HashMap::from([(1795u16, 5_000.0)]);
    sde.types.insert(940_055, ac_ty);

    let mut ammo_ty = base;
    ammo_ty.type_id = 940_056;
    ammo_ty.group_id = 83;
    ammo_ty.category_id = 8;
    ammo_ty.name = "Test EMP S".into();
    ammo_ty.volume = 0.005;
    ammo_ty.portion_size = 100;
    ammo_ty.default_attributes.clear();
    sde.types.insert(940_056, ammo_ty);
    let sde_opt = Some(Arc::new(sde));

    let char_id = 90_000_001_i64;
    let ship_id = 70_001_i64;
    let target_id = 70_002_i64;
    let paste_cargo_id = 70_100_i64;
    let turret_id = 70_201_i64;
    let turret_charge_id = 70_202_i64;
    let cargo_ammo_id = 70_203_i64;
    let crystal_mod_id = 70_204_i64;
    let crystal_charge_id = 70_205_i64;
    let cap_booster_mod_id = 70_206_i64;
    let cap_booster_charge_id = 70_207_i64;
    let asb_mod_id = 70_208_i64;
    let remote_aar_mod_id = 70_209_i64;
    let nanite_mod_id = 70_210_i64;

    db.with_mut_snapshot(|snap| {
        // 1 unit of paste in cargo, but nanite job needs 4 -> halts mid-repair
        snap.items.insert(
            paste_cargo_id,
            make_item(paste_cargo_id, 28_668, char_id, ship_id, InventoryFlag::Cargo, 1),
        );
        let mut nanite_mod = make_item(nanite_mod_id, 2048, char_id, ship_id, InventoryFlag::LoSlot0, 1);
        nanite_mod.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 40.0);
        snap.items.insert(nanite_mod_id, nanite_mod);

        snap.items.insert(
            turret_id,
            make_item(turret_id, 940_055, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        // 1 round loaded -> depletes to 0 and triggers auto-reload from cargo_ammo_id
        snap.items.insert(
            turret_charge_id,
            make_item(turret_charge_id, 940_056, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            cargo_ammo_id,
            make_item(cargo_ammo_id, 940_056, char_id, ship_id, InventoryFlag::Cargo, 200),
        );

        // Volatility crystal with quantity=2 and existing damage=0.8 -> shatters 1 crystal and resets damage
        snap.items.insert(
            crystal_mod_id,
            make_item(crystal_mod_id, 940_055, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        let mut crystal_it = make_item(crystal_charge_id, 940_056, char_id, ship_id, InventoryFlag::HiSlot1, 2);
        crystal_it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 0.8);
        snap.items.insert(crystal_charge_id, crystal_it);

        // Cap booster with 1 charge -> consumes charge and injects cap; second cycle has 0 charges -> stops
        snap.items.insert(
            cap_booster_mod_id,
            make_item(cap_booster_mod_id, 2048, char_id, ship_id, InventoryFlag::MedSlot0, 1),
        );
        snap.items.insert(
            cap_booster_charge_id,
            make_item(cap_booster_charge_id, 11_283, char_id, ship_id, InventoryFlag::MedSlot0, 1),
        );
        snap.items.insert(
            asb_mod_id,
            make_item(asb_mod_id, 32_772, char_id, ship_id, InventoryFlag::MedSlot1, 1),
        );
        snap.items.insert(
            remote_aar_mod_id,
            make_item(remote_aar_mod_id, 41_476, char_id, ship_id, InventoryFlag::MedSlot2, 1),
        );
    });

    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(make_ball(ship_id, 587, DVec3::ZERO));
    bp.combat_states.insert(
        ship_id,
        CombatState {
            capacitor: 400.0,
            max_capacitor: 1_000.0,
            shield_hp: 100.0,
            max_shield_hp: 1_000.0,
            armor_hp: 100.0,
            max_armor_hp: 1_000.0,
            structure_hp: 100.0,
            max_structure_hp: 1_000.0,
            ..CombatState::default()
        },
    );
    bp.add_ball(make_ball(target_id, 587, DVec3::new(1_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        target_id,
        CombatState {
            capacitor: 500.0,
            max_capacitor: 500.0,
            shield_hp: 10.0,
            max_shield_hp: 500.0,
            armor_hp: 10.0,
            max_armor_hp: 500.0,
            structure_hp: 50.0,
            max_structure_hp: 50.0,
            ..CombatState::default()
        },
    );

    let mut nanite_repairs = vec![
        ActiveNaniteRepair {
            ship_id,
            char_id,
            module_id: nanite_mod_id,
            initial_damage_hp: 40.0,
            duration_s: 4.0,
            elapsed_s: 0.0,
            paste_total_needed: 4,
            paste_consumed_so_far: 0,
        },
        ActiveNaniteRepair {
            ship_id: 999_999, // missing ship -> pruned immediately
            char_id,
            module_id: 999_998,
            initial_damage_hp: 10.0,
            duration_s: 2.0,
            elapsed_s: 0.0,
            paste_total_needed: 1,
            paste_consumed_so_far: 0,
        },
    ];

    let mut pending_disruptors = vec![crate::system_actor::PendingAnchoringDisruptor {
        id: 70_900,
        type_id: 12_198,
        position: DVec3::new(500.0, 0.0, 0.0),
        radius: 15_000.0,
        remaining_s: 2.5,
    }];

    let mut cycles = vec![
        // Projectile turret firing last round + auto-reload
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: turret_id,
            module_type_id: 940_055,
            target_id: Some(target_id),
            ammo_type_id: Some(940_056),
            linked_turrets: vec![turret_id],
            damage_vector: DamageVector { em: 10.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 },
            duration_s: 1.0,
            repeat: 1,
            ..ActiveModuleCycle::default()
        },
        // Volatility crystal shattering 1 of 2
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: crystal_mod_id,
            module_type_id: 940_055,
            target_id: Some(target_id),
            ammo_type_id: Some(940_056),
            linked_turrets: vec![crystal_mod_id],
            crystal_volatility_chance: 1.0,
            crystal_volatility_damage: 0.5,
            duration_s: 1.0,
            repeat: 1,
            ..ActiveModuleCycle::default()
        },
        // Smartbomb with pending overload OFF + RepeatCycleStart
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: 70_220,
            module_type_id: 940_072,
            is_overloaded: true,
            pending_overloaded: Some(false),
            cap_need_per_cycle: 10.0,
            shield_boost_amount: 15.0,
            duration_s: 1.0,
            repeat: 1,
            ..ActiveModuleCycle::default()
        },
        // Cap Booster with 1 charge
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: cap_booster_mod_id,
            cap_booster_inject_amount: 120.0,
            duration_s: 1.0,
            repeat: 2,
            ..ActiveModuleCycle::default()
        },
        // Unfueled ASB -> falls back to asb_fallback_cap_need
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: asb_mod_id,
            is_asb: true,
            asb_fallback_cap_need: 35.0,
            shield_boost_amount: 50.0,
            duration_s: 1.0,
            repeat: 1,
            ..ActiveModuleCycle::default()
        },
        // Unfueled Remote AAR (type 41476 -> updates remote_armor_amount)
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: remote_aar_mod_id,
            module_type_id: 41_476,
            target_id: Some(target_id),
            is_aar: true,
            aar_paste_rate: 2,
            aar_base_armor_repair: 45.0,
            duration_s: 1.0,
            repeat: 1,
            ..ActiveModuleCycle::default()
        },
        // MJFG field jump + Lance warmup requeue
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: 70_221,
            mjd_jump_distance_m: Some(100_000.0),
            mjfg_field_params: Some((6_000.0, 25, 0)),
            duration_s: 1.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: 70_222,
            effect_name: "superWeaponLanceWarmup".into(),
            duration_s: 1.0,
            repeat: 2,
            ..ActiveModuleCycle::default()
        },
    ];

    let mut pending_actions = Vec::new();
    let mut rah_accum = HashMap::new();
    let mut heat = HashMap::new();
    let tac = Arc::new(RwLock::new(Default::default()));

    // Tick 1: nanite halts after 1 paste; turret auto-reloads; crystal decrements 2->1; lance requeues as Beam
    let _ = step_cycles_and_capacitor(
        &mut bp,
        &mut cycles,
        &mut pending_disruptors,
        &mut nanite_repairs,
        &mut heat,
        &mut rah_accum,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &Some(Arc::clone(&db)),
        &None,
        &tac,
        &mut pending_actions,
        2.0,
        None,
    );
    assert!(nanite_repairs.is_empty(), "exhausted nanite repair should halt");
    assert_eq!(pending_disruptors.len(), 1);
    assert!(cycles.iter().any(|c| c.effect_name == "superWeaponLanceBeam" && c.repeat == 1));
    assert!(pending_actions.iter().any(|a| matches!(a, PendingCycleAction::SyncOverloadState { module_id: 70_220, is_overloaded: false, .. })));
    assert!(pending_actions.iter().any(|a| matches!(a, PendingCycleAction::RepeatCycleStart { module_id: 70_220, .. })));

    db.with_snapshot(|snap| {
        assert_eq!(snap.items[&crystal_charge_id].quantity, 1);
        assert!(!snap.cluster_and_runtime.module_reload_until_unix.is_empty());
        assert!(snap.cluster_and_runtime.auto_reloaded_event.contains_key(&turret_id));
    });

    // Tick 2: disruptor finishes anchoring (remaining_s <= 0), cap booster runs out of charges & stops
    let _ = step_cycles_and_capacitor(
        &mut bp,
        &mut cycles,
        &mut pending_disruptors,
        &mut nanite_repairs,
        &mut heat,
        &mut rah_accum,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &Some(Arc::clone(&db)),
        &None,
        &tac,
        &mut pending_actions,
        1.0,
        None,
    );
    assert!(pending_disruptors.is_empty());
    assert!(bp.balls.contains_key(&70_900));
    assert!(!cycles.iter().any(|c| c.module_id == cap_booster_mod_id));
}

#[tokio::test]
async fn system_handle_warp_module_removal_auto_step_and_full_tick_hooks() {
    let sys_id = 30_000_142;
    let db = Arc::new(WorldRepository::with_demo_seed());
    let reg = crate::system_actor::SystemRegistry::with_sde_and_db(make_extra_sde(), Arc::clone(&db));
    let handle = reg.get_or_spawn(sys_id);

    let ship_id = 71_001_i64;
    let mod_norm = 71_101_i64;
    let mod_null = 71_102_i64;

    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_norm,
            make_item(mod_norm, 2048, 90_000_001, ship_id, InventoryFlag::LoSlot0, 1),
        );
        snap.module_active_states.insert(mod_norm);
        snap.module_active_charges.insert(mod_norm, 5);
    });

    handle
        .spawn_ball_with_combat(
            make_ball(ship_id, 587, DVec3::ZERO),
            CombatState {
                shield_hp: 100.0,
                max_shield_hp: 100.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    handle.set_last_auto_stepped_unix(12345);
    assert!(handle.take_auto_stepped_at(12345));
    assert!(!handle.take_auto_stepped_at(12345));

    {
        let mut st = handle.actor_state.write().unwrap();
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(90_000_001),
            module_id: mod_norm,
            duration_s: 10.0,
            ..Default::default()
        });
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(90_000_001),
            module_id: mod_null,
            grants_interdiction_nullification: true,
            duration_s: 10.0,
            ..Default::default()
        });
        let state = &mut *st;
        let mut bp = handle.ballpark.write().unwrap();
        remove_non_warp_ship_module_cycles(
            &mut bp,
            &mut state.active_module_cycles,
            &state.base_scramble_strengths,
            &state.base_core_strengths,
            &state.db_opt,
            ship_id,
            None,
        );
        assert_eq!(state.active_module_cycles.len(), 1);
        assert_eq!(state.active_module_cycles[0].module_id, mod_null);

        // Put mod_norm back in DB and call remove_all_ship_module_cycles to test dirty DB cleanup
        db.with_mut_snapshot(|snap| {
            snap.module_active_states.insert(mod_norm);
            snap.module_active_charges.insert(mod_norm, 3);
        });
        let removed = remove_all_ship_module_cycles(
            &mut bp,
            &mut state.active_module_cycles,
            &state.base_scramble_strengths,
            &state.base_core_strengths,
            &state.db_opt,
            ship_id,
            None,
        );
        assert_eq!(removed.len(), 1);
    }

    assert!(!handle.has_active_modules_or_cloak_sync(ship_id));
    let tick = handle.try_step_full_tick(1.0);
    assert!(tick >= 1);

    handle.prepare_disconnected_warp_sync(
        71_009,
        587,
        40.0,
        1_000_000.0,
        2.0,
        300.0,
        3.0,
        DVec3::ZERO,
        DVec3::new(1_000_000.0, 0.0, 0.0),
    );
    assert!(handle.get_ball_position_sync(71_009).is_none() || handle.ballpark.read().unwrap().balls.contains_key(&71_009));
}

#[tokio::test]
async fn cycles_grappler_autotarget_repeat_burnout_and_full_tick_hooks() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mut sde = (*make_extra_sde()).clone();
    let base = sde.types.values().next().unwrap().clone();

    // Group 1672 = Stasis Grappler (tests SDE optimal/falloff/speedFactor lookups & cutoff)
    let mut grappler_ty = base.clone();
    grappler_ty.type_id = 941_672;
    grappler_ty.group_id = 1672;
    grappler_ty.category_id = 7;
    grappler_ty.name = "Heavy Stasis Grappler".into();
    grappler_ty.default_attributes = HashMap::from([(54u16, 1_000.0), (158u16, 2_000.0), (20u16, -80.0)]);
    sde.types.insert(941_672, grappler_ty);

    // Smartbomb group 72 with capacity = 0.0 (covers mt.capacity <= 0.0 in auto-reload)
    let mut autocannon_ty = base.clone();
    autocannon_ty.type_id = 941_055;
    autocannon_ty.group_id = 55;
    autocannon_ty.category_id = 7;
    autocannon_ty.capacity = 0.0;
    autocannon_ty.default_attributes = HashMap::from([(1795u16, 4_000.0), (1180u16, 0.25)]);
    sde.types.insert(941_055, autocannon_ty);

    let mut ammo_ty = base;
    ammo_ty.type_id = 941_056;
    ammo_ty.group_id = 83;
    ammo_ty.category_id = 8;
    ammo_ty.volume = 0.01;
    ammo_ty.default_attributes.clear();
    sde.types.insert(941_056, ammo_ty);
    let sde_arc = Arc::new(sde);
    let sde_opt = Some(Arc::clone(&sde_arc));

    let char_id = 90_000_001_i64;
    let ship_id = 72_001_i64;
    let target_id = 72_002_i64;
    let ally_id = 72_003_i64;
    let far_id = 72_004_i64;
    let dead_target_id = 72_005_i64;
    let tethered_mod_id = 72_101_i64;
    let nanite_mod_id = 72_102_i64;
    let paste_id = 72_103_i64;
    let rah_mod_id = 72_104_i64;
    let adc_mod_id = 72_105_i64;
    let ac_mod_id = 72_106_i64;
    let ac_empty_charge_id = 72_107_i64;
    let ac_cargo_ammo_id = 72_108_i64;
    let ovl_mod_id = 72_109_i64;
    let ovl_other_cycle_id = 72_110_i64;

    db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = ship_id;
        }
        let mut t_mod = make_item(tethered_mod_id, 941_055, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        t_mod.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 10.0);
        snap.items.insert(tethered_mod_id, t_mod);

        let mut n_mod = make_item(nanite_mod_id, 941_055, char_id, ship_id, InventoryFlag::HiSlot1, 1);
        n_mod.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 20.0);
        snap.items.insert(nanite_mod_id, n_mod);
        snap.items.insert(paste_id, make_item(paste_id, 28_668, char_id, ship_id, InventoryFlag::Cargo, 20));

        // Seed existing transient attrs for RAH (2nd cycle read branch lines 347-353) + ADC
        let rah_tr = snap.transient_item_attributes.entry(rah_mod_id).or_default();
        rah_tr.insert(267, 0.75);
        rah_tr.insert(268, 0.85);
        rah_tr.insert(269, 0.90);
        rah_tr.insert(270, 0.90);
        snap.transient_item_attributes.entry(ship_id).or_default().insert(267, 0.75);

        // Pre-damage overload module to 99.0 / 100.0 HP so overload tick burns it out!
        let mut ovl = make_item(ovl_mod_id, 941_055, char_id, ship_id, InventoryFlag::HiSlot2, 1);
        ovl.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 99.5);
        ovl.dynamic_attributes.insert(eve_sde::attr_ids::HP_STRUCT, 40.0);
        snap.items.insert(ovl_mod_id, ovl);
        snap.items.insert(ovl_other_cycle_id, make_item(ovl_other_cycle_id, 941_055, char_id, ship_id, InventoryFlag::HiSlot3, 1));

        // Autocannon with qty=0 stale charge & exact 1-round cargo stack -> tests retention + exact depletion + capacity=0
        snap.items.insert(ac_mod_id, make_item(ac_mod_id, 941_055, char_id, ship_id, InventoryFlag::HiSlot4, 1));
        snap.items.insert(ac_empty_charge_id, make_item(ac_empty_charge_id, 941_056, char_id, ship_id, InventoryFlag::HiSlot4, 0));
        snap.items.insert(ac_cargo_ammo_id, make_item(ac_cargo_ammo_id, 941_056, char_id, ship_id, InventoryFlag::Cargo, 1));
    });

    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(make_ship(ship_id, 587, DVec3::ZERO));
    bp.add_ball(make_ship(target_id, 587, DVec3::new(500.0, 0.0, 0.0)));
    bp.add_ball(make_ship(ally_id, 587, DVec3::new(500.0, 0.0, 0.0)));
    bp.add_ball(make_ship(far_id, 587, DVec3::new(50_000.0, 0.0, 0.0)));
    bp.add_ball(make_ship(dead_target_id, 587, DVec3::new(200.0, 0.0, 0.0)));
    bp.combat_states.insert(ship_id, CombatState { capacitor: 1_000.0, max_capacitor: 1_000.0, is_tethered: true, ..CombatState::default() });
    bp.combat_states.insert(target_id, CombatState { capacitor: 500.0, max_capacitor: 500.0, ..CombatState::default() });
    bp.combat_states.insert(ally_id, CombatState { shield_hp: 10.0, max_shield_hp: 100.0, armor_hp: 10.0, max_armor_hp: 100.0, structure_hp: 10.0, max_structure_hp: 100.0, capacitor: 100.0, max_capacitor: 500.0, ..CombatState::default() });
    bp.combat_states.insert(far_id, CombatState::default());
    bp.combat_states.insert(dead_target_id, CombatState { shield_hp: 0.0, max_shield_hp: 10.0, armor_hp: 0.0, max_armor_hp: 10.0, structure_hp: 1.0, max_structure_hp: 10.0, ..CombatState::default() });

    let mut cycles = vec![
        // Grappler within range + grappler beyond cutoff (dist = 50_000 > opt + 3*fo)
        ActiveModuleCycle {
            ship_id,
            module_id: 72_201,
            module_type_id: 941_672,
            target_id: Some(target_id),
            target_velocity_multiplier: Some(0.5),
            duration_s: 5.0,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id,
            module_id: 72_202,
            module_type_id: 941_672,
            target_id: Some(far_id),
            target_velocity_multiplier: Some(0.5),
            duration_s: 5.0,
            ..Default::default()
        },
        // 2nd RAH cycle reading existing transient_item_attributes
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: rah_mod_id,
            module_type_id: 2048,
            is_rah: true,
            rah_shift_amount: 6.0,
            duration_s: 1.0,
            ..Default::default()
        },
        // Auto-targeter cycle
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: 72_203,
            module_type_id: 2048,
            is_auto_targeter: true,
            auto_target_max_range: 50_000.0,
            auto_target_max_locks: 4,
            auto_target_scan_res: 200.0,
            duration_s: 1.0,
            ..Default::default()
        },
        // Remote hull repair + lethal damage with owner_char_id: None (DB fallback lookup lines 428-434)
        ActiveModuleCycle {
            ship_id,
            owner_char_id: None,
            module_id: 72_204,
            module_type_id: 2048,
            target_id: Some(dead_target_id),
            damage_vector: DamageVector { em: 100.0, ..Default::default() },
            duration_s: 1.0,
            ..Default::default()
        },
        // Repeating remote shield/cap/neut + hull cycle -> tests lines 518-519 & 959-978
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: 72_205,
            module_type_id: 2048,
            target_id: Some(ally_id),
            remote_hull_amount: 30.0,
            remote_shield_amount: 40.0,
            remote_cap_amount: 25.0,
            cap_drain_target: 10.0,
            remote_optimal: 10_000.0,
            remote_falloff: 5_000.0,
            duration_s: 1.0,
            repeat: 1,
            ..Default::default()
        },
        // Autocannon cycle with stale qty=0 charge -> clears stale stack & reloads 1 from cargo
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: ac_mod_id,
            module_type_id: 941_055,
            linked_turrets: vec![ac_mod_id],
            ammo_type_id: Some(941_056),
            damage_vector: DamageVector { kinetic: 5.0, ..Default::default() },
            duration_s: 1.0,
            ..Default::default()
        },
        // Overloaded cycle that burns out (high rack heat 1.0 -> burned_out + burned_ship_modules lines 564-567 & 1000-1011)
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: ovl_mod_id,
            module_type_id: 941_055,
            is_overloaded: true,
            duration_s: 1.0,
            repeat: 1,
            ..Default::default()
        },
        // ADC cycle for cleanup_removed_module_cycle test
        ActiveModuleCycle {
            ship_id,
            owner_char_id: Some(char_id),
            module_id: adc_mod_id,
            is_adc: true,
            duration_s: 1.0,
            ..Default::default()
        },
    ];
    let mut nanite_repairs = vec![ActiveNaniteRepair {
        ship_id,
        char_id,
        module_id: nanite_mod_id,
        initial_damage_hp: 20.0,
        duration_s: 5.0,
        elapsed_s: 0.0,
        paste_total_needed: 10,
        paste_consumed_so_far: 0,
    }];
    let mut heat = HashMap::from([(ship_id, [1.0, 0.0, 0.0])]);
    let mut rah_accum = HashMap::new();
    let mut pending_disruptors = Vec::new();
    let mut pending_actions = Vec::new();
    let rng_arc = Some(Arc::new(crate::rng::MechanicRng::seeded(42)));
    let destroyed = step_cycles_and_capacitor(
        &mut bp,
        &mut cycles,
        &mut pending_disruptors,
        &mut nanite_repairs,
        &mut heat,
        &mut rah_accum,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &Some(Arc::clone(&db)),
        &rng_arc,
        &Arc::new(RwLock::new(Default::default())),
        &mut pending_actions,
        1.0,
        None,
    );
    assert!(destroyed.iter().any(|(bid, cid)| *bid == dead_target_id && *cid == Some(char_id)));
    assert!(bp.combat_states[&ally_id].structure_hp > 10.0);
    assert!(bp.combat_states[&ally_id].shield_hp > 10.0);
    assert!((cycles[0].target_velocity_multiplier.unwrap() - 0.2).abs() < 0.05);
    assert_eq!(cycles[1].target_velocity_multiplier, Some(1.0));

    // Also test SystemHandle::try_step_full_tick with target_lock_completed_hook, npc_death_hook, system_step_hook
    let reg = crate::system_actor::SystemRegistry::with_sde_and_db(sde_arc, Arc::clone(&db));
    let lock_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lock_hits_c = Arc::clone(&lock_hits);
    reg.set_target_lock_completed_hook(Arc::new(move |cid, _tid| {
        if cid == char_id {
            lock_hits_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }));
    let death_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let death_hits_c = Arc::clone(&death_hits);
    reg.set_npc_death_hook(Arc::new(move |_sys, _bid, _cid| {
        let dh = Arc::clone(&death_hits_c);
        Box::pin(async move {
            dh.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    }));
    let step_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let step_hits_c = Arc::clone(&step_hits);
    reg.set_system_step_hook(Arc::new(move |_sys, _dt, _dead| {
        let sh = Arc::clone(&step_hits_c);
        Box::pin(async move {
            sh.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    }));
    let h = reg.get_or_spawn(30_000_142);
    let h_ship = 72_501_i64;
    let h_victim = 72_502_i64;
    db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = h_ship;
        }
    });
    h.spawn_ball_with_combat(make_ship(h_ship, 587, DVec3::ZERO), CombatState::default()).await.unwrap();
    h.spawn_ball_with_combat(make_ship(h_victim, 587, DVec3::new(100.0, 0.0, 0.0)), CombatState { shield_hp: 0.0, max_shield_hp: 10.0, armor_hp: 0.0, max_armor_hp: 10.0, structure_hp: 1.0, max_structure_hp: 10.0, ..CombatState::default() }).await.unwrap();
    h.add_target_lock(h_ship, eve_destiny::TargetLock {
        target_id: h_victim,
        lock_time_ms: 500.0,
        remaining_lock_ms: 500.0,
        locked: false,
        max_range_m: None,
    }).await.unwrap();
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: h_ship,
        owner_char_id: Some(char_id),
        module_id: 72_599,
        target_id: Some(h_victim),
        damage_vector: DamageVector { em: 50.0, ..Default::default() },
        duration_s: 0.5,
        ..Default::default()
    }).await.unwrap();
    let _ = h.try_step_full_tick(1.0);
    tokio::task::yield_now().await;
    assert!(lock_hits.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    assert!(death_hits.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    assert!(step_hits.load(std::sync::atomic::Ordering::SeqCst) >= 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn w4_mutant_killers_cycles_cloak_turret_missile_rah_reload_cleanup_and_flush_notifs() {
    use eve_db::clock::VirtualServerClock;
    let mut sde_mut = (*make_extra_sde()).clone();
    let mut reload_ty = sde_mut.types.values().next().unwrap().clone();
    reload_ty.type_id = 941_999;
    reload_ty.capacity = 20.0;
    reload_ty.default_attributes = HashMap::from([(1795u16, 5_100.0)]);
    sde_mut.types.insert(941_999, reload_ty);
    let mut ammo_ty = sde_mut.types.values().next().unwrap().clone();
    ammo_ty.type_id = 941_998;
    ammo_ty.volume = 1.0;
    sde_mut.types.insert(941_998, ammo_ty);
    let sde_arc = Arc::new(sde_mut);
    let sde_opt = Some(Arc::clone(&sde_arc));
    let clk = Arc::new(VirtualServerClock::with_fixed_base(1_776_300_000));
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.set_clock(clk);
    let db_opt = Some(Arc::clone(&db));
    let rng_opt = Some(Arc::new(crate::rng::MechanicRng::seeded(7)));
    let tac: RwLock<crate::services::tactical_modules::TacticalModulesState> =
        RwLock::new(Default::default());

    let char_atk = 91_201_i64;
    let char_vic = 91_202_i64;
    let ship_atk = 101_201_i64;
    let ship_vic = 101_202_i64;
    let npc_target = 101_203_i64;
    db.with_mut_snapshot(|snap| {
        let mut c1 = snap.characters.values().next().cloned().expect("char");
        c1.character_id = char_atk;
        c1.user_id = 91_201;
        c1.active_ship_id = ship_atk;
        snap.characters.insert(char_atk, c1.clone());
        let mut c2 = c1;
        c2.character_id = char_vic;
        c2.user_id = 91_202;
        c2.active_ship_id = ship_vic;
        snap.characters.insert(char_vic, c2);
        snap.items.insert(ship_atk, make_item(ship_atk, 587, char_atk, 30_000_142, InventoryFlag::None, 1));
        snap.items.insert(ship_vic, make_item(ship_vic, 587, char_vic, 30_000_142, InventoryFlag::None, 1));
    });

    // 1. Cloak decloaked cancellation matrix (`L266`)
    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(make_ship(ship_atk, 587, DVec3::ZERO));
    bp.add_ball(make_ship(ship_vic, 587, DVec3::new(1_000.0, 0.0, 0.0)));
    bp.add_ball(make_ship(npc_target, 587, DVec3::new(5_000.0, 0.0, 0.0)));
    bp.combat_states.insert(ship_atk, CombatState { is_cloaked: false, cloak_velocity_multiplier: None, shield_hp: 1000.0, max_shield_hp: 1000.0, armor_hp: 1000.0, max_armor_hp: 1000.0, structure_hp: 1000.0, max_structure_hp: 1000.0, capacitor: 1000.0, max_capacitor: 1000.0, ..Default::default() });
    bp.combat_states.insert(ship_vic, CombatState { shield_hp: 5000.0, max_shield_hp: 5000.0, armor_hp: 5000.0, max_armor_hp: 5000.0, structure_hp: 5000.0, max_structure_hp: 5000.0, signature_radius: 0.0, base_signature_radius: Some(400.0), ..Default::default() });
    bp.combat_states.insert(npc_target, CombatState { shield_hp: 5000.0, max_shield_hp: 5000.0, armor_hp: 5000.0, max_armor_hp: 5000.0, structure_hp: 5000.0, max_structure_hp: 5000.0, signature_radius: 0.0, base_signature_radius: None, ..Default::default() });

    // Decloaked ship keeps non-cloak cycle, removes cloak cycle when !is_cloaked && cloak_velocity_multiplier.is_none()
    let mut cycles = vec![
        ActiveModuleCycle { ship_id: ship_atk, module_id: 92_001, effect_name: "afterburner".into(), duration_s: 5.0, repeat: 1, ..Default::default() },
        ActiveModuleCycle { ship_id: ship_atk, module_id: 92_002, effect_name: "cloakingWarpSafe".into(), duration_s: 5.0, repeat: 1, ..Default::default() },
    ];
    let mut pending_dis = Vec::new();
    let mut nanites = Vec::new();
    let mut heat = HashMap::new();
    let mut rah = HashMap::new();
    let mut pa = Vec::new();
    let dead = step_cycles_and_capacitor(&mut bp, &mut cycles, &mut pending_dis, &mut nanites, &mut heat, &mut rah, &HashMap::new(), &HashMap::new(), &sde_opt, &db_opt, &rng_opt, &tac, &mut pa, 1.0, None);
    assert!(dead.is_empty());
    assert_eq!(cycles.len(), 1);
    assert_eq!(cycles[0].module_id, 92_001);

    // Keeps cloak cycle if is_cloaked == true OR cloak_velocity_multiplier.is_some()
    for (is_cl, vel_mult) in [(true, None), (false, Some(0.25))] {
        bp.combat_states.get_mut(&ship_atk).unwrap().is_cloaked = is_cl;
        bp.combat_states.get_mut(&ship_atk).unwrap().cloak_velocity_multiplier = vel_mult;
        let mut c2 = vec![ActiveModuleCycle { ship_id: ship_atk, module_id: 92_002, effect_name: "cloaking".into(), duration_s: 5.0, repeat: 1, ..Default::default() }];
        step_cycles_and_capacitor(&mut bp, &mut c2, &mut pending_dis, &mut nanites, &mut heat, &mut rah, &HashMap::new(), &HashMap::new(), &sde_opt, &db_opt, &rng_opt, &tac, &mut pa, 1.0, None);
        assert_eq!(c2.len(), 1, "is_cl={is_cl} vel_mult={vel_mult:?}");
    }
    bp.combat_states.get_mut(&ship_atk).unwrap().is_cloaked = false;
    bp.combat_states.get_mut(&ship_atk).unwrap().cloak_velocity_multiplier = None;

    // 2. RAH incoming damage accumulation (`+=` on 0 then nonzero -> kills -= and *=) + DB attacker/target char resolution & CycleWeaponDamage gating
    let mut rah_cycles = vec![
        ActiveModuleCycle {
            ship_id: ship_atk,
            owner_char_id: None, // force DB lookup via active_ship_id!
            module_id: 92_010,
            target_id: Some(ship_vic),
            damage_vector: DamageVector::new(10.0, 20.0, 30.0, 40.0),
            duration_s: 1.0,
            ..Default::default()
        },
        ActiveModuleCycle {
            ship_id: ship_atk,
            owner_char_id: None,
            module_id: 92_011,
            target_id: Some(ship_vic),
            damage_vector: DamageVector::new(10.0, 20.0, 30.0, 40.0),
            duration_s: 1.0,
            ..Default::default()
        },
    ];
    pa.clear();
    step_cycles_and_capacitor(&mut bp, &mut rah_cycles, &mut pending_dis, &mut nanites, &mut heat, &mut rah, &HashMap::new(), &HashMap::new(), &sde_opt, &db_opt, &rng_opt, &tac, &mut pa, 1.0, None);
    assert_eq!(rah.get(&ship_vic).copied(), Some([20.0, 40.0, 60.0, 80.0]));
    assert!(pa.iter().any(|a| matches!(a, super::PendingCycleAction::CycleWeaponDamage { attacker_char_id: Some(c1), target_char_id: Some(c2), .. } if *c1 == char_atk && *c2 == char_vic)));

    // Same-char target filtered out (`Some(cid) == attacker_char_id`); unknown attacker + unknown target emits NO CycleWeaponDamage!
    let mut self_or_anon = vec![
        ActiveModuleCycle { ship_id: ship_atk, owner_char_id: Some(char_atk), module_id: 92_012, target_id: Some(ship_atk), damage_vector: DamageVector::new(10.0, 0.0, 0.0, 0.0), duration_s: 1.0, ..Default::default() },
        ActiveModuleCycle { ship_id: npc_target, owner_char_id: None, module_id: 92_013, target_id: Some(999_888), damage_vector: DamageVector::new(10.0, 0.0, 0.0, 0.0), duration_s: 1.0, ..Default::default() },
    ];
    bp.add_ball(make_ship(999_888, 587, DVec3::new(100.0, 0.0, 0.0)));
    bp.combat_states.insert(999_888, CombatState { shield_hp: 100.0, max_shield_hp: 100.0, ..Default::default() });
    pa.clear();
    step_cycles_and_capacitor(&mut bp, &mut self_or_anon, &mut pending_dis, &mut nanites, &mut heat, &mut rah, &HashMap::new(), &HashMap::new(), &sde_opt, &db_opt, &rng_opt, &tac, &mut pa, 1.0, None);
    let self_act = pa.iter().find(|a| matches!(a, super::PendingCycleAction::CycleWeaponDamage { attacker_ship_id, .. } if *attacker_ship_id == ship_atk)).expect("self act");
    assert!(matches!(self_act, super::PendingCycleAction::CycleWeaponDamage { target_char_id: None, .. }));
    assert!(!pa.iter().any(|a| matches!(a, super::PendingCycleAction::CycleWeaponDamage { attacker_ship_id, .. } if *attacker_ship_id == npc_target)));

    // 3. Missile exact boundary (dist == optimal hits, dist == optimal + 1 misses, optimal == 0 hits at long range) + stationary speed = max_velocity * speed_fraction
    bp.balls.get_mut(&ship_vic).unwrap().position = DVec3::new(1_000.0, 0.0, 0.0);
    bp.balls.get_mut(&ship_vic).unwrap().velocity = DVec3::ZERO;
    bp.balls.get_mut(&ship_vic).unwrap().max_velocity = 500.0;
    bp.balls.get_mut(&ship_vic).unwrap().speed_fraction = 0.0;
    let s0 = bp.combat_states[&ship_vic].shield_hp;
    let mut m_exact = vec![ActiveModuleCycle {
        ship_id: ship_atk, owner_char_id: Some(char_atk), module_id: 92_020, target_id: Some(ship_vic),
        weapon_is_missile: true, weapon_optimal_m: 1_000.0, damage_vector: DamageVector::new(100.0, 0.0, 0.0, 0.0),
        missile_explosion_radius_m: 0.0, missile_explosion_velocity_ms: 0.0, missile_drf: 0.0, duration_s: 1.0, ..Default::default()
    }];
    step_cycles_and_capacitor(&mut bp, &mut m_exact, &mut pending_dis, &mut nanites, &mut heat, &mut rah, &HashMap::new(), &HashMap::new(), &sde_opt, &db_opt, &rng_opt, &tac, &mut pa, 1.0, None);
    assert!(bp.combat_states[&ship_vic].shield_hp < s0, "missile at dist == optimal must hit");

    let s1 = bp.combat_states[&ship_vic].shield_hp;
    let mut m_oob = vec![ActiveModuleCycle {
        ship_id: ship_atk, owner_char_id: Some(char_atk), module_id: 92_021, target_id: Some(ship_vic),
        weapon_is_missile: true, weapon_optimal_m: 999.0, damage_vector: DamageVector::new(100.0, 0.0, 0.0, 0.0),
        duration_s: 1.0, ..Default::default()
    }];
    step_cycles_and_capacitor(&mut bp, &mut m_oob, &mut pending_dis, &mut nanites, &mut heat, &mut rah, &HashMap::new(), &HashMap::new(), &sde_opt, &db_opt, &rng_opt, &tac, &mut pa, 1.0, None);
    assert_eq!(bp.combat_states[&ship_vic].shield_hp, s1, "missile out of range must deal 0");

    // weapon_optimal_m == 0.0 still hits + target moving via max_velocity * speed_fraction reduces light missile damage vs 40m radius
    bp.balls.get_mut(&ship_vic).unwrap().speed_fraction = 1.0;
    bp.combat_states.get_mut(&ship_vic).unwrap().signature_radius = 40.0;
    let mut m_fast = vec![ActiveModuleCycle {
        ship_id: ship_atk, owner_char_id: Some(char_atk), module_id: 92_022, target_id: Some(ship_vic),
        weapon_is_missile: true, weapon_optimal_m: 0.0, damage_vector: DamageVector::new(100.0, 0.0, 0.0, 0.0),
        missile_explosion_radius_m: 40.0, missile_explosion_velocity_ms: 100.0, missile_drf: 0.8, duration_s: 1.0, ..Default::default()
    }];
    step_cycles_and_capacitor(&mut bp, &mut m_fast, &mut pending_dis, &mut nanites, &mut heat, &mut rah, &HashMap::new(), &HashMap::new(), &sde_opt, &db_opt, &rng_opt, &tac, &mut pa, 1.0, None);
    let dmg_fast = s1 - bp.combat_states[&ship_vic].shield_hp;
    assert!(dmg_fast > 0.0 && dmg_fast < 100.0, "expected partial missile damage from speed_fraction*max_vel, got {dmg_fast}");

    // 4. Turret attacker_pylon_mult, sig_res, target_sig fallbacks, and roll_seed XOR
    bp.balls.get_mut(&ship_vic).unwrap().position = DVec3::new(0.0, 1_000.0, 0.0);
    bp.balls.get_mut(&ship_vic).unwrap().velocity = DVec3::new(10.0, 0.0, 0.0);
    bp.combat_states.get_mut(&ship_vic).unwrap().signature_radius = 0.0;
    bp.combat_states.get_mut(&ship_atk).unwrap().tracking_speed_multiplier = 0.0;
    let s2 = bp.combat_states[&ship_vic].shield_hp;
    let mut t_cyc = vec![ActiveModuleCycle {
        ship_id: ship_atk, owner_char_id: Some(char_atk), module_id: 92_030, target_id: Some(ship_vic),
        weapon_is_turret: true, weapon_optimal_m: 5_000.0, weapon_falloff_m: 5_000.0, weapon_tracking_speed: 2.0,
        weapon_sig_resolution_m: 0.0, damage_vector: DamageVector::new(100.0, 0.0, 0.0, 0.0), duration_s: 1.0, ..Default::default()
    }];
    step_cycles_and_capacitor(&mut bp, &mut t_cyc, &mut pending_dis, &mut nanites, &mut heat, &mut rah, &HashMap::new(), &HashMap::new(), &sde_opt, &db_opt, &rng_opt, &tac, &mut pa, 1.0, None);
    assert!(bp.combat_states[&ship_vic].shield_hp < s2, "turret fallback tracking_speed_mult=1.0 & sig_res=40_000 must hit");

    // 5. Auto-reload `reload_until = now_unix + (reload_time_ms / 1000.0).ceil() as u64`
    let turret_mid = 92_040_i64;
    let ammo_tid = 941_998_u32;
    let cargo_ammo_id = 92_041_i64;
    db.with_mut_snapshot(|snap| {
        snap.items.insert(turret_mid, make_item(turret_mid, 941_999, char_atk, ship_atk, InventoryFlag::HiSlot0, 1));
        snap.items.insert(cargo_ammo_id, make_item(cargo_ammo_id, ammo_tid, char_atk, ship_atk, InventoryFlag::Cargo, 50));
    });
    let mut reload_cyc = vec![ActiveModuleCycle {
        ship_id: ship_atk, owner_char_id: Some(char_atk), module_id: turret_mid, module_type_id: 941_999, linked_turrets: vec![turret_mid],
        ammo_type_id: Some(ammo_tid), damage_vector: DamageVector::new(10.0, 0.0, 0.0, 0.0), repeat: 1, duration_s: 1.0, ..Default::default()
    }];
    step_cycles_and_capacitor(&mut bp, &mut reload_cyc, &mut pending_dis, &mut nanites, &mut heat, &mut rah, &HashMap::new(), &HashMap::new(), &sde_opt, &db_opt, &rng_opt, &tac, &mut pa, 1.0, None);
    db.with_snapshot(|snap| {
        assert_eq!(snap.cluster_and_runtime.module_reload_until_unix.get(&turret_mid).copied(), Some(1_776_300_006));
    });

    // 6. cleanup_removed_module_cycle gate_cloak_remaining_ticks == 0 vs > 0, and ehe_hull_resists only
    bp.combat_states.get_mut(&ship_atk).unwrap().gate_cloak_remaining_ticks = 5;
    bp.combat_states.get_mut(&ship_atk).unwrap().decloak_targeting_delay_remaining_s = 0.0;
    bp.combat_states.get_mut(&ship_atk).unwrap().cloak_configured_targeting_delay_s = 7.0;
    let rem_cl = ActiveModuleCycle { ship_id: ship_atk, effect_name: "cloaking".into(), ..Default::default() };
    cleanup_removed_module_cycle(&mut bp, &rem_cl, &db_opt);
    assert_eq!(bp.combat_states[&ship_atk].decloak_targeting_delay_remaining_s, 0.0);
    bp.combat_states.get_mut(&ship_atk).unwrap().gate_cloak_remaining_ticks = 0;
    cleanup_removed_module_cycle(&mut bp, &rem_cl, &db_opt);
    assert_eq!(bp.combat_states[&ship_atk].decloak_targeting_delay_remaining_s, 7.0);

    db.with_mut_snapshot(|snap| {
        snap.transient_item_attributes.entry(ship_atk).or_default().insert(267, 0.5);
    });
    let rem_ehe = ActiveModuleCycle { ship_id: ship_atk, is_adc: false, ehe_hull_resists: Some([0.2; 4]), ..Default::default() };
    cleanup_removed_module_cycle(&mut bp, &rem_ehe, &db_opt);
    db.with_snapshot(|snap| {
        assert!(!snap.transient_item_attributes.get(&ship_atk).is_some_and(|m| m.contains_key(&267)));
    });

    // 7. flush_target_lost_notifications & flush_pending_cycle_damage_notifications
    let q_p: RwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>> = RwLock::new(HashMap::new());
    flush_target_lost_notifications(
        &bp,
        &[
            eve_destiny::DestinyEvent::OnTarget { mode: "locked".into(), source_id: ship_atk, target_id: ship_vic },
            eve_destiny::DestinyEvent::OnTarget { mode: "lost".into(), source_id: ship_atk, target_id: ship_atk },
            eve_destiny::DestinyEvent::OnTarget { mode: "lost".into(), source_id: ship_atk, target_id: ship_vic },
        ],
        Some(&db),
        Some(&q_p),
        None,
    );
    {
        let g = q_p.read().unwrap();
        assert_eq!(g.get(&char_atk).map(|v| v.iter().filter(|n| n.event == "OnTarget").count()), Some(2));
        assert_eq!(g.get(&char_vic).map(|v| v.len()), Some(1));
    }

    // Queue cap eviction at MAX_PENDING_NOTIFICATIONS_PER_CHAR + notified/zero/attacker_char_id==0 gating
    {
        let mut g = q_p.write().unwrap();
        let entry = g.entry(char_atk).or_default();
        let template = entry[0].clone();
        while entry.len() < crate::router::MAX_PENDING_NOTIFICATIONS_PER_CHAR {
            entry.push(template.clone());
        }
    }
    let mut act_flushes = vec![
        super::PendingCycleAction::CycleWeaponDamage { attacker_char_id: Some(char_atk), attacker_ship_id: ship_atk, attacker_ship_type_id: 587, target_id: ship_vic, target_char_id: Some(char_vic), module_type_id: 484, applied_dmg: 0.0, notified: false },
        super::PendingCycleAction::CycleWeaponDamage { attacker_char_id: Some(0), attacker_ship_id: ship_atk, attacker_ship_type_id: 587, target_id: ship_vic, target_char_id: Some(char_vic), module_type_id: 484, applied_dmg: 25.0, notified: false },
        super::PendingCycleAction::CycleWeaponDamage { attacker_char_id: Some(char_atk), attacker_ship_id: ship_atk, attacker_ship_type_id: 587, target_id: ship_vic, target_char_id: Some(char_vic), module_type_id: 484, applied_dmg: 42.0, notified: false },
    ];
    flush_pending_cycle_damage_notifications(Some(&db), Some(&q_p), None, &mut act_flushes);
    let g2 = q_p.read().unwrap();
    assert_eq!(g2[&char_atk].len(), crate::router::MAX_PENDING_NOTIFICATIONS_PER_CHAR);
    assert!(g2[&char_vic].iter().any(|n| n.event == "OnDamageMessage"));

    // 8. SystemHandle::try_step_full_tick increments from initial Ballpark tick (1) -> 2, 3
    let reg = crate::system_actor::SystemRegistry::with_sde_and_db(sde_arc, db);
    let sh = reg.get_or_spawn(30_000_142);
    assert_eq!(sh.try_step_full_tick(1.0), 2);
    assert_eq!(sh.try_step_full_tick(1.0), 3);
}
