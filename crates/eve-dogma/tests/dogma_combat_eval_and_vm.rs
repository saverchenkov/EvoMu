//! Comprehensive tests for `eve-dogma` combat evaluation, EWAR, capacitor simulation,
//! Dogma VM expression evaluation/scaling, and wormhole phenomena effects.

use eve_dogma::{
    apply_wormhole_phenomenon_to_attributes, capacitor_c_dot, compile_effect_to_vm,
    compile_modifier_spec_to_vm, compute_burst_jammer_radius_m, compute_ecm_jam_against_ship,
    compute_ecm_jam_chance, compute_effective_max_locked_targets, compute_local_repair_cycle,
    compute_passive_recharge, compute_target_lock_time, eval_command_burst,
    eval_energy_neutralizer, eval_energy_nosferatu, eval_energy_nosferatu_with_hull,
    eval_propulsion_module, eval_scripted_sensor_dampener, eval_scripted_tracking_disruptor,
    eval_sensor_dampener_stacking, eval_stasis_webifier_stacking,
    eval_target_painter_stacking, eval_target_painter_with_overload,
    eval_target_painters_sig_radius, eval_tracking_disruptor_stacking,
    eval_warp_scrambler_effects, evaluate_capacitor_stability, get_module_falloff,
    get_ship_sensor_strength, is_blood_raider_nosferatu_hull, item_has_any_required_skill,
    item_requires_skill, resolve_system_wormhole_phenomenon, simulate_capacitor,
    source_is_stacking_exempt, wormhole_phenomenon_type_for_class_and_name, DogmaDomain,
    DogmaEffectContext, DogmaExpressionNode, DogmaItemState, DogmaModifierFunc, DogmaVmOpcode,
    ProjectedEwarEffect, ShipFittingEngine, ShipSensorType,
};
use eve_sde::{attr_ids, DogmaEffectType, EffectModifierSpec, InvType, SdeRegistry, SolarSystem};
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

fn node(opcode: DogmaVmOpcode, value_f64: Option<f64>) -> DogmaExpressionNode {
    DogmaExpressionNode {
        opcode,
        value_f64,
        attribute_id: None,
        operator: None,
        arg1: None,
        arg2: None,
    }
}

fn bin_node(
    opcode: DogmaVmOpcode,
    a1: DogmaExpressionNode,
    a2: DogmaExpressionNode,
) -> DogmaExpressionNode {
    DogmaExpressionNode {
        opcode,
        value_f64: None,
        attribute_id: None,
        operator: None,
        arg1: Some(Box::new(a1)),
        arg2: Some(Box::new(a2)),
    }
}

