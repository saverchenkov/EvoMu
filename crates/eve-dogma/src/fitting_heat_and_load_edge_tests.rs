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

#[test]
fn activate_offline_and_unfit_clear_pending_overload_queue() {
    let mut sde = eve_sde::SdeRegistry::with_canonical_seed();
    sde.types
        .get_mut(&587)
        .expect("Rifter must exist in SDE")
        .default_attributes
        .insert(2045, 100.0);

    let active_eff_id = 9901_u16;
    let overload_eff_id = 9902_u16;
    sde.effects.insert(
        active_eff_id,
        eve_sde::DogmaEffectType {
            effect_id: active_eff_id,
            name: "syntheticActiveSpeedBoost".to_string(),
            effect_category: 1,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "shipID".to_string(),
                func: "ItemModifier".to_string(),
                modified_attribute_id: eve_sde::attr_ids::MAX_VELOCITY,
                modifying_attribute_id: 9910,
                operator: 6, // PostPercent
                group_id: None,
                skill_type_id: None,
                effect_id: Some(active_eff_id),
            }],
            ..eve_sde::DogmaEffectType::default()
        },
    );
    sde.effects.insert(
        overload_eff_id,
        eve_sde::DogmaEffectType {
            effect_id: overload_eff_id,
            name: "syntheticOverloadAttrBoost".to_string(),
            effect_category: 5,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "shipID".to_string(),
                func: "ItemModifier".to_string(),
                modified_attribute_id: 2045,
                modifying_attribute_id: 9911,
                operator: 6, // PostPercent
                group_id: None,
                skill_type_id: None,
                effect_id: Some(overload_eff_id),
            }],
            ..eve_sde::DogmaEffectType::default()
        },
    );
    sde.types.insert(
        7099,
        eve_sde::InvType {
            type_id: 7099,
            group_id: 46,
            category_id: 7,
            name: "Synthetic Overload Test Module".into(),
            mass: 1_000.0,
            volume: 5.0,
            capacity: 0.0,
            radius: 1.0,
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
            default_effect_id: Some(active_eff_id),
            description: String::new(),
            effect_ids: vec![16, 12, active_eff_id, overload_eff_id],
            default_attributes: std::collections::HashMap::from([
                (eve_sde::attr_ids::CPU, 5.0),
                (eve_sde::attr_ids::POWER, 5.0),
                (9910, 50.0),
                (9911, 50.0),
            ]),
        },
    );

    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde);
    eng.fit_and_online_module(8001, 7099, 27, &sde).unwrap();
    let base_vel = eng
        .evaluated_context(&sde)
        .ship
        .get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &sde);
    assert!((eng.evaluated_context(&sde).ship.get_attribute(2045, &sde) - 100.0).abs() < 1e-9);

    let queue_pre_overload = |eng: &mut ShipFittingEngine| {
        eng.modules.get_mut(&8001).unwrap().state = ModuleState::Overloaded;
        eng.pending_overload_modules.insert(8001);
        eng.recalculate_stats(&sde);
        assert!(eng.pending_overload_modules.contains(&8001));
    };

    // Pre-overload suppresses both active and overload modifiers until activated.
    queue_pre_overload(&mut eng);
    let ctx_pending = eng.evaluated_context(&sde);
    assert!((ctx_pending.ship.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &sde) - base_vel).abs() < 1e-6);
    assert!((ctx_pending.ship.get_attribute(2045, &sde) - 100.0).abs() < 1e-6);

    // Failed activate (burned out) must preserve pending overload state.
    let saved_dmg = eng.modules[&8001].damage_hp;
    eng.modules.get_mut(&8001).unwrap().damage_hp = eng.modules[&8001].max_hp;
    assert!(eng.activate_module(8001, true, &sde).is_err());
    assert!(eng.pending_overload_modules.contains(&8001));
    eng.modules.get_mut(&8001).unwrap().damage_hp = saved_dmg;

    // Direct activate_module(overload=true) clears pending overload and applies both active + overload modifiers.
    eng.activate_module(8001, true, &sde).unwrap();
    assert!(!eng.pending_overload_modules.contains(&8001));
    assert_eq!(eng.modules[&8001].state, ModuleState::Overloaded);
    let ctx_overloaded = eng.evaluated_context(&sde);
    assert!(
        (ctx_overloaded.ship.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &sde) - base_vel * 1.5).abs() < 1e-6
    );
    assert!((ctx_overloaded.ship.get_attribute(2045, &sde) - 150.0).abs() < 1e-6);

    // Direct activate_module(overload=false) clears pending overload and activates without overload bonus.
    queue_pre_overload(&mut eng);
    eng.activate_module(8001, false, &sde).unwrap();
    assert!(!eng.pending_overload_modules.contains(&8001));
    assert_eq!(eng.modules[&8001].state, ModuleState::Active);
    let ctx_active_only = eng.evaluated_context(&sde);
    assert!(
        (ctx_active_only.ship.get_attribute(eve_sde::attr_ids::MAX_VELOCITY, &sde) - base_vel * 1.5).abs() < 1e-6
    );
    assert!((ctx_active_only.ship.get_attribute(2045, &sde) - 100.0).abs() < 1e-6);

    // take_module_offline clears pending overload immediately.
    queue_pre_overload(&mut eng);
    eng.take_module_offline(8001, &sde).unwrap();
    assert!(!eng.pending_overload_modules.contains(&8001));
    assert_eq!(eng.modules[&8001].state, ModuleState::Offline);

    // unfit_module clears pending overload cleanly even if item ID 8001 is later re-fitted.
    eng.set_module_online_skip_skills(8001, &sde).unwrap();
    queue_pre_overload(&mut eng);
    eng.unfit_module(8001, &sde).unwrap();
    assert!(!eng.pending_overload_modules.contains(&8001));

    eng.fit_and_online_module(8001, 7099, 27, &sde).unwrap();
    assert!(!eng.pending_overload_modules.contains(&8001));
}

