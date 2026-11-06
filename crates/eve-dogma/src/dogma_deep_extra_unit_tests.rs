use super::*;
use eve_sde::{
    attr_ids, ConstellationInfo, DogmaAttributeType, DogmaEffectType, EffectModifierSpec,
    InvType, RegionInfo, SdeRegistry, SolarSystem,
};
use std::collections::HashMap;

fn n(opcode: DogmaVmOpcode) -> DogmaExpressionNode {
    DogmaExpressionNode {
        opcode,
        value_f64: None,
        attribute_id: None,
        operator: None,
        arg1: None,
        arg2: None,
    }
}

fn n_int(v: i64) -> DogmaExpressionNode {
    let mut node = n(DogmaVmOpcode::DefInt);
    node.value_f64 = Some(v as f64);
    node
}

fn n_float(v: f64) -> DogmaExpressionNode {
    let mut node = n(DogmaVmOpcode::DefFloat);
    node.value_f64 = Some(v);
    node
}

fn n_attr(attr_id: u16) -> DogmaExpressionNode {
    let mut node = n(DogmaVmOpcode::DefAttribute);
    node.attribute_id = Some(attr_id);
    node
}

fn make_item(item_id: i64, type_id: u32, group_id: Option<u32>, category_id: Option<u32>) -> DogmaItemState {
    DogmaItemState {
        item_id,
        type_id,
        group_id,
        category_id,
        base_attributes: HashMap::new(),
        modifiers: Vec::new(),
        skill_levels: HashMap::new(),
    }
}

fn make_inv_type(type_id: u32, group_id: u32, category_id: u32, name: &str) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.to_string(),
        mass: 1_000_000.0,
        volume: 500.0,
        capacity: 100.0,
        radius: 50.0,
        base_price: 1000.0,
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
        effect_ids: Vec::new(),
        default_attributes: HashMap::new(),
    }
}

fn make_solar_system(id: u32, const_id: u32, reg_id: u32, wh_class: Option<i32>) -> SolarSystem {
    SolarSystem {
        solar_system_id: id,
        constellation_id: const_id,
        region_id: reg_id,
        name: format!("Sys-{id}"),
        security_status: -1.0,
        radius: 1.0e11,
        galactic_pos_ly: [0.0; 3],
        planet_ids: Vec::new(),
        stations: Vec::new(),
        wormhole_class_id: wh_class,
    }
}

fn mod_spec(
    domain: &str,
    func: &str,
    modified_attribute_id: u16,
    modifying_attribute_id: u16,
    operator: i32,
    group_id: Option<u32>,
    skill_type_id: Option<u32>,
    effect_id: Option<u16>,
) -> EffectModifierSpec {
    EffectModifierSpec {
        domain: domain.to_string(),
        func: func.to_string(),
        modified_attribute_id,
        modifying_attribute_id,
        operator,
        group_id,
        skill_type_id,
        effect_id,
    }
}

fn make_mod_node(
    opcode: DogmaVmOpcode,
    op: ModifierOperator,
    target_attr: u16,
    source_attr: u16,
    filter: Option<DogmaExpressionNode>,
) -> DogmaExpressionNode {
    DogmaExpressionNode {
        opcode,
        value_f64: None,
        attribute_id: Some(target_attr),
        operator: Some(op),
        arg1: filter.map(Box::new),
        arg2: Some(Box::new(n_attr(source_attr))),
    }
}