#[test]
fn test_propulsion_neuts_nos_burst_jammer_and_warp_scram() {
    let mut sde = SdeRegistry::with_canonical_seed();

    // Ship with 400 m/s vel, 1,000,000 kg mass, 40 m sig
    let mut ship = DogmaItemState::from_sde(1, 587, &sde);
    ship.base_attributes.insert(attr_ids::MAX_VELOCITY, 400.0);
    ship.base_attributes.insert(attr_ids::MASS, 1_000_000.0);
    ship.base_attributes.insert(attr_ids::SIGNATURE_RADIUS, 40.0);

    // MWD type 60001 (effect 6730) using fallback sig attr 986 & custom overload 1223
    let mut mwd_attrs = HashMap::new();
    mwd_attrs.insert(attr_ids::SPEED_FACTOR, 500.0);
    mwd_attrs.insert(attr_ids::MASS_ADDITION, 500_000.0);
    mwd_attrs.insert(986, 500.0);
    mwd_attrs.insert(1223, 50.0);
    sde.types.insert(
        60001,
        inv_type(60001, 46, 7, "5MN Microwarpdrive", mwd_attrs, vec![6730]),
    );
    let mwd = DogmaItemState::from_sde(2, 60001, &sde);

    // Inactive -> base stats
    let inact = eval_propulsion_module(&ship, &mwd, false, false, false, &sde);
    assert!((inact.max_velocity - 400.0).abs() < 1e-6);
    assert!(inact.can_activate);

    // Scrambled MWD -> blocked
    let scrammed = eval_propulsion_module(&ship, &mwd, true, true, true, &sde);
    assert!(!scrammed.can_activate);
    assert!((scrammed.max_velocity - 400.0).abs() < 1e-6);

    // Overloaded MWD -> speed_factor = 500 * 1.5 = 750%, vel = 400 * 8.5 = 3400, mass = 1.5M, sig = 40 * 6 = 240
    let ovl = eval_propulsion_module(&ship, &mwd, true, true, false, &sde);
    assert!(ovl.can_activate);
    assert!((ovl.max_velocity - 3400.0).abs() < 1e-6);
    assert!((ovl.mass - 1_500_000.0).abs() < 1e-6);
    assert!((ovl.signature_radius - 240.0).abs() < 1e-6);

    // AB type 60002 (effect 6731) without attr 796 uses attr MASS (4), unaffected by scram
    let mut ab_attrs = HashMap::new();
    ab_attrs.insert(attr_ids::SPEED_FACTOR, 100.0);
    ab_attrs.insert(attr_ids::MASS, 250_000.0);
    sde.types.insert(
        60002,
        inv_type(60002, 46, 7, "1MN Afterburner", ab_attrs, vec![6731]),
    );
    let ab = DogmaItemState::from_sde(3, 60002, &sde);
    let ab_scram = eval_propulsion_module(&ship, &ab, true, false, true, &sde);
    assert!(ab_scram.can_activate);
    assert!((ab_scram.max_velocity - 800.0).abs() < 1e-6);
    assert!((ab_scram.mass - 1_250_000.0).abs() < 1e-6);

    // Neutralizer & falloff (2044 vs 158 vs 0)
    let mut neut = DogmaItemState::from_sde(4, 60001, &sde);
    assert_eq!(get_module_falloff(&neut, &sde), 0.0);
    neut.base_attributes.insert(97, 300.0);
    neut.base_attributes.insert(attr_ids::MAX_RANGE, 10_000.0);
    neut.base_attributes.insert(attr_ids::FALLOFF, 5_000.0);
    assert_eq!(get_module_falloff(&neut, &sde), 5_000.0);
    let n_res = eval_energy_neutralizer(&neut, 15_000.0, 200.0, &sde);
    assert!((n_res.falloff_factor - 0.5).abs() < 1e-6);
    assert!((n_res.actual_drain - 150.0).abs() < 1e-6);
    assert!((n_res.target_remaining_cap - 50.0).abs() < 1e-6);

    // Nosferatu normal hull vs Blood Raider hull
    let mut nos = DogmaItemState::from_sde(5, 60001, &sde);
    nos.base_attributes.insert(90, 120.0);
    nos.base_attributes.insert(attr_ids::MAX_RANGE, 10_000.0);
    // Normal hull: target_cap <= source_cap -> 0 transfer
    let nos_blocked = eval_energy_nosferatu(&nos, 5_000.0, 500.0, 1000.0, 400.0, 1000.0, &sde);
    assert_eq!(nos_blocked.actual_transferred, 0.0);
    // Target cap > source cap -> transfers 120
    let nos_ok = eval_energy_nosferatu(&nos, 5_000.0, 300.0, 1000.0, 600.0, 1000.0, &sde);
    assert!((nos_ok.actual_transferred - 120.0).abs() < 1e-6);
    assert!((nos_ok.source_new_cap - 420.0).abs() < 1e-6);
    // Blood Raider hull detection & bypass
    assert!(is_blood_raider_nosferatu_hull(17920, &sde)); // Bhaalgorn
    assert!(!is_blood_raider_nosferatu_hull(587, &sde));
    let nos_br = eval_energy_nosferatu_with_hull(&nos, 5_000.0, 800.0, 1000.0, 200.0, true, &sde);
    assert!((nos_br.actual_transferred - 120.0).abs() < 1e-6);
    assert!((nos_br.target_new_cap - 80.0).abs() < 1e-6);

    // Burst jammer radius (group 80 half-span vs group 353 full-span vs MAX_RANGE override)
    let mut bj_attrs = HashMap::new();
    bj_attrs.insert(142, 12_000.0);
    let bj_t80 = inv_type(2117, 80, 7, "Burst Jammer II", bj_attrs.clone(), vec![]);
    let mut bj_state = DogmaItemState::from_sde(6, 2117, &sde);
    bj_state.base_attributes = bj_attrs;
    assert!((compute_burst_jammer_radius_m(&bj_t80, &bj_state, &sde) - 6_000.0).abs() < 1e-6);
    let qa_bj = InvType {
        group_id: 353,
        ..bj_t80.clone()
    };
    assert!((compute_burst_jammer_radius_m(&qa_bj, &bj_state, &sde) - 12_000.0).abs() < 1e-6);
    bj_state.base_attributes.insert(attr_ids::MAX_RANGE, 8_500.0);
    assert!((compute_burst_jammer_radius_m(&bj_t80, &bj_state, &sde) - 8_500.0).abs() < 1e-6);

    // Warp scrambler with EffectStopper vs Disruptor without EffectStopper
    sde.effects.insert(
        9910,
        DogmaEffectType {
            effect_id: 9910,
            name: "warpScrambleStopper".into(),
            effect_category: 2,
            modifiers: vec![EffectModifierSpec {
                domain: "targetID".into(),
                func: "EffectStopper".into(),
                modified_attribute_id: 0,
                modifying_attribute_id: 0,
                operator: 0,
                group_id: None,
                skill_type_id: None,
                effect_id: Some(6730),
            }],
            ..Default::default()
        },
    );
    sde.types.insert(
        60003,
        inv_type(60003, 52, 7, "Synthetic Warp Scrambler", HashMap::new(), vec![9910]),
    );
    let mut scram = DogmaItemState::from_sde(7, 60003, &sde);
    scram.base_attributes.insert(attr_ids::MAX_RANGE, 9_000.0);
    scram.base_attributes.insert(105, 2.0);
    let mut point = DogmaItemState::from_sde(8, 60001, &sde);
    point.base_attributes.insert(attr_ids::MAX_RANGE, 24_000.0);
    point.base_attributes.insert(104, 1.0);

    let ws_in = eval_warp_scrambler_effects(&[&scram, &point], &[8_000.0, 20_000.0], &sde);
    assert_eq!(ws_in.scramble_strength, 3);
    assert!(ws_in.blocks_mwd);

    let ws_far = eval_warp_scrambler_effects(&[&scram, &point], &[12_000.0, 20_000.0], &sde);
    assert_eq!(ws_far.scramble_strength, 1);
    assert!(!ws_far.blocks_mwd);
}

