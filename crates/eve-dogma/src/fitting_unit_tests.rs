//! Unit and mutation-killing tests for `fitting`.

use super::*;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;

fn make_type(
    type_id: TypeId,
    group_id: u32,
    category_id: u32,
    attrs: &[(AttributeId, f64)],
    effects: &[u16],
) -> InvType {
    let mut default_attributes = HashMap::new();
    for &(k, v) in attrs {
        default_attributes.insert(k, v);
    }
    InvType {
        type_id,
        group_id,
        category_id,
        name: format!("Type{type_id}"),
        mass: 100.0,
        volume: 10.0,
        capacity: 0.0,
        radius: 10.0,
        base_price: 0.0,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id: None,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: effects.to_vec(),
        default_attributes,
    }
}

#[test]
fn fitting_validation_errors_and_restrictions() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(
        587,
        make_type(
            587,
            25,
            6,
            &[
                (attr_ids::LOW_SLOTS, 2.0),
                (attr_ids::MED_SLOTS, 2.0),
                (attr_ids::HI_SLOTS, 2.0),
                (attr_ids::RIG_SLOTS, 2.0),
                (attr_ids::TURRET_SLOTS_LEFT, 1.0),
                (attr_ids::LAUNCHER_SLOTS_LEFT, 1.0),
                (attr_ids::RIG_SIZE, 1.0),
                (attr_ids::UPGRADE_CAPACITY, 400.0),
                (attr_ids::CPU_OUTPUT, 100.0),
                (attr_ids::POWER_OUTPUT, 100.0),
            ],
            &[],
        ),
    );
    // Non-module category (category 8 = Charge)
    sde.types.insert(10, make_type(10, 80, 8, &[], &[11]));
    // Empty effect_ids
    sde.types.insert(11, make_type(11, 80, 7, &[], &[]));
    // Subsystem requiring slot 126
    sde.types
        .insert(12, make_type(12, 954, 32, &[(1366, 126.0)], &[3772]));
    // Turret (effect 12 hiPower + 42 turretFitted)
    sde.types.insert(13, make_type(13, 53, 7, &[], &[12, 42]));
    // Launcher (effect 12 hiPower + 40 launcherFitted)
    sde.types.insert(14, make_type(14, 509, 7, &[], &[12, 40]));
    // MaxGroupFitted = 1, MaxTypeFitted = 1
    sde.types.insert(
        15,
        make_type(15, 60, 7, &[(1548, 1.0), (1544, 1.0)], &[11]),
    );
    // MaxGroupOnline = 1 (propulsion group 46)
    sde.types.insert(16, make_type(16, 46, 7, &[(978, 1.0)], &[13]));
    // Hull-restricted module (only group 999)
    sde.types
        .insert(17, make_type(17, 61, 7, &[(1298, 999.0)], &[11]));
    // Rig with wrong rigSize (2 != 1)
    sde.types.insert(
        18,
        make_type(18, 700, 7, &[(attr_ids::RIG_SIZE, 2.0)], &[2663]),
    );
    // Skill-requiring module (requires skill 3300 level 3)
    sde.types.insert(
        19,
        make_type(
            19,
            62,
            7,
            &[
                (attr_ids::REQUIRED_SKILL_1, 3300.0),
                (attr_ids::REQUIRED_SKILL_1_LEVEL, 3.0),
            ],
            &[11],
        ),
    );

    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde);
    assert!(eng.fit_module(100, 9999, 11, &sde).is_err()); // unknown module
    assert!(eng.fit_module(100, 10, 11, &sde).is_err()); // not category 7 or 32
    assert!(eng.fit_module(100, 11, 11, &sde).is_err()); // empty effect_ids
    assert!(eng.fit_module(100, 12, 125, &sde).is_err()); // subsystem slot mismatch
    assert!(eng.fit_module(100, 12, 126, &sde).is_err()); // hull not group 963 (0 subsystem slots)
    assert!(eng.fit_module(100, 15, 99, &sde).is_err()); // invalid flag
    assert!(eng.fit_module(100, 15, 19, &sde).is_err()); // lacks medPower effect
    assert!(eng.fit_module(100, 15, 13, &sde).is_err()); // exceeds low slot count (2)

    // Turret & launcher hardpoint limits
    assert!(eng.fit_module(101, 13, 27, &sde).is_ok());
    assert!(eng.fit_module(102, 13, 28, &sde).is_err()); // 2nd turret exceeds 1 hardpoint
    assert!(eng.unfit_module(101, &sde).is_ok());
    assert!(eng.fit_module(103, 14, 27, &sde).is_ok());
    assert!(eng.fit_module(104, 14, 28, &sde).is_err()); // 2nd launcher exceeds 1 hardpoint
    assert!(eng.unfit_module(103, &sde).is_ok());

    // Occupied slot & MaxGroupFitted
    assert!(eng.fit_module(105, 15, 11, &sde).is_ok());
    assert!(eng.fit_module(106, 15, 11, &sde).is_err()); // slot 11 occupied
    assert!(eng.fit_module(106, 15, 12, &sde).is_err()); // MaxGroupFitted = 1

    // MaxGroupOnline
    assert!(eng.fit_module(107, 16, 19, &sde).is_ok());
    assert!(eng.fit_module_with_state(108, 16, 20, true, &sde).is_err());
    assert!(eng.fit_module_with_state(108, 16, 20, false, &sde).is_ok()); // offline allowed
    assert!(eng.set_module_online(108, &sde).is_err()); // MaxGroupOnline on set_module_online

    // Hull restriction, rig size mismatch, missing skill
    assert!(eng.fit_module(109, 17, 12, &sde).is_err());
    assert!(eng.fit_module(110, 18, 92, &sde).is_err());
    assert!(eng.fit_module(111, 19, 12, &sde).is_err());
    eng.set_character_skills(HashMap::from([(3300, 3)]));
    assert!(eng.fit_module(111, 19, 12, &sde).is_ok());
}

