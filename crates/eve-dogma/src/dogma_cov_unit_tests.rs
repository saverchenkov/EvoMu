//! Comprehensive unit and mutation-killing tests for `eve-dogma`.

use super::*;
use crate::combat_eval::{
    compute_effective_max_locked_targets, compute_target_lock_time, evaluate_capacitor_stability,
};
use eve_sde::{EffectModifierSpec, SdeRegistry};
use std::collections::HashMap;

fn base_sde_type() -> eve_sde::InvType {
    SdeRegistry::with_canonical_seed()
        .types
        .into_values()
        .next()
        .expect("canonical seed has at least one type")
}

#[test]
fn test_cov_context_falloff_and_effect_stopper_branches() {
    let mut sde = SdeRegistry::default();
    let ship_tid = 90_010;
    let mod_tid = 90_011;
    let target_tid = 90_012;
    let stopped_eid: u16 = 8100;
    let cap_bonus_attr: u16 = 317;

    let mut ship_t = base_sde_type();
    ship_t.type_id = ship_tid;
    ship_t.group_id = 25;
    ship_t.category_id = 6;
    ship_t.default_attributes = HashMap::from([(attr_ids::MAX_VELOCITY, 300.0)]);
    sde.types.insert(ship_tid, ship_t);

    let mut target_t = base_sde_type();
    target_t.type_id = target_tid;
    target_t.group_id = 25;
    target_t.category_id = 6;
    target_t.default_attributes = HashMap::from([
        (attr_ids::MAX_VELOCITY, 400.0),
        (attr_ids::CAPACITOR_NEED, 100.0),
    ]);
    sde.types.insert(target_tid, target_t);

    let mut mod_t = base_sde_type();
    mod_t.type_id = mod_tid;
    mod_t.group_id = 53;
    mod_t.category_id = 7;
    mod_t.default_attributes = HashMap::from([
        (attr_ids::SPEED_FACTOR, -50.0),
        (cap_bonus_attr, -20.0),
    ]);
    sde.types.insert(mod_tid, mod_t);

    let ship_state = DogmaItemState::from_sde(1, ship_tid, &sde);
    let target_state = DogmaItemState::from_sde(2, target_tid, &sde);
    let mod_state = DogmaItemState::from_sde(3, mod_tid, &sde);

    let mut ctx = DogmaEffectContext::new(
        mod_state,
        ship_state,
        DogmaItemState::from_sde(-1, 1373, &sde),
        target_state.clone(),
        target_state.clone(),
        target_state,
    );

    // Unknown domain / func return 0 cleanly
    let bad_dom = EffectModifierSpec {
        domain: "nonexistentDomain".into(),
        func: "ItemModifier".into(),
        modified_attribute_id: attr_ids::MAX_VELOCITY,
        modifying_attribute_id: attr_ids::SPEED_FACTOR,
        operator: 6,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    };
    assert_eq!(ctx.apply_modifier_spec_with_effect(3, None, &bad_dom, &sde), 0);
    assert_eq!(
        ctx.apply_modifier_spec_with_falloff(3, None, &bad_dom, 0.0, 1000.0, 1000.0, &sde),
        0
    );

    let bad_func = EffectModifierSpec {
        domain: "shipID".into(),
        func: "UnknownFunc".into(),
        modified_attribute_id: attr_ids::MAX_VELOCITY,
        modifying_attribute_id: attr_ids::SPEED_FACTOR,
        operator: 6,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    };
    assert_eq!(ctx.apply_modifier_spec_with_effect(3, None, &bad_func, &sde), 0);
    assert_eq!(
        ctx.apply_modifier_spec_with_falloff(3, None, &bad_func, 0.0, 1000.0, 1000.0, &sde),
        0
    );

    // EffectStopper with Some(eid) vs None
    let stopper_none = EffectModifierSpec {
        domain: "targetID".into(),
        func: "EffectStopper".into(),
        modified_attribute_id: 0,
        modifying_attribute_id: 0,
        operator: 9,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    };
    assert_eq!(ctx.apply_modifier_spec_with_effect(3, None, &stopper_none, &sde), 0);
    assert_eq!(
        ctx.apply_modifier_spec_with_falloff(3, None, &stopper_none, 0.0, 1000.0, 1000.0, &sde),
        0
    );

    let stopper_some = EffectModifierSpec {
        effect_id: Some(stopped_eid),
        ..stopper_none.clone()
    };
    assert_eq!(ctx.apply_modifier_spec_with_effect(3, None, &stopper_some, &sde), 1);
    assert!(ctx.stopped_target_effects.contains(&stopped_eid));
    assert_eq!(
        ctx.apply_modifier_spec_with_falloff(3, None, &stopper_some, 0.0, 1000.0, 1000.0, &sde),
        1
    );

    // OwnerRequiredSkillModifier applies to owned_items
    let mut drone = DogmaItemState::from_sde(50, target_tid, &sde);
    drone.base_attributes.insert(182, 3300.0);
    ctx.owned_items.push(drone);
    let own_spec = EffectModifierSpec {
        domain: "charID".into(),
        func: "OwnerRequiredSkillModifier".into(),
        modified_attribute_id: attr_ids::MAX_VELOCITY,
        modifying_attribute_id: attr_ids::SPEED_FACTOR,
        operator: 6,
        group_id: None,
        skill_type_id: Some(3300),
        effect_id: None,
    };
    assert_eq!(ctx.apply_modifier_spec_with_effect(3, None, &own_spec, &sde), 1);
    assert!((ctx.owned_items[0].get_attribute(attr_ids::MAX_VELOCITY, &sde) - 200.0).abs() < 1e-6);

    // ItemModifier with falloff on targetID at optimal+falloff distance (0.5x attenuation)
    let web_spec = EffectModifierSpec {
        domain: "targetID".into(),
        func: "ItemModifier".into(),
        modified_attribute_id: attr_ids::MAX_VELOCITY,
        modifying_attribute_id: attr_ids::SPEED_FACTOR,
        operator: 6,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    };
    assert_eq!(
        ctx.apply_modifier_spec_with_falloff(3, None, &web_spec, 15_000.0, 10_000.0, 5_000.0, &sde),
        1
    );
    // -50% * 0.5 = -25% => 400.0 * 0.75 = 300.0
    assert!((ctx.target.get_attribute(attr_ids::MAX_VELOCITY, &sde) - 300.0).abs() < 1e-6);

    // LocationModifier with falloff on shipID located items
    ctx.ship_located_items
        .push(DogmaItemState::from_sde(77, target_tid, &sde));
    let loc_spec = EffectModifierSpec {
        domain: "shipID".into(),
        func: "LocationModifier".into(),
        modified_attribute_id: attr_ids::CAPACITOR_NEED,
        modifying_attribute_id: cap_bonus_attr,
        operator: 6,
        group_id: None,
        skill_type_id: None,
        effect_id: None,
    };
    assert_eq!(
        ctx.apply_modifier_spec_with_falloff(3, None, &loc_spec, 15_000.0, 10_000.0, 5_000.0, &sde),
        1
    );
    assert!(
        (ctx.ship_located_items[0].get_attribute(attr_ids::CAPACITOR_NEED, &sde) - 90.0).abs()
            < 1e-6
    );
}