#[test]
fn test_ewar_stacking_sensors_scripts_cap_lock_and_projected() {
    let mut sde = SdeRegistry::with_canonical_seed();

    // Stacking helpers
    let sig = eval_target_painters_sig_radius(40.0, &[30.0, 30.0]);
    assert!((sig - 40.0 * 1.3 * (1.0 + 0.3 * 0.8691199808)).abs() < 1e-4);
    assert!((compute_ecm_jam_chance(10.0, 20.0, 0.0, 20_000.0, 10_000.0) - 0.5).abs() < 1e-6);

    let mut web = DogmaItemState::from_sde(10, 587, &sde);
    web.base_attributes.insert(20, -60.0);
    web.base_attributes.insert(attr_ids::MAX_RANGE, 10_000.0);
    assert!((eval_stasis_webifier_stacking(&[&web], &[5_000.0], 500.0, &sde) - 200.0).abs() < 1e-6);

    let mut tp = DogmaItemState::from_sde(11, 587, &sde);
    tp.base_attributes.insert(554, 30.0);
    tp.base_attributes.insert(attr_ids::MAX_RANGE, 30_000.0);
    assert!((eval_target_painter_stacking(&[&tp], &[10_000.0], 100.0, &sde) - 130.0).abs() < 1e-6);
    let tp_ovl = eval_target_painter_with_overload(&tp, true, 10_000.0, &sde);
    assert!((tp_ovl - 1.36).abs() < 1e-6);

    // Sensor dampener & scripting (None, 29013 ScanRes, 29015 Range)
    let mut damp = DogmaItemState::from_sde(12, 587, &sde);
    damp.base_attributes.insert(566, -25.0);
    damp.base_attributes.insert(309, -25.0);
    damp.base_attributes.insert(attr_ids::MAX_RANGE, 25_000.0);
    let (sr, rng) = eval_sensor_dampener_stacking(&[&damp], &[10_000.0], 400.0, 80_000.0, &sde);
    assert!((sr - 300.0).abs() < 1e-6);
    assert!((rng - 60_000.0).abs() < 1e-6);
    assert_eq!(eval_scripted_sensor_dampener(&damp, None, 10_000.0, &sde), (0.75, 0.75));
    assert_eq!(eval_scripted_sensor_dampener(&damp, Some(29013), 10_000.0, &sde), (0.5, 1.0));
    assert_eq!(eval_scripted_sensor_dampener(&damp, Some(29015), 10_000.0, &sde), (1.0, 0.5));

    // Tracking disruptor & scripting (None, 29007 Tracking, 29005 Optimal/Falloff)
    let mut td = DogmaItemState::from_sde(13, 587, &sde);
    td.base_attributes.insert(767, -20.0);
    td.base_attributes.insert(351, -20.0);
    td.base_attributes.insert(349, -20.0);
    td.base_attributes.insert(attr_ids::MAX_RANGE, 25_000.0);
    let (tr, opt, fall) = eval_tracking_disruptor_stacking(&[&td], &[10_000.0], 0.1, 10_000.0, 5_000.0, &sde);
    assert!((tr - 0.08).abs() < 1e-6);
    assert!((opt - 8_000.0).abs() < 1e-6);
    assert!((fall - 4_000.0).abs() < 1e-6);
    assert_eq!(eval_scripted_tracking_disruptor(&td, None, 10_000.0, &sde), (0.8, 0.8, 0.8));
    assert_eq!(eval_scripted_tracking_disruptor(&td, Some(29007), 10_000.0, &sde), (0.6, 1.0, 1.0));
    assert_eq!(eval_scripted_tracking_disruptor(&td, Some(29005), 10_000.0, &sde), (1.0, 0.6, 0.6));

    // All 4 racial sensor types + ECM against ship
    let mut ecm = DogmaItemState::from_sde(14, 587, &sde);
    ecm.base_attributes.insert(241, 4.0); // Radar
    ecm.base_attributes.insert(239, 5.0); // Ladar
    ecm.base_attributes.insert(240, 6.0); // Mag
    ecm.base_attributes.insert(238, 7.0); // Grav
    ecm.base_attributes.insert(attr_ids::MAX_RANGE, 20_000.0);
    for (stype, aid, expected_p) in [
        (ShipSensorType::Radar, 208_u16, 0.2),
        (ShipSensorType::Ladar, 209, 0.25),
        (ShipSensorType::Magnetometric, 210, 0.3),
        (ShipSensorType::Gravimetric, 211, 0.35),
    ] {
        assert_eq!(stype.attribute_id(), aid);
        let mut target = DogmaItemState::from_sde(15, 587, &sde);
        target.base_attributes.insert(aid, 20.0);
        assert_eq!(get_ship_sensor_strength(&target, &sde), (stype, 20.0));
        assert!((compute_ecm_jam_against_ship(&ecm, &target, 10_000.0, &sde) - expected_p).abs() < 1e-6);
    }

    // Command burst & local repair cycle
    let mut cb = DogmaItemState::from_sde(16, 587, &sde);
    cb.base_attributes.insert(1892, 20_000.0);
    cb.base_attributes.insert(attr_ids::DURATION, 30_000.0);
    let mut cb_charge = DogmaItemState::from_sde(17, 587, &sde);
    cb_charge.base_attributes.insert(2470, 22.5);
    let cb_res = eval_command_burst(&cb, Some(&cb_charge), &[10_000.0, 25_000.0], &sde);
    assert_eq!(cb_res.affected_fleet_count, 1);
    assert!((cb_res.duration_s - 30.0).abs() < 1e-6);
    assert!((cb_res.aggregated_bonus - 22.5).abs() < 1e-6);

    let mut rep = DogmaItemState::from_sde(18, 587, &sde);
    rep.base_attributes.insert(attr_ids::DURATION, 4_000.0);
    rep.base_attributes.insert(attr_ids::CAPACITOR_NEED, 80.0);
    rep.base_attributes.insert(68, 250.0);
    let rep_ovl = compute_local_repair_cycle(&rep, true, &sde);
    assert!((rep_ovl.shield_boost_amount - 300.0).abs() < 1e-6);
    assert!((rep_ovl.duration_s - 4.0).abs() < 1e-6);

    // Capacitor stability, lock time, max locked targets, and projected warfare/repairs
    let cap_stable = evaluate_capacitor_stability(1000.0, 200.0, 5.0);
    assert!(cap_stable.is_stable);
    assert!(cap_stable.stable_level_pct.unwrap() > 50.0);
    let cap_unstable = evaluate_capacitor_stability(1000.0, 200.0, 20.0);
    assert!(!cap_unstable.is_stable);
    assert!(cap_unstable.depletion_time_s.unwrap() > 0.0);

    let passive = compute_passive_recharge(250.0, 1000.0, 200_000.0, 1_000.0);
    assert!(passive > 250.0 && (passive - 262.5).abs() < 0.5);
    assert!(simulate_capacitor(250.0, 1000.0, 200.0, 5.0) > 250.0);
    assert!((capacitor_c_dot(250.0, 1000.0, 200.0) - 12.5).abs() < 1e-4);

    assert!(compute_target_lock_time(200.0, 40.0) >= 0.25);
    assert_eq!(compute_effective_max_locked_targets(6, 5, 3), 6);
    assert_eq!(compute_effective_max_locked_targets(12, 3, 0), 5);

    // Projected EWAR on ShipFittingEngine
    sde.types.insert(
        60020,
        inv_type(60020, 291, 7, "Tracking Disruptor II", HashMap::new(), vec![]),
    );
    let mut eng = ShipFittingEngine::from_sde(1, 587, &sde);
    eng.upsert_projected_ewar(
        ProjectedEwarEffect {
            source_module_id: 901,
            source_module_type_id: 60020,
            charge_type_id: None,
            distance: 5_000.0,
            overloaded: false,
            effective_module: td,
        },
        &sde,
    );
    assert_eq!(eng.projected_ewar.len(), 1);
    assert!(eng.evaluate_projected_energy_warfare(&ecm, 5_000.0, &sde) == 0.0);
    let mut rrep = DogmaItemState::from_sde(19, 587, &sde);
    rrep.base_attributes.insert(84, 300.0);
    rrep.base_attributes.insert(attr_ids::MAX_RANGE, 10_000.0);
    assert!((eng.evaluate_projected_remote_repair(&rrep, 5_000.0, &sde) - 300.0).abs() < 1e-6);
    assert!(eng.remove_projected_ewar(901, &sde));
    assert!(!eng.remove_projected_ewar(901, &sde));
}