#[test]
fn vm_evaluate_all_expression_branches() {
    let mut sde = SdeRegistry::default();
    let mut skill_type = make_inv_type(3300, 255, 16, "Gunnery");
    skill_type.default_attributes.insert(280, 4.0);
    sde.types.insert(3300, skill_type);

    let mut target_req_type = make_inv_type(7001, 25, 6, "TestHull");
    target_req_type.default_attributes.insert(attr_ids::REQUIRED_SKILL_1, 3300.0);
    target_req_type.default_attributes.insert(attr_ids::REQUIRED_SKILL_1_LEVEL, 3.0);
    sde.types.insert(7001, target_req_type);

    let mut skill_item = make_item(10, 3300, Some(255), Some(16));
    skill_item.base_attributes.insert(280, 5.0);

    let mut ship = make_item(20, 7001, None, Some(6));
    ship.base_attributes.insert(50, 42.0);
    ship.skill_levels.insert(3300, 4);

    let mut get_via_arg2 = n(DogmaVmOpcode::GetAttr);
    get_via_arg2.arg2 = Some(Box::new(n_attr(50)));
    assert_eq!(get_via_arg2.evaluate(&ship, &sde), 42.0);
    let mut att_empty = n(DogmaVmOpcode::Att);
    att_empty.arg2 = Some(Box::new(n_attr(9999)));
    assert_eq!(att_empty.evaluate(&ship, &sde), 0.0);

    let mut eval_node = n(DogmaVmOpcode::Eval);
    eval_node.arg1 = Some(Box::new(n_float(7.5)));
    assert_eq!(eval_node.evaluate(&ship, &sde), 7.5);
    let mut efct_node = n(DogmaVmOpcode::Efct);
    efct_node.arg2 = Some(Box::new(n_float(9.25)));
    assert_eq!(efct_node.evaluate(&ship, &sde), 9.25);
    let mut eff_val = n(DogmaVmOpcode::Eff);
    eff_val.value_f64 = Some(3.5);
    assert_eq!(eff_val.evaluate(&ship, &sde), 3.5);
    let mut set_node = n(DogmaVmOpcode::Set);
    set_node.arg2 = Some(Box::new(n_float(11.0)));
    assert_eq!(set_node.evaluate(&ship, &sde), 11.0);

    let mut comb = n(DogmaVmOpcode::Combine);
    comb.arg1 = Some(Box::new(n_int(1)));
    comb.arg2 = Some(Box::new(n_int(88)));
    assert_eq!(comb.evaluate(&ship, &sde), 88.0);
    assert_eq!(n(DogmaVmOpcode::Splice).evaluate(&ship, &sde), 1.0);

    let mut sc_explicit = n(DogmaVmOpcode::SkillCheck);
    sc_explicit.arg1 = Some(Box::new(n_int(3300)));
    assert_eq!(sc_explicit.evaluate(&skill_item, &sde), 1.0);

    let sc_auto = n(DogmaVmOpcode::SkillCheck);
    assert_eq!(sc_auto.evaluate(&ship, &sde), 1.0);
    ship.base_attributes.insert(attr_ids::REQUIRED_SKILL_1, 3300.0);
    ship.base_attributes.insert(attr_ids::REQUIRED_SKILL_1_LEVEL, 5.0);
    assert_eq!(sc_auto.evaluate(&ship, &sde), 0.0);
    ship.base_attributes.remove(&attr_ids::REQUIRED_SKILL_1);
    ship.base_attributes.remove(&attr_ids::REQUIRED_SKILL_1_LEVEL);

    let mut tts = n(DogmaVmOpcode::ToolTargetSkills);
    tts.arg1 = Some(Box::new(n_int(7001)));
    assert_eq!(tts.evaluate(&ship, &sde), 1.0);
    ship.skill_levels.insert(3300, 1);
    assert_eq!(tts.evaluate(&ship, &sde), 0.0);

    assert_eq!(n(DogmaVmOpcode::VerifyTargetGroup).evaluate(&ship, &sde), 1.0);
    let mut vtg_match = n(DogmaVmOpcode::VerifyTargetGroup);
    vtg_match.arg1 = Some(Box::new(n_int(25)));
    assert_eq!(vtg_match.evaluate(&ship, &sde), 1.0);
    let mut vtg_miss = n(DogmaVmOpcode::VerifyTargetGroup);
    vtg_miss.arg1 = Some(Box::new(n_int(26)));
    assert_eq!(vtg_miss.evaluate(&ship, &sde), 0.0);
    assert_eq!(n(DogmaVmOpcode::UserError).evaluate(&ship, &sde), 0.0);
}