#[test]
fn module_lifecycle_constraints_and_subsystem_unfit_guard() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(
        29984,
        make_type(
            29984,
            963,
            6,
            &[
                (attr_ids::CPU_OUTPUT, 50.0),
                (attr_ids::POWER_OUTPUT, 50.0),
            ],
            &[],
        ),
    );
    sde.types.insert(
        30001,
        make_type(
            30001,
            954,
            32,
            &[(1374, 2.0), (1375, 2.0), (1376, 2.0)],
            &[3772],
        ),
    );
    sde.types.insert(
        200,
        make_type(200, 60, 7, &[(attr_ids::CPU, 30.0), (attr_ids::POWER, 10.0)], &[12]),
    );
    sde.types.insert(
        201,
        make_type(201, 61, 7, &[(attr_ids::CPU, 20.0), (attr_ids::POWER, 10.0)], &[11]),
    );

    let mut eng = ShipFittingEngine::from_sde(1, 29984, &sde);
    assert!(eng.fit_module(1, 30001, 125, &sde).is_ok());
    assert!(eng.fit_module(2, 200, 27, &sde).is_ok());
    assert!(eng.fit_module(3, 201, 11, &sde).is_ok());

    // Cannot unfit subsystem while modules occupy its slots
    assert!(eng.unfit_module(1, &sde).is_err());

    // Load charge, activate, overload, deactivate, offline
    assert!(eng.load_charge(2, 6002, 20, &sde).is_ok());
    assert!(eng.load_charge(999, 6002, 20, &sde).is_err());
    assert!(eng.activate_module(2, false, &sde).is_ok());
    assert_eq!(eng.modules[&2].state, ModuleState::Active);
    assert!(eng.activate_module(2, true, &sde).is_ok());
    assert_eq!(eng.modules[&2].state, ModuleState::Overloaded);
    assert!(eng.deactivate_module(2, &sde).is_ok());
    assert_eq!(eng.modules[&2].state, ModuleState::Online);
    assert!(eng.take_module_offline(2, &sde).is_ok());
    assert!(eng.deactivate_module(2, &sde).is_err()); // already offline
    assert!(eng.activate_module(2, false, &sde).is_err()); // cannot activate offline

    // Burned out module cannot be onlined or activated
    eng.modules.get_mut(&2).unwrap().damage_hp = 40.0;
    assert!(eng.set_module_online(2, &sde).is_err());
    eng.modules.get_mut(&2).unwrap().damage_hp = 0.0;
    assert!(eng.set_module_online(2, &sde).is_ok());

    // Reduce CPU_OUTPUT so enforce_post_unfit_or_offline_constraints forces high-slot module 2 offline first
    eng.ship.base_attributes.insert(attr_ids::CPU_OUTPUT, 25.0);
    sde.types
        .get_mut(&29984)
        .unwrap()
        .default_attributes
        .insert(attr_ids::CPU_OUTPUT, 25.0);
    eng.recalculate_stats(&sde);
    let forced = eng.enforce_post_unfit_or_offline_constraints(&sde);
    assert_eq!(forced, vec![2]);
    assert_eq!(eng.modules[&2].state, ModuleState::Offline);
    assert_eq!(eng.modules[&3].state, ModuleState::Online);
}