#[test]
fn test_cov_combat_eval_precision_and_stability() {
    // Falloff boundary cases (attenuation & cutoff)
    assert_eq!(compute_falloff_attenuation(5_000.0, 10_000.0, 0.0), 1.0);
    assert_eq!(compute_falloff_attenuation(10_000.0, 10_000.0, 0.0), 1.0);
    assert_eq!(compute_falloff_attenuation(10_001.0, 10_000.0, 0.0), 1.0);
    assert_eq!(compute_falloff_attenuation(8_000.0, 10_000.0, 5_000.0), 1.0);
    assert!((compute_falloff_attenuation(15_000.0, 10_000.0, 5_000.0) - 0.5).abs() < 1e-9);
    assert!((compute_falloff_attenuation(20_000.0, 10_000.0, 5_000.0) - 0.0625).abs() < 1e-9);
    assert_eq!(compute_falloff_with_cutoff(10_000.0, 10_000.0, 5_000.0), 1.0);
    assert!((compute_falloff_with_cutoff(15_000.0, 10_000.0, 5_000.0) - 0.5).abs() < 1e-9);
    assert_eq!(compute_falloff_with_cutoff(25_001.0, 10_000.0, 5_000.0), 0.0);

    // Capacitor stability: sustainable vs draining to depletion
    let sustainable = evaluate_capacitor_stability(1_000.0, 200.0, 5.0);
    assert!(sustainable.is_stable);
    assert!(sustainable.stable_level_pct.is_some());
    assert!(sustainable.depletion_time_s.is_none());

    let unstable = evaluate_capacitor_stability(1_000.0, 400.0, 50.0);
    assert!(!unstable.is_stable);
    assert!(unstable.depletion_time_s.is_some());

    // Target lock time and max locked targets bounds
    let lock_t = compute_target_lock_time(250.0, 35.0);
    assert!(lock_t > 0.25);
    assert_eq!(compute_effective_max_locked_targets(10, 5, 5), 10);
    assert_eq!(compute_effective_max_locked_targets(10, 2, 1), 5);
}