#[test]
fn vm_execute_scaled_and_filtered_modifiers() {
    let mut sde = SdeRegistry::default();
    let mut item_type = make_inv_type(8001, 46, 7, "TestArmorPlate");
    item_type.default_attributes.insert(attr_ids::REQUIRED_SKILL_1, 3394.0);
    sde.types.insert(8001, item_type);

    let mut source = make_item(100, 8002, Some(74), Some(7));
    source.base_attributes.extend([(64, 2.0), (65, 0.5), (66, 0.0), (67, 777.0)]);

    let mut target = make_item(200, 8001, None, Some(7));
    for attr in [1159, 1160, 1161, 1162] {
        target.base_attributes.insert(attr, 100.0);
    }
    target.base_attributes.insert(1163, 50.0);

    let mut wrong_grp = n(DogmaVmOpcode::DefGroup);
    wrong_grp.value_f64 = Some(99.0);
    make_mod_node(DogmaVmOpcode::AddLocGrpMod, ModifierOperator::ModAdd, 1163, 64, Some(wrong_grp))
        .execute_on_target_with_effect_and_scale(100, Some(1), &source, &mut target, 1.0, &sde);
    assert_eq!(target.get_attribute(1163, &sde), 50.0);

    let mut skill_match = n(DogmaVmOpcode::DefType);
    skill_match.value_f64 = Some(3394.0);
    make_mod_node(DogmaVmOpcode::AddLocSrqMod, ModifierOperator::ModAdd, 1163, 64, Some(skill_match))
        .execute_on_target_with_effect_and_scale(101, Some(2), &source, &mut target, 1.0, &sde);
    assert_eq!(target.get_attribute(1163, &sde), 52.0);

    let mut skill_miss = n(DogmaVmOpcode::DefType);
    skill_miss.value_f64 = Some(9999.0);
    make_mod_node(DogmaVmOpcode::AddOwnSrqMod, ModifierOperator::ModAdd, 1163, 64, Some(skill_miss))
        .execute_on_target_with_effect_and_scale(102, Some(3), &source, &mut target, 1.0, &sde);
    assert_eq!(target.get_attribute(1163, &sde), 52.0);

    let mut if_false = n(DogmaVmOpcode::If);
    if_false.arg1 = Some(Box::new(n_int(0)));
    if_false.arg2 = Some(Box::new(make_mod_node(
        DogmaVmOpcode::AddItmMod,
        ModifierOperator::PostAssign,
        1163,
        67,
        None,
    )));
    if_false.execute_on_target_with_effect_and_scale(103, Some(4), &source, &mut target, 1.0, &sde);
    assert_eq!(target.get_attribute(1163, &sde), 52.0);

    let mut comb = n(DogmaVmOpcode::Combine);
    comb.arg1 = Some(Box::new(make_mod_node(DogmaVmOpcode::AddItmMod, ModifierOperator::PreMul, 1159, 64, None)));
    comb.arg2 = Some(Box::new(make_mod_node(DogmaVmOpcode::AddItmMod, ModifierOperator::PreDiv, 1160, 64, None)));
    let mut if_true = n(DogmaVmOpcode::If);
    if_true.arg1 = Some(Box::new(n_int(1)));
    if_true.arg2 = Some(Box::new(comb));
    if_true.execute_on_target_with_effect_and_scale(104, Some(5), &source, &mut target, 0.5, &sde);
    assert!((target.get_attribute(1159, &sde) - 150.0).abs() < 1e-6);
    assert!((target.get_attribute(1160, &sde) - 75.0).abs() < 1e-6);

    make_mod_node(DogmaVmOpcode::AddItmMod, ModifierOperator::PreDiv, 1161, 65, None)
        .execute_on_target_with_effect_and_scale(105, Some(6), &source, &mut target, -1.0, &sde);
    assert_eq!(target.modifiers.last().unwrap().value, 0.5);

    make_mod_node(DogmaVmOpcode::AddItmMod, ModifierOperator::PostDiv, 1162, 66, None)
        .execute_on_target_with_effect_and_scale(106, Some(7), &source, &mut target, 0.5, &sde);
    assert_eq!(target.modifiers.last().unwrap().value, 0.0);

    n(DogmaVmOpcode::UserError).execute_on_target_with_effect_and_scale(107, Some(8), &source, &mut target, 1.0, &sde);
}