#[test]
fn thermodynamics_boosters_dbuffs_abyssal_and_stances() {
    let mut sde = SdeRegistry::default();
    // T3D Confessor (34317, group 1305) with stanceSwitchTime (1985) = 10_000 ms
    sde.types.insert(
        34317,
        make_type(
            34317,
            1305,
            6,
            &[
                (attr_ids::HI_SLOTS, 4.0),
                (attr_ids::RIG_SLOTS, 2.0),
                (attr_ids::RIG_SIZE, 1.0),
                (attr_ids::CPU_OUTPUT, 200.0),
                (attr_ids::POWER_OUTPUT, 200.0),
                (1985, 10_000.0),
            ],
            &[],
        ),
    );
    sde.types
        .insert(34319, make_type(34319, 1306, 6, &[], &[]));
    sde.types
        .insert(34321, make_type(34321, 1306, 6, &[], &[]));
    sde.types
        .insert(34564, make_type(34564, 1306, 6, &[], &[])); // Svipul stance
    sde.types
        .insert(300, make_type(300, 53, 7, &[(1211, 20.0)], &[12]));
    sde.types.insert(
        301,
        make_type(301, 700, 7, &[(attr_ids::RIG_SIZE, 1.0)], &[2663]),
    );

    let mut eng = ShipFittingEngine::from_sde(1, 34317, &sde);
    assert!(eng.fit_module(10, 300, 27, &sde).is_ok());
    assert!(eng.fit_module(11, 300, 28, &sde).is_ok());
    assert!(eng.fit_module(12, 301, 92, &sde).is_ok());

    // Heat cycle on non-heat-rack flag (rig 92) fails
    assert!(eng.apply_heat_cycle(12, 0.1, 0.05, 5.0, 0.0, &sde).is_err());
    assert!(eng.apply_heat_cycle(999, 0.1, 0.05, 5.0, 0.0, &sde).is_err());

    // Two heat cycles with base_heat_dmg=20.0 and Thermodynamics 0 -> burns out module 10 (40 HP) and damages adjacent 11
    let r1 = eng.apply_heat_cycle(10, 0.2, 0.05, 10.0, 0.0, &sde).unwrap();
    assert!(!r1.module_burned);
    assert_eq!(r1.module_damage, 20.0);
    assert_eq!(r1.adjacent_damaged_modules.len(), 1);
    let r2 = eng.apply_heat_cycle(10, 0.2, 0.05, 10.0, 0.0, &sde).unwrap();
    assert!(r2.module_burned);
    assert_eq!(eng.modules[&10].state, ModuleState::Offline);

    // Thermodynamics submodule unit formulas
    assert!((thermodynamics::step_rack_heat(0.0, 0.5, 0.1, 2.0) - (1.0 - (-1.0_f64).exp())).abs() < 1e-12);
    assert!((thermodynamics::step_rack_heat(1.0, 0.0, 0.5, 2.0) - (-1.0_f64).exp()).abs() < 1e-12);
    assert_eq!(thermodynamics::heat_distance_attenuation(0, 0.5), 1.0);
    assert!((thermodynamics::heat_damage_probability(0.8, 1, 0.5) - 0.8 * (-0.5_f64).exp()).abs() < 1e-12);
    assert!((thermodynamics::effective_heat_damage(10.0, 5) - 7.5).abs() < 1e-12);
    assert_eq!(thermodynamics::nanite_paste_required(10.2, 1.0), 11);
    assert_eq!(thermodynamics::nanite_paste_required(-5.0, 1.0), 0);

    // Booster side effect & dbuff aggregation & abyssal rolling
    assert_eq!(evaluate_booster_side_effect(0.4, -20.0, 5, 5, 0.29), Some(-15.0));
    assert_eq!(evaluate_booster_side_effect(0.4, -20.0, 5, 5, 0.31), None);
    assert_eq!(aggregate_dbuff_values("Maximum", &[]), 0.0);
    assert_eq!(aggregate_dbuff_values("Minimum", &[-5.0, -15.0, -10.0]), -15.0);
    assert_eq!(aggregate_dbuff_values("Additive", &[5.0, 10.0, 15.0]), 30.0);
    assert_eq!(aggregate_dbuff_values("Maximum", &[5.0, 20.0, 10.0]), 20.0);

    let rolled = roll_abyssal_attributes(
        &HashMap::from([(10, 100.0), (20, 50.0)]),
        &HashMap::from([(10, [0.8, 1.2]), (20, [1.0, 2.0])]),
        &HashMap::from([(10, 1.0)]), // attr 20 defaults to u=0.5 -> mult=1.5
    );
    assert!((rolled[&10] - 120.0).abs() < 1e-9);
    assert!((rolled[&20] - 75.0).abs() < 1e-9);

    // Dynamic attribute & gang burst helpers
    eng.set_module_dynamic_attribute_and_recalculate(11, attr_ids::CPU, 12.0, &sde);
    eng.set_module_dynamic_attributes_and_recalculate(
        11,
        HashMap::from([(attr_ids::POWER, 8.0)]),
        &sde,
    );
    eng.add_gang_burst(GangBurstEffect {
        source_item_id: 1,
        source_type_id: None,
        dbuff_id: None,
        value: 0.0,
        modifiers: Vec::new(),
    });
    eng.apply_gang_burst(
        GangBurstEffect {
            source_item_id: 2,
            source_type_id: None,
            dbuff_id: None,
            value: 0.0,
            modifiers: Vec::new(),
        },
        &sde,
    );
    assert_eq!(eng.gang_bursts.len(), 2);

    // T3D stance defaults, ownership, set_ship_stance, and switch_ship_stance cooldown
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(34317), Some(34319));
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(34562), Some(34564));
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(34828), Some(35676));
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(35683), Some(35686));
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(587), None);
    assert!(ShipFittingEngine::t3d_stance_belongs_to_hull(34317, 34319));
    assert!(!ShipFittingEngine::t3d_stance_belongs_to_hull(34317, 34564));

    assert!(eng.set_ship_stance(34564, &sde).is_err()); // wrong hull's stance
    assert!(eng.set_ship_stance(300, &sde).is_err()); // not group 1306
    assert!(eng.set_ship_stance(34319, &sde).is_ok());
    assert!(eng.switch_ship_stance(34321, 1000, &sde).is_ok());
    assert!(eng.switch_ship_stance(34319, 5000, &sde).is_err()); // cooldown active (4000 < 10000)
    assert!(eng.switch_ship_stance(34319, 11_000, &sde).is_ok()); // cooldown elapsed
}

