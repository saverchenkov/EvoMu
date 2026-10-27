//! `formulas` — Mathematical mechanics engine.
//!
//! Deterministic Rust implementations of game mechanics formulas (`#1` through `#161`).

#![forbid(unsafe_code)]

use eve_destiny::{combat, DamageVector, AU_METERS};
use eve_dogma::{
    aggregate_dbuff_values, compute_passive_recharge, evaluate_booster_side_effect,
    roll_abyssal_attributes, stacking_penalty_factor, thermodynamics,
};
use glam::DVec3;
use std::collections::HashMap;

// ============================================================================
// 1. Skills and Learning (#1 - #8)
// ============================================================================

/// #1: Skill Points required for `level` (`1..=5`) given `rank` (`1..=16`):
/// $$\text{SP}(L, R) = 250 \cdot R \cdot (\sqrt{32})^{L - 1}$$
pub fn sp_for_skill_level(rank: f64, level: u8) -> f64 {
    if level == 0 {
        return 0.0;
    }
    let l = level.min(5) as i32;
    (250.0 * rank * 2.0_f64.powf(2.5 * (l - 1) as f64)).ceil()
}

/// #2: SP per minute (`Omega = 1.0`, `Alpha = 0.5`):
/// $$\text{SP/min} = \left(A_{\text{primary}} + \frac{A_{\text{secondary}}}{2}\right) \cdot M_{\text{clone}}$$
pub fn sp_per_minute(primary_attr: f64, secondary_attr: f64, clone_multiplier: f64) -> f64 {
    (primary_attr + 0.5 * secondary_attr) * clone_multiplier
}

/// #3: Training time in seconds from `current_sp` to `target_sp`:
pub fn skill_training_time_seconds(
    current_sp: f64,
    target_sp: f64,
    primary_attr: f64,
    secondary_attr: f64,
    clone_multiplier: f64,
) -> f64 {
    let sp_min = sp_per_minute(primary_attr, secondary_attr, clone_multiplier).max(1e-6);
    ((target_sp - current_sp).max(0.0) / sp_min) * 60.0
}

/// #4: Large Skill Injector diminishing returns yield (`500k` -> `400k` -> `300k` -> `150k`):
pub fn skill_injector_sp_yield(character_total_sp: f64) -> f64 {
    if character_total_sp < 5_000_000.0 {
        500_000.0
    } else if character_total_sp < 50_000_000.0 {
        400_000.0
    } else if character_total_sp < 80_000_000.0 {
        300_000.0
    } else {
        150_000.0
    }
}

/// #5: Clone jump cooldown in seconds (`24h - 1h * Infomorph Synchronizing level`):
pub fn clone_jump_cooldown_seconds(infomorph_sync_level: u8) -> u64 {
    (24 - (infomorph_sync_level.min(5) as u64)) * 3600
}

// ============================================================================
// 2. NPC Standings & Security Status (#9 - #17)
// ============================================================================

/// #9: Positive standing gain asymptotic law (`S_new = S_old + (10 - S_old) * delta_pct`):
pub fn apply_standing_increase(current_standing: f64, pct_gain: f64) -> f64 {
    let delta = (pct_gain / 100.0).clamp(0.0, 1.0);
    (current_standing + (10.0 - current_standing) * delta).clamp(-10.0, 10.0)
}

/// #10: Negative standing penalty asymptotic law (`S_new = S_old + (-10 - S_old) * delta_pct`):
pub fn apply_standing_decrease(current_standing: f64, pct_loss: f64) -> f64 {
    let delta = (pct_loss.abs() / 100.0).clamp(0.0, 1.0);
    (current_standing + (-10.0 - current_standing) * delta).clamp(-10.0, 10.0)
}

/// #11-#13: Effective standing with Connections / Diplomacy / Criminal Connections (`4%/lvl`):
pub fn compute_effective_standing(
    raw_standing: f64,
    connections_level: u8,
    diplomacy_level: u8,
) -> f64 {
    if raw_standing >= 0.0 {
        raw_standing + (10.0 - raw_standing) * 0.04 * (connections_level.min(5) as f64)
    } else {
        raw_standing + (10.0 - raw_standing) * 0.04 * (diplomacy_level.min(5) as f64)
    }
}

/// #14: Agent quality / level access threshold (`L1 = -1.99`, `L2 = 1.0`, `L3 = 3.0`, `L4 = 5.0`, `L5 = 7.0`):
pub fn required_standing_for_agent_level(agent_level: u8) -> f64 {
    match agent_level {
        1 => -1.99,
        2 => 1.0,
        3 => 3.0,
        4 => 5.0,
        _ => 7.0,
    }
}