#[test]
fn refit_and_online_rollback_preserves_pending_overload_state() {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(
        9587,
        make_test_type(
            9587,
            25,
            6,
            &[
                (attr_ids::LOW_SLOTS, 2.0),
                (attr_ids::MED_SLOTS, 2.0),
                (attr_ids::HI_SLOTS, 3.0),
                (attr_ids::CPU_OUTPUT, 100.0),
                (attr_ids::POWER_OUTPUT, 100.0),
                (attr_ids::MAX_VELOCITY, 200.0),
                (2045, 100.0),
            ],
            &[],
        ),
    );

    let active_eff_id = 9901_u16;
    let overload_eff_id = 9902_u16;
    sde.effects.insert(
        active_eff_id,
        DogmaEffectType {
            effect_id: active_eff_id,
            name: "syntheticActiveSpeedBoost".to_string(),
            effect_category: 1,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".to_string(),
                func: "ItemModifier".to_string(),
                modified_attribute_id: attr_ids::MAX_VELOCITY,
                modifying_attribute_id: 9910,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: Some(active_eff_id),
            }],
            ..DogmaEffectType::default()
        },
    );
    sde.effects.insert(
        overload_eff_id,
        DogmaEffectType {
            effect_id: overload_eff_id,
            name: "syntheticOverloadAttrBoost".to_string(),
            effect_category: 5,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".to_string(),
                func: "ItemModifier".to_string(),
                modified_attribute_id: 2045,
                modifying_attribute_id: 9911,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: Some(overload_eff_id),
            }],
            ..DogmaEffectType::default()
        },
    );

    let mut base_mod = make_test_type(
        9601,
        55,
        7,
        &[
            (attr_ids::CPU, 20.0),
            (attr_ids::POWER, 20.0),
            (9910, 50.0),
            (9911, 50.0),
        ],
        &[16, 12, active_eff_id, overload_eff_id],
    );
    base_mod.default_effect_id = Some(active_eff_id);
    sde.types.insert(9601, base_mod);

    sde.types.insert(
        9602,
        make_test_type(
            9602,
            55,
            7,
            &[
                (attr_ids::CPU, 150.0),
                (attr_ids::POWER, 10.0),
                (9910, 50.0),
                (9911, 50.0),
            ],
            &[16, 12, active_eff_id, overload_eff_id],
        ),
    );
    sde.types.insert(
        9603,
        make_test_type(
            9603,
            55,
            7,
            &[
                (attr_ids::CPU, 10.0),
                (attr_ids::POWER, 150.0),
                (9910, 50.0),
                (9911, 50.0),
            ],
            &[16, 12, active_eff_id, overload_eff_id],
        ),
    );

    let mut eng = ShipFittingEngine::from_sde(1, 9587, &sde);
    eng.fit_and_online_module(8101, 9601, 27, &sde).unwrap();
    let base_vel = eng
        .evaluated_context(&sde)
        .ship
        .get_attribute(attr_ids::MAX_VELOCITY, &sde);
    let base_overload_attr = eng.evaluated_context(&sde).ship.get_attribute(2045, &sde);

    let queue_pre_overload = |eng: &mut ShipFittingEngine| {
        eng.modules.get_mut(&8101).unwrap().state = ModuleState::Overloaded;
        eng.pending_overload_modules.insert(8101);
        eng.recalculate_stats(&sde);
    };
    let assert_pre_overload_intact = |eng: &ShipFittingEngine, label: &str| {
        assert!(
            eng.pending_overload_modules.contains(&8101),
            "{label}: pending_overload_modules lost item"
        );
        let m = &eng.modules[&8101];
        assert_eq!(m.type_id, 9601, "{label}: module type changed on rollback");
        assert_eq!(
            m.state,
            ModuleState::Overloaded,
            "{label}: module state changed on rollback"
        );
        let ctx = eng.evaluated_context(&sde);
        assert!(
            (ctx.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - base_vel).abs() < 1e-6,
            "{label}: pending overload must keep active speed modifier suppressed"
        );
        assert!(
            (ctx.ship.get_attribute(2045, &sde) - base_overload_attr).abs() < 1e-6,
            "{label}: pending overload must keep overload modifier suppressed"
        );
    };

    queue_pre_overload(&mut eng);
    assert_pre_overload_intact(&eng, "initial pending overload");

    // fit_module_with_state failing CPU or PG rollback must preserve pending_overload_modules.
    let cpu_err = eng.fit_module_with_state(8101, 9602, 27, true, &sde).unwrap_err();
    assert!(cpu_err.contains("Insufficient CPU"), "{cpu_err}");
    assert_pre_overload_intact(&eng, "fit_module_with_state CPU rollback");

    let pg_err = eng.fit_module_with_state(8101, 9603, 27, true, &sde).unwrap_err();
    assert!(pg_err.contains("Insufficient Powergrid"), "{pg_err}");
    assert_pre_overload_intact(&eng, "fit_module_with_state PG rollback");

    // fit_module_with_dynamic_attributes failing CPU or PG rollback must preserve pending_overload_modules.
    let dyn_cpu_err = eng
        .fit_module_with_dynamic_attributes(
            8101,
            9601,
            27,
            true,
            HashMap::from([(attr_ids::CPU, 150.0)]),
            &sde,
        )
        .unwrap_err();
    assert!(dyn_cpu_err.contains("Insufficient CPU"), "{dyn_cpu_err}");
    assert!(eng.modules[&8101].dynamic_attribute_overrides.is_empty());
    assert_pre_overload_intact(&eng, "dynamic fit CPU rollback");

    let dyn_pg_err = eng
        .fit_module_with_dynamic_attributes(
            8101,
            9601,
            27,
            true,
            HashMap::from([(attr_ids::POWER, 150.0)]),
            &sde,
        )
        .unwrap_err();
    assert!(dyn_pg_err.contains("Insufficient Powergrid"), "{dyn_pg_err}");
    assert!(eng.modules[&8101].dynamic_attribute_overrides.is_empty());
    assert_pre_overload_intact(&eng, "dynamic fit PG rollback");

    // set_module_online failing CPU rollback must preserve pending_overload_modules as well.
    eng.modules
        .get_mut(&8101)
        .unwrap()
        .dynamic_attribute_overrides
        .insert(attr_ids::CPU, 150.0);
    let online_err = eng.set_module_online(8101, &sde).unwrap_err();
    assert!(online_err.contains("Insufficient CPU"), "{online_err}");
    eng.modules
        .get_mut(&8101)
        .unwrap()
        .dynamic_attribute_overrides
        .clear();
    eng.recalculate_stats(&sde);
    assert_pre_overload_intact(&eng, "set_module_online CPU rollback");

    // Contrast: actively overloaded module (not pending) stays non-pending on failed re-fit.
    eng.activate_module(8101, true, &sde).unwrap();
    assert!(!eng.pending_overload_modules.contains(&8101));
    assert!(eng.fit_module_with_state(8101, 9602, 27, true, &sde).is_err());
    assert!(!eng.pending_overload_modules.contains(&8101));
    assert_eq!(eng.modules[&8101].state, ModuleState::Overloaded);
    let ctx_active = eng.evaluated_context(&sde);
    assert!(
        (ctx_active.ship.get_attribute(attr_ids::MAX_VELOCITY, &sde) - base_vel * 1.5).abs() < 1e-6
    );
    assert!((ctx_active.ship.get_attribute(2045, &sde) - base_overload_attr * 1.5).abs() < 1e-6);

    // Contrast: valid re-fit to Online clears pending overload cleanly.
    queue_pre_overload(&mut eng);
    eng.fit_module_with_state(8101, 9601, 27, true, &sde).unwrap();
    assert!(!eng.pending_overload_modules.contains(&8101));
    assert_eq!(eng.modules[&8101].state, ModuleState::Online);
}