#[test]
fn test_issue_424_heat_cycle_single_thermodynamics_and_per_slot_rolls() {
    use eve_sde::EffectModifierSpec;

    let mut sde = SdeRegistry::with_canonical_seed();
    let ship_tid = 91_001;
    let mod_tid = 91_002;
    let thermo_tid: u32 = 28_164;
    let heat_eff_id: u16 = 9_150;

    let mut eff = sde.effects.values().next().unwrap().clone();
    eff.effect_id = heat_eff_id;
    eff.name = "testThermoHeatReduction".to_string();
    eff.modifiers = vec![EffectModifierSpec {
        domain: "shipID".to_string(),
        func: "LocationRequiredSkillModifier".to_string(),
        modified_attribute_id: 1211, // heatDamage
        modifying_attribute_id: 1229, // thermodynamicsHeatDamage bonus (-5%/lvl)
        operator: 2,                  // PostPercent
        group_id: None,
        skill_type_id: Some(thermo_tid),
        effect_id: Some(heat_eff_id),
    }];
    sde.effects.insert(heat_eff_id, eff);
    sde.types.insert(
        thermo_tid,
        make_type(thermo_tid, 268, 16, &[(1229, -5.0)], &[heat_eff_id]),
    );
    sde.types.insert(
        ship_tid,
        make_type(
            ship_tid,
            25,
            6,
            &[
                (attr_ids::HI_SLOTS, 4.0),
                (attr_ids::CPU_OUTPUT, 500.0),
                (attr_ids::POWER_OUTPUT, 500.0),
            ],
            &[],
        ),
    );
    sde.types.insert(
        mod_tid,
        make_type(
            mod_tid,
            53,
            7,
            &[
                (attr_ids::CPU, 10.0),
                (attr_ids::POWER, 10.0),
                (attr_ids::HP_STRUCT, 10_000.0),
                (1211, 10.0),
            ],
            &[12, 16],
        ),
    );

    let mut eng = ShipFittingEngine::from_sde(1, ship_tid, &sde);
    eng.set_character_skills(HashMap::from([(thermo_tid, 5)]));
    eng.fit_module(101, mod_tid, 27, &sde).unwrap();
    eng.fit_module(102, mod_tid, 28, &sde).unwrap();
    eng.fit_module(103, mod_tid, 29, &sde).unwrap();

    // Thermodynamics V must be applied once (7.5), never double-applied (5.625)
    eng.rack_heat[0] = 0.80;
    let rep = eng.apply_heat_cycle(102, 0.0, 0.0, 0.0, 0.99, &sde).unwrap();
    assert!((rep.module_damage - 7.5).abs() < 1e-6);

    // Symmetric neighbors at equal slot distance (slot 27 & slot 29 around 28) receive
    // independent per-slot RNG rolls rather than lockstep all-or-nothing hits.
    let mut split_observed = false;
    for step in 0..80 {
        for m in eng.modules.values_mut() {
            m.damage_hp = 0.0;
        }
        let probe = (step as f64) * 0.01;
        let r = eng.apply_heat_cycle(102, 0.0, 0.0, 0.0, probe, &sde).unwrap();
        if r.adjacent_damaged_modules.len() == 1 {
            split_observed = true;
            break;
        }
    }
    assert!(split_observed, "Symmetric neighbors must roll independently per slot");
}
