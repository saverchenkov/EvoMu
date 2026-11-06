//! Unit and mutation-killing tests for `context`.

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

#[test]
fn falloff_recharge_and_stacking_math_exact_boundaries() {
    // compute_falloff_attenuation
    assert_eq!(compute_falloff_attenuation(5_000.0, 10_000.0, 5_000.0), 1.0);
    assert_eq!(compute_falloff_attenuation(10_000.0, 10_000.0, 5_000.0), 1.0);
    assert_eq!(compute_falloff_attenuation(15_000.0, 10_000.0, 0.0), 1.0);
    assert_eq!(compute_falloff_attenuation(15_000.0, 10_000.0, -5.0), 1.0);
    assert!((compute_falloff_attenuation(15_000.0, 10_000.0, 5_000.0) - 0.5).abs() < 1e-12);
    assert!((compute_falloff_attenuation(20_000.0, 10_000.0, 5_000.0) - 0.0625).abs() < 1e-12);

    // compute_falloff_with_cutoff
    assert_eq!(compute_falloff_with_cutoff(10_000.0, 10_000.0, 5_000.0), 1.0);
    assert_eq!(compute_falloff_with_cutoff(15_000.0, 10_000.0, 0.0), 1.0);
    assert!((compute_falloff_with_cutoff(15_000.0, 10_000.0, 5_000.0) - 0.5).abs() < 1e-12);
    // Exact boundary at optimal + 3 * falloff = 25,000 -> 0.5^9 = 1/512
    let at_cutoff = compute_falloff_with_cutoff(25_000.0, 10_000.0, 5_000.0);
    assert!((at_cutoff - 0.5_f64.powi(9)).abs() < 1e-12);
    assert_eq!(compute_falloff_with_cutoff(25_000.1, 10_000.0, 5_000.0), 0.0);

    // compute_passive_recharge_rate
    assert_eq!(compute_passive_recharge_rate(250.0, 0.0, 100.0), 0.0);
    assert_eq!(compute_passive_recharge_rate(250.0, 1000.0, 0.0), 0.0);
    assert_eq!(compute_passive_recharge_rate(0.0, 1000.0, 100.0), 0.0);
    assert_eq!(compute_passive_recharge_rate(-1.0, 1000.0, 100.0), 0.0);
    assert_eq!(compute_passive_recharge_rate(1000.0, 1000.0, 100.0), 0.0);
    assert_eq!(compute_passive_recharge_rate(1200.0, 1000.0, 100.0), 0.0);
    // Peak at 25% of 1000 with tau=100s -> 10 * 1000 / 100 * (0.5 - 0.25) = 25.0
    assert!((compute_passive_recharge_rate(250.0, 1000.0, 100.0) - 25.0).abs() < 1e-12);

    // stacking_penalty_factor
    assert_eq!(stacking_penalty_factor(0), 1.0);
    assert!((stacking_penalty_factor(1) - (-1.0_f64 / 7.1289).exp()).abs() < 1e-12);
    assert!((stacking_penalty_factor(2) - (-4.0_f64 / 7.1289).exp()).abs() < 1e-12);

    // compute_passive_recharge
    assert_eq!(compute_passive_recharge(50.0, 0.0, 100_000.0, 1000.0), 0.0);
    assert_eq!(compute_passive_recharge(50.0, 100.0, 0.0, 1000.0), 50.0);
    assert_eq!(compute_passive_recharge(120.0, 100.0, 100_000.0, 1000.0), 100.0);
    let r = compute_passive_recharge(25.0, 100.0, 50_000.0, 10_000.0);
    let exp_r = 100.0 * (1.0 + (0.5 - 1.0) * (-1.0_f64).exp()).powi(2);
    assert!((r - exp_r).abs() < 1e-12);

    // simulate_capacitor & capacitor_c_dot
    assert_eq!(capacitor_c_dot(250.0, 0.0, 100.0), 0.0);
    assert_eq!(capacitor_c_dot(250.0, 1000.0, 0.0), 0.0);
    assert!((capacitor_c_dot(250.0, 1000.0, 100.0) - 25.0).abs() < 1e-12);
    assert_eq!(simulate_capacitor(250.0, 0.0, 100.0, 2.0), 250.0);
    assert_eq!(simulate_capacitor(250.0, 1000.0, 0.0, 2.0), 250.0);
    assert!((simulate_capacitor(250.0, 1000.0, 100.0, 2.0) - 300.0).abs() < 1e-12);
}

