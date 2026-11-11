//! Comprehensive edge-case and boundary unit tests for `formulas.rs`.

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{DamageVector, AU_METERS};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;

#[test]
fn skills_learning_standings_and_sec_status_all_boundaries() {
    // sp_for_skill_level: level 0, 1..=5, > 5, fractional rank
    assert_eq!(sp_for_skill_level(1.0, 0), 0.0);
    assert_eq!(sp_for_skill_level(16.0, 0), 0.0);
    assert_eq!(sp_for_skill_level(1.0, 1), 250.0);
    assert_eq!(sp_for_skill_level(1.0, 2), 1_415.0);
    assert_eq!(sp_for_skill_level(1.0, 3), 8_000.0);
    assert_eq!(sp_for_skill_level(1.0, 4), 45_255.0);
    assert_eq!(sp_for_skill_level(1.0, 5), 256_000.0);
    assert_eq!(sp_for_skill_level(1.0, 99), 256_000.0);
    assert_eq!(sp_for_skill_level(5.0, 1), 1_250.0);

    // sp_per_minute & skill_training_time_seconds
    assert!((sp_per_minute(20.0, 10.0, 1.0) - 25.0).abs() < 1e-12);
    assert!((sp_per_minute(20.0, 10.0, 0.5) - 12.5).abs() < 1e-12);
    assert_eq!(skill_training_time_seconds(1_000.0, 500.0, 20.0, 20.0, 1.0), 0.0);
    assert_eq!(skill_training_time_seconds(1_000.0, 1_000.0, 20.0, 20.0, 1.0), 0.0);
    assert!((skill_training_time_seconds(0.0, 25.0, 20.0, 10.0, 1.0) - 60.0).abs() < 1e-12);
    // Floor at 1e-6 when attributes/clone_multiplier are zero or negative
    let floor_time = skill_training_time_seconds(0.0, 1.0, 0.0, 0.0, 0.0);
    assert!((floor_time - 60_000_000.0).abs() < 1e-3);

    // skill_injector_sp_yield thresholds
    assert_eq!(skill_injector_sp_yield(-100.0), 500_000.0);
    assert_eq!(skill_injector_sp_yield(4_999_999.9), 500_000.0);
    assert_eq!(skill_injector_sp_yield(5_000_000.0), 400_000.0);
    assert_eq!(skill_injector_sp_yield(49_999_999.9), 400_000.0);
    assert_eq!(skill_injector_sp_yield(50_000_000.0), 300_000.0);
    assert_eq!(skill_injector_sp_yield(79_999_999.9), 300_000.0);
    assert_eq!(skill_injector_sp_yield(80_000_000.0), 150_000.0);

    // clone_jump_cooldown_seconds
    for lvl in 0..=5u8 {
        assert_eq!(clone_jump_cooldown_seconds(lvl), (24 - u64::from(lvl)) * 3600);
    }
    assert_eq!(clone_jump_cooldown_seconds(10), 19 * 3600);

    // apply_standing_increase & apply_standing_decrease clamping
    assert!((apply_standing_increase(5.0, -10.0) - 5.0).abs() < 1e-12);
    assert!((apply_standing_increase(5.0, 150.0) - 10.0).abs() < 1e-12);
    assert!((apply_standing_increase(12.0, 10.0) - 10.0).abs() < 1e-12);
    assert!((apply_standing_increase(-15.0, 0.0) - (-10.0)).abs() < 1e-12);
    assert!((apply_standing_decrease(0.0, -25.0) - (-2.5)).abs() < 1e-12);
    assert!((apply_standing_decrease(0.0, 150.0) - (-10.0)).abs() < 1e-12);
    assert!((apply_standing_decrease(15.0, 0.0) - 10.0).abs() < 1e-12);

    // compute_effective_standing
    assert!((compute_effective_standing(0.0, 5, 0) - 2.0).abs() < 1e-12);
    assert!((compute_effective_standing(0.0, 0, 5) - 0.0).abs() < 1e-12);
    assert!((compute_effective_standing(-2.0, 5, 0) - (-2.0)).abs() < 1e-12);
    assert!((compute_effective_standing(-2.0, 0, 5) - 0.4).abs() < 1e-12);
    assert!((compute_effective_standing(-2.0, 9, 9) - 0.4).abs() < 1e-12);

    // required_standing_for_agent_level
    assert_eq!(required_standing_for_agent_level(0), 7.0);
    assert_eq!(required_standing_for_agent_level(1), -1.99);
    assert_eq!(required_standing_for_agent_level(2), 1.0);
    assert_eq!(required_standing_for_agent_level(3), 3.0);
    assert_eq!(required_standing_for_agent_level(4), 5.0);
    assert_eq!(required_standing_for_agent_level(5), 7.0);
    assert_eq!(required_standing_for_agent_level(6), 7.0);

    // apply_security_tag_repair including [-10, 10] clamps
    assert_eq!(apply_security_tag_repair(0.0, 5, 0.0), 0.0);
    assert_eq!(apply_security_tag_repair(1.0, 5, 0.0), 1.0);
    assert!((apply_security_tag_repair(-5.0, 2, 0.0) - (-4.0)).abs() < 1e-12);
    assert!((apply_security_tag_repair(8.0, 10, 12.0) - 10.0).abs() < 1e-12);
    assert!((apply_security_tag_repair(-15.0, 0, -5.0) - (-10.0)).abs() < 1e-12);
}

