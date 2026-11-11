//! Unit and mutation-killing tests for `eve-dogma` root module (`lib.rs`).

use super::*;
use eve_sde::{DogmaAttributeType, DogmaEffectType, EffectModifierSpec, InvType, SdeRegistry};
use std::collections::HashMap;

fn empty_item(item_id: i64, type_id: TypeId) -> DogmaItemState {
    DogmaItemState {
        item_id,
        type_id,
        group_id: None,
        category_id: None,
        base_attributes: HashMap::new(),
        modifiers: Vec::new(),
        skill_levels: HashMap::new(),
    }
}

fn leaf(opcode: DogmaVmOpcode, val: Option<f64>) -> Box<DogmaExpressionNode> {
    Box::new(DogmaExpressionNode {
        opcode,
        value_f64: val,
        attribute_id: None,
        operator: None,
        arg1: None,
        arg2: None,
    })
}

fn bin_node(
    opcode: DogmaVmOpcode,
    a: Option<Box<DogmaExpressionNode>>,
    b: Option<Box<DogmaExpressionNode>>,
) -> DogmaExpressionNode {
    DogmaExpressionNode {
        opcode,
        value_f64: None,
        attribute_id: None,
        operator: None,
        arg1: a,
        arg2: b,
    }
}

#[test]
fn modifier_operator_and_opcode_conversions_and_precedence() {
    let expected = [
        (-1, ModifierOperator::PreAssign),
        (0, ModifierOperator::PreMul),
        (1, ModifierOperator::PreDiv),
        (2, ModifierOperator::ModAdd),
        (3, ModifierOperator::ModSub),
        (4, ModifierOperator::PostMul),
        (5, ModifierOperator::PostDiv),
        (6, ModifierOperator::PostPercent),
        (8, ModifierOperator::PostPercentUnNerfed),
        (9, ModifierOperator::SpecialSkillRelationship),
        (7, ModifierOperator::PostAssign),
    ];
    for w in expected.windows(2) {
        assert!(w[0].1 < w[1].1);
    }
    for (sde_op, op) in expected {
        assert_eq!(ModifierOperator::try_from_sde_op(sde_op), Some(op));
    }
    assert_eq!(ModifierOperator::try_from_sde_op(-2), None);
    assert_eq!(ModifierOperator::try_from_sde_op(10), None);

    assert_eq!(DogmaVmOpcode::from_u8(0), None);
    assert_eq!(DogmaVmOpcode::from_u8(76), None);
    for raw in 1u8..=75 {
        let op = DogmaVmOpcode::from_u8(raw).expect("valid opcode 1..=75");
        assert_eq!(op as u8, raw);
    }

    assert_eq!(DogmaDomain::from_sde_str("itemID"), Some(DogmaDomain::Item));
    assert_eq!(DogmaDomain::from_sde_str("shipID"), Some(DogmaDomain::Ship));
    assert_eq!(DogmaDomain::from_sde_str("charID"), Some(DogmaDomain::Char));
    assert_eq!(
        DogmaDomain::from_sde_str("targetID"),
        Some(DogmaDomain::Target)
    );
    assert_eq!(
        DogmaDomain::from_sde_str("target"),
        Some(DogmaDomain::Target)
    );
    assert_eq!(
        DogmaDomain::from_sde_str("otherID"),
        Some(DogmaDomain::Other)
    );
    assert_eq!(
        DogmaDomain::from_sde_str("structureID"),
        Some(DogmaDomain::Structure)
    );
    assert_eq!(DogmaDomain::from_sde_str("unknown"), None);

    assert_eq!(
        DogmaModifierFunc::from_sde_str("ItemModifier"),
        Some(DogmaModifierFunc::ItemModifier)
    );
    assert_eq!(
        DogmaModifierFunc::from_sde_str("ItemModifierWithFalloff"),
        Some(DogmaModifierFunc::ItemModifierWithFalloff)
    );
    assert_eq!(
        DogmaModifierFunc::from_sde_str("LocationModifier"),
        Some(DogmaModifierFunc::LocationModifier)
    );
    assert_eq!(
        DogmaModifierFunc::from_sde_str("LocationGroupModifier"),
        Some(DogmaModifierFunc::LocationGroupModifier)
    );
    assert_eq!(
        DogmaModifierFunc::from_sde_str("LocationRequiredSkillModifier"),
        Some(DogmaModifierFunc::LocationRequiredSkillModifier)
    );
    assert_eq!(
        DogmaModifierFunc::from_sde_str("OwnerRequiredSkillModifier"),
        Some(DogmaModifierFunc::OwnerRequiredSkillModifier)
    );
    assert_eq!(
        DogmaModifierFunc::from_sde_str("EffectStopper"),
        Some(DogmaModifierFunc::EffectStopper)
    );
    assert_eq!(DogmaModifierFunc::from_sde_str("InvalidFunc"), None);
}

