//! Integration tests for `eve-dogma` real SDE fitting cascades, dbuff operators,
//! VM opcodes, range falloff cutoffs, and wormhole phenomena (`DG-1`..`DG-4`).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::Path;

use eve_dogma::{
    compute_falloff_with_cutoff, compute_passive_recharge_rate, resolve_system_wormhole_phenomenon,
    AttributeModifier, DogmaEffectContext, DogmaExpressionNode, DogmaItemState, DogmaVmOpcode,
    ModifierOperator, ModuleState, ShipFittingEngine, REQUIRED_SKILL_ATTR_IDS,
    REQUIRED_SKILL_LEVEL_ATTR_IDS,
};
use eve_sde::{
    attr_ids, DbuffCollectionInfo, DbuffGroupModifier, DbuffSkillModifier, DogmaEffectType,
    EffectModifierSpec, InvType, SdeRegistry,
};

fn load_sde() -> SdeRegistry {
    let p = std::env::var("EVOMU_SDE_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| Path::new("/Users/sergeya/ai/EvoMu/data/evomu_sde.sqlite").to_path_buf());
    SdeRegistry::open(p).expect("SDE sqlite")
}

fn find_type(sde: &SdeRegistry, name: &str) -> u32 {
    sde.types
        .values()
        .find(|t| t.name == name)
        .map(|t| t.type_id)
        .unwrap_or_else(|| panic!("missing SDE type {name}"))
}

fn make_type(
    base: &InvType,
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
        published: true,
        default_attributes,
        effect_ids,
        ..base.clone()
    }
}

fn const_node(v: f64) -> DogmaExpressionNode {
    DogmaExpressionNode {
        opcode: DogmaVmOpcode::DefFloat,
        value_f64: Some(v),
        attribute_id: None,
        operator: None,
        arg1: None,
        arg2: None,
    }
}

#[test]
fn test_real_sde_combat_ship_fit_overloads_scripts_and_drones() {
    let sde = load_sde();
    let hull_id = find_type(&sde, "Hurricane");
    let mut engine = ShipFittingEngine::from_sde(100, hull_id, &sde);

    // Equip full level-V skills so all T2 modules & drones qualify cleanly
    let mut skills = HashMap::new();
    for tid in sde
        .types
        .values()
        .filter(|t| t.category_id == 16 && t.published)
        .map(|t| t.type_id)
    {
        skills.insert(tid, 5u8);
    }
    engine.set_character_skills(skills);
    engine.recalculate_stats(&sde);

    let base_vel = engine.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
    let base_sig = engine.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde);

    // High slot: 425mm AutoCannon II with Hail M loaded & active + overloaded
    let ac_id = find_type(&sde, "425mm AutoCannon II");
    let hail_m = find_type(&sde, "Hail M");
    engine.fit_module_with_state(1001, ac_id, 27, true, &sde).unwrap();
    engine.load_charge(1001, hail_m, 40, &sde).unwrap();
    engine.activate_module(1001, true, &sde).unwrap();

    // Med slots: 50MN Y-T8 Compact Microwarpdrive (active), Multispectrum Shield Hardener II (active + overloaded),
    // Tracking Computer II with Optimal Range Script
    let mwd_id = find_type(&sde, "50MN Y-T8 Compact Microwarpdrive");
    let hardener_id = find_type(&sde, "Multispectrum Shield Hardener II");
    let tc_id = find_type(&sde, "Tracking Computer II");
    let opt_script = find_type(&sde, "Optimal Range Script");
    engine.fit_module_with_state(1002, mwd_id, 19, true, &sde).unwrap();
    engine.activate_module(1002, false, &sde).unwrap();
    engine.fit_module_with_state(1003, hardener_id, 20, true, &sde).unwrap();
    engine.activate_module(1003, true, &sde).unwrap();
    engine.fit_module_with_state(1004, tc_id, 21, true, &sde).unwrap();
    engine.load_charge(1004, opt_script, 1, &sde).unwrap();
    engine.activate_module(1004, false, &sde).unwrap();

    // Low slots: Damage Control II, Gyrostabilizer II, Nanofiber Internal Structure II
    let dcu_id = find_type(&sde, "Damage Control II");
    let gyro_id = find_type(&sde, "Gyrostabilizer II");
    let nano_id = find_type(&sde, "Nanofiber Internal Structure II");
    engine.fit_module_with_state(1005, dcu_id, 11, true, &sde).unwrap();
    engine.fit_module_with_state(1006, gyro_id, 12, true, &sde).unwrap();
    engine.fit_module_with_state(1007, nano_id, 13, true, &sde).unwrap();

    // Launch Warrior II drone & inspect effective drone attributes
    let warrior_id = find_type(&sde, "Warrior II");
    engine.drones.push((5001, warrior_id));
    engine.recalculate_stats(&sde);

    let eff_vel = engine.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde);
    let eff_sig = engine.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, &sde);
    let eff_dmg_mult = engine.get_module_effective_attribute(1001, attr_ids::DAMAGE_MULTIPLIER, &sde);
    let base_dmg_mult = sde
        .types
        .get(&ac_id)
        .and_then(|t| t.default_attributes.get(&attr_ids::DAMAGE_MULTIPLIER).copied())
        .unwrap_or(1.0);
    assert!(eff_vel > base_vel * 2.5, "MWD should strongly boost velocity");
    assert!(eff_sig > base_sig * 2.0, "MWD active effect blooms signature radius");
    assert!(eff_dmg_mult > base_dmg_mult, "Gyro + overload should boost turret damage multiplier");

    let base_drone_dmg = sde
        .types
        .get(&warrior_id)
        .and_then(|t| t.default_attributes.get(&attr_ids::DAMAGE_MULTIPLIER).copied())
        .unwrap_or(1.0);
    let eff_drone_dmg = engine.get_drone_effective_attribute(5001, attr_ids::DAMAGE_MULTIPLIER, &sde);
    assert!(eff_drone_dmg >= base_drone_dmg);
    assert_eq!(engine.get_drone_effective_attribute(999_999_999, 64, &sde), 0.0);

    // Offline active/overloaded module automatically deactivates & de-overloads it
    engine.take_module_offline(1001, &sde).unwrap();
    assert_eq!(engine.modules.get(&1001).unwrap().state, ModuleState::Offline);
    engine.set_module_online(1001, &sde).unwrap();
    assert_eq!(engine.modules.get(&1001).unwrap().state, ModuleState::Online);
}