#[test]
fn dogma_item_state_operator_precedence_and_ewar_normalization() {
    let mut sde = SdeRegistry::default();
    // Attribute 100: non-stackable, high_is_good = true, default = 50.0
    sde.attributes.insert(
        100,
        DogmaAttributeType {
            attribute_id: 100,
            name: "testHighGood".to_string(),
            default_value: 50.0,
            high_is_good: true,
            stackable: false,
            published: true,
        },
    );
    // Attribute 101: stackable, high_is_good = false
    sde.attributes.insert(
        101,
        DogmaAttributeType {
            attribute_id: 101,
            name: "testLowGood".to_string(),
            default_value: 10.0,
            high_is_good: false,
            stackable: true,
            published: true,
        },
    );
    sde.types.insert(
        587,
        InvType {
            type_id: 587,
            group_id: 25,
            category_id: 6,
            name: "Rifter".to_string(),
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

    let mut st = DogmaItemState::from_sde(1, 587, &sde);
    assert_eq!(st.group_id, Some(25));
    assert_eq!(st.category_id, Some(6));
    assert_eq!(st.get_attribute(attr_ids::MASS, &sde), 1_000_000.0);
    assert_eq!(st.get_attribute(attr_ids::CAPACITY, &sde), 140.0);
    assert_eq!(st.get_attribute(161, &sde), 27_500.0);
    assert_eq!(st.get_attribute(162, &sde), 35.0);
    assert_eq!(st.get_attribute(100, &sde), 50.0); // schema default

    st.set_skill_level(99, 8);
    assert_eq!(st.skill_levels.get(&99), Some(&5));

    // Apply full operator chain on attr 100:
    // PreAssign: 80 vs 100 (high_is_good=true -> 100)
    // PreMul: * 2.0 -> 200
    // PreDiv: / 4.0 (and / 0.0 ignored) -> 50
    // ModAdd: + 30 -> 80
    // ModSub: - 10 -> 70
    // Unpenalized PostMul (* 1.5 -> +50%) & PostPercentUnNerfed (+20%) -> 70 * 1.5 * 1.2 = 126.0
    // Penalized positive PostPercent (+20%, +10%) & PostDiv (/ 0.5 -> +100%, / 0.0 ignored)
    // Penalized negative PostPercent (-10%, -20%)
    // SpecialSkillRelationship (ignored)
    let mods = [
        (ModifierOperator::PreAssign, 80.0, false),
        (ModifierOperator::PreAssign, 100.0, false),
        (ModifierOperator::PreMul, 2.0, false),
        (ModifierOperator::PreDiv, 4.0, false),
        (ModifierOperator::PreDiv, 0.0, false),
        (ModifierOperator::ModAdd, 30.0, false),
        (ModifierOperator::ModSub, 10.0, false),
        (ModifierOperator::PostMul, 1.5, false),
        (ModifierOperator::PostPercentUnNerfed, 20.0, true),
        (ModifierOperator::PostDiv, 0.0, true),
        (ModifierOperator::PostPercent, 20.0, true),
        (ModifierOperator::PostPercent, 10.0, true),
        (ModifierOperator::PostPercent, -10.0, true),
        (ModifierOperator::PostPercent, -20.0, true),
        (ModifierOperator::SpecialSkillRelationship, 500.0, false),
    ];
    for (i, (op, val, pen)) in mods.into_iter().enumerate() {
        st.add_modifier(AttributeModifier {
            source_item_id: (i + 1) as i64,
            effect_id: Some(10),
            target_attribute_id: 100,
            operator: op,
            value: val,
            stacking_penalized: pen,
        });
    }
    let expected = 126.0
        * (1.0 + 0.20 * stacking_penalty_factor(0))
        * (1.0 + 0.10 * stacking_penalty_factor(1))
        * (1.0 - 0.20 * stacking_penalty_factor(0))
        * (1.0 - 0.10 * stacking_penalty_factor(1));
    assert!((st.get_attribute(100, &sde) - expected).abs() < 1e-9);

    // PostAssign overrides everything; for attr 101 (high_is_good=false), picks minimum
    st.add_modifier(AttributeModifier {
        source_item_id: 99,
        effect_id: Some(11),
        target_attribute_id: 101,
        operator: ModifierOperator::PostAssign,
        value: 5.0,
        stacking_penalized: false,
    });
    st.add_modifier(AttributeModifier {
        source_item_id: 100,
        effect_id: None,
        target_attribute_id: 101,
        operator: ModifierOperator::PostAssign,
        value: 25.0,
        stacking_penalized: false,
    });
    assert_eq!(st.get_attribute(101, &sde), 5.0);

    // Remove by source+effect vs source
    st.remove_modifiers_from_source_effect(99, Some(11));
    assert_eq!(st.get_attribute(101, &sde), 25.0);
    st.remove_modifiers_from_source_effect(100, None);
    assert_eq!(st.get_attribute(101, &sde), 10.0);

    // Near-zero EWAR resistance normalization on attr 2045
    st.base_attributes.insert(2045, 1.0);
    st.add_modifier(AttributeModifier {
        source_item_id: 500,
        effect_id: None,
        target_attribute_id: 2045,
        operator: ModifierOperator::PostPercent,
        value: -99.995, // leaves 0.00005 <= 1e-4
        stacking_penalized: false,
    });
    assert_eq!(st.get_attribute(2045, &sde), 0.0);
}

#[test]
fn dogma_effect_context_domains_apply_and_remove() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(
        300,
        InvType {
            type_id: 300,
            group_id: 50,
            category_id: 7,
            name: "Mod".to_string(),
            mass: 0.0,
            volume: 1.0,
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
            effect_ids: Vec::new(),
            default_attributes: HashMap::from([(attr_ids::REQUIRED_SKILL_1, 3300.0)]),
        },
    );

    let mut src = empty_item(1, 1);
    src.base_attributes.insert(20, 50.0);
    let mut ctx = DogmaEffectContext::new(
        src,
        empty_item(2, 2),
        empty_item(3, 3),
        empty_item(4, 4),
        empty_item(5, 5),
        empty_item(6, 6),
    );

    for (dom, expected_id) in [
        (DogmaDomain::Item, 1),
        (DogmaDomain::Ship, 2),
        (DogmaDomain::Char, 3),
        (DogmaDomain::Target, 4),
        (DogmaDomain::Other, 5),
        (DogmaDomain::Structure, 6),
    ] {
        assert_eq!(ctx.entity_for_domain(dom).item_id, expected_id);
        assert_eq!(ctx.entity_for_domain_mut(dom).item_id, expected_id);
        ctx.located_items_for_domain_mut(dom)
            .push(DogmaItemState::from_sde(10 + expected_id, 300, &sde));
        assert_eq!(ctx.located_items_for_domain(dom).len(), 1);
    }
    ctx.owned_items
        .push(DogmaItemState::from_sde(99, 300, &sde));

    // Invalid domain / func / operator return 0
    let bad_dom = EffectModifierSpec {
        domain: "unknown".to_string(),
        func: "ItemModifier".to_string(),
        modified_attribute_id: 37,
        modifying_attribute_id: 20,
        operator: 6,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    };
    assert_eq!(ctx.apply_modifier_spec(1, &bad_dom, &sde), 0);
    assert_eq!(
        ctx.apply_modifier_spec_with_falloff(1, None, &bad_dom, 0.0, 1000.0, 500.0, &sde),
        0
    );

    let eff = DogmaEffectType {
        effect_id: 777,
        name: "combo".to_string(),
        effect_category: 1,
        modifiers: vec![
            EffectModifierSpec {
                domain: "shipID".to_string(),
                func: "ItemModifier".to_string(),
                modified_attribute_id: 37,
                modifying_attribute_id: 20,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            },
            EffectModifierSpec {
                domain: "shipID".to_string(),
                func: "LocationGroupModifier".to_string(),
                modified_attribute_id: 37,
                modifying_attribute_id: 20,
                operator: 6,
                group_id: Some(50),
                skill_type_id: None,
                effect_id: None,
            },
            EffectModifierSpec {
                domain: "charID".to_string(),
                func: "OwnerRequiredSkillModifier".to_string(),
                modified_attribute_id: 37,
                modifying_attribute_id: 20,
                operator: 6,
                group_id: None,
                skill_type_id: Some(3300),
                effect_id: None,
            },
            EffectModifierSpec {
                domain: "targetID".to_string(),
                func: "EffectStopper".to_string(),
                modified_attribute_id: 0,
                modifying_attribute_id: 0,
                operator: 0,
                group_id: None,
                skill_type_id: None,
                effect_id: Some(6730),
            },
        ],
        ..Default::default()
    };

    assert_eq!(ctx.apply_effect(1, &eff, &sde), 4);
    assert_eq!(ctx.stopped_target_effects, vec![6730]);
    assert_eq!(ctx.ship.modifiers.len(), 1);
    assert_eq!(ctx.ship_located_items[0].modifiers.len(), 1);
    assert_eq!(ctx.owned_items[0].modifiers.len(), 1);

    // Apply with range falloff (distance = optimal + falloff -> 0.5x scale)
    ctx.remove_effect(1, &eff);
    assert!(ctx.stopped_target_effects.is_empty());
    assert!(ctx.ship.modifiers.is_empty());
    assert_eq!(
        ctx.apply_effect_with_range(1, &eff, 15_000.0, 10_000.0, 5_000.0, &sde),
        4
    );
    assert!((ctx.ship.modifiers[0].value - 25.0).abs() < 1e-9);
    ctx.remove_effect(1, &eff);
    assert!(ctx.ship.modifiers.is_empty());
}

#[test]
fn test_issue_422_premul_and_prediv_apply_stacking_penalties() {
    let mut sde = SdeRegistry::default();
    let target_attr = attr_ids::SHIELD_EM_DAMAGE_RESONANCE; // non-stackable attribute
    sde.attributes.insert(
        target_attr,
        DogmaAttributeType {
            attribute_id: target_attr,
            name: "shieldEmDamageResonance".into(),
            default_value: 1.0,
            high_is_good: false,
            stackable: false,
            published: true,
        },
    );
    let mut item = empty_item(100, 1);
    item.base_attributes.insert(target_attr, 100.0);

    // Two identical stacking-penalized PreMul(1.5) modifiers must apply CCP stacking penalty on 2nd
    for src in [1_i64, 2] {
        item.add_modifier(AttributeModifier {
            source_item_id: src,
            effect_id: Some(10),
            target_attribute_id: target_attr,
            operator: ModifierOperator::PreMul,
            value: 1.5,
            stacking_penalized: true,
        });
    }
    let penalized_mul = item.get_attribute(target_attr, &sde);
    let expected_penalized_mul = 100.0 * 1.5 * (1.0 + 0.5 * stacking_penalty_factor(1));
    assert!((penalized_mul - expected_penalized_mul).abs() < 1e-6);

    // Unpenalized PreMul modifiers apply full product without penalty
    item.modifiers.clear();
    for src in [1_i64, 2] {
        item.add_modifier(AttributeModifier {
            source_item_id: src,
            effect_id: Some(10),
            target_attribute_id: target_attr,
            operator: ModifierOperator::PreMul,
            value: 1.5,
            stacking_penalized: false,
        });
    }
    assert!((item.get_attribute(target_attr, &sde) - 225.0).abs() < 1e-6);

    // Two stacking-penalized PreDiv(2.0) modifiers apply CCP stacking penalty on 2nd
    item.modifiers.clear();
    for src in [1_i64, 2] {
        item.add_modifier(AttributeModifier {
            source_item_id: src,
            effect_id: Some(11),
            target_attribute_id: target_attr,
            operator: ModifierOperator::PreDiv,
            value: 2.0,
            stacking_penalized: true,
        });
    }
    let penalized_div = item.get_attribute(target_attr, &sde);
    let expected_penalized_div = 100.0 * 0.5 * (1.0 - 0.5 * stacking_penalty_factor(1));
    assert!((penalized_div - expected_penalized_div).abs() < 1e-6);
}

#[test]
fn test_issue_423_two_pass_cpu_pg_load_and_self_location_modifier_preservation() {
    let mut sde = SdeRegistry::default();
    let ship_tid = 99_001_u32;
    let weapon_tid = 99_002_u32;
    let self_mod_tid = 99_003_u32;
    let rig_tid = 99_004_u32;

    let make_inv_type =
        |type_id: u32,
         group_id: u32,
         category_id: u32,
         name: &str,
         effect_ids: Vec<u16>,
         default_attributes: HashMap<u16, f64>| InvType {
            type_id,
            group_id,
            category_id,
            name: name.into(),
            mass: 0.0,
            volume: 1.0,
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
            effect_ids,
            default_attributes,
        };

    sde.types.insert(
        ship_tid,
        make_inv_type(
            ship_tid,
            25,
            6,
            "Test Frigate",
            Vec::new(),
            HashMap::from([
                (attr_ids::CPU_OUTPUT, 500.0),
                (attr_ids::POWER_OUTPUT, 1_000.0),
                (attr_ids::UPGRADE_CAPACITY, 400.0),
                (attr_ids::HI_SLOTS, 4.0),
                (attr_ids::MED_SLOTS, 4.0),
                (attr_ids::RIG_SLOTS, 3.0),
                (attr_ids::RIG_SIZE, 1.0),
            ]),
        ),
    );
    for (eid, name) in [(12_u16, "hiPower"), (13_u16, "medPower"), (2663_u16, "rigSlot")] {
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
    sde.types.insert(
        weapon_tid,
        make_inv_type(
            weapon_tid,
            55,
            7,
            "Test Turret",
            vec![12],
            HashMap::from([(attr_ids::CPU, 100.0), (attr_ids::POWER, 200.0)]),
        ),
    );
    let make_loc_mod = |modified_attribute_id: u16, modifying_attribute_id: u16, group_id: u32| {
        EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationGroupModifier".into(),
            modified_attribute_id,
            modifying_attribute_id,
            operator: 6,
            group_id: Some(group_id),
            skill_type_id: None,
            effect_id: None,
        }
    };
    let self_mod_eff = 65_001_u16;
    sde.effects.insert(
        self_mod_eff,
        DogmaEffectType {
            effect_id: self_mod_eff,
            name: "testSelfLocMod".into(),
            effect_category: 0,
            modifiers: vec![make_loc_mod(attr_ids::CPU, 202, 76)],
            ..Default::default()
        },
    );
    sde.types.insert(
        self_mod_tid,
        make_inv_type(
            self_mod_tid,
            76,
            7,
            "Test Capacitor Booster",
            vec![13, self_mod_eff],
            HashMap::from([
                (attr_ids::CPU, 40.0),
                (attr_ids::POWER, 10.0),
                (202, 25.0),
            ]),
        ),
    );
    let rig_eff = 65_002_u16;
    sde.effects.insert(
        rig_eff,
        DogmaEffectType {
            effect_id: rig_eff,
            name: "testRigDrawback".into(),
            effect_category: 0,
            modifiers: vec![
                make_loc_mod(attr_ids::CPU, 202, 55),
                make_loc_mod(attr_ids::POWER, 1082, 55),
            ],
            ..Default::default()
        },
    );
    sde.types.insert(
        rig_tid,
        make_inv_type(
            rig_tid,
            773,
            7,
            "Test Weapon Rig",
            vec![2663, rig_eff],
            HashMap::from([
                (attr_ids::UPGRADE_COST, 100.0),
                (attr_ids::RIG_SIZE, 1.0),
                (202, 10.0),
                (1082, 15.0),
            ]),
        ),
    );
    let mut engine = ShipFittingEngine::from_sde(1, ship_tid, &sde);
    engine.fit_and_online_module(10, weapon_tid, 27, &sde).unwrap();
    engine.fit_and_online_module(11, self_mod_tid, 19, &sde).unwrap();
    engine.fit_and_online_module(12, rig_tid, 92, &sde).unwrap();
    let ctx = engine.evaluated_context(&sde);
    assert!((ctx.ship.get_attribute(attr_ids::CPU_LOAD, &sde) - 160.0).abs() < 1e-6);
    assert!((ctx.ship.get_attribute(attr_ids::POWER_LOAD, &sde) - 240.0).abs() < 1e-6);
    let self_mod_state = ctx.ship_located_items.iter().find(|i| i.item_id == 11).unwrap();
    assert!((self_mod_state.get_attribute(attr_ids::CPU, &sde) - 50.0).abs() < 1e-6);
}