/// #15-#17: Security status repair via Clone Soldier Tags (`+0.5` sec status per tag up to band cap):
pub fn apply_security_tag_repair(current_sec: f64, tags_used: u32, max_band_sec: f64) -> f64 {
    if current_sec >= max_band_sec {
        return current_sec;
    }
    (current_sec + 0.5 * (tags_used as f64)).min(max_band_sec).clamp(-10.0, 10.0)
}

// ============================================================================
// 3. Kinematics, Velocity, Acceleration, Propulsion, Warp, & Jump Drives (#18 - #26, #143 - #148)
// ============================================================================

/// #18: Sub-warp exponential velocity $v(t) = v_{\max} (1 - e^{-t / \tau})$ where $\tau = I \cdot M / 10^6$:
pub fn subwarp_velocity_at_time(v_max: f64, inertia: f64, mass_kg: f64, t_sec: f64) -> f64 {
    let tau = ((inertia * mass_kg) / 1_000_000.0).max(1e-6);
    v_max * (1.0 - (-t_sec / tau).exp())
}

/// #19: Sub-warp distance traveled $x(t) = v_{\max} (t - \tau (1 - e^{-t / \tau}))$:
pub fn subwarp_distance_at_time(v_max: f64, inertia: f64, mass_kg: f64, t_sec: f64) -> f64 {
    let tau = ((inertia * mass_kg) / 1_000_000.0).max(1e-6);
    v_max * (t_sec - tau * (1.0 - (-t_sec / tau).exp()))
}

/// #20: Align time to warp in seconds ($t_{\text{align}} = \lceil -\ln(0.25) \cdot I \cdot M / 10^6 \rceil$):
pub fn align_time_ticks(inertia: f64, mass_kg: f64) -> u64 {
    let tau = (inertia * mass_kg) / 1_000_000.0;
    (-0.25_f64.ln() * tau).ceil().max(1.0) as u64
}

/// #21: Propulsion module (AB/MWD) thrust-to-mass velocity boost:
/// $$v_{\text{new}} = v_{\text{base}} \cdot \left(1 + \frac{B_{\text{speed}}}{100} \cdot \frac{F_{\text{thrust}}}{M_{\text{base}} + M_{\text{add}}}\right)$$
pub fn propulsion_boosted_velocity(
    base_velocity: f64,
    speed_factor_pct: f64,
    thrust_newtons: f64,
    base_mass_kg: f64,
    prop_mass_add_kg: f64,
) -> f64 {
    let total_mass = (base_mass_kg + prop_mass_add_kg).max(1.0);
    base_velocity * (1.0 + (speed_factor_pct / 100.0) * (thrust_newtons / total_mass))
}

/// #22-#26: Total warp duration (seconds) across acceleration, cruise, and deceleration phases:
pub fn compute_warp_duration_seconds(
    distance_meters: f64,
    warp_speed_au_s: f64,
    subwarp_max_speed: f64,
) -> f64 {
    let k_a = warp_speed_au_s.max(0.1);
    let k_d = (k_a / 3.0).clamp(0.1, 2.0);
    let v_cruise = k_a * AU_METERS;
    let v_drop = (subwarp_max_speed * 0.5).clamp(1.0, 100.0);
    let d_accel = AU_METERS;
    let d_decel = v_cruise / k_d;

    if distance_meters >= d_accel + d_decel {
        let t_accel = (v_cruise / k_a).ln() / k_a;
        let t_decel = (v_cruise / v_drop).ln() / k_d;
        let t_cruise = (distance_meters - (d_accel + d_decel)) / v_cruise;
        t_accel + t_cruise + t_decel
    } else {
        let v_peak = (distance_meters * k_a * k_d / (k_a + k_d)).max(v_drop * 1.1);
        let t_accel = (v_peak / k_a).ln() / k_a;
        let t_decel = (v_peak / v_drop).ln() / k_d;
        (t_accel + t_decel).max(1.0)
    }
}

/// #143-#145: Capital Jump Drive Fuel consumption (`base_isotopes * dist_ly * (1 - 0.1 * JFC_lvl) * (1 - 0.05 * JF_lvl)`):
pub fn compute_jump_fuel_isotopes(
    base_isotopes_per_ly: f64,
    distance_ly: f64,
    jump_fuel_conservation_level: u8,
    jump_freighter_level: u8,
) -> u64 {
    let jfc_mult = 1.0 - 0.10 * (jump_fuel_conservation_level.min(5) as f64);
    let jf_mult = 1.0 - 0.05 * (jump_freighter_level.min(5) as f64);
    (base_isotopes_per_ly * distance_ly * jfc_mult * jf_mult).ceil() as u64
}

