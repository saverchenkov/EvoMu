use super::*;
use eve_sde::{
    attr_ids, CategoryId, DogmaAttributeType, DogmaEffectType, EffectModifierSpec, GroupId,
    InvType, SdeRegistry,
};
use std::collections::HashMap;

fn f_type(
    type_id: TypeId,
    group_id: GroupId,
    category_id: CategoryId,
    name: &str,
    attrs: &[(AttributeId, f64)],
    effects: &[EffectId],
) -> InvType {
    let mut default_attributes = HashMap::new();
    for &(k, v) in attrs {
        default_attributes.insert(k, v);
    }
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.to_string(),
        mass: 1_000_000.0,
        volume: 10.0,
        capacity: 100.0,
        radius: 10.0,
        base_price: 100.0,
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
        default_effect_id: effects.first().copied(),
        description: String::new(),
        effect_ids: effects.to_vec(),
        default_attributes,
    }
}

fn f_eff(effect_id: EffectId, name: &str, mods: Vec<EffectModifierSpec>) -> DogmaEffectType {
    DogmaEffectType {
        effect_id,
        name: name.to_string(),
        effect_category: 0,
        published: true,
        modifiers: mods,
        ..Default::default()
    }
}

fn f_mod(domain: &str, func: &str, mod_attr: AttributeId, by_attr: AttributeId, op: i32) -> EffectModifierSpec {
    EffectModifierSpec {
        domain: domain.to_string(),
        func: func.to_string(),
        modified_attribute_id: mod_attr,
        modifying_attribute_id: by_attr,
        operator: op,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    }
}

