//! Edge-case and mutation-killing unit tests for `evaluation` (`evaluated_context` fitting load
//! modifier emission) and `fitting` (`apply_heat_cycle` and `thermodynamics::per_slot_heat_roll`).

use super::*;
use eve_sde::{DogmaEffectType, EffectModifierSpec, InvType, SdeRegistry};
use std::collections::HashMap;

fn make_test_type(
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
        name: format!("TestType{type_id}"),
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
fn evaluated_context_skips_zero_fitting_load_modifiers_and_emits_positive_ones() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(
        587,
        make_test_type(
            587,
            25,
            6,
            &[
                (attr_ids::LOW_SLOTS, 2.0),
                (attr_ids::MED_SLOTS, 2.0),
                (attr_ids::HI_SLOTS, 2.0),
                (attr_ids::RIG_SLOTS, 2.0),
                (attr_ids::RIG_SIZE, 1.0),
                (attr_ids::UPGRADE_CAPACITY, 400.0),
                (attr_ids::CPU_OUTPUT, 100.0),
                (attr_ids::POWER_OUTPUT, 100.0),
            ],
            &[],
        ),
    );
    // Zero-load hi-slot module (CPU=0, POWER=0)
    sde.types.insert(
        7001,
        make_test_type(
            7001,
            53,
            7,
            &[(attr_ids::CPU, 0.0), (attr_ids::POWER, 0.0)],
            &[12],
        ),
    );
    // Zero-calibration rig (UPGRADE_COST=0)
    sde.types.insert(
        7002,
        make_test_type(
            7002,
            700,
            7,
            &[(attr_ids::RIG_SIZE, 1.0), (attr_ids::UPGRADE_COST, 0.0)],
            &[2663],
        ),
    );
    // Positive-load hi-slot module & positive-calibration rig
    sde.types.insert(
        7003,
        make_test_type(
            7003,
            53,
            7,
            &[(attr_ids::CPU, 12.0), (attr_ids::POWER, 8.0)],
            &[12],
        ),
    );
    sde.types.insert(
        7004,
        make_test_type(
            7004,
            700,
            7,
            &[(attr_ids::RIG_SIZE, 1.0), (attr_ids::UPGRADE_COST, 50.0)],
            &[2663],
        ),
    );

    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde);
    eng.fit_module(101, 7001, 27, &sde).unwrap();
    eng.fit_module(102, 7002, 92, &sde).unwrap();
    eng.fit_module(103, 7003, 28, &sde).unwrap();
    eng.fit_module(104, 7004, 93, &sde).unwrap();

    let ctx = eng.evaluated_context(&sde);
    assert!((ctx.ship.get_attribute(attr_ids::CPU_LOAD, &sde) - 12.0).abs() < 1e-12);
    assert!((ctx.ship.get_attribute(attr_ids::POWER_LOAD, &sde) - 8.0).abs() < 1e-12);
    assert!((ctx.ship.get_attribute(attr_ids::UPGRADE_CAPACITY, &sde) - 350.0).abs() < 1e-12);

    // Modules with 0 CPU / 0 PG / 0 calibration must NOT emit zero-valued load modifiers onto ship
    assert!(!ctx.ship.modifiers.iter().any(|md| {
        md.source_item_id == 101
            && (md.target_attribute_id == attr_ids::CPU_LOAD
                || md.target_attribute_id == attr_ids::POWER_LOAD)
    }));
    assert!(!ctx
        .ship
        .modifiers
        .iter()
        .any(|md| md.source_item_id == 102 && md.target_attribute_id == attr_ids::UPGRADE_CAPACITY));

    // Positive-load modules DO emit their load modifiers
    assert!(ctx
        .ship
        .modifiers
        .iter()
        .any(|md| md.source_item_id == 103 && md.target_attribute_id == attr_ids::CPU_LOAD));
    assert!(ctx
        .ship
        .modifiers
        .iter()
        .any(|md| md.source_item_id == 103 && md.target_attribute_id == attr_ids::POWER_LOAD));
    assert!(ctx
        .ship
        .modifiers
        .iter()
        .any(|md| md.source_item_id == 104 && md.target_attribute_id == attr_ids::UPGRADE_CAPACITY));
}

