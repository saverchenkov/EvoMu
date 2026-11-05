//! Unit and mutation-killing tests for `combat_eval`.

use super::*;
use eve_sde::{DogmaEffectType, EffectModifierSpec, InvType, SdeRegistry};
use std::collections::HashMap;

fn make_item(item_id: i64, type_id: TypeId, attrs: &[(AttributeId, f64)]) -> DogmaItemState {
    let mut base_attributes = HashMap::new();
    for &(k, v) in attrs {
        base_attributes.insert(k, v);
    }
    DogmaItemState {
        item_id,
        type_id,
        group_id: None,
        category_id: None,
        base_attributes,
        modifiers: Vec::new(),
        skill_levels: HashMap::new(),
    }
}

fn make_inv_type(
    type_id: TypeId,
    group_id: u32,
    name: &str,
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
        category_id: 7,
        name: name.to_string(),
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
        effect_ids: effects.to_vec(),
        default_attributes,
    }
}

#[test]
fn propulsion_module_ab_and_mwd_branches() {
    let mut sde = SdeRegistry::default();
    // Type 1: Afterburner (no 6730 effect)
    sde.types
        .insert(1, make_inv_type(1, 46, "AB", &[(796, 500_000.0)], &[6731]));
    // Type 2: MWD (has 6730 effect)
    sde.types
        .insert(2, make_inv_type(2, 46, "MWD", &[], &[6730]));

    let ship = make_item(
        10,
        587,
        &[
            (attr_ids::MAX_VELOCITY, 200.0),
            (attr_ids::MASS, 1_000_000.0),
            (attr_ids::SIGNATURE_RADIUS, 40.0),
        ],
    );
    let ab = make_item(
        11,
        1,
        &[
            (attr_ids::SPEED_FACTOR, 100.0),
            (796, 500_000.0),
            (554, 500.0), // ignored for AB
        ],
    );

    // Inactive AB returns base stats
    let inactive = eval_propulsion_module(&ship, &ab, false, true, false, &sde);
    assert_eq!(inactive.max_velocity, 200.0);
    assert_eq!(inactive.mass, 1_000_000.0);
    assert_eq!(inactive.signature_radius, 40.0);
    assert_eq!(inactive.speed_factor, 0.0);
    assert_eq!(inactive.mass_addition, 0.0);
    assert_eq!(inactive.signature_radius_bonus, 0.0);
    assert!(!inactive.is_overloaded);
    assert!(inactive.can_activate);

    // Active AB while scrambled still activates (not MWD), default overload bonus 30% when attr 1223 == 0
    let ab_ovl = eval_propulsion_module(&ship, &ab, true, true, true, &sde);
    assert!(ab_ovl.can_activate);
    assert!(ab_ovl.is_scrambled);
    assert!(ab_ovl.is_overloaded);
    assert!((ab_ovl.speed_factor - 130.0).abs() < 1e-9);
    assert!((ab_ovl.max_velocity - 460.0).abs() < 1e-9);
    assert!((ab_ovl.mass - 1_500_000.0).abs() < 1e-9);
    assert_eq!(ab_ovl.signature_radius, 40.0);
    assert_eq!(ab_ovl.signature_radius_bonus, 0.0);

    // Active MWD when scrambled cannot activate
    let mwd = make_item(
        12,
        2,
        &[
            (attr_ids::SPEED_FACTOR, 500.0),
            (1223, 50.0),
            (attr_ids::MASS, 1_500_000.0),
            (554, 500.0),
        ],
    );
    let mwd_scram = eval_propulsion_module(&ship, &mwd, true, true, true, &sde);
    assert!(!mwd_scram.can_activate);
    assert_eq!(mwd_scram.max_velocity, 200.0);
    assert_eq!(mwd_scram.mass, 1_000_000.0);
    assert_eq!(mwd_scram.signature_radius, 40.0);

    // Active MWD unscrambled with explicit attr 1223=50%, mass fallback to MASS, sig bloom via attr 554
    let mwd_active = eval_propulsion_module(&ship, &mwd, true, true, false, &sde);
    assert!(mwd_active.can_activate);
    assert!((mwd_active.speed_factor - 750.0).abs() < 1e-9);
    assert!((mwd_active.max_velocity - 1700.0).abs() < 1e-9);
    assert!((mwd_active.mass - 2_500_000.0).abs() < 1e-9);
    assert!((mwd_active.signature_radius_bonus - 500.0).abs() < 1e-9);
    assert!((mwd_active.signature_radius - 240.0).abs() < 1e-9);

    // MWD with attr 554 <= 0 falls back to attr 986
    let mwd_986 = make_item(
        13,
        2,
        &[
            (attr_ids::SPEED_FACTOR, 500.0),
            (554, 0.0),
            (986, 400.0),
            (796, 250_000.0),
        ],
    );
    let res_986 = eval_propulsion_module(&ship, &mwd_986, true, false, false, &sde);
    assert!((res_986.signature_radius_bonus - 400.0).abs() < 1e-9);
    assert!((res_986.signature_radius - 200.0).abs() < 1e-9);
    assert!((res_986.mass_addition - 250_000.0).abs() < 1e-9);

    // MWD with both 554 <= 0 and 986 <= 0 leaves sig unchanged
    let mwd_no_sig = make_item(14, 2, &[(attr_ids::SPEED_FACTOR, 500.0), (554, -5.0), (986, 0.0)]);
    let res_no_sig = eval_propulsion_module(&ship, &mwd_no_sig, true, false, false, &sde);
    assert_eq!(res_no_sig.signature_radius, 40.0);
}