fn build_fitting_sde() -> SdeRegistry {
    let mut sde = SdeRegistry::default();
    for &id in &[
        attr_ids::CPU_OUTPUT,
        attr_ids::POWER_OUTPUT,
        attr_ids::CPU,
        attr_ids::POWER,
        attr_ids::UPGRADE_CAPACITY,
        attr_ids::UPGRADE_COST,
        attr_ids::HI_SLOTS,
        attr_ids::MED_SLOTS,
        attr_ids::LOW_SLOTS,
        attr_ids::RIG_SLOTS,
        1367u16,
        attr_ids::TURRET_SLOTS_LEFT,
        attr_ids::LAUNCHER_SLOTS_LEFT,
    ] {
        sde.attributes.insert(
            id,
            DogmaAttributeType {
                attribute_id: id,
                name: format!("a{id}"),
                default_value: 0.0,
                high_is_good: true,
                stackable: true,
                published: true,
            },
        );
    }

    sde.effects.insert(11, f_eff(11, "loPower", vec![]));
    sde.effects.insert(12, f_eff(12, "hiPower", vec![]));
    sde.effects.insert(13, f_eff(13, "medPower", vec![]));
    sde.effects.insert(16, f_eff(16, "online", vec![]));
    sde.effects.insert(40, f_eff(40, "launcherFitted", vec![]));
    sde.effects.insert(42, f_eff(42, "turretFitted", vec![]));
    sde.effects.insert(2663, f_eff(2663, "rigSlot", vec![]));
    sde.effects.insert(3772, f_eff(3772, "subSystem", vec![]));

    // Standard Cruiser hull (group 26, category 6)
    sde.types.insert(
        624,
        f_type(
            624,
            26,
            6,
            "Caracal",
            &[
                (attr_ids::CPU_OUTPUT, 200.0),
                (attr_ids::POWER_OUTPUT, 200.0),
                (attr_ids::UPGRADE_CAPACITY, 100.0),
                (attr_ids::HI_SLOTS, 2.0),
                (attr_ids::MED_SLOTS, 2.0),
                (attr_ids::LOW_SLOTS, 2.0),
                (attr_ids::RIG_SLOTS, 2.0),
                (attr_ids::TURRET_SLOTS_LEFT, 1.0),
                (attr_ids::LAUNCHER_SLOTS_LEFT, 1.0),
                (1547, 2.0), // rigSize = Medium
            ],
            &[],
        ),
    );

    // T3C hull (group 963) + Subsystem adding low/med/high slots + MJD bonus rig (6766/2542)
    sde.types.insert(
        29984,
        f_type(
            29984,
            963,
            6,
            "Tengu",
            &[
                (attr_ids::CPU_OUTPUT, 500.0),
                (attr_ids::POWER_OUTPUT, 500.0),
                (attr_ids::UPGRADE_CAPACITY, 400.0),
                (attr_ids::HI_SLOTS, 1.0),
                (attr_ids::MED_SLOTS, 1.0),
                (attr_ids::LOW_SLOTS, 1.0),
                (attr_ids::RIG_SLOTS, 3.0),
                (1367u16, 4.0),
                (attr_ids::TURRET_SLOTS_LEFT, 4.0),
                (attr_ids::LAUNCHER_SLOTS_LEFT, 4.0),
                (1547, 2.0),
            ],
            &[],
        ),
    );
    sde.effects.insert(
        9100,
        f_eff(
            9100,
            "t3cSlotBonus",
            vec![
                f_mod("shipID", "ItemModifier", attr_ids::HI_SLOTS, 1374, 2),
                f_mod("shipID", "ItemModifier", attr_ids::MED_SLOTS, 1375, 2),
                f_mod("shipID", "ItemModifier", attr_ids::LOW_SLOTS, 1376, 2),
            ],
        ),
    );
    sde.types.insert(
        30101,
        f_type(
            30101,
            954,
            32,
            "T3C Slot Sub",
            &[(1374, 2.0), (1375, 2.0), (1376, 2.0), (1366, 125.0), (1380, 29984.0)],
            &[3772, 9100],
        ),
    );
    sde.types.insert(
        30102,
        f_type(
            30102,
            954,
            32,
            "Wrong Hull Sub",
            &[(1366, 126.0), (1380, 29988.0)],
            &[3772],
        ),
    );

    // MJD rig (effect 6766 with attr 2542 = 1.0)
    sde.effects.insert(
        6766,
        f_eff(
            6766,
            "rigMjdBonus",
            vec![f_mod("shipID", "ItemModifier", 2542, 2542, 2)],
        ),
    );
    sde.types.insert(
        40001,
        f_type(40001, 773, 7, "MJD Rig", &[(1153, 50.0), (1547, 2.0), (2542, 1.0)], &[2663, 6766]),
    );
    sde.types.insert(
        40002,
        f_type(40002, 773, 7, "Heavy Calibration Rig", &[(1153, 999.0), (1547, 2.0)], &[2663]),
    );
    sde.types.insert(
        40003,
        f_type(40003, 773, 7, "Large Rig", &[(1153, 25.0), (1547, 3.0)], &[2663]),
    );

    // Low, Med, High modules for various validations
    sde.types.insert(10001, f_type(10001, 60, 7, "Low Mod", &[(attr_ids::CPU, 30.0), (attr_ids::POWER, 30.0)], &[11, 16]));
    sde.types.insert(10002, f_type(10002, 60, 7, "Med Mod", &[(attr_ids::CPU, 40.0), (attr_ids::POWER, 20.0)], &[13, 16]));
    sde.types.insert(10003, f_type(10003, 60, 7, "Hi Mod", &[(attr_ids::CPU, 45.0), (attr_ids::POWER, 45.0)], &[12, 16]));
    sde.types.insert(10004, f_type(10004, 53, 7, "Turret", &[(attr_ids::CPU, 10.0), (attr_ids::POWER, 10.0)], &[12, 16, 42]));
    sde.types.insert(10005, f_type(10005, 508, 7, "Launcher", &[(attr_ids::CPU, 10.0), (attr_ids::POWER, 10.0)], &[12, 16, 40]));
    // maxGroupFitted via 1548
    sde.types.insert(10006, f_type(10006, 901, 7, "MaxGrp1548", &[(1548, 1.0)], &[11, 16]));
    // maxGroupFitted via 1544
    sde.types.insert(10007, f_type(10007, 902, 7, "MaxGrp1544", &[(1544, 1.0)], &[11, 16]));
    // maxTypeFitted via 2431
    sde.types.insert(10008, f_type(10008, 903, 7, "MaxType2431", &[(2431, 1.0)], &[11, 16]));
    // maxTypeFitted via 1544 when 1548 >= 1544
    sde.types.insert(10009, f_type(10009, 904, 7, "MaxType1544", &[(1544, 1.0), (1548, 5.0)], &[11, 16]));
    // maxGroupOnline (978 = 1)
    sde.types.insert(10010, f_type(10010, 905, 7, "MaxOnline978", &[(978, 1.0)], &[13, 16]));
    // Prop mod (group 46) with 763 = 1
    sde.types.insert(10011, f_type(10011, 46, 7, "Afterburner", &[(763, 1.0)], &[13, 16]));
    // MJD mod (group 1770, base maxGroupOnline 978 = 1.0)
    sde.types.insert(10012, f_type(10012, 1770, 7, "MJD", &[(978, 1.0)], &[13, 16]));
    // CanFitShipGroup restricted: allows group 26, and another only allowing group 27
    sde.types.insert(10013, f_type(10013, 60, 7, "Group26Only", &[(1298, 26.0)], &[11, 16]));
    sde.types.insert(10014, f_type(10014, 60, 7, "Group27Only", &[(1298, 27.0)], &[11, 16]));
    // CanFitShipType restricted: allows 624, and another allowing 587
    sde.types.insert(10015, f_type(10015, 60, 7, "Type624Only", &[(1302, 624.0)], &[11, 16]));
    sde.types.insert(10016, f_type(10016, 60, 7, "Type587Only", &[(1302, 587.0)], &[11, 16]));
    // Required skills 1..=6 (all 6 slots checked)
    sde.types.insert(
        10017,
        f_type(
            10017,
            60,
            7,
            "All6SkillsMod",
            &[
                (182, 3300.0), (277, 1.0),
                (183, 3301.0), (278, 1.0),
                (184, 3302.0), (279, 1.0),
                (1285, 3303.0), (1286, 1.0),
                (1289, 3304.0), (1287, 1.0),
                (1290, 3305.0), (1288, 2.0),
            ],
            &[11, 16],
        ),
    );
    // Module with no effects / non-module category
    sde.types.insert(10018, f_type(10018, 60, 7, "NoEffects", &[], &[]));
    sde.types.insert(10019, f_type(10019, 60, 8, "ChargeCat", &[], &[11]));
    // Heavy CPU/PG modules for overflow & overload/offline tests
    sde.types.insert(10020, f_type(10020, 60, 7, "HugeCpu", &[(attr_ids::CPU, 500.0)], &[11, 16]));
    sde.types.insert(10021, f_type(10021, 60, 7, "HugePg", &[(attr_ids::POWER, 500.0)], &[11, 16]));

    // T3D hull (Confessor 34317, group 1305) & stances (group 1306) & non-stance / other hull stance
    sde.types.insert(
        34317,
        f_type(
            34317,
            1305,
            6,
            "Confessor",
            &[
                (attr_ids::CPU_OUTPUT, 200.0),
                (attr_ids::POWER_OUTPUT, 200.0),
                (1985, 10_000.0),
            ],
            &[],
        ),
    );
    sde.types.insert(34319, f_type(34319, 1306, 87, "Confessor Defense", &[], &[]));
    sde.types.insert(34321, f_type(34321, 1306, 87, "Confessor Propulsion", &[], &[]));
    sde.types.insert(34564, f_type(34564, 1306, 87, "Svipul Defense", &[], &[]));

    sde
}