/// #146-#148: Jump Fatigue & Jump Activation Cooldown (`cooldown = 1 + dist_ly * (1 - 0.09 * JDC_lvl)` minutes, capped at 5h fatigue):
// parity: feat:ships.jump-drive-fatigue-accumulation
pub fn compute_jump_fatigue_and_cooldown_minutes(
    current_fatigue_min: f64,
    distance_ly: f64,
    fatigue_reduction_bonus: f64,
) -> (f64, f64) {
    let ship_fatigue_multiplier = (1.0 - fatigue_reduction_bonus.clamp(0.0, 0.95)).max(0.05);
    let effective_ly = distance_ly * ship_fatigue_multiplier;
    let cooldown_min = (1.0 + effective_ly).max(current_fatigue_min / 10.0);
    let new_fatigue_min = (current_fatigue_min.max(10.0) * (1.0 + effective_ly)).min(300.0);
    (cooldown_min, new_fatigue_min)
}

// ============================================================================
// 4. Dogma, Capacitor, Tanking, Overheating, Crystals, Boosters, Dbuffs (#27 - #42)
// ============================================================================

/// #27-#28: Re-export stacking penalty and passive recharge from `eve-dogma`:
pub fn dogma_stacking_penalty(index: usize) -> f64 {
    stacking_penalty_factor(index)
}

pub fn passive_capacitor_or_shield_recharge(
    current: f64,
    max_capacity: f64,
    recharge_time_ms: f64,
    dt_ms: f64,
) -> f64 {
    compute_passive_recharge(current, max_capacity, recharge_time_ms, dt_ms)
}

/// Peak passive recharge rate ($\text{Peak} = 2.5 \cdot C_{\max} / \tau_{\text{sec}}$ at $25\%$ capacity):
pub fn peak_passive_recharge_per_sec(max_capacity: f64, recharge_time_sec: f64) -> f64 {
    2.5 * max_capacity / recharge_time_sec.max(1e-3)
}

/// #29-#34: Thermodynamics wrappers:
pub fn rack_heat_step(current_heat: f64, heat_gen: f64, dissipation: f64, dt_sec: f64) -> f64 {
    thermodynamics::step_rack_heat(current_heat, heat_gen, dissipation, dt_sec)
}

pub fn slot_heat_damage_chance(rack_heat: f64, slot_dist: usize, attenuation: f64) -> f64 {
    thermodynamics::heat_damage_probability(rack_heat, slot_dist, attenuation)
}

pub fn nanite_repair_paste_needed(damaged_hp: f64, paste_per_hp: f64) -> u32 {
    thermodynamics::nanite_paste_required(damaged_hp, paste_per_hp)
}

/// #35-#38: Booster, Dbuff, and Abyssal mutaplasmid wrappers:
pub fn booster_side_effect_check(
    base_chance: f64,
    base_penalty_pct: f64,
    neurotoxin_control_level: u8,
    neurotoxin_recovery_level: u8,
    roll_u: f64,
) -> Option<f64> {
    evaluate_booster_side_effect(
        base_chance,
        base_penalty_pct,
        neurotoxin_control_level,
        neurotoxin_recovery_level,
        roll_u,
    )
}

pub fn command_burst_aggregate(mode: &str, values: &[f64]) -> f64 {
    aggregate_dbuff_values(mode, values)
}

pub fn abyssal_attribute_roll(
    base: &HashMap<u16, f64>,
    ranges: &HashMap<u16, [f64; 2]>,
    rolls: &HashMap<u16, f64>,
) -> HashMap<u16, f64> {
    roll_abyssal_attributes(base, ranges, rolls)
}

/// #39-#41: Effective Hit Points (EHP) for a layer given 4-channel resonances (`1 - resist`) and incoming damage profile:
pub fn compute_layer_ehp(
    raw_hp: f64,
    resonances: [f64; 4],
    damage_profile_weights: [f64; 4],
) -> f64 {
    let weight_sum: f64 = damage_profile_weights.iter().sum();
    let norm = if weight_sum > 0.0 { weight_sum } else { 1.0 };
    let weighted_resonance: f64 = resonances
        .iter()
        .zip(damage_profile_weights.iter())
        .map(|(res, w)| res.clamp(0.0, 1.0) * (w / norm))
        .sum();
    raw_hp / weighted_resonance.max(1e-6)
}

/// #42: Crystal volatility damage per cycle (`expected_cycles = crystal_hp / (volatility * volatility_dmg)`):
pub fn crystal_expected_cycles(
    crystal_hp: f64,
    volatility_chance: f64,
    volatility_damage: f64,
) -> f64 {
    crystal_hp / (volatility_chance * volatility_damage).max(1e-6)
}

