//! Comprehensive tests for `eve-dogma` ship fitting constraints, thermodynamics,
//! T3C subsystems, T3D stances, drones, gang bursts, boosters, and dynamic attributes.

#![forbid(unsafe_code)]

use eve_dogma::{
    aggregate_dbuff_values, evaluate_booster_side_effect, roll_abyssal_attributes, thermodynamics,
    AttributeModifier, GangBurstEffect, ModifierOperator, ModuleState, ShipFittingEngine,
};
use eve_sde::{attr_ids, DogmaEffectType, EffectModifierSpec, InvType, SdeRegistry};
use std::collections::HashMap;

fn inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    default_attributes: HashMap<u16, f64>,
    effect_ids: Vec<u16>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass: 1_000_000.0,
        volume: 10.0,
        capacity: 100.0,
        radius: 50.0,
        base_price: 1_000.0,
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
        default_effect_id: effect_ids.first().copied(),
        description: String::new(),
        effect_ids,
        default_attributes,
    }
}

fn make_synthetic_sde() -> SdeRegistry {
    let mut sde = SdeRegistry::with_canonical_seed();

    // High-slot turret module (type 50001): CPU 25, PG 10, effect 12 (hiPower) + 42 (turretFitted)
    let mut turret_attrs = HashMap::new();
    turret_attrs.insert(attr_ids::CPU, 25.0);
    turret_attrs.insert(attr_ids::POWER, 10.0);
    turret_attrs.insert(1211, 5.0); // heatDamage
    sde.types.insert(
        50001,
        inv_type(50001, 55, 7, "Synthetic Autocannon I", turret_attrs, vec![16, 12, 42]),
    );

    // High-slot launcher module with maxTypeFitted = 1 (type 50002): effect 12 (hiPower) + 40 (launcherFitted)
    let mut lch_attrs = HashMap::new();
    lch_attrs.insert(attr_ids::CPU, 15.0);
    lch_attrs.insert(attr_ids::POWER, 5.0);
    lch_attrs.insert(2431, 1.0); // maxTypeFitted
    sde.types.insert(
        50002,
        inv_type(50002, 506, 7, "Synthetic Rocket Launcher", lch_attrs, vec![16, 12, 40]),
    );

    // Med-slot module with maxGroupFitted = 1, maxGroupOnline = 1 (type 50003)
    let mut med_attrs = HashMap::new();
    med_attrs.insert(attr_ids::CPU, 20.0);
    med_attrs.insert(attr_ids::POWER, 8.0);
    med_attrs.insert(1548, 1.0); // maxGroupFitted
    med_attrs.insert(978, 1.0); // maxGroupOnline
    sde.types.insert(
        50003,
        inv_type(50003, 763, 7, "Synthetic Ancillary Shield Booster", med_attrs, vec![16, 13]),
    );

    // Rig module with wrong rigSize = 2 vs Rifter rigSize = 1 (type 50004)
    let mut rig_attrs = HashMap::new();
    rig_attrs.insert(attr_ids::UPGRADE_COST, 100.0);
    rig_attrs.insert(1547, 2.0);
    sde.types.insert(
        50004,
        inv_type(50004, 773, 7, "Medium Synthetic Rig", rig_attrs, vec![2663]),
    );

    // Low-slot module restricted to ship group 999 (type 50005)
    let mut low_attrs = HashMap::new();
    low_attrs.insert(1298, 999.0);
    sde.types.insert(
        50005,
        inv_type(50005, 59, 7, "Group-Restricted Gyro", low_attrs, vec![11]),
    );

    // Low-slot module requiring skill 3300 at level 5 (type 50006)
    let mut sk_attrs = HashMap::new();
    sk_attrs.insert(182, 3300.0);
    sk_attrs.insert(277, 5.0);
    sde.types.insert(
        50006,
        inv_type(50006, 59, 7, "Skill-Gated Gyro", sk_attrs, vec![11]),
    );

    // Module with no fitting effect_ids (type 50007)
    sde.types.insert(
        50007,
        inv_type(50007, 59, 7, "No-Effect Module", HashMap::new(), vec![]),
    );

    // Rig granting +1 maxGroupOnline bonus (type 50008, group 1770, effect 6766)
    let mut anc_rig_attrs = HashMap::new();
    anc_rig_attrs.insert(attr_ids::UPGRADE_COST, 50.0);
    anc_rig_attrs.insert(1547, 1.0);
    anc_rig_attrs.insert(2542, 1.0);
    sde.types.insert(
        50008,
        inv_type(50008, 1770, 7, "Ancillary Current Rig", anc_rig_attrs, vec![2663, 6766]),
    );

    // T3C hull (group 963, type 50010) and subsystem (category 32, type 50011)
    let mut t3c_attrs = HashMap::new();
    t3c_attrs.insert(attr_ids::CPU_OUTPUT, 500.0);
    t3c_attrs.insert(attr_ids::POWER_OUTPUT, 1000.0);
    t3c_attrs.insert(1367, 4.0); // maxSubSystems
    t3c_attrs.insert(attr_ids::UPGRADE_CAPACITY, 400.0);
    sde.types.insert(
        50010,
        inv_type(50010, 963, 6, "Synthetic T3C", t3c_attrs, vec![]),
    );

    let mut sub_attrs = HashMap::new();
    sub_attrs.insert(1366, 125.0); // subSystemSlot = 125
    sub_attrs.insert(1374, 2.0); // hiSlots added
    sub_attrs.insert(1375, 2.0); // medSlots added
    sub_attrs.insert(1376, 1.0); // lowSlots added
    sub_attrs.insert(1368, 2.0); // turretSlots added
    sub_attrs.insert(1369, 1.0); // launcherSlots added
    sde.types.insert(
        50011,
        inv_type(50011, 954, 32, "Synthetic Offensive Subsystem", sub_attrs, vec![3772]),
    );

    // T3D hull Confessor (34317, group 1305) and stances (group 1306)
    let mut t3d_attrs = HashMap::new();
    t3d_attrs.insert(attr_ids::CPU_OUTPUT, 200.0);
    t3d_attrs.insert(attr_ids::POWER_OUTPUT, 100.0);
    t3d_attrs.insert(attr_ids::HI_SLOTS, 4.0);
    t3d_attrs.insert(1985, 10_000.0); // stanceSwitchTime = 10s
    sde.types.insert(
        34317,
        inv_type(34317, 1305, 6, "Confessor", t3d_attrs, vec![]),
    );
    for (sid, name) in [
        (34319, "Confessor Defense Mode"),
        (34321, "Confessor Propulsion Mode"),
    ] {
        sde.types
            .insert(sid, inv_type(sid, 1306, 7, name, HashMap::new(), vec![]));
    }

    // Charge with otherID modifier boosting module attribute DAMAGE_MULTIPLIER (+50%) and missile speed
    sde.effects.insert(
        9901,
        DogmaEffectType {
            effect_id: 9901,
            name: "syntheticChargeOtherMod".into(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "otherID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: attr_ids::DAMAGE_MULTIPLIER,
                modifying_attribute_id: 120,
                operator: 6, // PostPercent
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    let mut charge_attrs = HashMap::new();
    charge_attrs.insert(120, 50.0);
    charge_attrs.insert(attr_ids::MAX_VELOCITY, 2500.0);
    sde.types.insert(
        50020,
        inv_type(50020, 83, 8, "Synthetic Charge", charge_attrs, vec![9901]),
    );

    // Drone (category 18, type 50030)
    let mut drone_attrs = HashMap::new();
    drone_attrs.insert(attr_ids::EM_DAMAGE, 32.0);
    sde.types.insert(
        50030,
        inv_type(50030, 100, 18, "Synthetic Scout Drone", drone_attrs, vec![]),
    );

    sde
}

#[test]
fn test_fitting_validation_errors_and_hardpoint_resource_limits() {
    let mut sde = make_synthetic_sde();
    if let Some(rifter) = sde.types.get_mut(&587) {
        rifter.default_attributes.insert(attr_ids::HI_SLOTS, 4.0);
        rifter.default_attributes.insert(attr_ids::MED_SLOTS, 3.0);
        rifter.default_attributes.insert(attr_ids::LOW_SLOTS, 3.0);
        rifter.default_attributes.insert(attr_ids::TURRET_SLOTS_LEFT, 3.0);
        rifter.default_attributes.insert(attr_ids::LAUNCHER_SLOTS_LEFT, 2.0);
        rifter.default_attributes.insert(attr_ids::CPU_OUTPUT, 130.0);
        rifter.default_attributes.insert(attr_ids::POWER_OUTPUT, 38.0);
        rifter.default_attributes.insert(attr_ids::UPGRADE_CAPACITY, 400.0);
        rifter.default_attributes.insert(1547, 1.0);
        rifter.default_attributes.insert(1137, 3.0);
    }

    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde);
    assert!(eng.fit_module(10, 999_999, 27, &sde).unwrap_err().contains("Unknown module type"));
    assert!(eng.fit_module(10, 587, 27, &sde).unwrap_err().contains("not a fittable module"));
    assert!(eng.fit_module(10, 50007, 27, &sde).unwrap_err().contains("no fitting slot effect_ids"));
    assert!(eng.fit_module(10, 50001, 99, &sde).unwrap_err().contains("Invalid fitting slot flag"));
    assert!(eng.fit_module(10, 50001, 11, &sde).unwrap_err().contains("loPower"));
    assert!(eng.fit_module(10, 50001, 19, &sde).unwrap_err().contains("medPower"));
    assert!(eng.fit_module(10, 50005, 27, &sde).unwrap_err().contains("hiPower"));
    assert!(eng.fit_module(10, 50001, 92, &sde).unwrap_err().contains("rigSlot"));
    assert!(eng.fit_module(10, 50001, 125, &sde).unwrap_err().contains("subSystem"));

    assert!(eng.fit_module(10, 50001, 31, &sde).unwrap_err().contains("exceeds hull slot capacity"));
    assert!(eng.fit_module(10, 50004, 92, &sde).unwrap_err().contains("rig"));
    assert!(eng.fit_module(10, 50005, 11, &sde).unwrap_err().contains("cannot be fitted"));
    assert!(eng.fit_module(10, 50006, 11, &sde).is_err());

    // Turret hardpoint overflow (3 turret hardpoints, 4 high slots -> 4th turret rejected)
    eng.fit_module(101, 50001, 27, &sde).expect("turret 1");
    eng.fit_module(102, 50001, 28, &sde).expect("turret 2");
    eng.fit_module(103, 50001, 29, &sde).expect("turret 3");
    assert!(eng.fit_module(104, 50001, 30, &sde).unwrap_err().contains("turret hardpoints"));
    assert!(eng.fit_module(105, 50002, 27, &sde).unwrap_err().contains("already occupied"));

    // 4th high slot takes a rocket launcher, then a 2nd rocket launcher hits maxTypeFitted = 1
    eng.fit_module(106, 50002, 30, &sde).expect("rocket 1");
    eng.unfit_module(103, &sde).expect("free slot 29");
    assert!(eng.fit_module(107, 50002, 29, &sde).unwrap_err().contains("MaxTypeFittedExceeded"));

    // CPU & PowerGrid overflow when fitting online, then rollback
    let err_cpu = eng
        .fit_module_with_dynamic_attributes(
            108,
            50001,
            29,
            true,
            HashMap::from([(attr_ids::CPU, 500.0)]),
            &sde,
        )
        .unwrap_err();
    assert!(err_cpu.contains("CPU"));
    let err_pg = eng
        .fit_module_with_dynamic_attributes(
            108,
            50001,
            29,
            true,
            HashMap::from([(attr_ids::POWER, 500.0)]),
            &sde,
        )
        .unwrap_err();
    assert!(err_pg.contains("Powergrid"));
    assert!(!eng.modules.contains_key(&108));

    // Fit offline bypasses CPU/PG check
    eng.fit_module_with_state(108, 50001, 29, false, &sde).expect("offline fit");
    assert_eq!(eng.modules.get(&108).unwrap().state, ModuleState::Offline);

    // maxGroupFitted = 1 on 50003 + group 1770 rig bonus
    eng.fit_module(201, 50003, 19, &sde).expect("med 1");
    assert!(eng.fit_module(202, 50003, 20, &sde).unwrap_err().contains("group"));
    eng.fit_module(301, 50008, 92, &sde).expect("rig 1770");
}

#[test]
fn test_thermodynamics_heat_burnout_and_nanite_paste() {
    let sde = make_synthetic_sde();
    let mut skills = HashMap::new();
    skills.insert(28164, 4); // Thermodynamics IV -> 20% damage reduction
    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde).with_character_skills(skills);

    eng.fit_module(10, 50001, 27, &sde).expect("fit 27");
    eng.fit_module(11, 50001, 28, &sde).expect("fit 28");

    eng.take_module_offline(10, &sde).expect("offline");
    assert!(eng.activate_module(10, false, &sde).is_err());
    assert!(eng.deactivate_module(10, &sde).is_err());

    eng.set_module_online(10, &sde).expect("online");
    eng.load_charge(10, 50020, 40, &sde).expect("load charge");
    assert!((eng.get_charge_effective_attribute(10, attr_ids::MAX_VELOCITY, &sde) - 2500.0).abs() < 1e-6);
    eng.activate_module(10, true, &sde).expect("overload");
    assert_eq!(eng.modules.get(&10).unwrap().state, ModuleState::Overloaded);
    eng.deactivate_module(10, &sde).expect("deactivate");
    assert_eq!(eng.modules.get(&10).unwrap().state, ModuleState::Online);

    assert!(eng.apply_heat_cycle(999, 0.5, 0.01, 5.0, 0.5, &sde).is_err());
    eng.fit_module(90, 50008, 92, &sde).expect("rig");
    assert!(eng.apply_heat_cycle(90, 0.5, 0.01, 5.0, 0.5, &sde).is_err());

    let r1 = eng.apply_heat_cycle(10, 0.5, 0.01, 5.0, 0.0, &sde).expect("heat 1");
    assert!(r1.rack_heat > 0.0 && r1.rack_heat <= 1.0);
    assert!((r1.module_damage - 4.0).abs() < 1e-6);
    assert!(!r1.module_burned);
    assert_eq!(r1.adjacent_damaged_modules.len(), 1);
    assert_eq!(r1.adjacent_damaged_modules[0].0, 11);

    let r2 = eng.apply_heat_cycle(10, 0.5, 0.01, 5.0, 0.999, &sde).expect("heat 2");
    assert!(r2.adjacent_damaged_modules.is_empty());

    for _ in 0..10 {
        let _ = eng.apply_heat_cycle(10, 0.5, 0.01, 5.0, 0.999, &sde).expect("heat loop");
    }
    let m10 = eng.modules.get(&10).unwrap();
    assert_eq!(m10.damage_hp, m10.max_hp);
    assert_eq!(m10.state, ModuleState::Offline);
    assert!(eng.activate_module(10, false, &sde).unwrap_err().contains("offline"));
    assert!(eng.set_module_online(10, &sde).unwrap_err().contains("burned out"));

    assert!((thermodynamics::step_rack_heat(0.0, 0.2, 0.0, 10.0) - (1.0 - (-2.0_f64).exp())).abs() < 1e-9);
    assert!((thermodynamics::step_rack_heat(0.5, 0.0, 0.1, 10.0) - 0.5 * (-1.0_f64).exp()).abs() < 1e-9);
    assert!((thermodynamics::heat_distance_attenuation(2, 0.5) - (-1.0_f64).exp()).abs() < 1e-9);
    assert!((thermodynamics::heat_damage_probability(0.4, 1, 0.5) - 0.4 * (-0.5_f64).exp()).abs() < 1e-9);
    assert_eq!(thermodynamics::nanite_paste_required(0.0, 5.0), 0);
    assert_eq!(thermodynamics::nanite_paste_required(2.5, 5.0), 13);
}

#[test]
fn test_t3c_subsystems_t3d_stances_drones_and_dynamic_attrs() {
    let sde = make_synthetic_sde();

    let mut t3c = ShipFittingEngine::from_sde(1, 50010, &sde);
    assert!(t3c.fit_module(50, 50011, 126, &sde).unwrap_err().contains("SubsystemSlotMismatch"));
    t3c.fit_module(50, 50011, 125, &sde).expect("fit sub 125");
    assert_eq!(t3c.ship.get_attribute(attr_ids::HI_SLOTS, &sde), 2.0);
    assert_eq!(t3c.ship.get_attribute(attr_ids::MED_SLOTS, &sde), 2.0);
    t3c.fit_module(60, 50001, 28, &sde).expect("fit hi slot 1 (flag 28)");
    assert!(t3c.unfit_module(50, &sde).unwrap_err().contains("SubsystemHasFittedModules"));
    assert!(t3c.modules.contains_key(&50));
    t3c.unfit_module(60, &sde).expect("unfit hi module first");
    t3c.unfit_module(50, &sde).expect("unfit sub succeeds once empty");

    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(34317), Some(34319));
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(34562), Some(34564));
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(34828), Some(35676));
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(35683), Some(35686));
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(587), None);

    assert!(t3c.set_ship_stance(34319, &sde).unwrap_err().contains("not a Tactical Destroyer"));
    let mut t3d = ShipFittingEngine::from_sde(2, 34317, &sde);
    assert!(t3d.set_ship_stance(34564, &sde).unwrap_err().contains("does not belong"));
    assert!(t3d.set_ship_stance(50001, &sde).unwrap_err().contains("not a Tactical Destroyer stance"));
    t3d.set_ship_stance(34319, &sde).expect("initial stance");
    assert_eq!(
        t3d.switch_ship_stance(34321, 1000, &sde).expect("first switch sets timer"),
        34321
    );
    assert!(t3d
        .switch_ship_stance(34319, 3000, &sde)
        .unwrap_err()
        .contains("StanceSwitchCooldownActive"));
    assert_eq!(
        t3d.switch_ship_stance(34319, 13_000, &sde).expect("cooldown elapsed"),
        34319
    );

    t3d.add_drone(700, 50030, &sde);
    assert!((t3d.get_drone_effective_attribute(700, attr_ids::EM_DAMAGE, &sde) - 32.0).abs() < 1e-6);
    assert!(t3d.remove_drone(700, &sde));
    assert!(!t3d.remove_drone(700, &sde));

    t3d.ship.base_attributes.insert(attr_ids::HI_SLOTS, 6.0);
    t3d.ship.base_attributes.insert(attr_ids::TURRET_SLOTS_LEFT, 4.0);
    t3d.ship.base_attributes.insert(attr_ids::CPU_OUTPUT, 200.0);
    t3d.ship.base_attributes.insert(attr_ids::POWER_OUTPUT, 60.0);
    t3d.fit_module(80, 50001, 27, &sde).expect("fit gun");
    let base_attrs = HashMap::from([(attr_ids::CPU, 25.0)]);
    let ranges = HashMap::from([(attr_ids::CPU, [0.5, 1.5])]);
    let rolls = HashMap::from([(attr_ids::CPU, 0.9)]);
    let rolled = roll_abyssal_attributes(&base_attrs, &ranges, &rolls);
    t3d.set_module_dynamic_attributes_and_recalculate(80, rolled, &sde);
    let cpu_eff = t3d.get_module_effective_attribute(80, attr_ids::CPU, &sde);
    assert!((cpu_eff - 35.0).abs() < 1e-6);

    t3d.add_gang_burst_and_recalculate(
        GangBurstEffect {
            source_item_id: 888,
            source_type_id: None,
            dbuff_id: None,
            value: 15.0,
            modifiers: vec![AttributeModifier {
                source_item_id: 888,
                effect_id: None,
                target_attribute_id: attr_ids::SHIELD_CAPACITY,
                operator: ModifierOperator::PostPercent,
                value: 20.0,
                stacking_penalized: false,
            }],
        },
        &sde,
    );
    assert_eq!(evaluate_booster_side_effect(0.4, 0.25, 0, 0, 0.3), Some(0.25));
    assert_eq!(evaluate_booster_side_effect(0.4, 0.25, 2, 2, 0.35), Some(0.225));
    assert_eq!(evaluate_booster_side_effect(0.4, 0.25, 0, 0, 0.4), None);
    assert!((aggregate_dbuff_values("Minimum", &[-10.0, -25.0]) - (-25.0)).abs() < 1e-9);
    assert!((aggregate_dbuff_values("Additive", &[5.0, 12.0]) - 17.0).abs() < 1e-9);
    assert!((aggregate_dbuff_values("Maximum", &[8.0, 22.0]) - 22.0).abs() < 1e-9);
}

