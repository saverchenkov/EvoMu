//! Comprehensive unit and mutation-killing tests for `formulas`.

#![forbid(unsafe_code)]

use super::*;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;

#[test]
fn skills_standings_and_kinematics_exact_formulas_and_boundaries() {
    // 1. sp_for_skill_level
    assert_eq!(sp_for_skill_level(2.0, 0), 0.0);
    assert_eq!(sp_for_skill_level(2.0, 1), 500.0);
    assert_eq!(sp_for_skill_level(1.0, 2), 1_415.0);
    assert_eq!(sp_for_skill_level(1.0, 3), 8_000.0);
    assert_eq!(sp_for_skill_level(1.0, 4), 45_255.0);
    assert_eq!(sp_for_skill_level(1.0, 5), 256_000.0);
    assert_eq!(sp_for_skill_level(1.0, 8), 256_000.0);

    // 2. sp_per_minute & skill_training_time_seconds
    assert!((sp_per_minute(24.0, 18.0, 0.5) - 16.5).abs() < 1e-12);
    assert!((skill_training_time_seconds(1_000.0, 1_990.0, 24.0, 18.0, 1.0) - 1_800.0).abs() < 1e-9);
    assert_eq!(skill_training_time_seconds(2_000.0, 1_000.0, 20.0, 20.0, 1.0), 0.0);
    assert!(skill_training_time_seconds(0.0, 10.0, 0.0, 0.0, 0.0) > 1e6);

    // 3. skill_injector_sp_yield exact boundaries
    assert_eq!(skill_injector_sp_yield(4_999_999.0), 500_000.0);
    assert_eq!(skill_injector_sp_yield(5_000_000.0), 400_000.0);
    assert_eq!(skill_injector_sp_yield(49_999_999.0), 400_000.0);
    assert_eq!(skill_injector_sp_yield(50_000_000.0), 300_000.0);
    assert_eq!(skill_injector_sp_yield(79_999_999.0), 300_000.0);
    assert_eq!(skill_injector_sp_yield(80_000_000.0), 150_000.0);

    // 4. clone_jump_cooldown_seconds
    assert_eq!(clone_jump_cooldown_seconds(0), 24 * 3600);
    assert_eq!(clone_jump_cooldown_seconds(3), 21 * 3600);
    assert_eq!(clone_jump_cooldown_seconds(5), 19 * 3600);
    assert_eq!(clone_jump_cooldown_seconds(9), 19 * 3600);

    // 5. Standings & security status repair
    assert!((apply_standing_increase(5.0, 20.0) - 6.0).abs() < 1e-12);
    assert!((apply_standing_increase(0.0, 200.0) - 10.0).abs() < 1e-12);
    assert!((apply_standing_decrease(0.0, 10.0) - (-1.0)).abs() < 1e-12);
    assert!((apply_standing_decrease(5.0, -20.0) - 2.0).abs() < 1e-12);
    assert!((apply_standing_decrease(5.0, 200.0) - (-10.0)).abs() < 1e-12);

    // compute_effective_standing: raw_standing >= 0 uses connections_level; < 0 uses diplomacy_level
    assert!((compute_effective_standing(0.0, 5, 0) - 2.0).abs() < 1e-12);
    assert!((compute_effective_standing(-5.0, 5, 5) - (-2.0)).abs() < 1e-12);
    assert!((compute_effective_standing(-5.0, 5, 0) - (-5.0)).abs() < 1e-12);

    assert_eq!(required_standing_for_agent_level(1), -1.99);
    assert_eq!(required_standing_for_agent_level(2), 1.0);
    assert_eq!(required_standing_for_agent_level(3), 3.0);
    assert_eq!(required_standing_for_agent_level(4), 5.0);
    assert_eq!(required_standing_for_agent_level(5), 7.0);
    assert_eq!(required_standing_for_agent_level(0), 7.0);

    assert_eq!(apply_security_tag_repair(2.0, 5, 0.0), 2.0);
    assert_eq!(apply_security_tag_repair(0.0, 5, 0.0), 0.0);
    assert!((apply_security_tag_repair(-2.0, 3, 0.0) - (-0.5)).abs() < 1e-12);
    assert!((apply_security_tag_repair(-1.0, 5, 0.0) - 0.0).abs() < 1e-12);

    // 6. Kinematics, propulsion, warp (long & short), jump fuel & fatigue
    let v_t = subwarp_velocity_at_time(300.0, 2.0, 1_000_000.0, 2.0);
    assert!((v_t - 300.0 * (1.0 - (-1.0_f64).exp())).abs() < 1e-9);
    let x_t = subwarp_distance_at_time(300.0, 2.0, 1_000_000.0, 2.0);
    assert!((x_t - 300.0 * 2.0 * (-1.0_f64).exp()).abs() < 1e-9);
    assert_eq!(align_time_ticks(0.0, 0.0), 1);

    let prop_v = propulsion_boosted_velocity(200.0, 500.0, 1_500_000.0, 1_000_000.0, 500_000.0);
    assert!((prop_v - 1_200.0).abs() < 1e-9);

    let warp_long = compute_warp_duration_seconds(10.0 * AU_METERS, 3.0, 300.0);
    let warp_short = compute_warp_duration_seconds(0.5 * AU_METERS, 3.0, 300.0);
    let warp_tiny = compute_warp_duration_seconds(100.0, 3.0, 300.0);
    assert!(warp_long > warp_short && warp_short >= 1.0 && warp_tiny >= 1.0);

    assert_eq!(compute_jump_fuel_isotopes(1_000.0, 5.0, 4, 3), 2_550);
    let (cd1, fat1) = compute_jump_fatigue_and_cooldown_minutes(200.0, 5.0, 0.5);
    assert!((cd1 - 20.0).abs() < 1e-9);
    assert!((fat1 - 300.0).abs() < 1e-9);
}

