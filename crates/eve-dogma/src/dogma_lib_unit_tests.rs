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