#[test]
fn fitting_validation_limits_rollback_and_dynamic_attributes_edges() {
    let sde = build_fitting_sde();

    // Unknown hull & invalid item errors
    let mut bad_hull = ShipFittingEngine::from_sde(1, 999_999, &sde);
    assert!(bad_hull.fit_module(1, 10001, 11, &sde).is_err());

    let mut eng = ShipFittingEngine::from_sde(1, 624, &sde);
    assert!(eng.fit_module(1, 999_999, 11, &sde).is_err());
    assert!(eng.fit_module(1, 10019, 11, &sde).is_err()); // category != 7/87/32
    assert!(eng.fit_module(1, 10018, 11, &sde).is_err()); // empty effect_ids
    assert!(eng.fit_module(1, 10001, 999, &sde).is_err()); // invalid flag
    assert!(eng.fit_module(1, 10001, 27, &sde).is_err()); // loPower fitted in high slot

    // Slot capacity exceeded for Low (2 slots) / Med / High / Rig
    eng.fit_module(1, 10001, 11, &sde).unwrap();
    // Occupying same slot with DIFFERENT item_id fails; refitting SAME item_id succeeds
    assert!(eng.fit_module(99, 10001, 11, &sde).is_err());
    eng.fit_module(1, 10001, 11, &sde).unwrap();
    eng.fit_module(2, 10001, 12, &sde).unwrap();
    assert!(eng.fit_module(3, 10001, 13, &sde).is_err()); // 3rd low slot > 2

    // Turret and Launcher hardpoints (cap = 1)
    eng.fit_module(4, 10004, 27, &sde).unwrap();
    assert!(eng.fit_module(5, 10004, 28, &sde).is_err());
    eng.fit_module(6, 10005, 28, &sde).unwrap();
    assert!(eng.unfit_module(4, &sde).is_ok());
    assert!(eng.unfit_module(6, &sde).is_ok());
    assert!(eng.unfit_module(1, &sde).is_ok());
    assert!(eng.unfit_module(2, &sde).is_ok());

    // maxGroupFitted (1548 / 1544), maxTypeFitted (2431 / 1544+1548)
    eng.fit_module(10, 10006, 11, &sde).unwrap();
    assert!(eng.fit_module(11, 10006, 12, &sde).is_err());
    eng.unfit_module(10, &sde).unwrap();
    eng.fit_module(10, 10007, 11, &sde).unwrap();
    assert!(eng.fit_module(11, 10007, 12, &sde).is_err());
    eng.unfit_module(10, &sde).unwrap();
    eng.fit_module(10, 10008, 11, &sde).unwrap();
    assert!(eng.fit_module(11, 10008, 12, &sde).is_err());
    eng.unfit_module(10, &sde).unwrap();
    eng.fit_module(10, 10009, 11, &sde).unwrap();
    assert!(eng.fit_module(11, 10009, 12, &sde).is_err());
    eng.unfit_module(10, &sde).unwrap();

    // maxGroupOnline (978): fit 1 online, 2nd offline allowed, 2nd online rejected
    eng.fit_module(12, 10010, 19, &sde).unwrap();
    eng.fit_module_with_state(13, 10010, 20, false, &sde).unwrap();
    assert!(eng.set_module_online(13, &sde).is_err());
    assert!(eng.fit_module(13, 10010, 20, &sde).is_err());
    eng.unfit_module(12, &sde).unwrap();
    eng.unfit_module(13, &sde).unwrap();

    // Prop mod (group 46): max 1 online
    eng.fit_module(14, 10011, 19, &sde).unwrap();
    eng.fit_module_with_state(15, 10011, 20, false, &sde).unwrap();
    assert!(eng.set_module_online(15, &sde).is_err());
    eng.unfit_module(14, &sde).unwrap();
    eng.unfit_module(15, &sde).unwrap();

    // Hull group/type restrictions + rig size + calibration overflow
    eng.fit_module(16, 10013, 11, &sde).unwrap(); // canFitShipGroup == 26 ok
    eng.fit_module(17, 10015, 12, &sde).unwrap(); // canFitShipType == 624 ok
    assert!(eng.unfit_module(16, &sde).is_ok());
    assert!(eng.unfit_module(17, &sde).is_ok());
    assert!(eng.fit_module(16, 10014, 11, &sde).is_err());
    assert!(eng.fit_module(17, 10016, 11, &sde).is_err());
    assert!(eng.fit_module(18, 40003, 92, &sde).is_err()); // Large rig on Medium hull
    assert!(eng.fit_module(18, 40002, 92, &sde).is_err()); // Calibration > 100

    // Required skills 1..=6: fail on missing skill 6 (3305), then pass when all 6 satisfied
    eng.set_character_skills(HashMap::from([
        (3300, 1), (3301, 1), (3302, 1), (3303, 1), (3304, 1), (3305, 1),
    ]));
    assert!(eng.fit_module(19, 10017, 11, &sde).is_err());
    eng.character_skills.insert(3305, 2);
    eng.fit_module(19, 10017, 11, &sde).unwrap();
    eng.unfit_module(19, &sde).unwrap();

    // fit_module_with_dynamic_attributes rollback on CPU/PG failure & success when reduced
    eng.fit_module(20, 10001, 11, &sde).unwrap(); // prev entry in slot 11
    assert!(eng
        .fit_module_with_dynamic_attributes(
            20,
            10020,
            11,
            true,
            HashMap::from([(attr_ids::CPU, 400.0)]),
            &sde
        )
        .is_err());
    assert_eq!(eng.modules[&20].type_id, 10001); // refit rollback preserved prev entry!

    eng.fit_module_with_dynamic_attributes(
        21,
        10020,
        12,
        true,
        HashMap::from([(attr_ids::CPU, 10.0)]),
        &sde,
    )
    .unwrap();
    assert_eq!(eng.modules[&21].dynamic_attribute_overrides[&attr_ids::CPU], 10.0);

    // Dynamic calibration override rollback on rig
    assert!(eng
        .fit_module_with_dynamic_attributes(
            22,
            40002,
            92,
            true,
            HashMap::from([(attr_ids::UPGRADE_COST, 500.0)]),
            &sde
        )
        .is_err());
    assert!(!eng.modules.contains_key(&22));

    // Burned-out online error & set_module_online unknown / burned out / rollback
    assert!(eng.set_module_online(999, &sde).is_err());
    eng.modules.get_mut(&20).unwrap().damage_hp = 40.0;
    eng.modules.get_mut(&20).unwrap().max_hp = 40.0;
    assert!(eng.fit_module(20, 10001, 11, &sde).is_err());
    assert!(eng.set_module_online(20, &sde).is_err());
    assert!(eng.activate_module(20, false, &sde).is_err());
    assert!(eng.activate_module(999, false, &sde).is_err());
    assert!(eng.deactivate_module(999, &sde).is_err());
    assert!(eng.load_charge(999, 1, 1, &sde).is_err());
    eng.modules.get_mut(&20).unwrap().damage_hp = 0.0;
    eng.take_module_offline(20, &sde).unwrap();
    assert!(eng.take_module_offline(999, &sde).is_err());
    assert!(eng.activate_module(20, false, &sde).is_err()); // offline cannot activate

    // MJD rig bonus (effect 6766 / attr 2542) allows 2 MJDs on Tengu, blocks 3rd
    let mut t3c = ShipFittingEngine::from_sde(2, 29984, &sde);
    assert!(t3c.fit_module(1, 30101, 126, &sde).is_err()); // wrong subsystem slot 126 != 125
    t3c.fit_module(1, 30101, 125, &sde).unwrap(); // gives +2 high/med/low
    assert!(t3c.fit_module(2, 30102, 126, &sde).is_err()); // wrong hull subsystem (fitsToShipType 29988 != 29984)
    t3c.fit_module(3, 40001, 92, &sde).unwrap(); // MJD rig (+1 max MJD)
    t3c.fit_module(4, 10012, 19, &sde).unwrap();
    t3c.fit_module(5, 10012, 20, &sde).unwrap();
    t3c.fit_module_with_state(6, 10012, 21, false, &sde).unwrap();
    assert!(t3c.set_module_online(6, &sde).is_err()); // 3rd online MJD blocked!
    t3c.unfit_module(6, &sde).unwrap();
    // Unfit subsystem blocked when extra low/med/high slots are occupied!
    assert!(t3c.unfit_module(1, &sde).is_err()); // 2 med mods > 1 base med slot!
    assert!(t3c.modules.contains_key(&1)); // restored!
}