#[test]
fn test_cov_fitting_t3d_stances_drones_and_calibration_errors() {
    let mut sde = SdeRegistry::default();
    let ship_tid = 34317; // Confessor
    let non_t3d_tid = 587;
    let rig_tid = 90_020;

    let mut conf = base_sde_type();
    conf.type_id = ship_tid;
    conf.group_id = 1305;
    conf.category_id = 6;
    conf.name = "Confessor".into();
    conf.default_attributes = HashMap::from([
        (attr_ids::UPGRADE_CAPACITY, 100.0),
        (attr_ids::RIG_SLOTS, 3.0),
        (attr_ids::RIG_SIZE, 1.0),
        (1985, 10_000.0),
    ]);
    sde.types.insert(ship_tid, conf);

    let mut rifter = base_sde_type();
    rifter.type_id = non_t3d_tid;
    rifter.group_id = 25;
    rifter.category_id = 6;
    rifter.name = "Rifter".into();
    sde.types.insert(non_t3d_tid, rifter);

    for (tid, gid, name) in [
        (34319_u32, 1306_u32, "Defense"),
        (34321, 1306, "Propulsion"),
        (34564, 1306, "Svipul Defense"),
    ] {
        let mut st = base_sde_type();
        st.type_id = tid;
        st.group_id = gid;
        st.category_id = 6;
        st.name = name.into();
        sde.types.insert(tid, st);
    }

    let mut rig = base_sde_type();
    rig.type_id = rig_tid;
    rig.group_id = 773;
    rig.category_id = 7;
    rig.name = "Big Rig".into();
    rig.effect_ids = vec![2663];
    rig.default_attributes = HashMap::from([
        (attr_ids::UPGRADE_COST, 80.0),
        (attr_ids::RIG_SIZE, 1.0),
    ]);
    sde.types.insert(rig_tid, rig);

    let bad_rig_tid = 90_021;
    let mut bad_rig = base_sde_type();
    bad_rig.type_id = bad_rig_tid;
    bad_rig.group_id = 773;
    bad_rig.category_id = 7;
    bad_rig.name = "Medium Rig".into();
    bad_rig.effect_ids = vec![2663];
    bad_rig.default_attributes = HashMap::from([
        (attr_ids::UPGRADE_COST, 25.0),
        (attr_ids::RIG_SIZE, 2.0),
    ]);
    sde.types.insert(bad_rig_tid, bad_rig);

    // Non-T3D hull rejects stance set & switch
    let mut non_t3d = ShipFittingEngine::from_sde(1, non_t3d_tid, &sde);
    assert!(non_t3d.set_ship_stance(34319, &sde).is_err());
    assert!(non_t3d.switch_ship_stance(34319, 1000, &sde).is_err());

    // Wrong stance type & foreign hull stance rejected
    let mut eng = ShipFittingEngine::from_sde(2, ship_tid, &sde);
    assert!(eng.set_ship_stance(999_999, &sde).is_err());
    assert!(eng.set_ship_stance(587, &sde).is_err());
    assert!(eng.set_ship_stance(34564, &sde).is_err());
    assert!(eng.switch_ship_stance(999_999, 1000, &sde).is_err());
    assert!(eng.switch_ship_stance(587, 1000, &sde).is_err());
    assert!(eng.switch_ship_stance(34564, 1000, &sde).is_err());

    // Initial switch succeeds, same stance succeeds without cooldown wait, rapid switch fails cooldown, later succeeds
    assert_eq!(eng.switch_ship_stance(34319, 1_000, &sde).unwrap(), 34319);
    assert_eq!(eng.switch_ship_stance(34319, 2_000, &sde).unwrap(), 34319);
    assert!(eng.switch_ship_stance(34321, 5_000, &sde).is_err());
    assert_eq!(eng.switch_ship_stance(34321, 12_000, &sde).unwrap(), 34321);

    // Rig size mismatch rejected cleanly, then calibration overflow rollback on second 80-cal rig (capacity 100)
    let size_err = eng.fit_module(49, bad_rig_tid, 92, &sde).unwrap_err();
    assert!(size_err.contains("Rig size mismatch"));
    eng.fit_module(50, rig_tid, 92, &sde).unwrap();
    let cal_err = eng.fit_module(51, rig_tid, 93, &sde).unwrap_err();
    assert!(cal_err.contains("Insufficient Calibration"));
    assert_eq!(eng.modules.len(), 1);
}

