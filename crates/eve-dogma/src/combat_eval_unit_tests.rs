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

#[test]
fn propulsion_nosferatu_burst_and_hull_detection_edge_mutants() {
    let mut sde = SdeRegistry::default();
    sde.attributes.insert(
        796,
        eve_sde::DogmaAttributeType {
            attribute_id: 796,
            name: "massAddition".into(),
            default_value: 500_000.0,
            high_is_good: true,
            stackable: true,
            published: true,
        },
    );
    sde.types
        .insert(1, make_inv_type(1, 46, "AB_Default796", &[(796, 500_000.0)], &[6731]));
    sde.types
        .insert(2, make_inv_type(2, 46, "MWD", &[], &[6730]));
    sde.types
        .insert(3, make_inv_type(3, 46, "AB_No796", &[], &[6731]));

    let ship = make_item(
        10,
        587,
        &[
            (attr_ids::MAX_VELOCITY, 200.0),
            (attr_ids::MASS, 1_000_000.0),
            (attr_ids::SIGNATURE_RADIUS, 40.0),
        ],
    );

    // Inactive scrambled MWD still sets can_activate=false, is_scrambled=true, is_overloaded=false
    let mwd_inactive = make_item(11, 2, &[(attr_ids::SPEED_FACTOR, 500.0)]);
    let res_inact_scram = eval_propulsion_module(&ship, &mwd_inactive, false, true, true, &sde);
    assert!(!res_inact_scram.can_activate);
    assert!(res_inact_scram.is_scrambled);
    assert!(!res_inact_scram.is_overloaded);
    assert_eq!(res_inact_scram.max_velocity, 200.0);

    // AB with NO 796 in base_attributes takes sde.types default_attributes check into get_attribute(796) (500_000, not MASS 999_999)
    let ab_sde_796 = make_item(
        12,
        1,
        &[(attr_ids::SPEED_FACTOR, 100.0), (attr_ids::MASS, 999_999.0)],
    );
    let res_ab = eval_propulsion_module(&ship, &ab_sde_796, true, false, false, &sde);
    assert!(res_ab.can_activate);
    assert!(!res_ab.is_overloaded);
    assert_eq!(res_ab.mass_addition, 500_000.0);
    assert_eq!(res_ab.mass, 1_500_000.0);
    assert_eq!(res_ab.speed_factor, 100.0);
    assert_eq!(res_ab.max_velocity, 400.0);

    // AB with NO 796 in base_attributes OR sde.types.default_attributes falls back to attr MASS
    let ab_fallback_mass = make_item(
        19,
        3,
        &[(attr_ids::SPEED_FACTOR, 100.0), (attr_ids::MASS, 250_000.0)],
    );
    let res_ab_mass = eval_propulsion_module(&ship, &ab_fallback_mass, true, false, false, &sde);
    assert_eq!(res_ab_mass.mass_addition, 250_000.0);
    assert_eq!(res_ab_mass.mass, 1_250_000.0);

    // MWD with negative overload attr 1223 <= 0 falls back to default 30.0%; overloaded MWD still blooms sig
    let mwd_neg_ovl = make_item(
        13,
        2,
        &[(attr_ids::SPEED_FACTOR, 500.0), (1223, -10.0), (554, 50.0)],
    );
    let res_mwd_ovl = eval_propulsion_module(&ship, &mwd_neg_ovl, true, true, false, &sde);
    assert!((res_mwd_ovl.speed_factor - 650.0).abs() < 1e-9);
    assert!((res_mwd_ovl.signature_radius - 60.0).abs() < 1e-9);

    // get_module_falloff explicit 0.0 in attr 2044 beats attr 158
    sde.types
        .insert(101, make_inv_type(101, 71, "Neut", &[(158, 5_000.0)], &[]));
    let zero_2044 = make_item(14, 101, &[(2044, 0.0), (158, 5_000.0)]);
    assert_eq!(get_module_falloff(&zero_2044, &sde), 0.0);

    // Energy neutralizer negative target cap + partial drain
    let neut = make_item(15, 101, &[(97, 60.0), (attr_ids::MAX_RANGE, 10_000.0), (2044, 5_000.0)]);
    let neut_neg = eval_energy_neutralizer(&neut, 0.0, -10.0, &sde);
    assert_eq!(neut_neg.actual_drain, 0.0);
    assert_eq!(neut_neg.target_remaining_cap, 0.0);
    let neut_part = eval_energy_neutralizer(&neut, 10_000.0, 100.0, &sde);
    assert_eq!(neut_part.actual_drain, 60.0);
    assert_eq!(neut_part.target_remaining_cap, 40.0);

    // Energy nosferatu exact comparison boundaries & overcharged source & exact 3*falloff cutoff
    let nos = make_item(16, 101, &[(90, 80.0), (attr_ids::MAX_RANGE, 10_000.0), (2044, 5_000.0)]);
    // target = 500 + 1e-6 > 500 drains target excess when not BR
    let tiny = eval_energy_nosferatu_with_hull(&nos, 10_000.0, 500.0, 1000.0, 500.0001, false, &sde);
    assert!((tiny.actual_transferred - 80.0).abs() < 1e-9);
    // Negative target cap with BR override transfers 0
    let neg_t = eval_energy_nosferatu_with_hull(&nos, 10_000.0, 100.0, 1000.0, -5.0, true, &sde);
    assert_eq!(neg_t.actual_transferred, 0.0);
    assert_eq!(neg_t.target_new_cap, 0.0);
    // Overcharged source_current_cap > source_max_cap caps missing capacity at 0
    let over = eval_energy_nosferatu_with_hull(&nos, 10_000.0, 1100.0, 1000.0, 1500.0, false, &sde);
    assert_eq!(over.actual_transferred, 0.0);
    assert_eq!(over.source_new_cap, 1000.0);
    // Exact 3*falloff boundary: 25_000.0 drains 80 / 512, 25_000.001 drops to 0.0
    let at_cut = eval_energy_nosferatu_with_hull(&nos, 25_000.0, 0.0, 1000.0, 500.0, true, &sde);
    assert!((at_cut.falloff_factor - 0.5_f64.powi(9)).abs() < 1e-12);
    assert!((at_cut.actual_transferred - 80.0 / 512.0).abs() < 1e-12);
    let past_cut = eval_energy_nosferatu_with_hull(&nos, 25_000.001, 0.0, 1000.0, 500.0, true, &sde);
    assert_eq!(past_cut.falloff_factor, 0.0);
    assert_eq!(past_cut.actual_transferred, 0.0);

    // Burst jammer: explicit 0.0 in attr 142 overrides default 10_000; negative MAX_RANGE ignored
    let bj_type = make_inv_type(580, 80, "Burst Jammer I", &[(142, 10_000.0)], &[]);
    let bj_zero_override = make_item(17, 580, &[(attr_ids::MAX_RANGE, -1.0), (142, 0.0)]);
    assert_eq!(compute_burst_jammer_radius_m(&bj_type, &bj_zero_override, &sde), 5_000.0);

    // Blood Raider hull detection: all 6 names + attr 1945 decimal > 0 + negative control
    for (idx, name) in ["Cruor", "Ashimmu", "Bhaalgorn", "Sentinel", "Curse", "Dagon"]
        .into_iter()
        .enumerate()
    {
        let tid = 3000 + idx as u32;
        sde.types.insert(tid, make_inv_type(tid, 25, name, &[], &[]));
        assert!(is_blood_raider_nosferatu_hull(tid, &sde), "name {name} must match");
    }
    sde.types.insert(3010, make_inv_type(3010, 25, "Vigil", &[(1945, 0.0)], &[]));
    assert!(!is_blood_raider_nosferatu_hull(3010, &sde));
    sde.types.insert(3011, make_inv_type(3011, 25, "CustomBR", &[(1945, 0.1)], &[]));
    assert!(is_blood_raider_nosferatu_hull(3011, &sde));
}