#[test]
fn test_fitting_calibration_overflow_max_group_online_and_offline_cascade() {
    let mut sde = load_sde();
    let base = sde.types.get(&587).expect("Rifter").clone();

    let hull_id = 990_001u32;
    sde.types.insert(
        hull_id,
        make_type(
            &base,
            hull_id,
            25,
            6,
            "Test Frigate",
            HashMap::from([
                (attr_ids::CPU_OUTPUT, 100.0),
                (attr_ids::POWER_OUTPUT, 100.0),
                (attr_ids::UPGRADE_CAPACITY, 100.0),
                (attr_ids::HI_SLOTS, 4.0),
                (attr_ids::MED_SLOTS, 4.0),
                (attr_ids::LOW_SLOTS, 4.0),
                (attr_ids::RIG_SLOTS, 3.0),
                (1547, 1.0),
            ]),
            vec![],
        ),
    );

    let rig_id = 990_002u32;
    sde.types.insert(
        rig_id,
        make_type(
            &base,
            rig_id,
            774,
            7,
            "Test Cal Rig",
            HashMap::from([(attr_ids::UPGRADE_COST, 75.0), (1547, 1.0)]),
            vec![2663],
        ),
    );

    let burst_id = 990_003u32;
    sde.types.insert(
        burst_id,
        make_type(
            &base,
            burst_id,
            1770,
            7,
            "Test Command Burst",
            HashMap::from([(978, 1.0), (attr_ids::CPU, 10.0)]),
            vec![16, 12],
        ),
    );

    let burst_rig_id = 990_004u32;
    sde.types.insert(
        burst_rig_id,
        make_type(
            &base,
            burst_rig_id,
            774,
            7,
            "Test Burst Rig",
            HashMap::from([(2542, 1.0), (attr_ids::UPGRADE_COST, 10.0), (1547, 1.0)]),
            vec![2663, 6766],
        ),
    );

    let mut st = ShipFittingEngine::from_sde(500, hull_id, &sde);
    st.fit_module_with_state(2001, rig_id, 92, true, &sde).unwrap();
    // Second 75-cal rig overflows 75 + 75 > 100 in fit_module_with_state
    assert!(st.fit_module_with_state(2002, rig_id, 93, true, &sde).is_err());
    // Fit dynamic-attribute 75-cal rig and verify rollback on Insufficient Calibration
    assert!(st
        .fit_module_with_dynamic_attributes(
            2002,
            burst_rig_id,
            93,
            true,
            HashMap::from([(attr_ids::UPGRADE_COST, 75.0)]),
            &sde,
        )
        .is_err());
    assert!(!st.modules.contains_key(&2002));

    // Command burst maxGroupOnline: 1 allowed without rig, 2 with burst rig, 3rd rejected
    st.fit_module_with_state(2010, burst_id, 27, true, &sde).unwrap();
    st.fit_module_with_state(2011, burst_id, 28, false, &sde).unwrap();
    assert!(st.set_module_online(2011, &sde).is_err());
    st.fit_module_with_state(2012, burst_rig_id, 93, true, &sde).unwrap();
    st.set_module_online(2011, &sde).unwrap();
    assert!(st.fit_module_with_state(2013, burst_id, 29, true, &sde).is_err());
    st.unfit_module(2010, &sde).unwrap();
    st.unfit_module(2011, &sde).unwrap();

    // Multi-rack offline cascade (enforce_post_unfit_or_offline_constraints)
    let mod_id = 990_005u32;
    sde.types.insert(
        mod_id,
        make_type(
            &base,
            mod_id,
            53,
            7,
            "Heavy CPU Module",
            HashMap::from([(attr_ids::CPU, 60.0), (attr_ids::POWER, 60.0)]),
            vec![16, 12, 13, 11],
        ),
    );
    st.fit_module_with_state(2020, mod_id, 30, false, &sde).unwrap(); // high slot
    st.fit_module_with_state(2021, mod_id, 19, false, &sde).unwrap(); // med slot
    st.fit_module_with_state(2022, mod_id, 11, false, &sde).unwrap(); // low slot
    for id in [2020, 2021, 2022] {
        st.modules.get_mut(&id).unwrap().state = ModuleState::Online;
    }
    st.recalculate_stats(&sde);
    let forced = st.enforce_post_unfit_or_offline_constraints(&sde);
    assert!(forced.len() >= 2);
    assert_eq!(forced[0], 2020);
    assert_eq!(forced[1], 2021);
}