#[test]
fn test_cov_kill_context_and_combat_mutants() {
    use crate::combat_eval::eval_warp_scrambler_effects;
    use crate::context::compute_passive_recharge_rate;
    use eve_sde::DogmaEffectType;

    let mut sde = SdeRegistry::default();
    let zero_tid = 90_099;
    let mut zero_t = base_sde_type();
    zero_t.type_id = zero_tid;
    zero_t.mass = 0.0;
    zero_t.capacity = 0.0;
    zero_t.volume = 0.0;
    zero_t.radius = 0.0;
    zero_t.default_attributes = HashMap::from([(attr_ids::MAX_VELOCITY, 5e-5)]);
    sde.types.insert(zero_tid, zero_t);

    // Zero mass/capacity/volume/radius never pre-populate attributes, and small non-EWAR attr is not zeroed
    let zero_state = DogmaItemState::from_sde(1, zero_tid, &sde);
    assert!(!zero_state.base_attributes.contains_key(&attr_ids::MASS));
    assert!(!zero_state.base_attributes.contains_key(&attr_ids::CAPACITY));
    assert!(!zero_state.base_attributes.contains_key(&161));
    assert!(!zero_state.base_attributes.contains_key(&162));
    assert_eq!(zero_state.get_attribute(attr_ids::MAX_VELOCITY, &sde), 5e-5);

    // Passive recharge negative current returns 0.0 even when current < max_capacity
    assert_eq!(compute_passive_recharge_rate(-5.0, 100.0, 10.0), 0.0);

    // Duplicate EffectStopper via falloff does not duplicate stopped_target_effects, and remove_effect clears it
    let stopper_spec = EffectModifierSpec {
        domain: "shipID".into(),
        func: "EffectStopper".into(),
        modified_attribute_id: 0,
        modifying_attribute_id: 0,
        operator: 6,
        group_id: None,
        skill_type_id: None,
        effect_id: Some(6441),
    };
    let mut ctx = DogmaEffectContext::new(
        zero_state.clone(),
        zero_state.clone(),
        zero_state.clone(),
        zero_state.clone(),
        zero_state.clone(),
        zero_state.clone(),
    );
    assert_eq!(ctx.apply_modifier_spec_with_falloff(9, Some(6441), &stopper_spec, 0.0, 100.0, 50.0, &sde), 1);
    assert_eq!(ctx.apply_modifier_spec_with_falloff(9, Some(6441), &stopper_spec, 0.0, 100.0, 50.0, &sde), 1);
    assert_eq!(ctx.stopped_target_effects, vec![6441]);
    let eff_stopper = DogmaEffectType {
        effect_id: 6441,
        modifiers: vec![stopper_spec],
        ..DogmaEffectType::default()
    };
    ctx.remove_effect(9, &eff_stopper);
    assert!(ctx.stopped_target_effects.is_empty());

    // Non-matching children in LocationGroupModifier & OwnerRequiredSkillModifier yield applied == 0
    ctx.ship_located_items.push(zero_state.clone());
    ctx.owned_items.push(zero_state.clone());
    let grp_spec = EffectModifierSpec {
        domain: "shipID".into(),
        func: "LocationGroupModifier".into(),
        modified_attribute_id: attr_ids::MAX_VELOCITY,
        modifying_attribute_id: attr_ids::SPEED_FACTOR,
        operator: 6,
        group_id: Some(777),
        skill_type_id: None,
        effect_id: None,
    };
    let own_spec = EffectModifierSpec {
        domain: "charID".into(),
        func: "OwnerRequiredSkillModifier".into(),
        modified_attribute_id: attr_ids::MAX_VELOCITY,
        modifying_attribute_id: attr_ids::SPEED_FACTOR,
        operator: 6,
        group_id: None,
        skill_type_id: Some(99_999),
        effect_id: None,
    };
    assert_eq!(ctx.apply_modifier_spec_with_effect(9, None, &grp_spec, &sde), 0);
    assert_eq!(ctx.apply_modifier_spec_with_falloff(9, None, &grp_spec, 0.0, 100.0, 50.0, &sde), 0);
    assert_eq!(ctx.apply_modifier_spec_with_falloff(9, None, &own_spec, 0.0, 100.0, 50.0, &sde), 0);

    // Warp scramble with missing effect ID in sde.effects
    let mut scram = DogmaItemState::from_sde(12, zero_tid, &sde);
    scram.base_attributes.insert(attr_ids::WARP_SCRAMBLE_STRENGTH, 2.0);
    scram.base_attributes.insert(attr_ids::MAX_RANGE, 10_000.0);
    if let Some(t) = sde.types.get_mut(&zero_tid) {
        t.effect_ids = vec![65_000];
    }
    let ws = eval_warp_scrambler_effects(&[&scram], &[5_000.0], &sde);
    assert_eq!(ws.scramble_strength, 2);
    assert!(!ws.blocks_mwd);
}