#[test]
fn get_module_falloff_precedence_and_neutralizer_nosferatu() {
    let mut sde = SdeRegistry::default();
    sde.types.insert(
        100,
        make_inv_type(100, 71, "Neut", &[(2044, 8000.0), (158, 4000.0)], &[]),
    );
    sde.types
        .insert(101, make_inv_type(101, 71, "Neut2", &[(158, 6000.0)], &[]));

    // 1. base_attributes[2044] wins first
    let m1 = make_item(1, 100, &[(2044, 12_000.0), (158, 2000.0)]);
    assert_eq!(get_module_falloff(&m1, &sde), 12_000.0);

    // 2. type.default_attributes[2044] wins second
    let m2 = make_item(2, 100, &[(158, 2000.0)]);
    assert_eq!(get_module_falloff(&m2, &sde), 8000.0);

    // 3. base_attributes[158] wins third when 2044 absent
    let m3 = make_item(3, 101, &[(158, 9000.0)]);
    assert_eq!(get_module_falloff(&m3, &sde), 9000.0);

    // 4. type.default_attributes[158] wins fourth
    let m4 = make_item(4, 101, &[]);
    assert_eq!(get_module_falloff(&m4, &sde), 6000.0);

    // 5. unknown type without 2044 returns 0.0
    let m5 = make_item(5, 999, &[(158, 5000.0)]);
    assert_eq!(get_module_falloff(&m5, &sde), 0.0);

    // Energy neutralizer at optimal + 1*falloff (attenuation = 0.5)
    let neut = make_item(
        6,
        101,
        &[(97, 100.0), (attr_ids::MAX_RANGE, 10_000.0), (2044, 5_000.0)],
    );
    let res = eval_energy_neutralizer(&neut, 15_000.0, 40.0, &sde);
    assert_eq!(res.nominal_drain, 100.0);
    assert!((res.falloff_factor - 0.5).abs() < 1e-9);
    assert!((res.actual_drain - 40.0).abs() < 1e-9);
    assert_eq!(res.target_remaining_cap, 0.0);

    // Energy nosferatu: target_cap <= source_cap fails without Blood Raider override
    let nos = make_item(
        7,
        101,
        &[(90, 80.0), (attr_ids::MAX_RANGE, 10_000.0), (2044, 5_000.0)],
    );
    let no_drain = eval_energy_nosferatu(&nos, 10_000.0, 500.0, 1000.0, 500.0, 1000.0, &sde);
    assert_eq!(no_drain.actual_transferred, 0.0);
    assert_eq!(no_drain.source_new_cap, 500.0);
    assert_eq!(no_drain.target_new_cap, 500.0);

    // With Blood Raider override, drains even when target_cap <= source_cap, capped by source missing cap
    let br_drain =
        eval_energy_nosferatu_with_hull(&nos, 10_000.0, 950.0, 1000.0, 500.0, true, &sde);
    assert_eq!(br_drain.actual_transferred, 50.0);
    assert_eq!(br_drain.source_new_cap, 1000.0);
    assert_eq!(br_drain.target_new_cap, 450.0);

    // Beyond optimal + 3*falloff cutoff -> 0.0 falloff_factor
    let cutoff =
        eval_energy_nosferatu_with_hull(&nos, 25_001.0, 100.0, 1000.0, 500.0, true, &sde);
    assert_eq!(cutoff.falloff_factor, 0.0);
    assert_eq!(cutoff.actual_transferred, 0.0);
}