#[test]
fn dogma_expression_node_evaluate_covers_arithmetic_logic_skills_and_groups() {
    let sde = SdeRegistry::default();
    let mut item = empty_item(1001, 587);
    item.group_id = Some(25);
    item.base_attributes.insert(37, 300.0);
    item.base_attributes.insert(182, 3300.0);
    item.base_attributes.insert(277, 3.0);
    item.skill_levels.insert(3300, 3);

    assert_eq!(leaf(DogmaVmOpcode::DefFloat, Some(4.5)).evaluate(&item, &sde), 4.5);
    assert_eq!(leaf(DogmaVmOpcode::DefInt, None).evaluate(&item, &sde), 0.0);
    assert_eq!(leaf(DogmaVmOpcode::GetType, None).evaluate(&item, &sde), 587.0);
    assert_eq!(leaf(DogmaVmOpcode::UserError, Some(9.0)).evaluate(&item, &sde), 0.0);
    assert_eq!(leaf(DogmaVmOpcode::Attack, None).evaluate(&item, &sde), 1.0);

    let get_attr = DogmaExpressionNode {
        opcode: DogmaVmOpcode::GetAttr,
        value_f64: None,
        attribute_id: Some(37),
        operator: None,
        arg1: None,
        arg2: None,
    };
    assert_eq!(get_attr.evaluate(&item, &sde), 300.0);

    assert_eq!(
        bin_node(
            DogmaVmOpcode::Add,
            Some(leaf(DogmaVmOpcode::DefFloat, Some(10.0))),
            Some(leaf(DogmaVmOpcode::DefFloat, Some(2.5))),
        )
        .evaluate(&item, &sde),
        12.5
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::Sub,
            Some(leaf(DogmaVmOpcode::DefFloat, Some(10.0))),
            Some(leaf(DogmaVmOpcode::DefFloat, Some(2.5))),
        )
        .evaluate(&item, &sde),
        7.5
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::Mul,
            Some(leaf(DogmaVmOpcode::DefFloat, Some(4.0))),
            Some(leaf(DogmaVmOpcode::DefFloat, Some(2.5))),
        )
        .evaluate(&item, &sde),
        10.0
    );

    // Boolean logic & relational boundaries
    assert_eq!(
        bin_node(
            DogmaVmOpcode::And,
            Some(leaf(DogmaVmOpcode::DefBool, Some(1.0))),
            Some(leaf(DogmaVmOpcode::DefBool, Some(1.0))),
        )
        .evaluate(&item, &sde),
        1.0
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::And,
            Some(leaf(DogmaVmOpcode::DefBool, Some(1.0))),
            Some(leaf(DogmaVmOpcode::DefBool, Some(0.0))),
        )
        .evaluate(&item, &sde),
        0.0
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::Or,
            Some(leaf(DogmaVmOpcode::DefBool, Some(0.0))),
            Some(leaf(DogmaVmOpcode::DefBool, Some(1.0))),
        )
        .evaluate(&item, &sde),
        1.0
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::Gt,
            Some(leaf(DogmaVmOpcode::DefFloat, Some(5.0))),
            Some(leaf(DogmaVmOpcode::DefFloat, Some(5.0))),
        )
        .evaluate(&item, &sde),
        0.0
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::Gte,
            Some(leaf(DogmaVmOpcode::DefFloat, Some(5.0))),
            Some(leaf(DogmaVmOpcode::DefFloat, Some(5.0))),
        )
        .evaluate(&item, &sde),
        1.0
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::Eq,
            Some(leaf(DogmaVmOpcode::DefFloat, Some(5.0))),
            Some(leaf(DogmaVmOpcode::DefFloat, Some(5.0))),
        )
        .evaluate(&item, &sde),
        1.0
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::If,
            Some(leaf(DogmaVmOpcode::DefBool, Some(1.0))),
            Some(leaf(DogmaVmOpcode::DefFloat, Some(7.0))),
        )
        .evaluate(&item, &sde),
        7.0
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::If,
            Some(leaf(DogmaVmOpcode::DefBool, Some(0.0))),
            Some(leaf(DogmaVmOpcode::DefFloat, Some(7.0))),
        )
        .evaluate(&item, &sde),
        0.0
    );

    // SkillCheck explicit and implicit
    let sk_ok = bin_node(
        DogmaVmOpcode::SkillCheck,
        Some(leaf(DogmaVmOpcode::DefInt, Some(3300.0))),
        Some(leaf(DogmaVmOpcode::DefInt, Some(3.0))),
    );
    assert_eq!(sk_ok.evaluate(&item, &sde), 1.0);
    let sk_fail = bin_node(
        DogmaVmOpcode::SkillCheck,
        Some(leaf(DogmaVmOpcode::DefInt, Some(3300.0))),
        Some(leaf(DogmaVmOpcode::DefInt, Some(4.0))),
    );
    assert_eq!(sk_fail.evaluate(&item, &sde), 0.0);
    let sk_all = bin_node(DogmaVmOpcode::SkillCheck, None, None);
    assert_eq!(sk_all.evaluate(&item, &sde), 1.0);
    assert_eq!(
        bin_node(DogmaVmOpcode::ToolTargetSkills, None, None).evaluate(&item, &sde),
        1.0
    );

    // VerifyTargetGroup
    assert_eq!(
        bin_node(
            DogmaVmOpcode::VerifyTargetGroup,
            Some(leaf(DogmaVmOpcode::DefGroup, Some(25.0))),
            None,
        )
        .evaluate(&item, &sde),
        1.0
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::VerifyTargetGroup,
            Some(leaf(DogmaVmOpcode::DefGroup, Some(26.0))),
            None,
        )
        .evaluate(&item, &sde),
        0.0
    );
}

#[test]
fn execute_on_target_filters_scaling_polarized_resists_and_compiler() {
    let mut sde = SdeRegistry::default();
    sde.attributes.insert(
        37,
        DogmaAttributeType {
            attribute_id: 37,
            name: "maxVelocity".into(),
            default_value: 100.0,
            high_is_good: true,
            stackable: false,
            published: true,
        },
    );

    let mut source = empty_item(100, 1236); // Overdrive II (Module, category 7)
    source.category_id = Some(7);
    source.base_attributes.insert(20, 20.0);

    let mut target = empty_item(1001, 587);
    target.group_id = Some(25);
    target.base_attributes.insert(37, 100.0);
    target.base_attributes.insert(182, 3300.0);
    target.base_attributes.insert(267, 0.5);

    assert!(!source_is_stacking_exempt(&source, &sde));
    let mut ship_src = empty_item(1002, 587);
    ship_src.category_id = Some(6);
    assert!(source_is_stacking_exempt(&ship_src, &sde));
    assert!(item_has_any_required_skill(&target, &sde));
    assert!(item_requires_skill(&target, 3300, &sde));
    assert!(!item_requires_skill(&target, 999_999, &sde));

    // Compile LocationGroupModifier and execute with 0.5 scale attenuation
    let spec = EffectModifierSpec {
        domain: "shipID".into(),
        func: "LocationGroupModifier".into(),
        modified_attribute_id: 37,
        modifying_attribute_id: 20,
        operator: 6, // PostPercent
        group_id: Some(25),
        skill_type_id: None,
        effect_id: None,
    };
    let add_vm = compile_modifier_spec_to_vm(&spec, false).unwrap();
    add_vm.execute_on_target_with_effect_and_scale(10, Some(99), &source, &mut target, 0.5, &sde);
    assert!((target.get_attribute(37, &sde) - 110.0).abs() < 1e-6);

    let rm_vm = compile_modifier_spec_to_vm(&spec, true).unwrap();
    rm_vm.execute_on_target_with_effect(10, Some(99), &source, &mut target, &sde);
    assert!((target.get_attribute(37, &sde) - 100.0).abs() < 1e-6);

    // Polarized effect 5995 PostAssign 100.0 -> normalized to 1.0 (0% resistance) on 267..=274
    let pol_node = DogmaExpressionNode {
        opcode: DogmaVmOpcode::AddLocMod,
        value_f64: Some(100.0),
        attribute_id: Some(267),
        operator: Some(ModifierOperator::PostAssign),
        arg1: None,
        arg2: None,
    };
    pol_node.execute_on_target_with_effect(11, Some(5995), &source, &mut target, &sde);
    assert!((target.get_attribute(267, &sde) - 1.0).abs() < 1e-6);

    // Effect 2008 covert cyno duration bonus adds attribute 1252 filter
    let eff_2008 = DogmaEffectType {
        effect_id: 2008,
        name: "cynosuralDurationBonus".into(),
        effect_category: 0,
        is_offensive: false,
        is_assistance: false,
        is_warp_safe: false,
        disallow_auto_repeat: false,
        electronic_chance: false,
        propulsion_chance: false,
        range_chance: false,
        published: true,
        guid: None,
        modifiers: vec![EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationRequiredSkillModifier".into(),
            modified_attribute_id: 73,
            modifying_attribute_id: 20,
            operator: 6,
            group_id: None,
            skill_type_id: Some(3300),
            effect_id: None,
        }],
    };
    let vm_2008 = compile_effect_to_vm(&eff_2008, false).unwrap();
    assert_eq!(
        vm_2008
            .arg1
            .as_ref()
            .and_then(|n| n.arg1.as_ref())
            .and_then(|n| n.attribute_id),
        Some(1252)
    );
}

