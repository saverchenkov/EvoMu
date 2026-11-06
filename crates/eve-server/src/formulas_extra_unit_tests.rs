//! Deep branch-coverage and mutation-killing unit tests for `formulas` (`crates/eve-server/src/formulas.rs`).

#![forbid(unsafe_code)]

use crate::formulas::*;
use eve_destiny::{DamageVector, AU_METERS};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;

#[test]
fn skills_standings_and_security_repair_mutation_killers() {
    // #1: sp_for_skill_level
    assert_eq!(sp_for_skill_level(1.0, 0), 0.0);
    assert_eq!(sp_for_skill_level(16.0, 0), 0.0);
    assert_eq!(sp_for_skill_level(3.0, 1), 750.0);
    assert_eq!(sp_for_skill_level(3.0, 2), 4_243.0);
    assert_eq!(sp_for_skill_level(3.0, 3), 24_000.0);
    assert_eq!(sp_for_skill_level(3.0, 4), 135_765.0);
    assert_eq!(sp_for_skill_level(3.0, 5), 768_000.0);
    assert_eq!(sp_for_skill_level(3.0, 6), 768_000.0);

    // #2 & #3: sp_per_minute & skill_training_time_seconds
    assert!((sp_per_minute(27.0, 21.0, 1.0) - 37.5).abs() < 1e-12);
    assert!((sp_per_minute(20.0, 10.0, 0.5) - 12.5).abs() < 1e-12);
    // Exact equality target_sp == current_sp -> 0.0
    assert_eq!(
        skill_training_time_seconds(5_000.0, 5_000.0, 20.0, 20.0, 1.0),
        0.0
    );
    // Floor clamp on sp_min at 1e-6 when attributes/multiplier are 0
    let zero_rate_time = skill_training_time_seconds(100.0, 101.0, 0.0, 0.0, 1.0);
    assert!((zero_rate_time - 60_000_000.0).abs() < 1e-3);

    // #4: skill_injector_sp_yield exact threshold transitions
    assert_eq!(skill_injector_sp_yield(0.0), 500_000.0);
    assert_eq!(skill_injector_sp_yield(4_999_999.9), 500_000.0);
    assert_eq!(skill_injector_sp_yield(5_000_000.0), 400_000.0);
    assert_eq!(skill_injector_sp_yield(49_999_999.9), 400_000.0);
    assert_eq!(skill_injector_sp_yield(50_000_000.0), 300_000.0);
    assert_eq!(skill_injector_sp_yield(79_999_999.9), 300_000.0);
    assert_eq!(skill_injector_sp_yield(80_000_000.0), 150_000.0);
    assert_eq!(skill_injector_sp_yield(200_000_000.0), 150_000.0);

    // #5: clone_jump_cooldown_seconds for every skill level 0..=6
    assert_eq!(clone_jump_cooldown_seconds(0), 86_400);
    assert_eq!(clone_jump_cooldown_seconds(1), 82_800);
    assert_eq!(clone_jump_cooldown_seconds(2), 79_200);
    assert_eq!(clone_jump_cooldown_seconds(3), 75_600);
    assert_eq!(clone_jump_cooldown_seconds(4), 72_000);
    assert_eq!(clone_jump_cooldown_seconds(5), 68_400);
    assert_eq!(clone_jump_cooldown_seconds(6), 68_400);

    // #9-#10: apply_standing_increase & apply_standing_decrease
    assert!((apply_standing_increase(2.0, -50.0) - 2.0).abs() < 1e-12);
    assert!((apply_standing_increase(2.0, 25.0) - 4.0).abs() < 1e-12);
    assert!((apply_standing_increase(-5.0, 10.0) - (-3.5)).abs() < 1e-12);
    assert!((apply_standing_decrease(2.0, 0.0) - 2.0).abs() < 1e-12);
    assert!((apply_standing_decrease(2.0, 25.0) - (-1.0)).abs() < 1e-12);
    assert!((apply_standing_decrease(2.0, -25.0) - (-1.0)).abs() < 1e-12);

    // #11-#13: compute_effective_standing exact 0.0 boundary and level > 5 cap
    assert!((compute_effective_standing(0.0, 5, 0) - 2.0).abs() < 1e-12);
    assert!((compute_effective_standing(0.0, 0, 5) - 0.0).abs() < 1e-12);
    assert!((compute_effective_standing(0.0, 8, 0) - 2.0).abs() < 1e-12);
    assert!((compute_effective_standing(-1.0, 5, 0) - (-1.0)).abs() < 1e-12);
    assert!((compute_effective_standing(-1.0, 0, 5) - 1.2).abs() < 1e-12);
    assert!((compute_effective_standing(-1.0, 0, 8) - 1.2).abs() < 1e-12);

    // #15-#17: apply_security_tag_repair when current_sec > max_band_sec vs == vs <
    assert_eq!(apply_security_tag_repair(3.5, 4, 2.0), 3.5);
    assert_eq!(apply_security_tag_repair(2.0, 4, 2.0), 2.0);
    assert!((apply_security_tag_repair(-4.0, 3, -2.0) - (-2.5)).abs() < 1e-12);
    assert!((apply_security_tag_repair(-4.0, 6, -2.0) - (-2.0)).abs() < 1e-12);
}