#[test]
fn test_t3c_subsystems_stance_errors_hypothetical_modules_and_overflow_rollbacks() {
    let p = std::env::var("EVOMU_SDE_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::Path::new("/Users/sergeya/ai/EvoMu/data/evomu_sde.sqlite").to_path_buf()
        });
    let sde = SdeRegistry::open(p).expect("SDE sqlite");

    // 1. Real V24.01 Strategic Cruiser (Tengu = 29984) + 4 real Tengu subsystems in flags 125..=128
    let mut t3c = ShipFittingEngine::from_sde(9100, 29984, &sde);
    for (idx, name) in [
        "Tengu Defensive - Amplification Node",
        "Tengu Offensive - Accelerated Ejection Bay",
        "Tengu Propulsion - Chassis Optimization",
        "Tengu Core - AugmentedGraviton Reactor",
    ]
    .iter()
    .enumerate()
    {
        if let Some(tid) = sde.types.values().find(|t| t.name == *name).map(|t| t.type_id) {
            let _ = t3c.fit_module_with_state(9110 + idx as i64, tid, 125 + idx as u16, true, &sde);
        }
    }
    t3c.recalculate_stats(&sde);

    // 2. Hypothetical (unfitted) module state & loaded charge attribute inspection
    let ac_id = sde
        .types
        .values()
        .find(|t| t.name == "200mm AutoCannon II")
        .map(|t| t.type_id)
        .unwrap();
    let hypo_overloaded = t3c.build_effective_module_state(999_888, ac_id, true, &sde);
    assert!(hypo_overloaded.get_attribute(attr_ids::DAMAGE_MULTIPLIER, &sde) > 0.0);
    assert_eq!(t3c.get_charge_effective_attribute(999_888, attr_ids::DAMAGE, &sde), 0.0);

    // 3. Non-dynamic CPU, PG, and Calibration overflow rollbacks on fit_module_with_state + offline/unfit error checks
    let mwd_id = sde
        .types
        .values()
        .find(|t| t.name == "500MN Microwarpdrive II")
        .map(|t| t.type_id)
        .unwrap();
    let mut frig = ShipFittingEngine::from_sde(9200, 587, &sde); // Rifter
    assert!(frig.fit_module_with_state(9201, mwd_id, 19, true, &sde).is_err()); // PG overflow on frigate
    let armor_plate_1600 = sde
        .types
        .values()
        .find(|t| t.name == "1600mm Steel Plates II")
        .map(|t| t.type_id)
        .unwrap();
    assert!(frig
        .fit_module_with_state(9202, armor_plate_1600, 11, true, &sde)
        .is_err());
    assert!(frig.unfit_module(999_999, &sde).is_err());
    assert!(frig.deactivate_module(999_999, &sde).is_err());
    assert!(frig.activate_module(999_999, false, &sde).is_err());

    // 4. T3D stance error paths (non-T3D hull, non-stance item, wrong hull stance, and cooldown active)
    assert!(frig.switch_ship_stance(34319, 1000, &sde).is_err()); // Rifter is not T3D
    let mut confessor = ShipFittingEngine::from_sde(9300, 34317, &sde);
    assert!(confessor.switch_ship_stance(587, 1000, &sde).is_err()); // not a stance
    assert!(confessor.switch_ship_stance(34564, 1000, &sde).is_err()); // Svipul stance on Confessor
    confessor.switch_ship_stance(34319, 5000, &sde).unwrap();
    assert!(confessor.switch_ship_stance(34321, 5100, &sde).is_err()); // cooldown active
}