#[test]
fn wormhole_phenomena_type_mapping_and_capsuleer_application() {
    assert_eq!(
        wormhole_phenomenon_type_for_class_and_name(1, "Pulsar"),
        Some(30844)
    );
    assert_eq!(
        wormhole_phenomenon_type_for_class_and_name(6, "Black Hole"),
        Some(30854)
    );
    assert_eq!(
        wormhole_phenomenon_type_for_class_and_name(4, "Cataclysmic Variable"),
        Some(30884)
    );
    assert_eq!(
        wormhole_phenomenon_type_for_class_and_name(3, "Magnetar"),
        Some(30861)
    );
    assert_eq!(
        wormhole_phenomenon_type_for_class_and_name(5, "Red Giant"),
        Some(30873)
    );
    assert_eq!(
        wormhole_phenomenon_type_for_class_and_name(6, "Wolf-Rayet"),
        Some(30879)
    );
    assert_eq!(
        wormhole_phenomenon_type_for_class_and_name(2, "Unknown"),
        None
    );

    let mut sde = SdeRegistry::default();
    sde.types.insert(
        30844,
        InvType {
            type_id: 30844,
            group_id: 920,
            category_id: 2,
            name: "Class 1 Pulsar".into(),
            mass: 0.0,
            volume: 0.0,
            capacity: 0.0,
            radius: 0.0,
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
            effect_ids: vec![9001],
            default_attributes: HashMap::from([(100, 30.0)]),
        },
    );
    sde.effects.insert(
        9001,
        DogmaEffectType {
            effect_id: 9001,
            name: "pulsarShield".into(),
            effect_category: 0,
            is_offensive: false,
            is_assistance: false,
            is_warp_safe: false,
            disallow_auto_repeat: false,
            electronic_chance: false,
            propulsion_chance: false,
            range_chance: false,
            published: true,
            guid: None,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".into(),
                func: "LocationModifier".into(),
                modified_attribute_id: 263,
                modifying_attribute_id: 100,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
        },
    );

    let mut attrs = HashMap::from([(263u16, 1000.0)]);
    // Sleepers / NPCs and non-ship categories are exempt
    assert!(!apply_wormhole_phenomenon_to_attributes(
        &sde, 30844, 6, true, &mut attrs
    ));
    assert!(!apply_wormhole_phenomenon_to_attributes(
        &sde, 30844, 11, false, &mut attrs
    ));
    // Capsuleer ship (category 6, !is_npc_or_sleeper) gets +30% shield capacity
    assert!(apply_wormhole_phenomenon_to_attributes(
        &sde, 30844, 6, false, &mut attrs
    ));
    assert!((attrs[&263] - 1300.0).abs() < 1e-6);
}