// ============================================================================
// 5. Targeting, Turrets, Missiles, EWAR, Drones, & Fighters (#43 - #65)
// ============================================================================

/// #43-#45: Target lock time & max locked targets:
pub fn target_lock_time_sec(scan_res: f64, sig_radius: f64) -> f64 {
    combat::compute_lock_time_seconds(scan_res, sig_radius)
}

/// #46-#49: Turret angular velocity, ChanceToHit, and Wrecking Shot damage resolution:
pub fn turret_angular_velocity(
    attacker_pos: DVec3,
    attacker_vel: DVec3,
    target_pos: DVec3,
    target_vel: DVec3,
) -> f64 {
    combat::compute_angular_velocity(attacker_pos, attacker_vel, target_pos, target_vel)
}

pub fn turret_chance_to_hit(
    omega: f64,
    tracking: f64,
    sig_radius: f64,
    distance: f64,
    optimal: f64,
    falloff: f64,
) -> f64 {
    combat::compute_turret_chance_to_hit(omega, tracking, sig_radius, distance, optimal, falloff)
}

pub fn turret_chance_to_hit_with_res(
    omega: f64,
    tracking: f64,
    sig_resolution: f64,
    sig_radius: f64,
    distance: f64,
    optimal: f64,
    falloff: f64,
) -> f64 {
    combat::compute_turret_chance_to_hit_with_res(
        omega,
        tracking,
        sig_resolution,
        sig_radius,
        distance,
        optimal,
        falloff,
    )
}

pub fn turret_shot_damage(
    base_damage: DamageVector,
    chance_to_hit: f64,
    roll_u: f64,
) -> (DamageVector, bool) {
    combat::resolve_turret_shot(base_damage, chance_to_hit, roll_u)
}

/// #50-#51: Missile damage application factor:
pub fn missile_damage_factor(
    target_sig: f64,
    target_vel: f64,
    explosion_radius: f64,
    explosion_velocity: f64,
    drf: f64,
) -> f64 {
    combat::compute_missile_damage_factor(
        target_sig,
        target_vel,
        explosion_radius,
        explosion_velocity,
        drf,
    )
}

/// #52: Breacher Pod DoT per tick ($\min(D_{\text{flat}}, f_{\text{pct}} \cdot \text{HP}_{\max})$):
pub fn breacher_pod_tick_damage(flat_dps: f64, pct_max_hp: f64, target_total_max_hp: f64) -> f64 {
    flat_dps.min(pct_max_hp * target_total_max_hp)
}

/// #53: Vorton Projector arc targets (up to 9 additional targets within `arc_range_m` of `primary_pos`, with no secondary hopping):
pub fn vorton_chain_targets(
    primary_id: i64,
    primary_pos: DVec3,
    candidates: &[(i64, DVec3)],
    arc_range_m: f64,
) -> Vec<i64> {
    vorton_arc_targets_with_cap(primary_id, primary_pos, candidates, arc_range_m, 9)
}

pub fn vorton_arc_targets_with_cap(
    primary_id: i64,
    primary_pos: DVec3,
    candidates: &[(i64, DVec3)],
    arc_range_m: f64,
    max_additional_targets: usize,
) -> Vec<i64> {
    let mut hit = vec![primary_id];
    let mut in_arc: Vec<(i64, f64)> = Vec::new();
    for &(cid, cpos) in candidates {
        if cid == primary_id || in_arc.iter().any(|&(id, _)| id == cid) {
            continue;
        }
        let d = (cpos - primary_pos).length();
        if d <= arc_range_m {
            in_arc.push((cid, d));
        }
    }
    in_arc.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    for (cid, _) in in_arc.into_iter().take(max_additional_targets.min(9)) {
        hit.push(cid);
    }
    hit
}

/// #54: Entropic Disintegrator spool multiplier:
pub fn entropic_spool_multiplier(cycles: u32, max_cycles: u32, step_bonus: f64) -> f64 {
    combat::compute_entropic_spool_multiplier(cycles, max_cycles, step_bonus)
}

/// #55-#57: ECM Jam Chance ($P_{\text{jam}} = \min(1.0, (S_{\text{jam}} / S_{\text{sensor}}) \cdot 0.5^{(\max(0, d - R_{\text{opt}}) / R_{\text{falloff}})^2})$):
pub fn compute_ecm_jam_chance(
    jam_strength: f64,
    target_sensor_strength: f64,
    distance: f64,
    optimal: f64,
    falloff: f64,
) -> f64 {
    let excess = (distance - optimal).max(0.0);
    let falloff_factor = if falloff > 0.0 {
        0.5_f64.powf((excess / falloff).powi(2))
    } else if excess > 0.0 {
        0.0
    } else {
        1.0
    };
    ((jam_strength / target_sensor_strength.max(1e-6)) * falloff_factor).clamp(0.0, 1.0)
}