#[test]
fn apply_heat_cycle_thermo_modifier_guard_zero_heat_boundary_and_per_slot_roll_exact() {
    let mut sde = SdeRegistry::default();
    // Effect 9002 on ship hull 587 modifies HEAT_DAMAGE (attr 1211) by 0% with source_item_id = 1 != 28164,
    // killing `md.source_item_id == 28164 -> !=` in `apply_heat_cycle`.
    sde.effects.insert(
        9002,
        DogmaEffectType {
            effect_id: 9002,
            name: "shipHeatBonusZeroTest".to_string(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".to_string(),
                func: "LocationModifier".to_string(),
                modified_attribute_id: 1211,
                modifying_attribute_id: 280,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..DogmaEffectType::default()
        },
    );
    sde.types.insert(
        587,
        make_test_type(
            587,
            25,
            6,
            &[
                (attr_ids::HI_SLOTS, 3.0),
                (attr_ids::CPU_OUTPUT, 100.0),
                (attr_ids::POWER_OUTPUT, 100.0),
                (280, 0.0),
            ],
            &[9002],
        ),
    );
    // Effect 9001 on skill 28164 (Thermodynamics) modifies CAPACITOR_NEED (attr 6 != 1211) on modules,
    // verifying `has_thermo` checks BOTH `source_item_id == 28164` AND `target_attribute_id == 1211`.
    sde.effects.insert(
        9001,
        DogmaEffectType {
            effect_id: 9001,
            name: "thermoCapTest".to_string(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".to_string(),
                func: "LocationModifier".to_string(),
                modified_attribute_id: attr_ids::CAPACITOR_NEED,
                modifying_attribute_id: 280,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..DogmaEffectType::default()
        },
    );
    sde.types.insert(28164, make_test_type(28164, 1216, 16, &[], &[9001]));
    sde.types.insert(
        7010,
        make_test_type(
            7010,
            53,
            7,
            &[
                (attr_ids::CAPACITOR_NEED, 10.0),
                (1211, 20.0),
                (attr_ids::HP_STRUCT, 100.0),
            ],
            &[12],
        ),
    );

    // Find an adjacent item ID whose jitter <= 0.0 so `per_slot_heat_roll(0.0, adj_id) == 0.0`.
    let zero_roll_adj_id = (1..=64_i64)
        .find(|&id| thermodynamics::per_slot_heat_roll(0.0, id) == 0.0)
        .expect("at least half of IDs have negative jitter");

    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde);
    eng.character_skills.insert(28164, 5); // 25% Thermodynamics damage reduction
    eng.fit_module(500, 7010, 27, &sde).unwrap();
    eng.fit_module(zero_roll_adj_id, 7010, 28, &sde).unwrap();

    // With heat_gen_rate = 0.0 and rack_heat = 0.0, `prob == 0.0` and `slot_roll == 0.0`.
    // Strict `<` (`0.0 < 0.0` == false) must NOT damage the adjacent module, and Thermodynamics V
    // must reduce the active module's 20.0 base heat damage to 15.0 (because effect 9001 targeted
    // attr 6, not attr 1211).
    let report = eng.apply_heat_cycle(500, 0.0, 0.0, 1.0, 0.0, &sde).unwrap();
    assert!((report.module_damage - 15.0).abs() < 1e-12);
    assert!(report.adjacent_damaged_modules.is_empty());
    assert_eq!(eng.modules[&zero_roll_adj_id].damage_hp, 0.0);

    // Exact SplitMix64 jitter & clamp verification for `thermodynamics::per_slot_heat_roll`
    let expected_roll = |roll_u: f64, slot_item_id: i64| -> f64 {
        let mut h = (slot_item_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        h ^= h >> 30;
        h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 27;
        let jitter = (((h & 0xFFFF) as f64) / 65536.0 - 0.5) * 0.08;
        (roll_u + jitter).clamp(0.0, 1.0 - f64::EPSILON)
    };
    for id in [1_i64, 2, 42, -99, 0x1234_5678_9ABC_DEF0_u64 as i64] {
        assert_eq!(
            thermodynamics::per_slot_heat_roll(0.5, id).to_bits(),
            expected_roll(0.5, id).to_bits()
        );
    }
    assert_eq!(thermodynamics::per_slot_heat_roll(-0.5, 1), 0.0);
    assert_eq!(
        thermodynamics::per_slot_heat_roll(1.5, 1),
        1.0 - f64::EPSILON
    );
}

#[test]
fn siege_and_bastion_ewar_immunity_requires_both_active_state_and_matching_effect() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(
        587,
        make_test_type(
            587,
            25,
            6,
            &[
                (attr_ids::HI_SLOTS, 3.0),
                (attr_ids::CPU_OUTPUT, 100.0),
                (attr_ids::POWER_OUTPUT, 100.0),
                (2045, 1.0),
            ],
            &[],
        ),
    );
    sde.effects.insert(
        6582,
        DogmaEffectType {
            effect_id: 6582,
            name: "siegeMode".to_string(),
            effect_category: 1,
            ..DogmaEffectType::default()
        },
    );
    sde.types
        .insert(7020, make_test_type(7020, 53, 7, &[], &[12, 6582]));
    sde.types
        .insert(7021, make_test_type(7021, 53, 7, &[], &[12]));

    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde);
    eng.fit_module(601, 7020, 27, &sde).unwrap();
    eng.fit_module(602, 7021, 28, &sde).unwrap();
    eng.modules.get_mut(&602).unwrap().state = ModuleState::Active;
    // Module 601 (Siege 6582) is only Online while module 602 (non-Siege) is Active:
    // `is_active && matches!(eid, 6582 | 6658)` must remain false (not `||`).
    let ctx_online = eng.evaluated_context(&sde);
    assert!((ctx_online.ship.get_attribute(2045, &sde) - 1.0).abs() < 1e-12);

    // Once Siege 6582 itself is Active, EWAR resistance attribute 2045 drops by 100% to 0.0.
    eng.modules.get_mut(&601).unwrap().state = ModuleState::Active;
    let ctx_active = eng.evaluated_context(&sde);
    assert!((ctx_active.ship.get_attribute(2045, &sde) - 0.0).abs() < 1e-12);
}