#[test]
fn kinematics_warp_and_jump_drive_mutation_killers() {
    // #18-#20: subwarp velocity, distance, and align time
    assert_eq!(subwarp_velocity_at_time(400.0, 2.5, 1_000_000.0, 0.0), 0.0);
    assert_eq!(subwarp_distance_at_time(400.0, 2.5, 1_000_000.0, 0.0), 0.0);
    // tau = 4.0s at inertia=4.0, mass=1_000_000.0; at t=4.0s:
    let v4 = subwarp_velocity_at_time(500.0, 4.0, 1_000_000.0, 4.0);
    let x4 = subwarp_distance_at_time(500.0, 4.0, 1_000_000.0, 4.0);
    assert!((v4 - 500.0 * (1.0 - (-1.0_f64).exp())).abs() < 1e-9);
    assert!((x4 - 500.0 * 4.0 * (-1.0_f64).exp()).abs() < 1e-9);

    // Align time ceil boundary: -ln(0.25) = ln(4) = 1.38629436...
    // For tau = 2.0 / ln(4), align_time = 2.0 -> 2 ticks; slightly above -> 3 ticks.
    let exact_tau_mass = (2.0 / (-0.25_f64.ln())) * 1_000_000.0;
    assert_eq!(align_time_ticks(1.0, exact_tau_mass), 2);
    assert_eq!(align_time_ticks(1.0, exact_tau_mass + 100.0), 3);

    // #21: propulsion_boosted_velocity with zero/negative mass floor at 1.0
    let v_floor = propulsion_boosted_velocity(100.0, 100.0, 50.0, 0.0, 0.0);
    assert!((v_floor - 5_100.0).abs() < 1e-9);

    // #22-#26: compute_warp_duration_seconds k_a, k_d, v_drop clamps and d_accel + d_decel boundary
    // For warp_speed_au_s = 3.0 -> k_a = 3.0, k_d = 1.0, v_cruise = 3 * AU, d_accel = 1 AU, d_decel = 3 AU -> threshold = 4 AU.
    let d_thresh = 4.0 * AU_METERS;
    let t_at_thresh = compute_warp_duration_seconds(d_thresh, 3.0, 200.0);
    let t_above_thresh = compute_warp_duration_seconds(d_thresh + 3.0 * AU_METERS, 3.0, 200.0);
    // Cruise speed is 3 AU/s, so +3 AU adds exactly 1.0 second!
    assert!((t_above_thresh - t_at_thresh - 1.0).abs() < 1e-9);
    // Fast warp speed > 6.0 AU/s clamps k_d at 2.0
    let t_fast = compute_warp_duration_seconds(20.0 * AU_METERS, 9.0, 500.0);
    assert!(t_fast > 1.0 && t_fast < t_at_thresh);
    // Slow warp speed < 0.1 AU/s clamps k_a at 0.1 and k_d at 0.1
    let t_slow = compute_warp_duration_seconds(0.01 * AU_METERS, 0.01, 0.5);
    assert!(t_slow >= 1.0);

    // #143-#148: Jump fuel isotopes & jump fatigue/cooldown
    // Skill levels > 5 clamped to 5: jfc_mult = 0.5, jf_mult = 0.75 -> 1000 * 4 * 0.5 * 0.75 = 1500
    assert_eq!(compute_jump_fuel_isotopes(1_000.0, 4.0, 10, 10), 1_500);
    // Fractional ceil check: 100 * 1.0 * 0.9 * 0.95 = 85.5 -> 86
    assert_eq!(compute_jump_fuel_isotopes(100.0, 1.0, 1, 1), 86);

    // Fatigue reduction bonus clamped to [0.0, 0.95] -> min multiplier 0.05
    let (cd_95, fat_95) = compute_jump_fatigue_and_cooldown_minutes(0.0, 10.0, 0.99);
    // effective_ly = 10.0 * 0.05 = 0.5 -> cooldown = 1.5, new_fatigue = 10.0 * 1.5 = 15.0
    assert!((cd_95 - 1.5).abs() < 1e-9);
    assert!((fat_95 - 15.0).abs() < 1e-9);

    // Cooldown capped at 30.0 when current_fatigue_min = 300.0 and distance is huge
    let (cd_cap, fat_cap) = compute_jump_fatigue_and_cooldown_minutes(300.0, 50.0, -0.5);
    assert!((cd_cap - 30.0).abs() < 1e-9);
    assert!((fat_cap - 300.0).abs() < 1e-9);
}