/// #58-#65: Drone bandwidth validation & Fighter squadron volley damage:
pub fn compute_fighter_squadron_damage(
    per_fighter_damage: f64,
    active_squadron_size: u32,
    ability_multiplier: f64,
) -> f64 {
    per_fighter_damage * (active_squadron_size as f64) * ability_multiplier
}

// ============================================================================
// 6. Market, Tax, Insurance, Industry, Reprocessing, & PI (#66 - #104)
// ============================================================================

/// #66-#70: Broker Fee & Sales Tax with Broker Relations, Accounting, and Faction/Corp Standings:
/// - Broker Fee: $\max(1.0\%, 3.0\% - 0.3\% \cdot L_{\text{Broker}} - 0.03\% \cdot S_{\text{faction}} - 0.02\% \cdot S_{\text{corp}})$
/// - Sales Tax: $7.5\% \cdot (1 - 0.11 \cdot L_{\text{Accounting}})$
pub fn compute_broker_fee_rate(
    broker_relations_level: u8,
    faction_standing: f64,
    corp_standing: f64,
) -> f64 {
    let rate_pct = 3.0
        - 0.3 * (broker_relations_level.min(5) as f64)
        - 0.03 * faction_standing.max(0.0)
        - 0.02 * corp_standing.max(0.0);
    (rate_pct.max(1.0)) / 100.0
}

pub fn compute_sales_tax_rate(accounting_level: u8) -> f64 {
    0.075 * (1.0 - 0.11 * (accounting_level.min(5) as f64))
}

/// #71-#74: Ship Insurance Platinum cost (`30%` of base value) & payout (`100%` of base value):
pub fn compute_insurance_quote(base_ship_price: f64, level_fraction: f64) -> (f64, f64) {
    let f = level_fraction.clamp(0.0, 1.0);
    let cost = base_ship_price * 0.30 * f;
    let payout = base_ship_price * (0.40 + 0.60 * f);
    (cost, payout)
}

/// DERIVED: Returns the ship insurance reference value from the SDE hull `base_price`
/// per `evemu_Crucible/src/eve-server/station/InsuranceService.cpp:65-67, 117-150`
/// (CCP's live per-hull insurance multipliers are server-secret per `wiki:Insurance.md:56`).
pub fn compute_ship_insurance_reference_value(
    sde: &eve_sde::SdeRegistry,
    ship_type_id: eve_sde::TypeId,
) -> f64 {
    sde.types
        .get(&ship_type_id)
        .map(|t| t.base_price.max(0.0))
        .unwrap_or(0.0)
}