#[test]
fn skill_helpers_and_compiler_branches() {
    let mut sde = SdeRegistry::default();
    let mut booster = make_inv_type(9001, 303, 20, "StandardDrop");
    booster.default_attributes.insert(attr_ids::REQUIRED_SKILL_1, 3300.0);
    sde.types.insert(9001, booster);

    let mut exempt_item = make_inv_type(9002, 46, 7, "ShieldModule");
    exempt_item.default_attributes.insert(attr_ids::REQUIRED_SKILL_1, 3426.0);
    sde.types.insert(9002, exempt_item);

    let booster_state = make_item(1, 9001, None, None);
    assert!(source_is_stacking_exempt(&booster_state, &sde));
    assert!(item_has_any_required_skill(&booster_state, &sde));
    assert!(!item_requires_skill(&booster_state, 0, &sde));

    let non_exempt_state = make_item(2, 9002, None, None);
    assert!(!source_is_stacking_exempt(&non_exempt_state, &sde));
    assert!(item_requires_skill(&non_exempt_state, 3426, &sde));
    let skill_exempt = make_item(3, 3300, None, Some(16));
    assert!(source_is_stacking_exempt(&skill_exempt, &sde));

    for (func, group_id, skill_id, eff_id) in [
        ("ItemModifier", None, None, None),
        ("LocationModifier", None, None, None),
        ("LocationGroupModifier", Some(46), None, None),
        ("LocationRequiredSkillModifier", None, Some(3300), None),
        ("OwnerRequiredSkillModifier", None, Some(3300), None),
        ("EffectStopper", None, None, Some(6441)),
    ] {
        let spec = mod_spec("shipID", func, 263, 64, 6, group_id, skill_id, eff_id);
        assert!(compile_modifier_spec_to_vm(&spec, false).is_some());
        assert!(compile_modifier_spec_to_vm(&spec, true).is_some());
    }
    let bad_spec = mod_spec("shipID", "UnknownFunc", 263, 64, 6, None, None, None);
    assert!(compile_modifier_spec_to_vm(&bad_spec, false).is_none());

    let covert_eff = DogmaEffectType {
        effect_id: 2008,
        name: "covertCloak".to_string(),
        effect_category: 1,
        modifiers: vec![
            mod_spec("itemID", "ItemModifier", 50, 64, 4, None, None, None),
            mod_spec("shipID", "ItemModifier", 37, 64, 4, None, None, None),
        ],
        ..Default::default()
    };
    let covert_vm = compile_effect_to_vm(&covert_eff, false).expect("covert VM");
    assert_eq!(covert_vm.opcode, DogmaVmOpcode::Combine);
    assert_eq!(covert_vm.arg2.as_ref().unwrap().opcode, DogmaVmOpcode::AddItmMod);

    let empty_eff = DogmaEffectType {
        effect_id: 9999,
        name: "empty".to_string(),
        ..Default::default()
    };
    assert!(compile_effect_to_vm(&empty_eff, false).is_none());
}