#[test]
fn overload_offline_enforcement_heat_abyssal_and_t3d_stance_edges() {
    let sde = build_fitting_sde();
    let mut eng = ShipFittingEngine::from_sde(1, 624, &sde);
    eng.fit_module(1, 10001, 11, &sde).unwrap(); // low 30/30
    eng.fit_module(2, 10002, 19, &sde).unwrap(); // med 40/20
    eng.fit_module(3, 10003, 27, &sde).unwrap(); // high 45/45

    // Force ship PG overload via direct insert of an online rig + verify high/med offlined before low
    eng.modules.insert(
        4,
        FittedModule {
            item_id: 4,
            type_id: 10021, // 500 PG in med slot 20
            flag_id: 20,
            state: ModuleState::Online,
            damage_hp: 0.0,
            max_hp: 40.0,
            charge_type_id: None,
            charge_quantity: 0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    eng.recalculate_stats(&sde);
    let offlined = eng.enforce_post_unfit_or_offline_constraints(&sde);
    assert!(offlined.contains(&4));
    assert_eq!(eng.modules[&4].state, ModuleState::Offline);

    // Heat cycles across High (27), Med (19), and Low (11) racks + offline module + adjacent burnout
    assert!(eng.apply_heat_cycle(999, 0.2, 0.05, 5.0, 0.5, &sde).is_err());
    let _rep_low = eng.apply_heat_cycle(1, 0.2, 0.05, 5.0, 0.99, &sde).unwrap();
    let _rep_med = eng.apply_heat_cycle(2, 0.8, 0.01, 20.0, 0.01, &sde).unwrap();
    // Pre-damage adjacent slot 20 so heat bleed burns it out cleanly
    eng.modules.get_mut(&4).unwrap().max_hp = 5.0;
    eng.modules.get_mut(&4).unwrap().damage_hp = 4.99;
    eng.modules.get_mut(&4).unwrap().state = ModuleState::Online;
    let rep_adj = eng.apply_heat_cycle(2, 0.9, 0.01, 30.0, 0.0, &sde).unwrap();
    assert!(!rep_adj.adjacent_damaged_modules.is_empty());

    // Thermodynamics helpers & booster side effects & dbuff aggregation & abyssal roll
    assert_eq!(thermodynamics::step_rack_heat(0.5, 0.0, 0.1, 10.0).round(), 0.0);
    assert!(thermodynamics::heat_distance_attenuation(1, 0.5) < 1.0);
    assert!(thermodynamics::heat_damage_probability(0.8, 1, 0.2) > 0.0);
    assert!((thermodynamics::effective_heat_damage(10.0, 5) - 7.5).abs() < 1e-6);
    assert!(thermodynamics::per_slot_heat_roll(0.5, 42) < 1.0);
    assert!(evaluate_booster_side_effect(0.4, 20.0, 0, 5, 0.1).is_some());
    assert!(evaluate_booster_side_effect(0.4, 20.0, 5, 0, 0.9).is_none());

    assert_eq!(aggregate_dbuff_values("Minimum", &[]), 0.0);
    assert_eq!(aggregate_dbuff_values("UnknownMode", &[1.0, 2.0]), 2.0);
    assert_eq!(aggregate_dbuff_values("Minimum", &[5.0, -3.0, 2.0]), -3.0);
    assert_eq!(aggregate_dbuff_values("Maximum", &[5.0, -3.0, 2.0]), 5.0);
    assert_eq!(aggregate_dbuff_values("Additive", &[5.0, -3.0, 2.0]), 4.0);

    let rolled = roll_abyssal_attributes(
        &HashMap::from([(9u16, 100.0)]),
        &HashMap::from([(9u16, [0.8, 1.2]), (999u16, [0.5, 1.5])]),
        &HashMap::from([(9u16, 5.0)]), // clamped to 1.0 -> 120.0
    );
    assert!((rolled[&9] - 120.0).abs() < 1e-6);
    assert!(!rolled.contains_key(&999));

    // Dynamic attribute setters (present + missing) & gang burst helpers
    eng.set_module_dynamic_attribute_and_recalculate(1, 50, 12.0, &sde);
    eng.set_module_dynamic_attributes_and_recalculate(1, HashMap::from([(51, 13.0)]), &sde);
    eng.set_module_dynamic_attribute(999, 50, 12.0);
    eng.set_module_dynamic_attributes(999, HashMap::from([(51, 13.0)]));
    eng.add_gang_burst_and_recalculate(
        GangBurstEffect {
            source_item_id: 1,
            source_type_id: None,
            dbuff_id: None,
            value: 0.0,
            modifiers: vec![],
        },
        &sde,
    );
    eng.apply_gang_burst(
        GangBurstEffect {
            source_item_id: 2,
            source_type_id: None,
            dbuff_id: None,
            value: 0.0,
            modifiers: vec![],
        },
        &sde,
    );

    // T3D stances: default_t3d_stance_type_id, t3d_stance_belongs_to_hull, set_ship_stance, switch_ship_stance
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(34317), Some(34319));
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(34562), Some(34564));
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(34828), Some(35676));
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(35683), Some(35686));
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(624), None);

    assert!(ShipFittingEngine::t3d_stance_belongs_to_hull(34317, 34321));
    assert!(!ShipFittingEngine::t3d_stance_belongs_to_hull(34317, 34564));
    assert!(ShipFittingEngine::t3d_stance_belongs_to_hull(34562, 34566));
    assert!(ShipFittingEngine::t3d_stance_belongs_to_hull(34828, 35677));
    assert!(ShipFittingEngine::t3d_stance_belongs_to_hull(35683, 35687));
    assert!(ShipFittingEngine::t3d_stance_belongs_to_hull(624, 34319));

    // Non-1305 hull rejected for stance
    assert!(eng.set_ship_stance(34319, &sde).is_err());
    assert!(eng.switch_ship_stance(34319, 1_000, &sde).is_err());

    let mut t3d = ShipFittingEngine::from_sde(10, 34317, &sde);
    assert!(t3d.set_ship_stance(999_999, &sde).is_err()); // unknown stance
    assert!(t3d.set_ship_stance(624, &sde).is_err()); // non-1306 stance
    assert!(t3d.set_ship_stance(34564, &sde).is_err()); // wrong hull stance
    t3d.set_ship_stance(34319, &sde).unwrap();
    // Same-stance switch inside cooldown is allowed (idempotent); different stance < 10_000 ms elapsed is rejected; >= 10_000 ms succeeds
    assert_eq!(t3d.switch_ship_stance(34319, 5_000, &sde).unwrap(), 34319);
    assert!(t3d.switch_ship_stance(34321, 9_999, &sde).is_err());
    assert_eq!(t3d.switch_ship_stance(34321, 15_000, &sde).unwrap(), 34321);
}