#[test]
fn burst_jammer_radius_and_blood_raider_hull_detection() {
    let mut sde = SdeRegistry::default();
    let bj_type = make_inv_type(580, 80, "Burst Jammer I", &[(142, 10_000.0)], &[]);
    let qa_type = make_inv_type(900, 353, "QA Burst", &[(142, 14_000.0)], &[]);
    let empty_bj = make_inv_type(581, 80, "Empty BJ", &[], &[]);
    let empty_qa = make_inv_type(901, 353, "Empty QA", &[], &[]);

    // MAX_RANGE > 0 takes precedence
    let with_max_r = make_item(1, 580, &[(attr_ids::MAX_RANGE, 7500.0), (142, 10_000.0)]);
    assert_eq!(compute_burst_jammer_radius_m(&bj_type, &with_max_r, &sde), 7500.0);

    // group 80 halves attr 142 (10,000 -> 5,000; 12,000 -> 6,000)
    let state_bj = make_item(2, 580, &[(142, 12_000.0)]);
    assert_eq!(compute_burst_jammer_radius_m(&bj_type, &state_bj, &sde), 6000.0);
    let state_default = make_item(3, 580, &[]);
    assert_eq!(compute_burst_jammer_radius_m(&bj_type, &state_default, &sde), 5000.0);
    assert_eq!(compute_burst_jammer_radius_m(&empty_bj, &state_default, &sde), 5000.0);

    // non-group-80 uses full attr 142 or 5,000 fallback
    assert_eq!(compute_burst_jammer_radius_m(&qa_type, &state_default, &sde), 14_000.0);
    assert_eq!(compute_burst_jammer_radius_m(&empty_qa, &state_default, &sde), 5000.0);

    // Blood Raider hulls: hardcoded IDs, names, attr 1945, effect 5854
    for id in [11192, 11963, 17920, 17922, 17924, 17926, 42242] {
        assert!(is_blood_raider_nosferatu_hull(id, &sde));
    }
    assert!(!is_blood_raider_nosferatu_hull(587, &sde));

    sde.types
        .insert(2001, make_inv_type(2001, 25, "Cruor", &[], &[]));
    sde.types
        .insert(2002, make_inv_type(2002, 25, "CustomBRAttr", &[(1945, 1.0)], &[]));
    sde.types
        .insert(2003, make_inv_type(2003, 25, "CustomBREff", &[(1945, 0.0)], &[5854]));
    sde.types
        .insert(2004, make_inv_type(2004, 25, "Rifter", &[(1945, 0.0)], &[]));
    assert!(is_blood_raider_nosferatu_hull(2001, &sde));
    assert!(is_blood_raider_nosferatu_hull(2002, &sde));
    assert!(is_blood_raider_nosferatu_hull(2003, &sde));
    assert!(!is_blood_raider_nosferatu_hull(2004, &sde));
}