#[test]
fn test_cov_fitting_thermo_t3d_and_charge_mutants() {
    use fitting::thermodynamics::*;
    let h_up = step_rack_heat(0.2, 0.5, 0.1, 2.0);
    let h_down = step_rack_heat(0.8, 0.0, 0.25, 4.0);
    assert!((h_up - (1.0 - 0.8 * (-1.0_f64).exp())).abs() < 1e-12);
    assert!((h_down - 0.8 * (-1.0_f64).exp()).abs() < 1e-12);
    assert!((heat_distance_attenuation(2, 0.4) - (-0.8_f64).exp()).abs() < 1e-12);
    assert!((heat_damage_probability(0.6, 1, 0.5) - 0.6 * (-0.5_f64).exp()).abs() < 1e-12);
    assert!((effective_heat_damage(20.0, 3) - 17.0).abs() < 1e-12);
    assert!((effective_heat_damage(20.0, 7) - 15.0).abs() < 1e-12);
    assert_eq!(nanite_paste_required(10.2, 2.5), 26);
    assert_eq!(nanite_paste_required(-3.0, 2.5), 0);

    let eff_p = evaluate_booster_side_effect(0.5, 25.0, 4, 4, 0.3999);
    assert!((eff_p.unwrap() - 20.0).abs() < 1e-12);
    assert!(evaluate_booster_side_effect(0.5, 25.0, 4, 4, 0.4).is_none());
    assert_eq!(aggregate_dbuff_values("", &[]), 0.0);
    assert!((aggregate_dbuff_values("Minimum", &[3.0, 1.5, 4.0]) - 1.5).abs() < 1e-12);
    assert!((aggregate_dbuff_values("Additive", &[3.0, 1.5, 4.0]) - 8.5).abs() < 1e-12);
    assert!((aggregate_dbuff_values("Maximum", &[3.0, 1.5, 4.0]) - 4.0).abs() < 1e-12);

    let rolled = roll_abyssal_attributes(
        &HashMap::from([(10, 100.0), (11, 200.0), (12, 50.0)]),
        &HashMap::from([(10, [0.8, 1.4]), (11, [0.9, 1.3]), (99, [0.5, 1.5])]),
        &HashMap::from([(10, 0.25)]),
    );
    assert!((rolled[&10] - 95.0).abs() < 1e-12);
    assert!((rolled[&11] - 220.0).abs() < 1e-12);
    assert!((rolled[&12] - 50.0).abs() < 1e-12);
    assert!(!rolled.contains_key(&99));

    for (hull, def, alts, bad) in [
        (34317, 34319, [34321, 34323], 34564),
        (34562, 34564, [34566, 34570], 34319),
        (34828, 35676, [35677, 35678], 35686),
        (35683, 35686, [35687, 35688], 35676),
    ] {
        assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(hull), Some(def));
        assert!(ShipFittingEngine::t3d_stance_belongs_to_hull(hull, def));
        assert!(ShipFittingEngine::t3d_stance_belongs_to_hull(hull, alts[0]));
        assert!(ShipFittingEngine::t3d_stance_belongs_to_hull(hull, alts[1]));
        assert!(!ShipFittingEngine::t3d_stance_belongs_to_hull(hull, bad));
    }
    assert_eq!(ShipFittingEngine::default_t3d_stance_type_id(587), None);

    let mut sde = SdeRegistry::with_canonical_seed();
    let mut confessor = ShipFittingEngine::from_sde(1, 34317, &sde);
    assert!(confessor.set_ship_stance(587, &sde).is_err());
    assert!(confessor.set_ship_stance(34564, &sde).is_err());
    assert!(confessor.set_ship_stance(999_999, &sde).is_err());
    assert!(confessor.switch_ship_stance(34564, 1_000, &sde).is_err());
    assert!(confessor.switch_ship_stance(587, 1_000, &sde).is_err());
    assert!(confessor.switch_ship_stance(999_999, 1_000, &sde).is_err());
    assert_eq!(confessor.switch_ship_stance(34319, 1_000, &sde).unwrap(), 34319);
    assert_eq!(confessor.switch_ship_stance(34319, 2_000, &sde).unwrap(), 34319);
    let cd_err = confessor.switch_ship_stance(34321, 11_999, &sde).unwrap_err();
    assert!(cd_err.contains("1ms remaining"));
    assert_eq!(confessor.switch_ship_stance(34321, 12_000, &sde).unwrap(), 34321);

    let mut rifter = ShipFittingEngine::from_sde(2, 587, &sde);
    assert!(rifter.set_ship_stance(34319, &sde).is_err());
    assert!(rifter.switch_ship_stance(34319, 50_000, &sde).is_err());
    if let Some(t) = sde.types.get_mut(&34317) {
        t.group_id = 963;
    }
    confessor.modules.insert(
        50,
        FittedModule {
            item_id: 50,
            type_id: 34317,
            flag_id: 126,
            state: ModuleState::Online,
            charge_type_id: None,
            charge_quantity: 0,
            damage_hp: 0.0,
            max_hp: 40.0,
            dynamic_attribute_overrides: HashMap::new(),
        },
    );
    if let Some(t) = sde.types.get_mut(&34317) {
        t.default_attributes.extend([(1374, 6.0), (1375, 5.0), (1376, 4.0), (1368, 3.0), (1369, 2.0)]);
    }
    confessor.set_module_dynamic_attribute_and_recalculate(50, attr_ids::CPU, 12.5, &sde);
    confessor.set_module_dynamic_attributes_and_recalculate(50, HashMap::from([(attr_ids::POWER, 25.0)]), &sde);
    assert!((confessor.ship.get_attribute(attr_ids::HI_SLOTS, &sde) - 6.0).abs() < 1e-9);
    assert!((confessor.ship.get_attribute(attr_ids::TURRET_SLOTS_LEFT, &sde) - 3.0).abs() < 1e-9);
    assert!((confessor.ship.get_attribute(attr_ids::LAUNCHER_SLOTS_LEFT, &sde) - 2.0).abs() < 1e-9);
}