#[test]
fn wormhole_phenomena_all_classes_resolution_and_application() {
    for p in ["Pulsar", "Black Hole", "Cataclysmic Variable", "Magnetar", "Red Giant", "Wolf-Rayet"] {
        assert_eq!(
            wormhole_phenomenon_type_for_class_and_name(0, p),
            wormhole_phenomenon_type_for_class_and_name(1, p)
        );
        for c in 1u8..=6 {
            assert!(wormhole_phenomenon_type_for_class_and_name(c, p).is_some());
        }
        assert_eq!(
            wormhole_phenomenon_type_for_class_and_name(7, p),
            wormhole_phenomenon_type_for_class_and_name(6, p)
        );
    }
    assert_eq!(wormhole_phenomenon_type_for_class_and_name(3, "InvalidPhenom"), None);

    let mut sde = SdeRegistry::default();
    sde.regions.insert(
        11000001,
        RegionInfo {
            region_id: 11000001,
            name: "RegWH".into(),
            center: [0.0; 3],
            faction_id: None,
            wormhole_class_id: Some(4),
        },
    );
    sde.constellations.insert(
        21000001,
        ConstellationInfo {
            constellation_id: 21000001,
            region_id: 11000001,
            name: "ConstWH".into(),
            center: [0.0; 3],
            faction_id: None,
            wormhole_class_id: Some(5),
        },
    );
    sde.solar_systems.insert(31000005, make_solar_system(31000005, 21000001, 11000001, Some(12)));
    sde.solar_systems.insert(31000001, make_solar_system(31000001, 21000001, 11000001, Some(13)));
    sde.solar_systems.insert(31000002, make_solar_system(31000002, 21000001, 11000001, Some(14)));
    sde.solar_systems.insert(31000003, make_solar_system(31000003, 21000001, 11000001, None));

    assert_eq!(resolve_system_wormhole_phenomenon(&sde, 31000005, Some("Magnetar")), None);
    assert_eq!(
        resolve_system_wormhole_phenomenon(&sde, 31000001, Some("Pulsar")),
        Some((13, 6, "Wolf-Rayet", 30879))
    );
    assert_eq!(
        resolve_system_wormhole_phenomenon(&sde, 31000002, Some("black hole")),
        Some((14, 2, "Black Hole", 30850))
    );
    assert_eq!(
        resolve_system_wormhole_phenomenon(&sde, 31000003, Some("Cataclysmic")),
        Some((5, 5, "Cataclysmic Variable", 30883))
    );
    assert_eq!(
        resolve_system_wormhole_phenomenon(&sde, 31000003, Some("red")),
        Some((5, 5, "Red Giant", 30873))
    );
    assert_eq!(resolve_system_wormhole_phenomenon(&sde, 31000003, Some("nothing")), None);

    let mut beacon = make_inv_type(30856, 920, 6, "Class 5 Cataclysmic Variable");
    beacon.default_attributes.extend([(101, 2.0), (102, 50.0), (103, 15.0)]);
    beacon.effect_ids.push(4400);
    sde.types.insert(30856, beacon);
    sde.effects.insert(
        4400,
        DogmaEffectType {
            effect_id: 4400,
            name: "whEffect".into(),
            modifiers: vec![
                mod_spec("shipID", "LocationGroupModifier", 263, 101, 4, Some(46), None, None),
                mod_spec("shipID", "ItemModifier", 265, 102, 6, None, None, None),
                mod_spec("shipID", "ItemModifier", 37, 103, 2, None, None, None),
                mod_spec("targetID", "ItemModifier", 37, 103, 2, None, None, None),
            ],
            ..Default::default()
        },
    );

    let mut ship_attrs = HashMap::from([(263_u16, 100.0), (265_u16, 200.0), (37_u16, 300.0)]);
    assert!(!apply_wormhole_phenomenon_to_attributes(&sde, 30856, 11, false, &mut ship_attrs));
    assert!(!apply_wormhole_phenomenon_to_attributes(&sde, 30856, 6, true, &mut ship_attrs));
    assert!(!apply_wormhole_phenomenon_to_attributes(&sde, 99999, 6, false, &mut ship_attrs));
    assert!(apply_wormhole_phenomenon_to_attributes(&sde, 30856, 6, false, &mut ship_attrs));
    assert!((ship_attrs[&263] - 200.0).abs() < 1e-6);
    assert!((ship_attrs[&265] - 300.0).abs() < 1e-6);
    assert!((ship_attrs[&37] - 315.0).abs() < 1e-6);
}