#[test]
fn ewar_sorting_multi_module_clamping_cap_and_projected_boundaries() {
    let sde = SdeRegistry::default();

    // Unsorted painter slice + filtered zero/negatives before indexing kills sort deletion/inversion
    let tp_sorted = eval_target_painters_sig_radius(100.0, &[40.0, 20.0, 10.0]);
    let tp_unsorted = eval_target_painters_sig_radius(100.0, &[0.0, 10.0, -5.0, 40.0, 20.0]);
    assert!((tp_unsorted - tp_sorted).abs() < 1e-12);
    let expected_tp = 100.0
        * (1.0 + 0.4 * stacking_penalty_factor(0))
        * (1.0 + 0.2 * stacking_penalty_factor(1))
        * (1.0 + 0.1 * stacking_penalty_factor(2));
    assert!((tp_unsorted - expected_tp).abs() < 1e-9);

    // Multi-module unsorted stacking for web, TP, damp, TD + empty distances & >=0 floors
    let w_small = make_item(1, 526, &[(20, -20.0), (attr_ids::MAX_RANGE, 10_000.0)]);
    let w_large = make_item(2, 526, &[(20, -60.0), (attr_ids::MAX_RANGE, 10_000.0)]);
    let w_over = make_item(3, 526, &[(20, -120.0), (attr_ids::MAX_RANGE, 10_000.0)]);
    let web_unsorted = eval_stasis_webifier_stacking(&[&w_small, &w_large], &[], 1000.0, &sde);
    let exp_web = 1000.0 * (1.0 - 0.6) * (1.0 - 0.2 * stacking_penalty_factor(1));
    assert!((web_unsorted - exp_web).abs() < 1e-9);
    assert_eq!(eval_stasis_webifier_stacking(&[&w_over], &[0.0], 1000.0, &sde), 0.0);

    let tp_small = make_item(4, 379, &[(554, 15.0), (attr_ids::MAX_RANGE, 20_000.0)]);
    let tp_large = make_item(5, 379, &[(554, 45.0), (attr_ids::MAX_RANGE, 20_000.0)]);
    let tp_multi = eval_target_painter_stacking(&[&tp_small, &tp_large], &[0.0], 100.0, &sde);
    let exp_tp_multi = 100.0 * 1.45 * (1.0 + 0.15 * stacking_penalty_factor(1));
    assert!((tp_multi - exp_tp_multi).abs() < 1e-9);

    let damp_s = make_item(6, 208, &[(566, -15.0), (309, -20.0), (attr_ids::MAX_RANGE, 20_000.0)]);
    let damp_l = make_item(7, 208, &[(566, -50.0), (309, -45.0), (attr_ids::MAX_RANGE, 20_000.0)]);
    let (sr_m, mr_m) = eval_sensor_dampener_stacking(&[&damp_s, &damp_l], &[0.0], 200.0, 1000.0, &sde);
    assert!((sr_m - 200.0 * 0.5 * (1.0 - 0.15 * stacking_penalty_factor(1))).abs() < 1e-9);
    assert!((mr_m - 1000.0 * 0.55 * (1.0 - 0.20 * stacking_penalty_factor(1))).abs() < 1e-9);

    let td_s = make_item(8, 209, &[(767, -10.0), (351, -15.0), (349, -20.0), (attr_ids::MAX_RANGE, 20_000.0)]);
    let td_l = make_item(9, 209, &[(767, -40.0), (351, -50.0), (349, -60.0), (attr_ids::MAX_RANGE, 20_000.0)]);
    let (t_m, o_m, f_m) = eval_tracking_disruptor_stacking(&[&td_s, &td_l], &[], 1.0, 1000.0, 1000.0, &sde);
    assert!((t_m - 0.6 * (1.0 - 0.1 * stacking_penalty_factor(1))).abs() < 1e-9);
    assert!((o_m - 1000.0 * 0.5 * (1.0 - 0.15 * stacking_penalty_factor(1))).abs() < 1e-9);
    assert!((f_m - 1000.0 * 0.4 * (1.0 - 0.2 * stacking_penalty_factor(1))).abs() < 1e-9);
    let td_over = make_item(10, 209, &[(767, -150.0), (351, -150.0), (349, -150.0), (attr_ids::MAX_RANGE, 20_000.0)]);
    assert_eq!(eval_tracking_disruptor_stacking(&[&td_over], &[0.0], 1.0, 1000.0, 1000.0, &sde), (0.0, 0.0, 0.0));

    // compute_ecm_jam_chance clamps & intermediate falloff math
    assert_eq!(compute_ecm_jam_chance(-5.0, 20.0, 0.0, 10_000.0, 5_000.0), 0.0);
    assert_eq!(compute_ecm_jam_chance(5.0, 0.0, 0.0, 10_000.0, 5_000.0), 1.0);
    assert!((compute_ecm_jam_chance(6.0, 20.0, 15_000.0, 10_000.0, 5_000.0) - 0.15).abs() < 1e-9);

    // Warp scram: pts.max(1) fallback when 105=0 & 104=0, rounding 1.6 -> 2, out-of-range doesn't block MWD
    let mut sde_scram = SdeRegistry::default();
    sde_scram.effects.insert(
        9001,
        DogmaEffectType {
            effect_id: 9001,
            name: "stop".into(),
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
    sde_scram.types.insert(448, make_inv_type(448, 52, "Scram", &[], &[9001]));
    let scram_zero_pts = make_item(11, 1, &[(attr_ids::MAX_RANGE, 10_000.0)]);
    let scram_round = make_item(12, 1, &[(105, 1.6), (attr_ids::MAX_RANGE, 10_000.0)]);
    let scram_oor = make_item(13, 448, &[(105, 2.0), (attr_ids::MAX_RANGE, 9_000.0)]);
    let ws = eval_warp_scrambler_effects(&[&scram_zero_pts, &scram_round, &scram_oor], &[5_000.0, 5_000.0, 9_001.0], &sde_scram);
    assert_eq!(ws.scramble_strength, 3);
    assert!(!ws.blocks_mwd);

    // Command burst zero attr 2470 fallback & empty distances; local repair duration_ms=0 -> 1s
    let cb = eval_command_burst(&make_item(14, 1, &[]), Some(&make_item(15, 2, &[(2470, 0.0)])), &[], &sde);
    assert_eq!(cb.affected_fleet_count, 0);
    assert_eq!(cb.aggregated_bonus, 15.0);
    assert_eq!(compute_local_repair_cycle(&make_item(16, 1, &[(68, 50.0)]), false, &sde).duration_s, 1.0);

    // Ship sensor strength all-zero ties to Gravimetric
    assert_eq!(get_ship_sensor_strength(&make_item(17, 587, &[]), &sde), (ShipSensorType::Gravimetric, 0.0));

    // Scripted damp / TD / overloaded TP unknown script, clamp [0.01, 1.0], and falloff
    let extreme_damp = make_item(18, 208, &[(566, -150.0), (309, 25.0), (attr_ids::MAX_RANGE, 10_000.0), (2044, 5_000.0)]);
    assert_eq!(eval_scripted_sensor_dampener(&extreme_damp, Some(99_999), 0.0, &sde), (0.01, 1.0));
    let extreme_td = make_item(19, 209, &[(767, -80.0), (351, -150.0), (349, 20.0), (attr_ids::MAX_RANGE, 10_000.0), (2044, 5_000.0)]);
    assert_eq!(eval_scripted_tracking_disruptor(&extreme_td, Some(99_999), 15_000.0, &sde), (0.6, 0.25, 1.0));
    let tp_fall = make_item(20, 379, &[(554, 50.0), (attr_ids::MAX_RANGE, 10_000.0), (2044, 5_000.0)]);
    assert!((eval_target_painter_with_overload(&tp_fall, true, 15_000.0, &sde) - 1.3).abs() < 1e-9);

    // Capacitor stability intermediate curve: peak = 10.0 GJ/s, draw = 6.4 -> rate_norm = 0.16 -> 64.0%
    let cap_mid = evaluate_capacitor_stability(1000.0, 250.0, 6.4);
    assert!(cap_mid.is_stable);
    assert!((cap_mid.peak_recharge_per_sec - 10.0).abs() < 1e-9);
    assert!((cap_mid.stable_level_pct.unwrap() - 64.0).abs() < 1e-9);
    assert!(cap_mid.depletion_time_s.is_none());
    let cap_zero_tau = evaluate_capacitor_stability(1000.0, 0.0, 0.0);
    assert!(cap_zero_tau.is_stable);

    // Lock time clamps & exact formula, plus skill level cap 5
    let exp_lock = 40_000.0 / (2.0 * 1.0_f64.asinh().powi(2));
    assert!((compute_target_lock_time(2.0, 0.0) - exp_lock).abs() < 1e-9);
    assert_eq!(compute_effective_max_locked_targets(15, 7, 9), 12);
    assert_eq!(compute_effective_max_locked_targets(4, 5, 5), 4);

    // ShipFittingEngine projected EWAR/repair/assist: resist clamp, shield & hull repair, 3*falloff cutoff divergence
    let mut eng = ShipFittingEngine::from_sde(100, 587, &sde);
    eng.ship.base_attributes.extend([(2045, 1.5), (2116, -0.5), (2135, 1.5)]);
    let neut_long = make_item(21, 1, &[(97, 100.0), (attr_ids::MAX_RANGE, 10_000.0), (2044, 5_000.0)]);
    // At distance 30_000 (beyond optimal + 3*falloff=25_000), EW & assist do NOT cut off, whereas remote_repair DOES!
    assert!(eng.evaluate_projected_energy_warfare(&neut_long, 30_000.0, &sde) > 0.0);
    assert!(eng.evaluate_projected_remote_assistance(&neut_long, 97, 30_000.0, &sde) > 0.0);
    eng.ship.base_attributes.insert(2116, 0.5);
    let rep_shield = make_item(22, 1, &[(attr_ids::SHIELD_BONUS, 200.0), (attr_ids::MAX_RANGE, 10_000.0), (2044, 5_000.0)]);
    let rep_hull = make_item(23, 1, &[(83, 120.0), (attr_ids::MAX_RANGE, 10_000.0), (2044, 5_000.0)]);
    assert!((eng.evaluate_projected_remote_repair(&rep_shield, 15_000.0, &sde) - 50.0).abs() < 1e-9);
    assert!((eng.evaluate_projected_remote_repair(&rep_hull, 10_000.0, &sde) - 60.0).abs() < 1e-9);
    assert_eq!(eng.evaluate_projected_remote_repair(&rep_shield, 25_001.0, &sde), 0.0);
    eng.ship.base_attributes.insert(2116, -0.5);
    assert_eq!(eng.evaluate_projected_remote_repair(&rep_shield, 0.0, &sde), 0.0);
}