#[test]
fn dogma_thermodynamics_combat_and_weapons_exact_formulas() {
    // Dogma & recharge wrappers
    assert!((dogma_stacking_penalty(0) - 1.0).abs() < 1e-9);
    assert!(dogma_stacking_penalty(1) < 0.90);
    let rech = passive_capacitor_or_shield_recharge(250.0, 1_000.0, 100_000.0, 1_000.0);
    assert!(rech > 250.0 && rech <= 1_000.0);
    assert!((peak_passive_recharge_per_sec(1_000.0, 100.0) - 25.0).abs() < 1e-9);

    // Thermodynamics wrappers
    let heat = rack_heat_step(0.2, 0.1, 0.01, 1.0);
    assert!(heat > 0.2);
    let dmg_ch = slot_heat_damage_chance(0.8, 0, 0.5);
    assert!(dmg_ch > 0.0 && dmg_ch <= 1.0);
    assert!(nanite_repair_paste_needed(50.0, 0.5) > 0);

    // Booster, dbuff, abyssal wrappers
    assert!(booster_side_effect_check(0.5, 20.0, 0, 0, 0.1).is_some());
    assert!(booster_side_effect_check(0.2, 20.0, 5, 5, 0.99).is_none());
    assert!((command_burst_aggregate("Maximum", &[5.0, 12.0, 8.0]) - 12.0).abs() < 1e-9);
    let base_map = HashMap::from([(10u16, 100.0)]);
    let range_map = HashMap::from([(10u16, [0.9, 1.1])]);
    let roll_map = HashMap::from([(10u16, 0.5)]);
    let rolled = abyssal_attribute_roll(&base_map, &range_map, &roll_map);
    assert!((rolled[&10] - 100.0).abs() < 1e-6);

    // Layer EHP & crystals
    let ehp_zero_weights = compute_layer_ehp(1_000.0, [0.5; 4], [0.0; 4]);
    assert!(ehp_zero_weights > 1_000.0);
    let ehp_weighted = compute_layer_ehp(1_000.0, [0.2, 0.4, 0.6, 0.8], [1.0, 0.0, 0.0, 0.0]);
    assert!((ehp_weighted - 5_000.0).abs() < 1e-9);
    assert!((crystal_expected_cycles(2.0, 0.5, 0.1) - 40.0).abs() < 1e-9);

    // Targeting, turrets, missiles, breacher, entropic
    assert!(target_lock_time_sec(400.0, 125.0) > 0.0);
    let omega = turret_angular_velocity(
        DVec3::ZERO,
        DVec3::ZERO,
        DVec3::new(10_000.0, 0.0, 0.0),
        DVec3::new(0.0, 500.0, 0.0),
    );
    assert!((omega - 0.05).abs() < 1e-9);
    let cth = turret_chance_to_hit(0.01, 10.0, 40.0, 5_000.0, 10_000.0, 5_000.0);
    let cth_res = turret_chance_to_hit_with_res(0.01, 10.0, 40_000.0, 40.0, 5_000.0, 10_000.0, 5_000.0);
    assert!((cth - 0.5).abs() < 1e-12 && (cth - cth_res).abs() < 1e-12);
    let (shot_dmg, wrecking) = turret_shot_damage(DamageVector::new(10.0, 10.0, 10.0, 10.0), 1.0, 0.005);
    assert!(wrecking && shot_dmg.total() > 40.0);
    let mdf = missile_damage_factor(100.0, 100.0, 100.0, 100.0, 0.6);
    assert!((mdf - 1.0).abs() < 1e-9);
    assert!((breacher_pod_tick_damage(500.0, 0.01, 20_000.0) - 200.0).abs() < 1e-12);
    assert!((breacher_pod_tick_damage(100.0, 0.01, 20_000.0) - 100.0).abs() < 1e-12);
    assert!((entropic_spool_multiplier(10, 20, 0.05) - 1.5).abs() < 1e-9);

    // Vorton arc targets: deduplication, tie-breaking, exact boundary d == arc_range_m, cap
    let vorton = vorton_arc_targets_with_cap(
        1,
        DVec3::ZERO,
        &[
            (1, DVec3::new(100.0, 0.0, 0.0)),   // primary_id skipped
            (5, DVec3::new(5_000.0, 0.0, 0.0)), // tie at 5000m
            (3, DVec3::new(0.0, 5_000.0, 0.0)), // tie at 5000m (3 < 5)
            (3, DVec3::new(0.0, 5_000.0, 0.0)), // duplicate skipped
            (9, DVec3::new(10_000.0, 0.0, 0.0)), // exact boundary <= 10_000.0
            (10, DVec3::new(10_000.1, 0.0, 0.0)), // outside
        ],
        10_000.0,
        2,
    );
    assert_eq!(vorton, vec![1, 3, 5]);

    // ECM jam chance falloff branches
    assert!((compute_ecm_jam_chance(4.0, 10.0, 30_000.0, 20_000.0, 10_000.0) - 0.2).abs() < 1e-12);
    assert_eq!(compute_ecm_jam_chance(4.0, 10.0, 20_001.0, 20_000.0, 0.0), 0.0);
    assert!((compute_ecm_jam_chance(4.0, 10.0, 20_000.0, 20_000.0, 0.0) - 0.4).abs() < 1e-12);
}