#[test]
fn context_domains_falloff_fitting_t3d_and_heat_branches() {
    let mut sde = SdeRegistry::default();
    sde.attributes.insert(
        263,
        DogmaAttributeType {
            attribute_id: 263,
            stackable: true,
            ..Default::default()
        },
    );

    let mut hull = make_inv_type(34317, 1305, 6, "Confessor");
    hull.default_attributes.extend([
        (attr_ids::HI_SLOTS, 4.0),
        (attr_ids::MED_SLOTS, 2.0),
        (attr_ids::LOW_SLOTS, 4.0),
        (attr_ids::RIG_SLOTS, 3.0),
        (attr_ids::RIG_SIZE, 1.0),
        (attr_ids::CPU_OUTPUT, 200.0),
        (attr_ids::POWER_OUTPUT, 50.0),
        (attr_ids::UPGRADE_CAPACITY, 300.0),
        (1985, 10_000.0),
    ]);
    sde.types.insert(34317, hull);
    sde.types.insert(34319, make_inv_type(34319, 1306, 6, "Confessor Defense Mode"));
    sde.types.insert(34564, make_inv_type(34564, 1306, 6, "Svipul Defense Mode"));
    sde.types.insert(9010, make_inv_type(9010, 46, 7, "NotAStance"));

    let mut hi_mod = make_inv_type(9011, 46, 7, "ShieldBooster");
    hi_mod.effect_ids = vec![12];
    hi_mod.default_attributes.extend([
        (attr_ids::CPU, 20.0),
        (attr_ids::POWER, 5.0),
        (attr_ids::HEAT_GENERATION_MULTIPLIER, 2.0),
    ]);
    sde.types.insert(9011, hi_mod);

    let mut rig_mod = make_inv_type(9012, 773, 7, "ArmorRig");
    rig_mod.effect_ids = vec![2663];
    rig_mod
        .default_attributes
        .extend([(attr_ids::UPGRADE_COST, 100.0), (attr_ids::RIG_SIZE, 1.0)]);
    sde.types.insert(9012, rig_mod);

    sde.effects.insert(
        5001,
        DogmaEffectType {
            effect_id: 5001,
            name: "setMult".into(),
            modifiers: vec![mod_spec(
                "charID",
                "LocationGroupModifier",
                101,
                102,
                0,
                Some(300),
                None,
                None,
            )],
            ..Default::default()
        },
    );
    let mut imp1 = make_inv_type(9020, 300, 20, "SlaveAlpha");
    imp1.effect_ids = vec![5001];
    imp1.default_attributes.insert(102, 1.15);
    sde.types.insert(9020, imp1);
    sde.types.insert(9021, make_inv_type(9021, 300, 20, "SlaveBeta"));

    let mut fit = ShipFittingEngine::from_sde(1000, 34317, &sde);
    fit.implants = vec![9020, 9021];

    assert!(fit.set_ship_stance(9010, &sde).is_err());
    assert!(fit.set_ship_stance(34564, &sde).is_err());
    assert!(fit.set_ship_stance(34319, &sde).is_ok());
    assert!(fit.switch_ship_stance(34564, 1_000, &sde).is_err());
    assert!(fit.switch_ship_stance(34319, 12_000, &sde).is_ok());

    assert!(fit.fit_and_online_module(1, 9011, 27, &sde).is_ok());
    assert!(fit.fit_module_with_state(2, 9011, 28, false, &sde).is_ok());
    assert!(fit.set_module_online(2, &sde).unwrap_err().contains("MaxGroupOnlineExceeded"));
    assert!(fit.set_module_online(999, &sde).is_err());

    let dyn_cal = HashMap::from([(attr_ids::UPGRADE_COST, 350.0)]);
    assert!(fit
        .fit_module_with_dynamic_attributes(3, 9012, 92, true, dyn_cal, &sde)
        .unwrap_err()
        .contains("Insufficient Calibration"));
    assert!(!fit.modules.contains_key(&3));

    fit.rack_heat[0] = 0.9;
    let report = fit.apply_heat_cycle(1, 1.0, 0.1, 2.0, 0.0, &sde).expect("heat report");
    assert!(!report.adjacent_damaged_modules.is_empty());
    fit.modules.get_mut(&1).unwrap().damage_hp = fit.modules[&1].max_hp;
    assert!(fit.set_module_online(1, &sde).unwrap_err().contains("ModuleBurnedOut"));

    let eff_mod = fit.build_effective_module_state(888, 9011, false, &sde);
    assert_eq!(eff_mod.get_attribute(attr_ids::CPU, &sde), 20.0);
    assert_eq!(fit.get_charge_effective_attribute(888, attr_ids::CPU, &sde), 0.0);

    let mut ctx = DogmaEffectContext::new(
        make_item(1, 9011, None, None),
        make_item(2, 34317, None, None),
        make_item(3, 1373, None, None),
        make_item(4, 34317, None, None),
        make_item(5, 9011, None, None),
        make_item(6, 35832, None, None),
    );
    ctx.item.base_attributes.insert(64, 0.8);
    ctx.target.base_attributes.insert(37, 100.0);
    let falloff_spec = mod_spec("targetID", "ItemModifierWithFalloff", 37, 64, 4, None, None, None);
    ctx.apply_modifier_spec_with_falloff(1, Some(10), &falloff_spec, 15_000.0, 10_000.0, 5_000.0, &sde);
    assert!((ctx.target.get_attribute(37, &sde) - 90.0).abs() < 1e-6);

    for dom in [
        DogmaDomain::Item,
        DogmaDomain::Ship,
        DogmaDomain::Char,
        DogmaDomain::Target,
        DogmaDomain::Other,
        DogmaDomain::Structure,
    ] {
        ctx.entity_for_domain_mut(dom).base_attributes.insert(500, 1.0);
        assert_eq!(ctx.entity_for_domain(dom).get_attribute(500, &sde), 1.0);
    }
}