#[test]
fn test_dbuff_modes_context_domains_vm_opcodes_and_wormhole_phenomena() {
    let mut sde = load_sde();
    let base = sde.types.get(&587).expect("Rifter").clone();

    // 1. Exercise dbuff operators ("PostMul", "ModAdd", "PostAssignment") & aggregate modes
    let ops = [
        (880_001u32, "PostMul", "Minimum"),
        (880_002u32, "ModAdd", "Maximum"),
        (880_003u32, "PostAssignment", "Average"),
    ];
    for (dbuff_id, op_str, agg_str) in ops {
        sde.dbuff_collections.insert(
            dbuff_id,
            DbuffCollectionInfo {
                dbuff_id,
                name: format!("test {op_str}"),
                operation: op_str.into(),
                aggregate_mode: agg_str.into(),
                display_name_id: None,
                show_output_value_in_ui: None,
                item_modifiers: vec![],
                location_modifiers: vec![attr_ids::MASS],
                location_group_modifiers: vec![DbuffGroupModifier {
                    attribute_id: attr_ids::SPEED_FACTOR,
                    group_id: 46,
                }],
                location_required_skill_modifiers: vec![DbuffSkillModifier {
                    attribute_id: attr_ids::DAMAGE_MULTIPLIER,
                    skill_id: 3300,
                }],
                target_attribute_ids: vec![],
            },
        );
    }

    let burst_mod_id = 880_010u32;
    let charge_id = 880_011u32;
    let ab_mod_id = 880_012u32;
    let hull_id = 880_013u32;

    sde.types.insert(
        hull_id,
        make_type(
            &base,
            hull_id,
            25,
            6,
            "Dbuff Frigate",
            HashMap::from([
                (attr_ids::CPU_OUTPUT, 500.0),
                (attr_ids::POWER_OUTPUT, 500.0),
                (attr_ids::HI_SLOTS, 4.0),
                (attr_ids::MED_SLOTS, 4.0),
                (attr_ids::LOW_SLOTS, 4.0),
            ]),
            vec![],
        ),
    );
    sde.types.insert(
        ab_mod_id,
        make_type(
            &base,
            ab_mod_id,
            46,
            7,
            "Dbuff Afterburner",
            HashMap::from([
                (attr_ids::SPEED_FACTOR, 100.0),
                (attr_ids::DAMAGE_MULTIPLIER, 2.0),
                (attr_ids::REQUIRED_SKILL_1, 3300.0),
            ]),
            vec![16, 13],
        ),
    );
    sde.types.insert(
        charge_id,
        make_type(
            &base,
            charge_id,
            1774,
            8,
            "Dbuff Charge",
            HashMap::from([
                (2457, 880_001.0),
                (2458, 1.25),
                (2459, 880_002.0),
                (2460, 10.0),
                (2461, 880_003.0),
                (2462, 5.0),
            ]),
            vec![],
        ),
    );
    sde.types.insert(
        burst_mod_id,
        make_type(
            &base,
            burst_mod_id,
            1770,
            7,
            "Dbuff Burst",
            HashMap::new(),
            vec![16, 12, 6753],
        ),
    );
    sde.effects.insert(
        6753,
        DogmaEffectType {
            effect_id: 6753,
            name: "commandBurstEffect".into(),
            effect_category: 1,
            is_offensive: false,
            is_assistance: true,
            is_warp_safe: true,
            modifiers: vec![],
            ..Default::default()
        },
    );

    let mut st = ShipFittingEngine::from_sde(700, hull_id, &sde);
    st.set_character_skills(HashMap::from([(3300, 5)]));
    st.fit_module_with_state(3001, ab_mod_id, 19, true, &sde).unwrap();
    st.fit_module_with_state(3002, burst_mod_id, 27, true, &sde).unwrap();
    st.load_charge(3002, charge_id, 10, &sde).unwrap();
    st.activate_module(3002, false, &sde).unwrap();
    let eval = st.evaluated_context(&sde);
    assert!(eval.ship.get_attribute(attr_ids::MASS, &sde) > 0.0);

    // 2. Context domains, range falloff cutoff, and modifier removal
    let mut ctx = DogmaEffectContext::new(
        DogmaItemState::from_sde(1, hull_id, &sde),
        DogmaItemState::from_sde(2, hull_id, &sde),
        DogmaItemState::from_sde(10, 1373, &sde),
        DogmaItemState::from_sde(20, hull_id, &sde),
        DogmaItemState::from_sde(30, charge_id, &sde),
        DogmaItemState::from_sde(35, hull_id, &sde),
    );
    ctx.char_located_items
        .push(DogmaItemState::from_sde(40, ab_mod_id, &sde));
    assert!((compute_falloff_with_cutoff(5_000.0, 10_000.0, 5_000.0) - 1.0).abs() < 1e-12);
    assert!(compute_falloff_with_cutoff(15_000.0, 10_000.0, 5_000.0) < 1.0);
    assert_eq!(compute_falloff_with_cutoff(30_000.0, 10_000.0, 5_000.0), 0.0);

    let range_eff = DogmaEffectType {
        effect_id: 59_100,
        name: "rangeTest".into(),
        effect_category: 1,
        is_offensive: true,
        is_assistance: false,
        is_warp_safe: true,
        modifiers: vec![EffectModifierSpec {
            func: "LocationModifier".into(),
            domain: "charID".into(),
            modified_attribute_id: attr_ids::MAX_VELOCITY,
            modifying_attribute_id: attr_ids::SPEED_FACTOR,
            operator: 6,
            group_id: None,
            skill_type_id: None,
            effect_id: None,
        }],
        ..Default::default()
    };
    ctx.apply_effect_with_range(3001, &range_eff, 12_000.0, 10_000.0, 5_000.0, &sde);
    assert_eq!(ctx.char_located_items[0].modifiers.len(), 1);
    ctx.char_located_items[0].remove_modifiers_from_source_effect(3001, Some(59_100));
    assert!(ctx.char_located_items[0].modifiers.is_empty());
    ctx.char_located_items[0].add_modifier(AttributeModifier {
        source_item_id: 3001,
        effect_id: None,
        target_attribute_id: attr_ids::MASS,
        operator: ModifierOperator::PostPercent,
        value: 10.0,
        stacking_penalized: false,
    });
    ctx.char_located_items[0].remove_modifiers_from_source(3001);
    assert!(ctx.char_located_items[0].modifiers.is_empty());

    assert_eq!(compute_passive_recharge_rate(0.0, 100.0, 10.0), 0.0);
    assert!(compute_passive_recharge_rate(25.0, 100.0, 10.0) > 0.0);

    // 3. VM opcodes (SkillCheck, VerifyTargetGroup, If true & false)
    let mut item = DogmaItemState::from_sde(1, hull_id, &sde);
    item.base_attributes.insert(REQUIRED_SKILL_ATTR_IDS[0], 3300.0);
    item.base_attributes.insert(REQUIRED_SKILL_LEVEL_ATTR_IDS[0], 3.0);
    let skill_chk_default = DogmaExpressionNode {
        opcode: DogmaVmOpcode::SkillCheck,
        value_f64: None,
        attribute_id: None,
        operator: None,
        arg1: None,
        arg2: None,
    };
    assert_eq!(skill_chk_default.evaluate(&item, &sde), 0.0);
    item.set_skill_level(3300, 3);
    assert_eq!(skill_chk_default.evaluate(&item, &sde), 1.0);

    let skill_chk_explicit = DogmaExpressionNode {
        opcode: DogmaVmOpcode::SkillCheck,
        value_f64: None,
        attribute_id: Some(3300),
        operator: None,
        arg1: Some(Box::new(const_node(4.0))),
        arg2: None,
    };
    assert_eq!(skill_chk_explicit.evaluate(&item, &sde), 0.0);

    let grp_check = DogmaExpressionNode {
        opcode: DogmaVmOpcode::VerifyTargetGroup,
        value_f64: Some(25.0),
        attribute_id: None,
        operator: None,
        arg1: None,
        arg2: None,
    };
    assert_eq!(grp_check.evaluate(&item, &sde), 1.0);

    let mut target_item = DogmaItemState::from_sde(2, hull_id, &sde);
    let inner_add = DogmaExpressionNode {
        opcode: DogmaVmOpcode::AddItmMod,
        value_f64: None,
        attribute_id: Some(attr_ids::MASS),
        operator: Some(ModifierOperator::PostPercent),
        arg1: None,
        arg2: Some(Box::new(const_node(15.0))),
    };
    let if_false = DogmaExpressionNode {
        opcode: DogmaVmOpcode::If,
        value_f64: None,
        attribute_id: None,
        operator: None,
        arg1: Some(Box::new(const_node(0.0))),
        arg2: Some(Box::new(inner_add.clone())),
    };
    if_false.execute_on_target_with_effect_and_scale(1, Some(1), &item, &mut target_item, 1.0, &sde);
    assert!(target_item.modifiers.is_empty());

    let if_true = DogmaExpressionNode {
        opcode: DogmaVmOpcode::If,
        value_f64: None,
        attribute_id: None,
        operator: None,
        arg1: Some(Box::new(const_node(1.0))),
        arg2: Some(Box::new(inner_add)),
    };
    if_true.execute_on_target_with_effect_and_scale(1, Some(1), &item, &mut target_item, 1.0, &sde);
    assert_eq!(target_item.modifiers.len(), 1);

    // 4. Wormhole phenomenon resolver across all 6 anomaly classes
    for (label, expected) in [
        ("Pulsar", "Pulsar"),
        ("Black Hole", "Black Hole"),
        ("Cataclysmic Variable", "Cataclysmic Variable"),
        ("Magnetar", "Magnetar"),
        ("Red Giant", "Red Giant"),
        ("Wolf-Rayet", "Wolf-Rayet"),
    ] {
        let resolved = resolve_system_wormhole_phenomenon(&sde, 31_000_001, Some(label)).unwrap();
        assert_eq!(resolved.2, expected);
    }
    assert!(resolve_system_wormhole_phenomenon(&sde, 31_000_001, Some("Unknown Anom")).is_none());

    // 5. All 6 wormhole classes x 6 phenomena, builder methods, CPU/PG rollbacks, implants, boosters, drones & remote assist
    for cls in 1u8..=6 {
        for name in [
            "Pulsar",
            "Black Hole",
            "Cataclysmic Variable",
            "Magnetar",
            "Red Giant",
            "Wolf-Rayet",
        ] {
            assert!(eve_dogma::wormhole_phenomenon_type_for_class_and_name(cls, name).is_some());
        }
    }
    for t3d in [34317_u32, 34562, 34828, 35683] {
        let def_st = ShipFittingEngine::default_t3d_stance_type_id(t3d).unwrap();
        assert!(ShipFittingEngine::t3d_stance_belongs_to_hull(t3d, def_st));
    }
    assert!(ShipFittingEngine::default_t3d_stance_type_id(587).is_none());

    let snake_alpha = find_type(&sde, "High-grade Snake Alpha");
    let drop_booster = find_type(&sde, "Standard Drop Booster");
    let mut eng2 = ShipFittingEngine::from_sde(800, hull_id, &sde)
        .with_character_skills(HashMap::from([(3300, 5)]))
        .with_implants(vec![snake_alpha])
        .with_boosters(vec![drop_booster]);
    eng2.booster_side_effects.insert(drop_booster, vec![]);
    let warrior_tid = find_type(&sde, "Warrior II");
    eng2.add_drone(9001, warrior_tid, &sde);
    eng2.add_drone(9001, warrior_tid, &sde);
    assert!(eng2.remove_drone(9001, &sde));
    eng2.add_gang_burst(eve_dogma::GangBurstEffect {
        source_item_id: 901,
        source_type_id: Some(burst_mod_id),
        dbuff_id: Some(880_002),
        value: 10.0,
        modifiers: vec![AttributeModifier {
            source_item_id: 901,
            effect_id: None,
            target_attribute_id: attr_ids::MASS,
            operator: ModifierOperator::ModAdd,
            value: 500.0,
            stacking_penalized: false,
        }],
    });
    eng2.apply_gang_burst(
        eve_dogma::GangBurstEffect {
            source_item_id: 902,
            source_type_id: Some(burst_mod_id),
            dbuff_id: Some(880_002),
            value: 15.0,
            modifiers: vec![],
        },
        &sde,
    );

    // CPU & PG overflow rollback on both fit_module_with_state and fit_module_with_dynamic_attributes
    assert!(eng2
        .fit_module_with_dynamic_attributes(
            8101,
            ab_mod_id,
            19,
            true,
            HashMap::from([(attr_ids::CPU, 99_999.0)]),
            &sde,
        )
        .is_err());
    assert!(eng2
        .fit_module_with_dynamic_attributes(
            8102,
            ab_mod_id,
            19,
            true,
            HashMap::from([(attr_ids::POWER, 99_999.0)]),
            &sde,
        )
        .is_err());

    // OwnerRequiredSkillModifier + Remote Assistance + Tracking Disruption
    ctx.owned_items
        .push(DogmaItemState::from_sde(60, warrior_tid, &sde));
    let own_srq_spec = EffectModifierSpec {
        func: "OwnerRequiredSkillModifier".into(),
        domain: "charID".into(),
        modified_attribute_id: attr_ids::DAMAGE_MULTIPLIER,
        modifying_attribute_id: attr_ids::SPEED_FACTOR,
        operator: 6,
        group_id: None,
        skill_type_id: Some(3436),
        effect_id: None,
    };
    let _ = ctx.apply_modifier_spec_with_falloff(3001, None, &own_srq_spec, 1_000.0, 10_000.0, 5_000.0, &sde);

    eng2.ship.add_modifier(AttributeModifier {
        source_item_id: 776,
        effect_id: None,
        target_attribute_id: 2135,
        operator: ModifierOperator::PostMul,
        value: 1e-6,
        stacking_penalized: false,
    });
    assert_eq!(eng2.ship.get_attribute(2135, &sde), 0.0);
    let mut remote_tc = DogmaItemState::from_sde(777, ab_mod_id, &sde);
    remote_tc.base_attributes.insert(attr_ids::MAX_RANGE, 20_000.0);
    remote_tc.base_attributes.insert(767, 30.0);
    assert_eq!(eng2.evaluate_projected_remote_assistance(&remote_tc, 767, 5_000.0, &sde), 0.0);

    let (track_m, opt_m, fall_m) =
        eve_dogma::eval_scripted_tracking_disruptor(&remote_tc, None, 5_000.0, &sde);
    assert!(track_m != 0.0 || opt_m == 0.0 || fall_m == 0.0);
}

