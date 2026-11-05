//! Deep branch and mutation-killing unit tests for `system_actor/cycles.rs`.

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{Ball, Ballpark, CombatState, MovementMode};
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