#[test]
fn has_required_skills_for_type_checks_unknown_zero_boundaries_and_all_six_tiers() {
    let mut sde = SdeRegistry::default();
    let ship_tid = 587;
    sde.types.insert(
        ship_tid,
        InvType {
            type_id: ship_tid,
            group_id: 25,
            category_id: 6,
            name: "Rifter".into(),
            mass: 1_000_000.0,
            volume: 27_500.0,
            capacity: 140.0,
            radius: 35.0,
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
            effect_ids: Vec::new(),
            default_attributes: HashMap::new(),
        },
    );
    let make_mod = |tid: TypeId, attrs: HashMap<AttributeId, f64>| InvType {
        type_id: tid,
        group_id: 53,
        category_id: 7,
        name: format!("Mod{tid}"),
        mass: 0.0,
        volume: 5.0,
        capacity: 0.0,
        radius: 1.0,
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
        effect_ids: vec![12],
        default_attributes: attrs,
    };

    // 2. No requiredSkill* attributes -> true
    sde.types.insert(80_001, make_mod(80_001, HashMap::new()));
    // 3. REQUIRED_SKILL_1 = 0.0 -> true even with empty skills
    sde.types
        .insert(80_002, make_mod(80_002, HashMap::from([(attr_ids::REQUIRED_SKILL_1, 0.0)])));
    // 4. REQUIRED_SKILL_1 = 3300.0 with omitted level attr (defaults to max(1.0) = 1)
    sde.types.insert(
        80_003,
        make_mod(80_003, HashMap::from([(attr_ids::REQUIRED_SKILL_1, 3300.0)])),
    );
    // 5. REQUIRED_SKILL_1_LEVEL = 0.0 -> still requires level 1 via .max(1.0)
    sde.types.insert(
        80_004,
        make_mod(
            80_004,
            HashMap::from([
                (attr_ids::REQUIRED_SKILL_1, 3300.0),
                (attr_ids::REQUIRED_SKILL_1_LEVEL, 0.0),
            ]),
        ),
    );
    // 6. All 6 tiers present: (182/277), (183/278), (184/279), (1285/1286), (1289/1287), (1290/1288)
    sde.types.insert(
        80_005,
        make_mod(
            80_005,
            HashMap::from([
                (attr_ids::REQUIRED_SKILL_1, 3300.0),
                (attr_ids::REQUIRED_SKILL_1_LEVEL, 2.0),
                (attr_ids::REQUIRED_SKILL_2, 3301.0),
                (attr_ids::REQUIRED_SKILL_2_LEVEL, 3.0),
                (attr_ids::REQUIRED_SKILL_3, 3302.0),
                (attr_ids::REQUIRED_SKILL_3_LEVEL, 2.0),
                (1285, 3303.0),
                (1286, 4.0),
                (1289, 3304.0),
                (1287, 3.0),
                (1290, 3305.0),
                (1288, 4.0),
            ]),
        ),
    );

    let mut eng = ShipFittingEngine::from_sde(1, ship_tid, &sde);
    // 1. Unknown type -> false
    assert!(!eng.has_required_skills_for_type(999_999, &sde));
    assert!(eng.has_required_skills_for_type(80_001, &sde));
    assert!(eng.has_required_skills_for_type(80_002, &sde));
    assert!(!eng.has_required_skills_for_type(80_003, &sde));
    assert!(!eng.has_required_skills_for_type(80_004, &sde));

    eng.character_skills.insert(3300, 0);
    assert!(!eng.has_required_skills_for_type(80_003, &sde));
    assert!(!eng.has_required_skills_for_type(80_004, &sde));
    eng.character_skills.insert(3300, 1);
    assert!(eng.has_required_skills_for_type(80_003, &sde));
    assert!(eng.has_required_skills_for_type(80_004, &sde));

    // Verify EVERY single tier (1..=6) is independently enforced on 80_005
    let all_six = [
        (3300_u32, 2_u8),
        (3301, 3),
        (3302, 2),
        (3303, 4),
        (3304, 3),
        (3305, 4),
    ];
    for missing_idx in 0..all_six.len() {
        eng.character_skills.clear();
        for (i, &(sid, lvl)) in all_six.iter().enumerate() {
            eng.character_skills
                .insert(sid, if i == missing_idx { lvl - 1 } else { lvl });
        }
        assert!(
            !eng.has_required_skills_for_type(80_005, &sde),
            "tier {} missing by 1 level must fail",
            missing_idx + 1
        );
    }
    eng.character_skills = all_six.into_iter().collect();
    assert!(eng.has_required_skills_for_type(80_005, &sde));
}

#[test]
fn set_module_online_skip_skills_bypasses_skills_but_enforces_burnout_max_group_and_resources() {
    let mut sde = SdeRegistry::default();
    for (eid, name) in [(12_u16, "hiPower"), (16, "online"), (2663, "rigSlot"), (6766, "rigGroupLimit")] {
        sde.effects.insert(
            eid,
            DogmaEffectType {
                effect_id: eid,
                name: name.into(),
                effect_category: 0,
                ..Default::default()
            },
        );
    }
    let inv = |tid: TypeId, grp: u32, cat: u32, name: &str, effs: Vec<u16>, attrs: HashMap<AttributeId, f64>| InvType {
        type_id: tid,
        group_id: grp,
        category_id: cat,
        name: name.into(),
        mass: 1_000.0,
        volume: 5.0,
        capacity: 10.0,
        radius: 1.0,
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
        effect_ids: effs,
        default_attributes: attrs,
    };

    let ship_tid = 587;
    sde.types.insert(
        ship_tid,
        inv(
            ship_tid,
            25,
            6,
            "Rifter",
            vec![],
            HashMap::from([
                (attr_ids::HI_SLOTS, 6.0),
                (attr_ids::RIG_SLOTS, 3.0),
                (attr_ids::RIG_SIZE, 1.0),
                (attr_ids::UPGRADE_CAPACITY, 400.0),
                (attr_ids::CPU_OUTPUT, 100.0),
                (attr_ids::POWER_OUTPUT, 100.0),
            ]),
        ),
    );
    // Skill-locked module (req skill 3300 lv 5, CPU=20, PG=10, maxGroupOnline=1)
    let skill_mod_tid = 81_001;
    sde.types.insert(
        skill_mod_tid,
        inv(
            skill_mod_tid,
            777,
            7,
            "SkillLockedMod",
            vec![12, 16],
            HashMap::from([
                (attr_ids::REQUIRED_SKILL_1, 3300.0),
                (attr_ids::REQUIRED_SKILL_1_LEVEL, 5.0),
                (attr_ids::CPU, 20.0),
                (attr_ids::POWER, 10.0),
                (978, 1.0),
            ]),
        ),
    );
    // Heavy CPU / PG modules for rollback assertions
    let heavy_cpu_tid = 81_002;
    sde.types.insert(
        heavy_cpu_tid,
        inv(
            heavy_cpu_tid,
            778,
            7,
            "HeavyCpuMod",
            vec![12, 16],
            HashMap::from([
                (attr_ids::REQUIRED_SKILL_1, 3300.0),
                (attr_ids::REQUIRED_SKILL_1_LEVEL, 5.0),
                (attr_ids::CPU, 90.0),
                (attr_ids::POWER, 5.0),
            ]),
        ),
    );
    let heavy_pg_tid = 81_003;
    sde.types.insert(
        heavy_pg_tid,
        inv(
            heavy_pg_tid,
            779,
            7,
            "HeavyPgMod",
            vec![12, 16],
            HashMap::from([
                (attr_ids::REQUIRED_SKILL_1, 3300.0),
                (attr_ids::REQUIRED_SKILL_1_LEVEL, 5.0),
                (attr_ids::CPU, 10.0),
                (attr_ids::POWER, 95.0),
            ]),
        ),
    );
    // Group 1770 (Entosis/Command Link style) boosted by rig effect 6766 attr 2542
    let grp1770_tid = 81_004;
    sde.types.insert(
        grp1770_tid,
        inv(
            grp1770_tid,
            1770,
            7,
            "Grp1770Mod",
            vec![12, 16],
            HashMap::from([(978, 1.0), (attr_ids::CPU, 5.0), (attr_ids::POWER, 5.0)]),
        ),
    );
    let bonus_rig_tid = 81_005;
    sde.types.insert(
        bonus_rig_tid,
        inv(
            bonus_rig_tid,
            773,
            7,
            "Grp1770Rig",
            vec![2663, 6766],
            HashMap::from([
                (attr_ids::RIG_SIZE, 1.0),
                (attr_ids::UPGRADE_COST, 50.0),
                (2542, 1.0),
            ]),
        ),
    );

    let mut eng = ShipFittingEngine::from_sde(1, ship_tid, &sde);
    // 1-2. Fit offline module -> set_module_online fails on missing skill, skip_skills succeeds
    eng.fit_module_with_state(50, skill_mod_tid, 27, false, &sde).unwrap();
    let sk_err = eng.set_module_online(50, &sde).unwrap_err();
    assert!(sk_err.contains("Missing required skill"));
    assert_eq!(eng.modules[&50].state, ModuleState::Offline);

    // 3. set_module_online_skip_skills succeeds & charges CPU/PG
    eng.set_module_online_skip_skills(50, &sde).unwrap();
    assert_eq!(eng.modules[&50].state, ModuleState::Online);
    assert!((eng.ship.get_attribute(attr_ids::CPU_LOAD, &sde) - 20.0).abs() < 1e-9);
    assert!((eng.ship.get_attribute(attr_ids::POWER_LOAD, &sde) - 10.0).abs() < 1e-9);

    // 4. Not fitted error
    assert_eq!(
        eng.set_module_online_skip_skills(99_999, &sde).unwrap_err(),
        "Module 99999 is not fitted"
    );

    // 5. Burned-out offline module rejected
    eng.fit_module_with_state(51, skill_mod_tid, 28, false, &sde).unwrap();
    eng.modules.get_mut(&51).unwrap().damage_hp = 40.0;
    let burn_err = eng.set_module_online_skip_skills(51, &sde).unwrap_err();
    assert!(burn_err.contains("ModuleBurnedOut"));
    assert_eq!(eng.modules[&51].state, ModuleState::Offline);

    // 6. Repair burnout -> MaxGroupOnlineExceeded still enforced
    eng.modules.get_mut(&51).unwrap().damage_hp = 0.0;
    let grp_err = eng.set_module_online_skip_skills(51, &sde).unwrap_err();
    assert!(grp_err.contains("MaxGroupOnlineExceeded"));
    assert_eq!(eng.modules[&51].state, ModuleState::Offline);

    // 7. Insufficient CPU and Insufficient Powergrid roll state & loads back cleanly
    eng.fit_module_with_state(52, heavy_cpu_tid, 29, false, &sde).unwrap();
    let cpu_err = eng.set_module_online_skip_skills(52, &sde).unwrap_err();
    assert!(cpu_err.contains("Insufficient CPU"));
    assert_eq!(eng.modules[&52].state, ModuleState::Offline);
    assert!((eng.ship.get_attribute(attr_ids::CPU_LOAD, &sde) - 20.0).abs() < 1e-9);

    eng.fit_module_with_state(53, heavy_pg_tid, 30, false, &sde).unwrap();
    let pg_err = eng.set_module_online_skip_skills(53, &sde).unwrap_err();
    assert!(pg_err.contains("Insufficient Powergrid"));
    assert_eq!(eng.modules[&53].state, ModuleState::Offline);
    assert!((eng.ship.get_attribute(attr_ids::CPU_LOAD, &sde) - 20.0).abs() < 1e-9);
    assert!((eng.ship.get_attribute(attr_ids::POWER_LOAD, &sde) - 10.0).abs() < 1e-9);

    // Group 1770 + effect 6766 rig limit extension works cleanly for both online fitting & skip_skills
    eng.fit_and_online_module(60, bonus_rig_tid, 92, &sde).unwrap();
    eng.fit_and_online_module(61, grp1770_tid, 31, &sde).unwrap();
    eng.fit_module_with_state(62, grp1770_tid, 32, false, &sde).unwrap();
    eng.set_module_online_skip_skills(62, &sde).unwrap();
    assert_eq!(eng.modules[&62].state, ModuleState::Online);
}