#[test]
fn test_dogma_vm_opcodes_scaling_context_and_wormhole_phenomena() {
    let sde = SdeRegistry::with_canonical_seed();

    // 1. DogmaExpressionNode::evaluate across arithmetic, boolean, comparison, and skill opcodes
    let mut item = DogmaItemState::from_sde(1, 587, &sde);
    item.base_attributes.insert(attr_ids::MAX_VELOCITY, 100.0);

    let add = bin_node(
        DogmaVmOpcode::Add,
        node(DogmaVmOpcode::DefInt, Some(3.0)),
        node(DogmaVmOpcode::DefFloat, Some(4.5)),
    );
    assert!((add.evaluate(&item, &sde) - 7.5).abs() < 1e-9);
    let sub = bin_node(
        DogmaVmOpcode::Sub,
        node(DogmaVmOpcode::DefInt, Some(10.0)),
        node(DogmaVmOpcode::DefInt, Some(3.0)),
    );
    assert!((sub.evaluate(&item, &sde) - 7.0).abs() < 1e-9);
    let mul = bin_node(
        DogmaVmOpcode::Mul,
        node(DogmaVmOpcode::DefInt, Some(6.0)),
        node(DogmaVmOpcode::DefInt, Some(7.0)),
    );
    assert!((mul.evaluate(&item, &sde) - 42.0).abs() < 1e-9);

    assert_eq!(
        bin_node(
            DogmaVmOpcode::And,
            node(DogmaVmOpcode::DefBool, Some(1.0)),
            node(DogmaVmOpcode::DefBool, Some(1.0)),
        )
        .evaluate(&item, &sde),
        1.0
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::Or,
            node(DogmaVmOpcode::DefBool, Some(0.0)),
            node(DogmaVmOpcode::DefBool, Some(1.0)),
        )
        .evaluate(&item, &sde),
        1.0
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::Gt,
            node(DogmaVmOpcode::DefInt, Some(5.0)),
            node(DogmaVmOpcode::DefInt, Some(3.0)),
        )
        .evaluate(&item, &sde),
        1.0
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::Gte,
            node(DogmaVmOpcode::DefInt, Some(5.0)),
            node(DogmaVmOpcode::DefInt, Some(5.0)),
        )
        .evaluate(&item, &sde),
        1.0
    );
    assert_eq!(
        bin_node(
            DogmaVmOpcode::Eq,
            node(DogmaVmOpcode::DefInt, Some(4.0)),
            node(DogmaVmOpcode::DefInt, Some(4.0)),
        )
        .evaluate(&item, &sde),
        1.0
    );
    assert_eq!(node(DogmaVmOpcode::UserError, None).evaluate(&item, &sde), 0.0);

    // 2. compile_effect_to_vm & scaled modifier execution + removal
    let eff = DogmaEffectType {
        effect_id: 2008, // special covert cyno branch
        name: "cynosuralDurationBonus".into(),
        effect_category: 0,
        modifiers: vec![EffectModifierSpec {
            domain: "shipID".into(),
            func: "LocationGroupModifier".into(),
            modified_attribute_id: attr_ids::DURATION,
            modifying_attribute_id: attr_ids::SPEED_FACTOR,
            operator: 6,
            group_id: Some(25),
            skill_type_id: None,
            effect_id: None,
        }],
        ..Default::default()
    };
    let vm_add = compile_effect_to_vm(&eff, false).expect("compile add");
    let vm_rm = compile_effect_to_vm(&eff, true).expect("compile rm");
    assert_eq!(DogmaDomain::from_sde_str("structureID"), Some(DogmaDomain::Structure));
    assert!(compile_modifier_spec_to_vm(&eff.modifiers[0], false).is_some());
    assert!(DogmaModifierFunc::from_sde_str("unknownFunc").is_none());

    let mut src = DogmaItemState::from_sde(2, 587, &sde);
    src.base_attributes.insert(attr_ids::SPEED_FACTOR, 20.0);
    item.base_attributes.insert(1252, 1.0); // isCovert = 1
    item.base_attributes.insert(attr_ids::DURATION, 100.0);
    vm_add.execute_on_target_with_effect_and_scale(2, Some(2008), &src, &mut item, 0.5, &sde);
    assert!((item.get_attribute(attr_ids::DURATION, &sde) - 110.0).abs() < 1e-6);
    vm_rm.execute_on_target_with_effect(2, Some(2008), &src, &mut item, &sde);
    assert!((item.get_attribute(attr_ids::DURATION, &sde) - 100.0).abs() < 1e-6);

    // DogmaEffectContext + item skill queries
    let mut ctx = DogmaEffectContext::new(
        item.clone(),
        item.clone(),
        item.clone(),
        item.clone(),
        item.clone(),
        item.clone(),
    );
    assert_eq!(ctx.entity_for_domain(DogmaDomain::Ship).item_id, 1);
    assert_eq!(ctx.entity_for_domain_mut(DogmaDomain::Char).item_id, 1);
    assert!(ctx.located_items_for_domain_mut(DogmaDomain::Target).is_empty());
    assert!(source_is_stacking_exempt(&item, &sde));
    let mut mod_item = item.clone();
    mod_item.category_id = Some(7);
    assert!(!source_is_stacking_exempt(&mod_item, &sde));
    let mut booster_item = mod_item.clone();
    booster_item.group_id = Some(303);
    assert!(source_is_stacking_exempt(&booster_item, &sde));
    item.base_attributes.insert(182, 3300.0);
    assert!(item_has_any_required_skill(&item, &sde));
    assert!(item_requires_skill(&item, 3300, &sde));
    assert!(!item_requires_skill(&item, 9999, &sde));

    // 3. Wormhole phenomena helpers & apply_wormhole_phenomenon_to_attributes
    assert_eq!(wormhole_phenomenon_type_for_class_and_name(1, "Pulsar"), Some(30844));
    assert_eq!(wormhole_phenomenon_type_for_class_and_name(6, "Black Hole"), Some(30854));
    assert_eq!(wormhole_phenomenon_type_for_class_and_name(4, "Cataclysmic Variable"), Some(30884));
    assert_eq!(wormhole_phenomenon_type_for_class_and_name(2, "Magnetar"), Some(30860));
    assert_eq!(wormhole_phenomenon_type_for_class_and_name(3, "Red Giant"), Some(30871));
    assert_eq!(wormhole_phenomenon_type_for_class_and_name(6, "Wolf-Rayet"), Some(30879));
    assert_eq!(wormhole_phenomenon_type_for_class_and_name(1, "None"), None);

    let mut wh_sde = sde.clone();
    wh_sde.solar_systems.insert(
        31000005,
        SolarSystem {
            solar_system_id: 31000005,
            constellation_id: 21000001,
            region_id: 11000001,
            name: "J000123".into(),
            security_status: -0.99,
            radius: 1e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: vec![],
            stations: vec![],
            wormhole_class_id: Some(13),
        },
    );
    wh_sde.effects.insert(
        9920,
        DogmaEffectType {
            effect_id: 9920,
            name: "whShieldBonus".into(),
            effect_category: 0,
            modifiers: vec![EffectModifierSpec {
                domain: "shipID".into(),
                func: "LocationModifier".into(),
                modified_attribute_id: attr_ids::SHIELD_CAPACITY,
                modifying_attribute_id: attr_ids::SHIELD_BONUS,
                operator: 6,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    let mut beacon_attrs = HashMap::new();
    beacon_attrs.insert(attr_ids::SHIELD_BONUS, 52.0);
    wh_sde.types.insert(
        30866,
        inv_type(30866, 920, 2, "Pulsar Effect Beacon Class 3 ", beacon_attrs, vec![9920]),
    );

    assert_eq!(
        resolve_system_wormhole_phenomenon(&wh_sde, 31000005, Some("Pulsar")),
        Some((13, 6, "Wolf-Rayet", 30879))
    );
    if let Some((&wh_sys_id, _)) = sde
        .solar_systems
        .iter()
        .find(|(&id, _)| resolve_system_wormhole_phenomenon(&sde, id, None).is_some())
    {
        let resolved = resolve_system_wormhole_phenomenon(&sde, wh_sys_id, None).unwrap();
        assert!((1..=6).contains(&resolved.1));
    }

    let mut ship_attrs = HashMap::new();
    ship_attrs.insert(attr_ids::SHIELD_CAPACITY, 1000.0);
    assert!(!apply_wormhole_phenomenon_to_attributes(&wh_sde, 30866, 11, false, &mut ship_attrs));
    assert!(!apply_wormhole_phenomenon_to_attributes(&wh_sde, 30866, 6, true, &mut ship_attrs));
    assert!(apply_wormhole_phenomenon_to_attributes(&wh_sde, 30866, 6, false, &mut ship_attrs));
    assert!((ship_attrs[&attr_ids::SHIELD_CAPACITY] - 1520.0).abs() < 1e-6);
}