#[test]
fn kinematics_warp_and_jump_drive_all_branches() {
    // Sub-warp with tau floor at 1e-6 and normal tau
    assert_eq!(subwarp_velocity_at_time(250.0, 0.0, 0.0, 0.0), 0.0);
    assert!(subwarp_velocity_at_time(250.0, 0.0, 0.0, 1.0) > 249.9);
    assert_eq!(subwarp_distance_at_time(250.0, 0.0, 0.0, 0.0), 0.0);
    assert!(subwarp_distance_at_time(250.0, 0.0, 0.0, 1.0) > 249.0);
    assert_eq!(align_time_ticks(0.0, 0.0), 1);
    assert_eq!(align_time_ticks(2.0, 1_000_000.0), 3);

    // Propulsion boosted velocity including total_mass <= 1.0 clamp
    assert!((propulsion_boosted_velocity(100.0, 500.0, 200.0, -10.0, 0.0) - 100_100.0).abs() < 1e-6);
    assert!((propulsion_boosted_velocity(200.0, 100.0, 1_000_000.0, 1_000_000.0, 0.0) - 400.0).abs() < 1e-9);

    // compute_warp_duration_seconds:
    // 1) Long warp (>= d_accel + d_decel), mid warp speed (3.0 AU/s)
    let long = compute_warp_duration_seconds(12.0 * AU_METERS, 3.0, 200.0);
    // 2) Short warp (< d_accel + d_decel) where v_peak > v_drop * 1.1
    let short_mid = compute_warp_duration_seconds(0.8 * AU_METERS, 3.0, 200.0);
    // 3) Slow tiny warp & fast ultra-short warp hitting max(1.0) floor
    let slow_tiny = compute_warp_duration_seconds(100.0, 0.01, 0.5);
    let tiny_clamp = compute_warp_duration_seconds(1.0, 100.0, 2.0);
    // 4) Ultra-fast warp clamping k_d at 2.0 and v_drop at 100.0
    let fast = compute_warp_duration_seconds(15.0 * AU_METERS, 12.0, 800.0);
    assert!(long > short_mid && short_mid >= 1.0);
    assert!(slow_tiny > 50.0);
    assert_eq!(tiny_clamp, 1.0);
    assert!(fast > 1.0 && fast < long);

    // Jump fuel & fatigue/cooldown
    assert_eq!(compute_jump_fuel_isotopes(500.0, 3.5, 0, 0), 1_750);
    assert_eq!(compute_jump_fuel_isotopes(500.0, 3.5, 9, 9), 657);
    let (cd0, fat0) = compute_jump_fatigue_and_cooldown_minutes(0.0, 0.0, 0.0);
    assert_eq!(cd0, 1.0);
    assert_eq!(fat0, 10.0);
    let (cd_clamp_high, fat_clamp_high) = compute_jump_fatigue_and_cooldown_minutes(300.0, 100.0, -1.0);
    assert_eq!(cd_clamp_high, 30.0);
    assert_eq!(fat_clamp_high, 300.0);
    let (cd_clamp_bonus, fat_clamp_bonus) = compute_jump_fatigue_and_cooldown_minutes(5.0, 10.0, 1.5);
    assert!((cd_clamp_bonus - 1.5).abs() < 1e-9);
    assert!((fat_clamp_bonus - 15.0).abs() < 1e-9);
}