#[test]
fn combat_vorton_ecm_market_industry_and_world_mutation_killers() {
    // #39-#42: compute_layer_ehp non-normalized weights & resonance clamping
    let ehp = compute_layer_ehp(1_200.0, [0.25, 0.75, 1.5, -0.5], [2.0, 2.0, 0.0, 0.0]);
    // Normalized weights: [0.5, 0.5, 0, 0], weighted_resonance = 0.5 * 0.25 + 0.5 * 0.75 = 0.5 -> 1200 / 0.5 = 2400
    assert!((ehp - 2_400.0).abs() < 1e-9);
    let ehp_invuln = compute_layer_ehp(1_000.0, [0.0, 0.0, 0.0, 0.0], [1.0, 1.0, 1.0, 1.0]);
    assert!((ehp_invuln - 1_000.0 / 1e-6).abs() < 1.0);
    assert!((crystal_expected_cycles(5.0, 0.0, 0.0) - 5.0 / 1e-6).abs() < 1.0);

    // #53: vorton_chain_targets hard cap at 9 additional targets (10 total)
    let mut many_candidates = Vec::new();
    for i in 2..=15 {
        many_candidates.push((i, DVec3::new((i as f64) * 100.0, 0.0, 0.0)));
    }
    let chain = vorton_chain_targets(1, DVec3::ZERO, &many_candidates, 10_000.0);
    assert_eq!(chain, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    let capped_12 = vorton_arc_targets_with_cap(1, DVec3::ZERO, &many_candidates, 10_000.0, 12);
    assert_eq!(capped_12.len(), 10);

    // #55-#65: ECM jam chance & fighter squadron damage
    assert!((compute_ecm_jam_chance(20.0, 5.0, 5_000.0, 10_000.0, 5_000.0) - 1.0).abs() < 1e-12);
    assert!((compute_ecm_jam_chance(5.0, 0.0, 5_000.0, 10_000.0, 5_000.0) - 1.0).abs() < 1e-12);
    assert!((compute_fighter_squadron_damage(120.0, 6, 1.5) - 1_080.0).abs() < 1e-9);

    // #66-#74: Broker fee, sales tax, insurance quote
    // Exact broker fee without hitting 1.0% floor: level 2 (0.6%), faction 5.0 (0.15%), corp 5.0 (0.10%)
    // 3.0 - 0.6 - 0.15 - 0.10 = 2.15% -> 0.0215
    assert!((compute_broker_fee_rate(2, 5.0, 5.0) - 0.0215).abs() < 1e-12);
    assert!((compute_sales_tax_rate(0) - 0.075).abs() < 1e-12);
    assert!((compute_sales_tax_rate(8) - 0.03375).abs() < 1e-12);
    let (ic0, ip0) = compute_insurance_quote(1_000.0, -1.0);
    assert!((ic0 - 0.0).abs() < 1e-12 && (ip0 - 400.0).abs() < 1e-12);
    let (ic1, ip1) = compute_insurance_quote(1_000.0, 2.0);
    assert!((ic1 - 300.0).abs() < 1e-12 && (ip1 - 1_000.0).abs() < 1e-12);

    // #75-#94: Manufacturing, invention, reprocessing
    // Material qty never drops below `runs` even with huge bonuses
    assert_eq!(compute_manufacturing_material_qty(15, 2, 10, 90.0, 90.0), 15);
    // Manufacturing time never drops below 1 second
    assert_eq!(compute_manufacturing_time_seconds(0, 100, 20, 5, 5, 0.0), 1);
    // Invention probability clamps to [0.0, 1.0] and caps skills at 5
    assert_eq!(compute_invention_probability(0.8, 8, 8, 8, 2.0), 1.0);
    assert_eq!(compute_invention_probability(0.5, 5, 5, 5, -1.0), 0.0);
    // Reprocessing yield rate exact product and clamp to [0.0, 1.0]
    let reproc = compute_reprocessing_yield_rate(0.50, 4, 3, 2, 0.02);
    let expected_reproc = 0.50 * 1.12 * 1.06 * 1.04 * 1.02;
    assert!((reproc - expected_reproc).abs() < 1e-12);
    assert_eq!(compute_reprocessing_yield_rate(0.90, 5, 5, 5, 0.50), 1.0);

    // #95-#108: PI link cost, POCO tax, R&D points, probe scan signal
    let (pg_d, cpu_d) = compute_pi_link_cost(10.0, 1);
    let d12 = 10.0_f64.powf(1.2);
    assert!((pg_d - (10.0 + 0.15 * d12) * 1.2).abs() < 1e-9);
    assert!((cpu_d - (15.0 + 0.20 * d12) * 1.2).abs() < 1e-9);
    assert_eq!(compute_poco_export_tax(1_000.0, -0.5, 5, -0.2), 0.0);
    assert!((compute_daily_research_points(1.5, 50.0, 4, 8) - 1.5 * 1.5 * 81.0).abs() < 1e-9);
    // Probe scan at exact boundary distance_from_probe_au == scan_range_au (dist_decay = 0.5)
    let sig_edge = compute_probe_scan_signal_pct(20.0, 40.0, 1.0, 1.0);
    // range_factor = (0.25 / 1.0)^0.5 = 0.5; dist_decay = 0.5; pct = (20/40) * 0.5 * 0.5 * 100 = 12.5
    assert!((sig_edge - 12.5).abs() < 1e-9);

    // #109-#161: Mission reward fleet split clamp (1..=10), DBS/ESS split clamp, structure damage cap
    let (isk_0, lp_0) = compute_mission_reward_with_skills(100_000, 200, 0, 0, 0);
    assert_eq!((isk_0, lp_0), (100_000, 200));
    let (isk_20, lp_20) = compute_mission_reward_with_skills(100_000, 200, 0, 0, 20);
    assert_eq!((isk_20, lp_20), (10_000, 20));

    let (pw_low, ct_low, mb_low, rb_low) = compute_dbs_and_ess_bounty_split(1_000.0, 0.10, -0.5);
    // dbs clamped to 0.35 -> gross = 350; direct_60 = 210; tax = 0; main = 105; reserve = 35
    assert!((pw_low - 210.0).abs() < 1e-9 && ct_low == 0.0);
    assert!((mb_low - 105.0).abs() < 1e-9 && (rb_low - 35.0).abs() < 1e-9);

    let (pw_high, ct_high, _, _) = compute_dbs_and_ess_bounty_split(1_000.0, 5.0, 2.0);
    // dbs clamped to 2.0 -> gross = 2000; direct_60 = 1200; tax clamped to 1.0 -> pw = 0, ct = 1200
    assert_eq!(pw_high, 0.0);
    assert!((ct_high - 1_200.0).abs() < 1e-9);

    // dt_sec clamped to min 0.1 in clamp_upwell_structure_damage
    assert!((clamp_upwell_structure_damage(9_000.0, 5_000.0, 0.0) - 500.0).abs() < 1e-9);

    let sde = SdeRegistry::with_canonical_seed();
    assert!(concord_response_delay_sec(0.9) > 0.0);
    let _ = compute_ship_insurance_reference_value(&sde, 606);
    let _ = turret_shot_damage(DamageVector::new(5.0, 5.0, 5.0, 5.0), 0.5, 0.25);
    let _ = HashMap::<u16, f64>::new();
}