#[test]
fn dogma_vm_opcodes_wormhole_phenomena_and_scale_boundaries() {
    let mut sde = SdeRegistry::default();
    let mut target_type = InvType {
        type_id: 700,
        group_id: 25,
        category_id: 6,
        name: "Hull700".into(),
        mass: 1000.0,
        volume: 10.0,
        capacity: 10.0,
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
        effect_ids: vec![],
        default_attributes: HashMap::from([
            (attr_ids::REQUIRED_SKILL_1, 3300.0),
            (attr_ids::REQUIRED_SKILL_1_LEVEL, 0.0),
            (attr_ids::REQUIRED_SKILL_2, 3301.0),
            (attr_ids::REQUIRED_SKILL_2_LEVEL, 2.0),
        ]),
    };
    sde.types.insert(700, target_type.clone());
    target_type.type_id = 701;
    target_type.default_attributes = HashMap::from([(1290, 3305.0), (1288, 2.0)]);
    sde.types.insert(701, target_type);

    let mut ship = empty_item(1, 700);
    ship.base_attributes.insert(1252, 0.0);
    ship.skill_levels.insert(3300, 0);
    ship.skill_levels.insert(3301, 2);

    // DefBool / DefType / DefAssociation / DefOperator / DefEnvid / DefString + Inc/Dec/Minus/Mul defaults
    for op in [
        DogmaVmOpcode::DefBool,
        DogmaVmOpcode::DefType,
        DogmaVmOpcode::DefAssociation,
        DogmaVmOpcode::DefOperator,
        DogmaVmOpcode::DefEnvid,
        DogmaVmOpcode::DefString,
    ] {
        assert_eq!(leaf(op, Some(3.25)).evaluate(&ship, &sde), 3.25);
        assert_eq!(leaf(op, None).evaluate(&ship, &sde), 0.0);
    }
    assert_eq!(bin_node(DogmaVmOpcode::Inc, None, None).evaluate(&ship, &sde), 1.0);
    assert_eq!(bin_node(DogmaVmOpcode::Dec, None, None).evaluate(&ship, &sde), -1.0);
    assert_eq!(bin_node(DogmaVmOpcode::Minus, Some(leaf(DogmaVmOpcode::DefFloat, Some(5.0))), None).evaluate(&ship, &sde), 4.0);
    assert_eq!(bin_node(DogmaVmOpcode::Mul, None, None).evaluate(&ship, &sde), 1.0);

    // SkillCheck explicit skill_id > 0: non-matching item.type_id defaults trained = 0; req_level = 0 clamps to 1
    assert_eq!(
        bin_node(
            DogmaVmOpcode::SkillCheck,
            Some(leaf(DogmaVmOpcode::DefInt, Some(3399.0))),
            Some(leaf(DogmaVmOpcode::DefInt, Some(0.0))),
        )
        .evaluate(&ship, &sde),
        0.0
    );
    // SkillCheck implicit with tier1 lvl 0 clamped to 1 -> fails when trained=0, passes at 1
    assert_eq!(bin_node(DogmaVmOpcode::SkillCheck, None, None).evaluate(&ship, &sde), 0.0);
    ship.skill_levels.insert(3300, 1);
    assert_eq!(bin_node(DogmaVmOpcode::SkillCheck, None, None).evaluate(&ship, &sde), 1.0);
    // ToolTargetSkills using value_f64 (701 -> requires tier 6 skill 3305 lvl 2)
    let mut tts_701 = bin_node(DogmaVmOpcode::ToolTargetSkills, None, None);
    tts_701.value_f64 = Some(701.0);
    assert_eq!(tts_701.evaluate(&ship, &sde), 0.0);
    ship.skill_levels.insert(3305, 2);
    assert_eq!(tts_701.evaluate(&ship, &sde), 1.0);

    // Scaled PostAssign (scale = 0.5) must NOT scale PostAssign value (hits L618 `_ => base_val`)
    let mut scaled_assign = empty_item(2, 700);
    let post_assign_node = DogmaExpressionNode {
        opcode: DogmaVmOpcode::AddItmMod,
        value_f64: Some(42.0),
        attribute_id: Some(37),
        operator: Some(ModifierOperator::PostAssign),
        arg1: None,
        arg2: None,
    };
    post_assign_node.execute_on_target_with_effect_and_scale(10, Some(1), &ship, &mut scaled_assign, 0.5, &sde);
    assert_eq!(scaled_assign.modifiers[0].value, 42.0);

    // Effect 2008 covert cyno attr 1252 filter rejects 0.0 and accepts > 0.0
    let eff_2008 = DogmaEffectType {
        effect_id: 2008,
        name: "cynoCovert".into(),
        modifiers: vec![EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationRequiredSkillModifier".into(),
            modified_attribute_id: 73,
            modifying_attribute_id: 20,
            operator: 6,
            group_id: None,
            skill_type_id: Some(3300),
            effect_id: None,
        }],
        ..Default::default()
    };
    let mut src = empty_item(9, 1);
    src.base_attributes.insert(20, -50.0);
    let mut mod_item = empty_item(10, 700);
    mod_item.base_attributes.insert(1252, 0.0);
    let vm_2008 = compile_effect_to_vm(&eff_2008, false).unwrap();
    vm_2008.execute_on_target(9, &src, &mut mod_item, &sde);
    assert!(mod_item.modifiers.is_empty());
    mod_item.base_attributes.insert(1252, 1.0);
    vm_2008.execute_on_target(9, &src, &mut mod_item, &sde);
    assert_eq!(mod_item.modifiers.len(), 1);

    // Invalid operator returns None from compile_modifier_spec_to_vm
    let bad_op_spec = EffectModifierSpec {
        domain: "shipID".into(),
        func: "ItemModifier".into(),
        modified_attribute_id: 37,
        modifying_attribute_id: 20,
        operator: 42,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    };
    assert!(compile_modifier_spec_to_vm(&bad_op_spec, false).is_none());

    // Wormhole phenomena: non-WH class (e.g. 7), Magnetar & Wolf-Rayet overrides, region fallback, secondary sun beacon
    sde.regions.insert(
        11_000_009,
        eve_sde::RegionInfo {
            region_id: 11_000_009,
            name: "Reg4".into(),
            center: [0.0; 3],
            faction_id: None,
            wormhole_class_id: Some(4),
        },
    );
    sde.constellations.insert(
        21_000_009,
        eve_sde::ConstellationInfo {
            constellation_id: 21_000_009,
            region_id: 11_000_009,
            name: "ConstNone".into(),
            center: [0.0; 3],
            faction_id: None,
            wormhole_class_id: None,
        },
    );
    sde.solar_systems.insert(
        31_000_077,
        eve_sde::SolarSystem {
            solar_system_id: 31_000_077,
            constellation_id: 21_000_009,
            region_id: 11_000_009,
            name: "KSpace".into(),
            security_status: 0.9,
            radius: 1.0e11,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: Some(7),
        },
    );
    sde.solar_systems.insert(
        31_000_078,
        eve_sde::SolarSystem {
            solar_system_id: 31_000_078,
            constellation_id: 21_000_009,
            region_id: 11_000_009,
            name: "RegionFallbackWH".into(),
            security_status: -1.0,
            radius: 1.0e11,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: None,
        },
    );
    assert_eq!(resolve_system_wormhole_phenomenon(&sde, 31_000_077, Some("Magnetar")), None);
    assert_eq!(
        resolve_system_wormhole_phenomenon(&sde, 31_000_078, Some("Magnetar")),
        Some((4, 4, "Magnetar", 30862))
    );
    assert_eq!(
        resolve_system_wormhole_phenomenon(&sde, 31_000_078, Some("wolf")),
        Some((4, 4, "Wolf-Rayet", 30877))
    );

    // apply_wormhole_phenomenon_to_attributes: op=4 (mul), op=2 (add), unsupported op=9 (false), missing eff/attr/domain
    sde.types.insert(
        30844,
        InvType {
            type_id: 30844,
            group_id: 920,
            category_id: 2,
            name: "Beacon".into(),
            mass: 0.0,
            volume: 0.0,
            capacity: 0.0,
            radius: 0.0,
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
            effect_ids: vec![9001, 9002, 9999],
            default_attributes: HashMap::from([(100, 2.0), (101, 50.0)]),
        },
    );
    sde.effects.insert(
        9001,
        DogmaEffectType {
            effect_id: 9001,
            name: "unsupportedAndMissing".into(),
            effect_category: 0,
            modifiers: vec![
                EffectModifierSpec {
                    domain: "targetID".into(),
                    func: "ItemModifier".into(),
                    modified_attribute_id: 263,
                    modifying_attribute_id: 100,
                    operator: 4,
                    group_id: None,
                    skill_type_id: None,
                    effect_id: None,
                },
                EffectModifierSpec {
                    domain: "shipID".into(),
                    func: "ItemModifier".into(),
                    modified_attribute_id: 263,
                    modifying_attribute_id: 999,
                    operator: 4,
                    group_id: None,
                    skill_type_id: None,
                    effect_id: None,
                },
                EffectModifierSpec {
                    domain: "shipID".into(),
                    func: "ItemModifier".into(),
                    modified_attribute_id: 263,
                    modifying_attribute_id: 100,
                    operator: 9,
                    group_id: None,
                    skill_type_id: None,
                    effect_id: None,
                },
            ],
            ..Default::default()
        },
    );
    let mut test_attrs = HashMap::from([(263_u16, 100.0), (264_u16, 25.0)]);
    assert!(!apply_wormhole_phenomenon_to_attributes(&sde, 30844, 6, false, &mut test_attrs));

    sde.effects.insert(
        9002,
        DogmaEffectType {
            effect_id: 9002,
            name: "mulAndAdd".into(),
            effect_category: 0,
            modifiers: vec![
                EffectModifierSpec {
                    domain: "shipID".into(),
                    func: "ItemModifier".into(),
                    modified_attribute_id: 263,
                    modifying_attribute_id: 100,
                    operator: 4,
                    group_id: None,
                    skill_type_id: None,
                    effect_id: None,
                },
                EffectModifierSpec {
                    domain: "charID".into(),
                    func: "LocationModifier".into(),
                    modified_attribute_id: 264,
                    modifying_attribute_id: 101,
                    operator: 2,
                    group_id: None,
                    skill_type_id: None,
                    effect_id: None,
                },
            ],
            ..Default::default()
        },
    );
    assert!(apply_wormhole_phenomenon_to_attributes(&sde, 30844, 6, false, &mut test_attrs));
    assert_eq!(test_attrs[&263], 200.0);
    assert_eq!(test_attrs[&264], 75.0);
}