#[test]
fn dogma_combat_vorton_ecm_market_industry_and_world_all_branches() {
    // Dogma & thermodynamics wrappers
    assert!((dogma_stacking_penalty(0) - 1.0).abs() < 1e-9);
    assert!(passive_capacitor_or_shield_recharge(500.0, 1_000.0, 50_000.0, 500.0) > 500.0);
    assert!((peak_passive_recharge_per_sec(1_000.0, 0.0) - 2_500_000.0).abs() < 1e-3);
    assert!(rack_heat_step(0.1, 0.2, 0.01, 1.0) > 0.1);
    assert!(slot_heat_damage_chance(0.9, 1, 0.5) >= 0.0);
    assert!(nanite_repair_paste_needed(100.0, 1.0) > 0);
    assert!(booster_side_effect_check(1.0, 15.0, 0, 0, 0.0).is_some());
    assert!(booster_side_effect_check(0.1, 15.0, 5, 5, 0.99).is_none());
    assert!((command_burst_aggregate("Maximum", &[2.0, 9.0, 4.0]) - 9.0).abs() < 1e-9);
    let rolled = abyssal_attribute_roll(
        &HashMap::from([(1u16, 50.0)]),
        &HashMap::from([(1u16, [0.8, 1.2])]),
        &HashMap::from([(1u16, 1.0)]),
    );
    assert!((rolled[&1] - 60.0).abs() < 1e-6);

    // Layer EHP & crystals
    assert!((compute_layer_ehp(1_000.0, [0.5, 0.5, 0.5, 0.5], [0.0; 4]) - 1_000.0 / 1e-6).abs() < 1.0);
    assert!((compute_layer_ehp(1_000.0, [0.5, 0.5, 0.5, 0.5], [0.25; 4]) - 2_000.0).abs() < 1e-9);
    assert!((compute_layer_ehp(1_000.0, [-0.5, 2.0, 0.5, 0.5], [1.0, 1.0, 0.0, 0.0]) - 2_000.0).abs() < 1e-9);
    assert!((compute_layer_ehp(1_000.0, [0.0; 4], [1.0; 4]) - 1_000.0 / 1e-6).abs() < 1.0);
    assert!((crystal_expected_cycles(1.0, 0.0, 0.0) - 1.0 / 1e-6).abs() < 1.0);

    // Combat wrappers
    assert!(target_lock_time_sec(200.0, 40.0) > 0.0);
    assert!(turret_angular_velocity(DVec3::ZERO, DVec3::ZERO, DVec3::X * 1000.0, DVec3::Y * 100.0) > 0.0);
    let cth1 = turret_chance_to_hit(0.0, 10.0, 40.0, 1_000.0, 5_000.0, 2_000.0);
    let cth2 = turret_chance_to_hit_with_res(0.0, 10.0, 40_000.0, 40.0, 1_000.0, 5_000.0, 2_000.0);
    assert!((cth1 - cth2).abs() < 1e-12);
    let (miss_dmg, wreck) = turret_shot_damage(DamageVector::new(10.0, 0.0, 0.0, 0.0), 0.1, 0.99);
    assert!(!wreck && miss_dmg.total() == 0.0);
    assert!((missile_damage_factor(100.0, 0.0, 50.0, 100.0, 0.5) - 1.0).abs() < 1e-9);
    assert!((breacher_pod_tick_damage(50.0, 0.01, 10_000.0) - 50.0).abs() < 1e-9);
    assert!((breacher_pod_tick_damage(500.0, 0.01, 10_000.0) - 100.0).abs() < 1e-9);
    assert!((entropic_spool_multiplier(5, 10, 0.1) - 1.5).abs() < 1e-9);

    // Vorton chaining: empty, duplicate, NaN pos, tie-break, cap > 9
    assert_eq!(vorton_chain_targets(1, DVec3::ZERO, &[], 10_000.0), vec![1]);
    let vorton_nan = vorton_arc_targets_with_cap(
        1,
        DVec3::ZERO,
        &[
            (1, DVec3::ZERO),
            (4, DVec3::new(1_000.0, 0.0, 0.0)),
            (2, DVec3::new(0.0, 1_000.0, 0.0)),
            (2, DVec3::new(0.0, 1_000.0, 0.0)),
            (99, DVec3::new(f64::NAN, 0.0, 0.0)),
        ],
        5_000.0,
        0,
    );
    assert_eq!(vorton_nan, vec![1]);

    // ECM jam chance all branches
    assert!((compute_ecm_jam_chance(5.0, 10.0, 10_000.0, 20_000.0, 10_000.0) - 0.5).abs() < 1e-12);
    assert!((compute_ecm_jam_chance(5.0, 10.0, 30_000.0, 20_000.0, 10_000.0) - 0.25).abs() < 1e-12);
    assert_eq!(compute_ecm_jam_chance(5.0, 10.0, 20_001.0, 20_000.0, 0.0), 0.0);
    assert!((compute_ecm_jam_chance(5.0, 10.0, 20_000.0, 20_000.0, 0.0) - 0.5).abs() < 1e-12);
    assert_eq!(compute_ecm_jam_chance(5.0, 0.0, 0.0, 20_000.0, 10_000.0), 1.0);
    assert_eq!(compute_fighter_squadron_damage(25.0, 9, 1.5), 337.5);

    // Market, insurance, industry, PI, R&D
    assert!((compute_broker_fee_rate(5, 10.0, 10.0) - 0.01).abs() < 1e-12);
    assert!((compute_broker_fee_rate(0, -10.0, -10.0) - 0.03).abs() < 1e-12);
    assert!((compute_sales_tax_rate(0) - 0.075).abs() < 1e-12);
    assert!((compute_sales_tax_rate(5) - 0.03375).abs() < 1e-12);
    assert_eq!(compute_insurance_quote(1000.0, -0.5), (0.0, 400.0));
    assert_eq!(compute_insurance_quote(1000.0, 1.5), (300.0, 1000.0));

    let sde = SdeRegistry::with_canonical_seed();
    assert!(compute_ship_insurance_reference_value(&sde, 587) >= 0.0);
    assert_eq!(compute_ship_insurance_reference_value(&sde, 999_999_999), 0.0);
    let tiers = compute_insurance_levels(200.0);
    assert_eq!(tiers.len(), 6);
    assert_eq!(tiers[0].0, "Basic");
    assert_eq!(tiers[5], ("Platinum", 60.0, 200.0));

    assert_eq!(compute_manufacturing_material_qty(5, 1, 10, 5.0, 5.0), 5);
    assert_eq!(compute_manufacturing_material_qty(5, 0, 10, 5.0, 5.0), 5);
    assert_eq!(compute_manufacturing_material_qty(10, 100, 10, 1.0, 2.0), 874);
    assert_eq!(compute_manufacturing_time_seconds(0, 100, 20, 5, 5, 0.0), 1);
    assert_eq!(compute_manufacturing_time_seconds(1, 100, 30, 8, 8, 0.0), 55);

    assert_eq!(compute_invention_probability(0.5, 5, 5, 5, 2.0), 1.0);
    assert_eq!(compute_invention_probability(0.5, 5, 5, 5, -1.0), 0.0);
    assert_eq!(compute_reprocessing_yield_rate(0.5, 5, 5, 5, 5.0), 1.0);
    assert_eq!(compute_reprocessing_yield_rate(0.5, 5, 5, 5, -5.0), 0.0);

    let (pg, cpu) = compute_pi_link_cost(-5.0, 0);
    assert!((pg - 10.0).abs() < 1e-12 && (cpu - 15.0).abs() < 1e-12);
    assert_eq!(compute_poco_export_tax(1_000.0, -1.0, 5, -1.0), 0.0);
    assert!((compute_daily_research_points(2.0, 50.0, 3, 2) - 75.0).abs() < 1e-9);

    // Probes, missions, DBS/ESS, CONCORD, Upwell, Living World
    assert_eq!(compute_probe_scan_signal_pct(40.0, 10.0, 1.0, 1.5), 0.0);
    assert_eq!(compute_probe_scan_signal_pct(40.0, 0.0, 0.05, 0.0), 100.0);
    assert_eq!(compute_mission_reward_with_skills(1000, 100, 5, 5, 0), (1250, 150));
    assert_eq!(compute_mission_reward_with_skills(1000, 100, 5, 5, 99), (125, 15));
    let (pw, ct, mb, rb) = compute_dbs_and_ess_bounty_split(1_000.0, 0.1, 2.0);
    assert_eq!(pw, 0.0);
    assert!((ct - 210.0).abs() < 1e-9);
    assert!((mb - 105.0).abs() < 1e-9);
    assert!((rb - 35.0).abs() < 1e-9);
    assert!(concord_response_delay_sec(1.0) > 0.0);
    assert_eq!(clamp_upwell_structure_damage(-10.0, 5_000.0, 1.0), 0.0);
    assert_eq!(clamp_upwell_structure_damage(5_000.0, 5_000.0, 0.01), 500.0);
    assert!((compute_living_world_fair_price(500.0, 100.0, 0.1) - 250.0).abs() < 1e-9);
    assert!((compute_living_world_fair_price(100.0, 100.0, 5.0) - 345.0).abs() < 1e-9);
}