#[test]
fn ewar_stacking_and_warp_scrambler_effects() {
    let mut sde = SdeRegistry::default();
    // Target painter sig radius helper ignores <= 0 bonuses and stacks positive ones
    let base_sig = 100.0;
    assert_eq!(eval_target_painters_sig_radius(base_sig, &[0.0, -20.0]), 100.0);
    let one_tp = eval_target_painters_sig_radius(base_sig, &[30.0]);
    assert!((one_tp - 130.0).abs() < 1e-9);
    let two_tp = eval_target_painters_sig_radius(base_sig, &[30.0, 30.0]);
    let expected_two = 100.0 * 1.3 * (1.0 + 0.3 * stacking_penalty_factor(1));
    assert!((two_tp - expected_two).abs() < 1e-9);

    // Stasis webifier stacking
    let web1 = make_item(1, 526, &[(20, -60.0), (attr_ids::MAX_RANGE, 10_000.0)]);
    let web2 = make_item(2, 526, &[(20, -50.0), (attr_ids::MAX_RANGE, 10_000.0)]);
    let webbed_vel = eval_stasis_webifier_stacking(&[&web1, &web2], &[5_000.0, 5_000.0], 1000.0, &sde);
    let exp_vel = 1000.0 * (1.0 - 0.6) * (1.0 - 0.5 * stacking_penalty_factor(1));
    assert!((webbed_vel - exp_vel).abs() < 1e-9);

    // Target painter module stacking
    let tp1 = make_item(3, 379, &[(554, 30.0), (attr_ids::MAX_RANGE, 20_000.0)]);
    let tp_sig = eval_target_painter_stacking(&[&tp1], &[10_000.0], 100.0, &sde);
    assert!((tp_sig - 130.0).abs() < 1e-9);

    // Sensor dampener stacking + floor at 1.0
    let damp1 = make_item(
        4,
        208,
        &[(566, -50.0), (309, -50.0), (attr_ids::MAX_RANGE, 30_000.0)],
    );
    let (sr, mr) = eval_sensor_dampener_stacking(&[&damp1], &[10_000.0], 200.0, 50_000.0, &sde);
    assert!((sr - 100.0).abs() < 1e-9);
    assert!((mr - 25_000.0).abs() < 1e-9);
    let (sr_floor, mr_floor) = eval_sensor_dampener_stacking(&[&damp1], &[0.0], 0.5, 0.5, &sde);
    assert_eq!(sr_floor, 1.0);
    assert_eq!(mr_floor, 1.0);

    // Tracking disruptor stacking
    let td1 = make_item(
        5,
        209,
        &[
            (767, -30.0),
            (351, -40.0),
            (349, -50.0),
            (attr_ids::MAX_RANGE, 20_000.0),
        ],
    );
    let (trk, opt, fall) =
        eval_tracking_disruptor_stacking(&[&td1], &[10_000.0], 1.0, 10_000.0, 5_000.0, &sde);
    assert!((trk - 0.7).abs() < 1e-9);
    assert!((opt - 6_000.0).abs() < 1e-9);
    assert!((fall - 2_500.0).abs() < 1e-9);

    // Warp scrambler effects: boundary dist == max_range vs dist > max_range, attr 105 vs 104, EffectStopper
    sde.effects.insert(
        9001,
        DogmaEffectType {
            effect_id: 9001,
            name: "scramStopper".to_string(),
            effect_category: 1,
            modifiers: vec![EffectModifierSpec {
                domain: "targetID".to_string(),
                func: "EffectStopper".to_string(),
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
    sde.types
        .insert(448, make_inv_type(448, 52, "Scram", &[], &[9001]));
    let scram = make_item(6, 448, &[(105, 2.0), (attr_ids::MAX_RANGE, 9_000.0)]);
    let disrupt = make_item(7, 447, &[(104, 3.0), (attr_ids::MAX_RANGE, 24_000.0)]);
    let out_of_range = make_item(8, 448, &[(105, 5.0), (attr_ids::MAX_RANGE, 9_000.0)]);

    let ws = eval_warp_scrambler_effects(
        &[&scram, &disrupt, &out_of_range],
        &[9_000.0, 20_000.0, 9_000.1],
        &sde,
    );
    assert_eq!(ws.scramble_strength, 5);
    assert!(ws.blocks_mwd);
}

#[test]
fn command_burst_repair_ecm_scripts_and_capacitor_stability() {
    let sde = SdeRegistry::default();
    // Command burst defaults vs explicit attributes
    let empty_burst = make_item(1, 1, &[]);
    let cb_default = eval_command_burst(&empty_burst, None, &[35_000.0, 35_000.1], &sde);
    assert_eq!(cb_default.burst_radius, 35_000.0);
    assert_eq!(cb_default.duration_s, 60.0);
    assert_eq!(cb_default.affected_fleet_count, 1);
    assert_eq!(cb_default.aggregated_bonus, 15.0);

    let custom_burst = make_item(2, 1, &[(1892, 50_000.0), (attr_ids::DURATION, 90_000.0)]);
    let charge = make_item(3, 2, &[(2470, 22.5)]);
    let cb_custom = eval_command_burst(&custom_burst, Some(&charge), &[10_000.0, 50_000.0], &sde);
    assert_eq!(cb_custom.burst_radius, 50_000.0);
    assert_eq!(cb_custom.duration_s, 90.0);
    assert_eq!(cb_custom.affected_fleet_count, 2);
    assert_eq!(cb_custom.aggregated_bonus, 22.5);

    // Local repair cycle
    let rep = make_item(
        4,
        1,
        &[
            (68, 100.0),
            (84, 200.0),
            (attr_ids::CAPACITOR_NEED, 50.0),
            (attr_ids::DURATION, 4000.0),
        ],
    );
    let normal_rep = compute_local_repair_cycle(&rep, false, &sde);
    assert_eq!(normal_rep.shield_boost_amount, 100.0);
    assert_eq!(normal_rep.armor_repair_amount, 200.0);
    assert_eq!(normal_rep.capacitor_need, 50.0);
    assert_eq!(normal_rep.duration_s, 4.0);
    let ovl_rep = compute_local_repair_cycle(&rep, true, &sde);
    assert!((ovl_rep.shield_boost_amount - 120.0).abs() < 1e-9);
    assert!((ovl_rep.armor_repair_amount - 240.0).abs() < 1e-9);

    // Sensor types and ECM jam against ship
    assert_eq!(ShipSensorType::Radar.attribute_id(), 208);
    assert_eq!(ShipSensorType::Ladar.attribute_id(), 209);
    assert_eq!(ShipSensorType::Magnetometric.attribute_id(), 210);
    assert_eq!(ShipSensorType::Gravimetric.attribute_id(), 211);

    let ecm = make_item(
        5,
        1,
        &[
            (241, 6.0),
            (239, 8.0),
            (240, 10.0),
            (238, 12.0),
            (attr_ids::MAX_RANGE, 50_000.0),
        ],
    );
    for (sensor_attr, expected_type, expected_jam) in [
        (208, ShipSensorType::Radar, 0.3),
        (209, ShipSensorType::Ladar, 0.4),
        (210, ShipSensorType::Magnetometric, 0.5),
        (211, ShipSensorType::Gravimetric, 0.6),
    ] {
        let target = make_item(6, 587, &[(sensor_attr, 20.0)]);
        let (st, val) = get_ship_sensor_strength(&target, &sde);
        assert_eq!(st, expected_type);
        assert_eq!(val, 20.0);
        let jam = compute_ecm_jam_against_ship(&ecm, &target, 10_000.0, &sde);
        assert!((jam - expected_jam).abs() < 1e-9);
    }

    // Scripted sensor dampener & tracking disruptor & overloaded painter
    let damp = make_item(7, 208, &[(566, -30.0), (309, -25.0), (attr_ids::MAX_RANGE, 20_000.0)]);
    assert_eq!(eval_scripted_sensor_dampener(&damp, None, 10_000.0, &sde), (0.7, 0.75));
    assert_eq!(eval_scripted_sensor_dampener(&damp, Some(29013), 10_000.0, &sde), (0.4, 1.0));
    assert_eq!(eval_scripted_sensor_dampener(&damp, Some(29015), 10_000.0, &sde), (1.0, 0.5));

    let td = make_item(
        8,
        209,
        &[
            (767, -25.0),
            (351, -20.0),
            (349, -30.0),
            (attr_ids::MAX_RANGE, 20_000.0),
        ],
    );
    assert_eq!(eval_scripted_tracking_disruptor(&td, None, 10_000.0, &sde), (0.75, 0.8, 0.7));
    assert_eq!(eval_scripted_tracking_disruptor(&td, Some(29007), 10_000.0, &sde), (0.5, 1.0, 1.0));
    assert_eq!(eval_scripted_tracking_disruptor(&td, Some(29005), 10_000.0, &sde), (1.0, 0.6, 0.4));

    let tp = make_item(9, 379, &[(554, 30.0), (attr_ids::MAX_RANGE, 20_000.0)]);
    assert!((eval_target_painter_with_overload(&tp, false, 10_000.0, &sde) - 1.3).abs() < 1e-9);
    assert!((eval_target_painter_with_overload(&tp, true, 10_000.0, &sde) - 1.36).abs() < 1e-9);
    let tp_custom_ov = make_item(10, 379, &[(554, 30.0), (1937, 50.0), (attr_ids::MAX_RANGE, 20_000.0)]);
    assert!((eval_target_painter_with_overload(&tp_custom_ov, true, 10_000.0, &sde) - 1.45).abs() < 1e-9);

    // Capacitor stability: 0 draw -> 100%, exact peak -> 25%, over peak -> unstable with depletion time
    let cap_full = evaluate_capacitor_stability(1000.0, 250.0, 0.0); // peak = 10.0 GJ/s
    assert!(cap_full.is_stable);
    assert_eq!(cap_full.stable_level_pct, Some(100.0));
    let cap_peak = evaluate_capacitor_stability(1000.0, 250.0, 10.0);
    assert!(cap_peak.is_stable);
    assert_eq!(cap_peak.stable_level_pct, Some(25.0));
    let cap_unstable = evaluate_capacitor_stability(1000.0, 250.0, 15.0);
    assert!(!cap_unstable.is_stable);
    assert_eq!(cap_unstable.depletion_time_s, Some(100.0)); // 1000 / (15 - 5)

    // Lock time & max locked targets
    assert_eq!(compute_target_lock_time(1_000_000.0, 1000.0), 0.25);
    assert!(compute_target_lock_time(100.0, 40.0) > 10.0);
    assert_eq!(compute_effective_max_locked_targets(0, 0, 0), 1);
    assert_eq!(compute_effective_max_locked_targets(10, 5, 5), 10);
    assert_eq!(compute_effective_max_locked_targets(20, 5, 5), 12);

    // Projected energy warfare, remote repair, remote assistance on ShipFittingEngine
    let mut eng = ShipFittingEngine::from_sde(100, 587, &sde);
    eng.ship.base_attributes.insert(2045, 0.5);
    eng.ship.base_attributes.insert(2116, 0.8);
    eng.ship.base_attributes.insert(2135, 0.6);
    let neut_mod = make_item(20, 1, &[(97, 200.0), (attr_ids::MAX_RANGE, 10_000.0)]);
    let nos_mod = make_item(21, 1, &[(90, 100.0), (attr_ids::MAX_RANGE, 10_000.0)]);
    assert!((eng.evaluate_projected_energy_warfare(&neut_mod, 5_000.0, &sde) - 100.0).abs() < 1e-9);
    assert!((eng.evaluate_projected_energy_warfare(&nos_mod, 5_000.0, &sde) - 50.0).abs() < 1e-9);

    let r_armor = make_item(22, 1, &[(84, 500.0), (attr_ids::MAX_RANGE, 10_000.0)]);
    let r_empty = make_item(23, 1, &[(attr_ids::MAX_RANGE, 10_000.0)]);
    assert!((eng.evaluate_projected_remote_repair(&r_armor, 5_000.0, &sde) - 400.0).abs() < 1e-9);
    assert_eq!(eng.evaluate_projected_remote_repair(&r_empty, 5_000.0, &sde), 0.0);
    let r_assist = make_item(24, 1, &[(566, 50.0), (attr_ids::MAX_RANGE, 10_000.0)]);
    assert!((eng.evaluate_projected_remote_assistance(&r_assist, 566, 5_000.0, &sde) - 30.0).abs() < 1e-9);
}