#[test]
fn fitting_uncovered_lifecycle_and_heat_edge_branches() {
    let mut sde = SdeRegistry::default();
    for (eid, name) in [(11_u16, "loPower"), (12, "hiPower"), (13, "medPower"), (16, "online"), (2663, "rigSlot")] {
        sde.effects.insert(
            eid,
            DogmaEffectType {
                effect_id: eid,
                name: name.into(),
                effect_category: 0,
                ..Default::default()
            },
        );
    }
    let inv = |tid: TypeId, grp: u32, cat: u32, name: &str, effs: Vec<u16>, attrs: HashMap<AttributeId, f64>| InvType {
        type_id: tid,
        group_id: grp,
        category_id: cat,
        name: name.into(),
        mass: 1_000.0,
        volume: 5.0,
        capacity: 10.0,
        radius: 1.0,
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
        effect_ids: effs,
        default_attributes: attrs,
    };

    let t3c_tid = 29984;
    sde.types.insert(
        t3c_tid,
        inv(
            t3c_tid,
            963,
            6,
            "Tengu",
            vec![],
            HashMap::from([
                (attr_ids::CPU_OUTPUT, 10.0),
                (attr_ids::POWER_OUTPUT, 500.0),
                (attr_ids::UPGRADE_CAPACITY, 100.0),
                (attr_ids::RIG_SLOTS, 2.0),
                (attr_ids::RIG_SIZE, 2.0),
            ]),
        ),
    );
    sde.effects.insert(
        3772,
        DogmaEffectType {
            effect_id: 3772,
            name: "subSystem".into(),
            effect_category: 0,
            ..Default::default()
        },
    );
    // Passive effect adding attr 312 onto ship CPU_OUTPUT
    sde.effects.insert(
        62_901,
        DogmaEffectType {
            effect_id: 62_901,
            name: "addCpuOut".into(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: attr_ids::CPU_OUTPUT,
                modifying_attribute_id: 312,
                operator: 2, // ModAdd
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    // Subsystem 1 granting 1 high, 1 med, 1 low slot + 90 CPU output (total 100)
    sde.types.insert(
        82_001,
        inv(
            82_001,
            954,
            32,
            "Sub1",
            vec![3772, 62_901],
            HashMap::from([
                (1366, 125.0),
                (1374, 1.0),
                (1375, 1.0),
                (1376, 1.0),
                (312, 90.0),
            ]),
        ),
    );
    // Subsystem 2 granting 4 high, 4 med, 4 low slots (no extra CPU)
    sde.types.insert(
        82_006,
        inv(
            82_006,
            954,
            32,
            "Sub2",
            vec![3772],
            HashMap::from([(1366, 125.0), (1374, 4.0), (1375, 4.0), (1376, 4.0)]),
        ),
    );
    // Low-slot CPU Boosters (Co-Processor style) for testing clean post-offline constraints
    sde.types.insert(
        82_007,
        inv(
            82_007,
            57,
            7,
            "CpuBoost60",
            vec![11, 16, 62_901],
            HashMap::from([(attr_ids::CPU, 0.0), (attr_ids::POWER, 0.0), (312, 60.0)]),
        ),
    );
    sde.types.insert(
        82_008,
        inv(
            82_008,
            58,
            7,
            "CpuBoost40",
            vec![11, 16, 62_901],
            HashMap::from([(attr_ids::CPU, 0.0), (attr_ids::POWER, 0.0), (312, 40.0)]),
        ),
    );
    // Rig with CPU consumption (to hit empty candidates break in enforce_post_unfit_or_offline_constraints)
    sde.types.insert(
        82_009,
        inv(
            82_009,
            59,
            7,
            "CpuRig",
            vec![2663],
            HashMap::from([
                (attr_ids::RIG_SIZE, 2.0),
                (attr_ids::UPGRADE_COST, 10.0),
                (attr_ids::CPU, 20.0),
            ]),
        ),
    );
    // Type with maxTypeFitted (2431 = 1)
    sde.types.insert(
        82_002,
        inv(
            82_002,
            53,
            7,
            "TypeCapped",
            vec![12, 16],
            HashMap::from([(2431, 1.0), (attr_ids::CPU, 60.0), (attr_ids::POWER, 60.0)]),
        ),
    );
    // Med & High & Low modules for priority enforce_post_unfit_or_offline_constraints & heat tests
    sde.types.insert(
        82_003,
        inv(
            82_003,
            54,
            7,
            "MedMod",
            vec![13, 16],
            HashMap::from([(attr_ids::CPU, 40.0), (attr_ids::POWER, 40.0)]),
        ),
    );
    sde.types.insert(
        82_004,
        inv(
            82_004,
            55,
            7,
            "LoMod",
            vec![11, 16],
            HashMap::from([(attr_ids::CPU, 40.0), (attr_ids::POWER, 40.0)]),
        ),
    );
    // High slot with explicit maxGroupOnline = 1 (group 56) & CPU=20, PG=20
    sde.types.insert(
        82_005,
        inv(
            82_005,
            56,
            7,
            "HiMod",
            vec![12, 16],
            HashMap::from([(attr_ids::CPU, 20.0), (attr_ids::POWER, 20.0), (978, 1.0)]),
        ),
    );

    let mut eng = ShipFittingEngine::from_sde(1, t3c_tid, &sde);
    assert!(ShipFittingEngine::t3d_stance_belongs_to_hull(587, 34319));
    assert!(eng.fit_and_online_module(1, 82_001, 125, &sde).is_ok());

    // Subsystem unfit blocked when med module (19) or hi module (27) occupies its slots
    eng.fit_module_with_state(2, 82_003, 19, false, &sde).unwrap();
    let sub_med_err = eng.unfit_module(1, &sde).unwrap_err();
    assert!(sub_med_err.contains("SubsystemHasFittedModules"));
    eng.unfit_module(2, &sde).unwrap();

    eng.fit_module_with_state(3, 82_005, 27, false, &sde).unwrap();
    let sub_hi_err = eng.unfit_module(1, &sde).unwrap_err();
    assert!(sub_hi_err.contains("SubsystemHasFittedModules"));
    assert!(eng.modules.contains_key(&1));

    // Slot flag 28 exceeds Sub1 capacity (1 slot), then swap Sub1 -> Sub2 (4 slots)
    eng.set_module_online(3, &sde).unwrap();
    assert!(eng
        .fit_and_online_module(4, 82_005, 28, &sde)
        .unwrap_err()
        .contains("Slot flag 28"));
    eng.unfit_module(3, &sde).unwrap();
    eng.unfit_module(1, &sde).unwrap();
    eng.fit_and_online_module(1, 82_006, 125, &sde).unwrap();

    // Fit two Co-Processor style CPU boosters (10 base + 60 + 40 = 110 CPU output)
    eng.fit_and_online_module(23, 82_007, 13, &sde).unwrap();
    eng.fit_and_online_module(24, 82_008, 14, &sde).unwrap();

    // MaxGroupOnlineExceeded with 4 high slots and plenty of CPU in Sub2
    eng.fit_and_online_module(3, 82_005, 27, &sde).unwrap();
    assert!(eng
        .fit_and_online_module(4, 82_005, 28, &sde)
        .unwrap_err()
        .contains("MaxGroupOnlineExceeded"));
    eng.unfit_module(3, &sde).unwrap();

    // MaxTypeFittedExceeded via attr 2431
    eng.fit_module_with_state(10, 82_002, 27, false, &sde).unwrap();
    assert!(eng
        .fit_module_with_state(11, 82_002, 28, false, &sde)
        .unwrap_err()
        .contains("MaxTypeFittedExceeded"));
    eng.unfit_module(10, &sde).unwrap();

    // load_charge error on unfitted module
    assert!(eng.load_charge(999, 1, 1, &sde).unwrap_err().contains("not fitted"));

    // activate_module on burned out module returns Err
    eng.fit_and_online_module(20, 82_003, 19, &sde).unwrap();
    eng.modules.get_mut(&20).unwrap().damage_hp = 40.0;
    assert!(eng.activate_module(20, true, &sde).unwrap_err().contains("burned out"));
    eng.modules.get_mut(&20).unwrap().damage_hp = 0.0;

    // enforce_post_unfit_or_offline_constraints picks high -> med -> low priority, and breaks if none left
    eng.fit_and_online_module(21, 82_005, 27, &sde).unwrap();
    eng.fit_and_online_module(22, 82_004, 11, &sde).unwrap();
    // Offline booster 23 (-60 CPU output): total output drops 110 -> 50 while load is 40+20+40 = 100
    eng.modules.get_mut(&23).unwrap().state = ModuleState::Offline;
    eng.recalculate_stats(&sde);
    let forced = eng.enforce_post_unfit_or_offline_constraints(&sde);
    assert_eq!(forced, vec![21, 20]); // high (27) first, then med (19), keeping low (11) online at 40 <= 50!
    // Offline booster 24 (-40 CPU output): output drops 50 -> 10 while remaining low (11) uses 40
    eng.modules.get_mut(&24).unwrap().state = ModuleState::Offline;
    eng.recalculate_stats(&sde);
    let forced2 = eng.enforce_post_unfit_or_offline_constraints(&sde);
    assert_eq!(forced2, vec![22]);
    // Re-online booster 24 (50 CPU output), fit CPU-using rig at flag 92, then drop booster 24 -> rig cannot be offlined (candidates.is_empty() break)
    eng.set_module_online(24, &sde).unwrap();
    eng.fit_and_online_module(29, 82_009, 92, &sde).unwrap();
    eng.modules.get_mut(&24).unwrap().state = ModuleState::Offline;
    eng.recalculate_stats(&sde);
    assert!(eng.enforce_post_unfit_or_offline_constraints(&sde).is_empty());
    // Unknown hull error in fit_module_with_dynamic_attributes
    eng.ship.type_id = 999_999;
    assert!(eng
        .fit_module_with_dynamic_attributes(30, 82_005, 92, false, HashMap::new(), &sde)
        .unwrap_err()
        .contains("Unknown ship hull"));
    eng.ship.type_id = t3c_tid;
    assert!(eng
        .fit_module_with_dynamic_attributes(30, 999_999, 92, false, HashMap::new(), &sde)
        .unwrap_err()
        .contains("Unknown module type"));

    // apply_heat_cycle on OFFLINE module uses dynamic_attribute_overrides fallback (attr 1211) & burns self
    eng.fit_module_with_dynamic_attributes(
        40,
        82_004,
        12,
        false,
        HashMap::from([(1211, 50.0)]),
        &sde,
    )
    .unwrap();
    let rep = eng.apply_heat_cycle(40, 0.5, 0.1, 2.0, 0.99, &sde).unwrap();
    assert!(rep.module_burned);
    assert_eq!(rep.module_damage, 50.0);
    assert_eq!(eng.modules[&40].damage_hp, eng.modules[&40].max_hp);
}