#[test]
fn market_industry_pi_probes_missions_and_structures_exact_formulas() {
    // Broker fee with standings & floor clamp at 1.0%
    assert!((compute_broker_fee_rate(5, 10.0, 10.0) - 0.01).abs() < 1e-12);
    assert!((compute_broker_fee_rate(0, -5.0, -5.0) - 0.03).abs() < 1e-12);

    // Insurance reference value & 6 tiers
    let sde = SdeRegistry::with_canonical_seed();
    assert!(compute_ship_insurance_reference_value(&sde, 587) > 0.0);
    assert_eq!(compute_ship_insurance_reference_value(&sde, 999_999_999), 0.0);
    let levels = compute_insurance_levels(1_000.0);
    assert_eq!(levels[0], ("Basic", 50.0, 500.0));
    assert_eq!(levels[1], ("Standard", 100.0, 600.0));
    assert_eq!(levels[2], ("Bronze", 150.0, 700.0));
    assert_eq!(levels[3], ("Silver", 200.0, 800.0));
    assert_eq!(levels[4], ("Gold", 250.0, 900.0));
    assert_eq!(levels[5], ("Platinum", 300.0, 1_000.0));

    // Manufacturing material qty (base_qty == 1 branch + rig_bonus_pct) & manufacturing time
    assert_eq!(compute_manufacturing_material_qty(7, 1, 10, 5.0, 5.0), 7);
    assert_eq!(compute_manufacturing_material_qty(10, 100, 10, 1.0, 2.0), 874);
    let mfg_time = compute_manufacturing_time_seconds(2, 1_000, 20, 5, 5, 10.0);
    // 2 * 1000 * 0.80 * 0.80 * 0.85 * 0.90 = 979.2 -> ceil = 980
    assert_eq!(mfg_time, 980);

    // PI link cost, POCO export tax, and daily research points
    let (pg0, cpu0) = compute_pi_link_cost(0.0, 2);
    assert!((pg0 - 14.0).abs() < 1e-12);
    assert!((cpu0 - 21.0).abs() < 1e-12);
    // eff_npc = 0.10 * (1.0 - 0.10 * 4) = 0.06; player_tax = 0.05 -> 1000 * 0.11 = 110.0
    assert!((compute_poco_export_tax(1_000.0, 0.10, 4, 0.05) - 110.0).abs() < 1e-12);
    // 2.0 * (1.0 + 20.0 / 100.0) * (3 + 4)^2 = 2.0 * 1.2 * 49 = 117.6
    assert!((compute_daily_research_points(2.0, 20.0, 3, 4) - 117.6).abs() < 1e-12);

    // Probe scan signal out-of-range vs in-range
    assert_eq!(compute_probe_scan_signal_pct(40.0, 10.0, 1.0, 1.01), 0.0);
    let sig_mid = compute_probe_scan_signal_pct(10.0, 20.0, 1.0, 0.5);
    assert!(sig_mid > 0.0 && sig_mid < 100.0);

    // Upwell structure damage cap & living world fair price
    assert!((clamp_upwell_structure_damage(1_000.0, 5_000.0, 0.5) - 1_000.0).abs() < 1e-12);
    assert!((clamp_upwell_structure_damage(5_000.0, 5_000.0, 0.5) - 2_500.0).abs() < 1e-12);
    assert_eq!(clamp_upwell_structure_damage(-50.0, 5_000.0, 1.0), 0.0);
    assert!((compute_living_world_fair_price(100.0, 200.0, 0.1) - 115.0).abs() < 1e-12);
    assert!((compute_living_world_fair_price(500.0, 200.0, 5.0) - 1_500.0).abs() < 1e-12);
}