/// Returns the 6 standard paid insurance tiers (`Basic`, `Standard`, `Bronze`, `Silver`, `Gold`, `Platinum`)
/// computed via [`compute_insurance_quote`] for a ship's reference value (`wiki:Insurance`).
pub fn compute_insurance_levels(reference_value: f64) -> [(&'static str, f64, f64); 6] {
    [
        ("Basic", (reference_value * 0.05 * 100.0).round() / 100.0, (reference_value * 0.50 * 100.0).round() / 100.0),
        ("Standard", (reference_value * 0.10 * 100.0).round() / 100.0, (reference_value * 0.60 * 100.0).round() / 100.0),
        ("Bronze", (reference_value * 0.15 * 100.0).round() / 100.0, (reference_value * 0.70 * 100.0).round() / 100.0),
        ("Silver", (reference_value * 0.20 * 100.0).round() / 100.0, (reference_value * 0.80 * 100.0).round() / 100.0),
        ("Gold", (reference_value * 0.25 * 100.0).round() / 100.0, (reference_value * 0.90 * 100.0).round() / 100.0),
        ("Platinum", (reference_value * 0.30 * 100.0).round() / 100.0, (reference_value * 1.00 * 100.0).round() / 100.0),
    ]
}

/// #75-#82: Manufacturing Material Efficiency (ME `0..10`) & Time Efficiency (TE `0..20`):
/// $$M_{\text{req}} = \max\left(\text{runs}, \left\lceil \text{runs} \cdot M_{\text{base}} \cdot \left(1 - \frac{\text{ME}}{100}\right) \cdot (1 - f_{\text{structure}}) \right\rceil\right)$$
pub fn compute_manufacturing_material_qty(
    runs: u32,
    base_qty: u64,
    me_level: u8,
    structure_role_bonus_pct: f64,
    rig_bonus_pct: f64,
) -> u64 {
    if base_qty == 1 {
        return runs as u64;
    }
    let me_mult = 1.0 - (me_level.min(10) as f64) / 100.0;
    let struct_mult = 1.0 - (structure_role_bonus_pct / 100.0);
    let rig_mult = 1.0 - (rig_bonus_pct / 100.0);
    let raw = (runs as f64) * (base_qty as f64) * me_mult * struct_mult * rig_mult;
    (raw.ceil() as u64).max(runs as u64)
}

pub fn compute_manufacturing_time_seconds(
    runs: u32,
    base_time_sec: u64,
    te_level: u8,
    industry_level: u8,
    adv_industry_level: u8,
    structure_time_bonus_pct: f64,
) -> u64 {
    let te_mult = 1.0 - (te_level.min(20) as f64) / 100.0;
    let ind_mult = 1.0 - 0.04 * (industry_level.min(5) as f64);
    let adv_mult = 1.0 - 0.03 * (adv_industry_level.min(5) as f64);
    let struct_mult = 1.0 - (structure_time_bonus_pct / 100.0);
    let raw =
        (runs as f64) * (base_time_sec as f64) * te_mult * ind_mult * adv_mult * struct_mult;
    raw.ceil().max(1.0) as u64
}

/// #83-#88: T2 Invention Success Probability:
/// $$P_{\text{inv}} = P_{\text{base}} \cdot \left(1 + \frac{L_{\text{d1}} + L_{\text{d2}}}{30} + \frac{L_{\text{enc}}}{40}\right) \cdot M_{\text{decryptor}}$$
pub fn compute_invention_probability(
    base_probability: f64,
    science_skill_1: u8,
    science_skill_2: u8,
    encryption_skill: u8,
    decryptor_prob_multiplier: f64,
) -> f64 {
    let skill_term = 1.0
        + ((science_skill_1.min(5) + science_skill_2.min(5)) as f64) / 30.0
        + (encryption_skill.min(5) as f64) / 40.0;
    (base_probability * skill_term * decryptor_prob_multiplier).clamp(0.0, 1.0)
}

/// #89-#94: Ore Reprocessing Yield (`base_yield * (1 + 0.03 * Reproc) * (1 + 0.02 * ReprocEff) * (1 + 0.02 * OreSkill) * (1 + implant)`):
pub fn compute_reprocessing_yield_rate(
    station_base_yield: f64,
    reprocessing_level: u8,
    reprocessing_efficiency_level: u8,
    ore_processing_level: u8,
    implant_bonus: f64,
) -> f64 {
    let r1 = 1.0 + 0.03 * (reprocessing_level.min(5) as f64);
    let r2 = 1.0 + 0.02 * (reprocessing_efficiency_level.min(5) as f64);
    let r3 = 1.0 + 0.02 * (ore_processing_level.min(5) as f64);
    let r4 = 1.0 + implant_bonus;
    (station_base_yield * r1 * r2 * r3 * r4).clamp(0.0, 1.0)
}

/// #95-#100: Planetary Interaction (PI) link CPU/Powergrid & POCO Customs Tax:
/// - Link Powergrid: $10 + 0.15 \cdot d_{\text{km}}^{1.2}$
/// - Link CPU: $15 + 0.2 \cdot d_{\text{km}}^{1.2}$
/// - NPC Customs Tax: $\max(0, 10\% \cdot (1 - 0.1 \cdot L_{\text{CustomsCodeExpertise}}))$
pub fn compute_pi_link_cost(distance_km: f64, upgrade_level: u8) -> (f64, f64) {
    let d_term = distance_km.max(0.0).powf(1.2);
    let mult = 1.0 + 0.2 * (upgrade_level as f64);
    let pg = (10.0 + 0.15 * d_term) * mult;
    let cpu = (15.0 + 0.20 * d_term) * mult;
    (pg, cpu)
}

pub fn compute_poco_export_tax(
    base_value_isk: f64,
    npc_tax_rate: f64,
    customs_code_expertise_level: u8,
    player_tax_rate: f64,
) -> f64 {
    let eff_npc = npc_tax_rate * (1.0 - 0.10 * (customs_code_expertise_level.min(5) as f64));
    base_value_isk * (eff_npc.max(0.0) + player_tax_rate.max(0.0))
}

/// #101-#104: R&D Datacore Daily Research Points ($\text{RP/day} = R_{\text{base}} \cdot (1 + \frac{Q_{\text{eff}}}{100}) \cdot (L_{\text{agent}} + L_{\text{skill}})^2$):
pub fn compute_daily_research_points(
    base_multiplier: f64,
    effective_quality: f64,
    agent_level: u8,
    player_skill_level: u8,
) -> f64 {
    let q_term = 1.0 + effective_quality / 100.0;
    let l_sum = (agent_level as f64) + (player_skill_level.min(5) as f64);
    base_multiplier * q_term * l_sum.powi(2)
}

// ============================================================================
// 7. Probe Scanning, Wormholes, Missions, FW, CrimeWatch, & Structures (#105 - #161)
// ============================================================================

/// #105-#108: Astrometric Probe Scan Signal Gain:
pub fn compute_probe_scan_signal_pct(
    probe_strength: f64,
    signature_strength: f64,
    scan_range_au: f64,
    distance_from_probe_au: f64,
) -> f64 {
    if distance_from_probe_au > scan_range_au {
        return 0.0;
    }
    let range_factor = (0.25 / scan_range_au.max(0.25)).powf(0.5);
    let dist_decay = 1.0 - 0.5 * (distance_from_probe_au / scan_range_au.max(0.25)).powi(2);
    let pct = (probe_strength / signature_strength.max(1.0)) * range_factor * dist_decay * 100.0;
    pct.clamp(0.0, 100.0)
}

/// #109-#119: Mission ISK/LP/Standing Fleet Split (shared equally up to 10 fleet members) & Negotiation (`5%/lvl`):
pub fn compute_mission_reward_with_skills(
    base_isk: i64,
    base_lp: i64,
    negotiation_level: u8,
    security_connections_level: u8,
    fleet_size: usize,
) -> (i64, i64) {
    let count = fleet_size.clamp(1, 10) as i64;
    let total_isk =
        ((base_isk as f64) * (1.0 + 0.05 * (negotiation_level.min(5) as f64))).round() as i64;
    let total_lp = ((base_lp as f64) * (1.0 + 0.10 * (security_connections_level.min(5) as f64)))
        .round() as i64;
    (total_isk / count, total_lp / count)
}

/// #120-#131: Faction Warfare Complex LP Payout & Dynamic Bounty System (DBS) / ESS split:
pub fn compute_dbs_and_ess_bounty_split(
    base_bounty_isk: f64,
    dbs_multiplier: f64,
    corp_tax_rate: f64,
) -> (f64, f64, f64, f64) {
    let gross = base_bounty_isk * dbs_multiplier.clamp(0.35, 2.00);
    let direct_60 = gross * 0.60;
    let corp_tax = direct_60 * corp_tax_rate.clamp(0.0, 1.0);
    let pilot_wallet = direct_60 - corp_tax;
    let ess_main_bank = gross * 0.30;
    let ess_reserve_bank = gross * 0.10;
    (pilot_wallet, corp_tax, ess_main_bank, ess_reserve_bank)
}

/// #132-#138: CrimeWatch CONCORD response delay:
pub fn concord_response_delay_sec(security_status: f64) -> f64 {
    combat::concord_response_seconds(security_status)
}

/// #139-#142: Upwell Structure DPS Damage Cap (clamps incoming DPS over a tick to `damage_cap_dps`):
pub fn clamp_upwell_structure_damage(incoming_damage: f64, damage_cap_dps: f64, dt_sec: f64) -> f64 {
    let max_tick_dmg = damage_cap_dps * dt_sec.max(0.1);
    incoming_damage.min(max_tick_dmg).max(0.0)
}

/// #149-#161: Living World Dual-Anchor Market Fair Value & Convoy Cargo Risk Valuation:
/// $$P_{\text{fair}}(T) = \max\left(P_{\text{base}}(T), \sum_{i} q_i \cdot P_{\text{mineral}}(m_i) \cdot 1.15\right)$$
pub fn compute_living_world_fair_price(
    sde_base_price: f64,
    bom_mineral_cost: f64,
    regional_scarcity_multiplier: f64,
) -> f64 {
    sde_base_price.max(bom_mineral_cost * 1.15) * regional_scarcity_multiplier.clamp(0.5, 3.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mechanics_formulas() {
        // 1. Skills & Learning (#1-#8)
        assert_eq!(sp_for_skill_level(1.0, 1), 250.0);
        assert_eq!(sp_for_skill_level(1.0, 5), 256_000.0);
        assert!((sp_per_minute(20.0, 20.0, 1.0) - 30.0).abs() < 1e-6);
        assert_eq!(skill_injector_sp_yield(4_000_000.0), 500_000.0);
        assert_eq!(skill_injector_sp_yield(90_000_000.0), 150_000.0);
        assert_eq!(clone_jump_cooldown_seconds(5), 19 * 3600);

        // 2. Standings & Security Status (#9-#17)
        assert!((apply_standing_increase(0.0, 10.0) - 1.0).abs() < 1e-6);
        assert!((compute_effective_standing(5.0, 5, 0) - 6.0).abs() < 1e-6);
        assert!((apply_security_tag_repair(-2.0, 2, 0.0) - (-1.0)).abs() < 1e-6);

        // 3. Kinematics, Warp, & Jump Drives (#18-#26, #143-#148)
        assert_eq!(align_time_ticks(3.45, 1_067_000.0), 6);
        assert!(compute_warp_duration_seconds(10.0 * AU_METERS, 3.0, 365.0) > 10.0);
        assert_eq!(compute_jump_fuel_isotopes(1000.0, 5.0, 5, 0), 2500);
        let (cd, fat) = compute_jump_fatigue_and_cooldown_minutes(0.0, 5.0, 0.0);
        assert_eq!(cd, 6.0);
        assert_eq!(fat, 60.0);

        // 4. Dogma, Tanking, Crystals (#27-#42)
        let ehp = compute_layer_ehp(1000.0, [0.5, 0.5, 0.5, 0.5], [0.25, 0.25, 0.25, 0.25]);
        assert!((ehp - 2000.0).abs() < 1e-6);
        assert!((crystal_expected_cycles(1.0, 0.1, 0.01) - 1000.0).abs() < 1e-6);

        // 5. Combat, Vorton, ECM, Fighters (#43-#65)
        let vorton = vorton_chain_targets(
            1,
            DVec3::ZERO,
            &[
                (2, DVec3::new(8_000.0, 0.0, 0.0)),  // 8 km from primary (<= 10 km -> hit)
                (3, DVec3::new(12_000.0, 0.0, 0.0)), // 12 km from primary (> 10 km, even though 4 km from #2 -> NOT hit, no hopping)
            ],
            10_000.0,
        );
        assert_eq!(vorton, vec![1, 2]);
        assert!((compute_ecm_jam_chance(5.0, 10.0, 10_000.0, 20_000.0, 20_000.0) - 0.5).abs() < 1e-6);
        assert_eq!(compute_fighter_squadron_damage(50.0, 9, 2.0), 900.0);

        // 6. Market, Tax, Insurance, Industry, Reprocessing, PI (#66-#104)
        assert!((compute_broker_fee_rate(5, 0.0, 0.0) - 0.015).abs() < 1e-6);
        assert!((compute_sales_tax_rate(5) - 0.03375).abs() < 1e-6);
        let (ins_cost, ins_pay) = compute_insurance_quote(1_000_000.0, 1.0);
        assert_eq!(ins_cost, 300_000.0);
        assert_eq!(ins_pay, 1_000_000.0);
        assert_eq!(compute_manufacturing_material_qty(10, 100, 10, 1.0, 0.0), 891);
        assert!((compute_invention_probability(0.30, 5, 5, 5, 1.0) - 0.30 * (1.0 + 10.0 / 30.0 + 5.0 / 40.0)).abs() < 1e-6);
        assert!(compute_reprocessing_yield_rate(0.50, 5, 5, 5, 0.04) > 0.65);
        let (pg, cpu) = compute_pi_link_cost(100.0, 0);
        assert!(pg > 10.0 && cpu > 15.0);

        // 7. Probes, Missions, DBS/ESS, CrimeWatch, Upwell, Living World (#105-#161)
        assert!(compute_probe_scan_signal_pct(40.0, 10.0, 0.25, 0.0) >= 100.0);
        let (isk_split, lp_split) = compute_mission_reward_with_skills(100_000, 100, 5, 5, 2);
        assert_eq!(isk_split, 62_500);
        assert_eq!(lp_split, 75);
        let (pilot, tax, main_b, res_b) = compute_dbs_and_ess_bounty_split(100_000.0, 1.5, 0.10);
        assert!((pilot - 81_000.0).abs() < 1e-6);
        assert!((tax - 9_000.0).abs() < 1e-6);
        assert!((main_b - 45_000.0).abs() < 1e-6);
        assert!((res_b - 15_000.0).abs() < 1e-6);
        assert_eq!(concord_response_delay_sec(1.0), 6.0);
        assert_eq!(clamp_upwell_structure_damage(20_000.0, 5_000.0, 1.0), 5_000.0);
        assert!(compute_living_world_fair_price(230_000.0, 250_000.0, 1.0) >= 287_500.0);
    }
}
