use crate::*;
use eve_sde::{AttributeId, SdeRegistry};
use serde::{Deserialize, Serialize};

/// Effective attributes resulting from propulsion module (Afterburner or MWD) activation (`DG-1`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PropulsionModuleStats {
    pub max_velocity: f64,
    pub mass: f64,
    pub signature_radius: f64,
    pub speed_factor: f64,
    pub mass_addition: f64,
    pub signature_radius_bonus: f64,
    pub is_overloaded: bool,
    pub is_scrambled: bool,
    pub can_activate: bool,
}

/// Evaluates propulsion module (Afterburner / MWD) activation, mass increase, and signature radius bloom (`DG-1`).
pub fn eval_propulsion_module(
    ship: &DogmaItemState,
    module: &DogmaItemState,
    is_active: bool,
    is_overloaded: bool,
    is_scrambled: bool,
    sde: &SdeRegistry,
) -> PropulsionModuleStats {
    let base_vel = ship.get_attribute(attr_ids::MAX_VELOCITY, sde);
    let base_mass = ship.get_attribute(attr_ids::MASS, sde);
    let base_sig = ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde);

    let is_mwd = sde
        .types
        .get(&module.type_id)
        .map(|t| t.effect_ids.contains(&6730))
        .unwrap_or(false);
    let can_activate = !(is_mwd && is_scrambled);

    if !is_active || !can_activate {
        return PropulsionModuleStats {
            max_velocity: base_vel,
            mass: base_mass,
            signature_radius: base_sig,
            speed_factor: 0.0,
            mass_addition: 0.0,
            signature_radius_bonus: 0.0,
            is_overloaded: false,
            is_scrambled,
            can_activate,
        };
    }

    let mut speed_factor = module.get_attribute(attr_ids::SPEED_FACTOR, sde);
    if is_overloaded {
        let ovl_bonus = module.get_attribute(1223, sde);
        let ovl_pct = if ovl_bonus > 0.0 { ovl_bonus } else { 30.0 };
        speed_factor *= 1.0 + (ovl_pct / 100.0);
    }

    let mass_addition = if module.base_attributes.contains_key(&796)
        || sde
            .types
            .get(&module.type_id)
            .map(|t| t.default_attributes.contains_key(&796))
            .unwrap_or(false)
    {
        module.get_attribute(796, sde)
    } else {
        module.get_attribute(attr_ids::MASS, sde)
    };

    let effective_vel = base_vel * (1.0 + speed_factor / 100.0);
    let effective_mass = base_mass + mass_addition;

    let mut effective_sig = base_sig;
    let mut sig_bonus = 0.0;
    if is_mwd {
        sig_bonus = module.get_attribute(554, sde);
        if sig_bonus <= 0.0 {
            sig_bonus = module.get_attribute(986, sde);
        }
        if sig_bonus > 0.0 {
            effective_sig *= 1.0 + sig_bonus / 100.0;
        }
    }

    PropulsionModuleStats {
        max_velocity: effective_vel,
        mass: effective_mass,
        signature_radius: effective_sig,
        speed_factor,
        mass_addition,
        signature_radius_bonus: sig_bonus,
        is_overloaded,
        is_scrambled,
        can_activate,
    }
}

/// Result of an Energy Neutralizer cycle (`DG-1`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EnergyNeutralizerResult {
    pub nominal_drain: f64,
    pub falloff_factor: f64,
    pub actual_drain: f64,
    pub target_remaining_cap: f64,
}

/// Helper to extract range falloff for modules (checking falloffEffectiveness attr 2044 first, then falloff attr 158).
#[inline]
pub fn get_module_falloff(module: &DogmaItemState, sde: &SdeRegistry) -> f64 {
    if let Some(&f) = module.base_attributes.get(&2044) {
        return f;
    }
    if let Some(t) = sde.types.get(&module.type_id) {
        if let Some(&f) = t.default_attributes.get(&2044) {
            return f;
        }
        if let Some(&f) = module.base_attributes.get(&158) {
            return f;
        }
        if let Some(&f) = t.default_attributes.get(&158) {
            return f;
        }
    }
    0.0
}

/// Evaluates energy neutralizer capacitor drain with range falloff attenuation (`DG-1`).
pub fn eval_energy_neutralizer(
    module: &DogmaItemState,
    distance: f64,
    target_current_cap: f64,
    sde: &SdeRegistry,
) -> EnergyNeutralizerResult {
    let nominal_drain = module.get_attribute(97, sde);
    let optimal = module.get_attribute(attr_ids::MAX_RANGE, sde);
    let falloff = get_module_falloff(module, sde);

    let falloff_factor = compute_falloff_attenuation(distance, optimal, falloff);
    let actual_drain = (nominal_drain * falloff_factor).min(target_current_cap.max(0.0));
    let target_remaining_cap = (target_current_cap - actual_drain).max(0.0);

    EnergyNeutralizerResult {
        nominal_drain,
        falloff_factor,
        actual_drain,
        target_remaining_cap,
    }
}

/// Result of an Energy Nosferatu cycle (`DG-1`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EnergyNosferatuResult {
    pub nominal_transfer: f64,
    pub falloff_factor: f64,
    pub actual_transferred: f64,
    pub source_new_cap: f64,
    pub target_new_cap: f64,
}

/// Evaluates energy nosferatu capacitor vampire transfer with range falloff and absolute GJ rule (`feat:combat.nosferatu-drain-cap`).
pub fn eval_energy_nosferatu(
    module: &DogmaItemState,
    distance: f64,
    source_current_cap: f64,
    source_max_cap: f64,
    target_current_cap: f64,
    _target_max_cap: f64,
    sde: &SdeRegistry,
) -> EnergyNosferatuResult {
    eval_energy_nosferatu_with_hull(
        module,
        distance,
        source_current_cap,
        source_max_cap,
        target_current_cap,
        false,
        sde,
    )
}

/// Computes the effective omnidirectional AoE radius in meters for a Burst Jammer (`group_id == 80`)
/// or QA ECM Burst (`group_id == 353`) module (`feat:combat.ecm-burst-radius`).
///
/// In V24.01 SDE, `Burst Jammer I` (`580`) and `Burst Jammer II` (`2117`) store full-diameter span
/// in base dogma attribute `142` (`ecmBurstRange` = `10,000.0` / `12,000.0` m) with no `maxRange` (`54`),
/// yielding exact `5,000.0` m (`5 km` T1) and `6,000.0` m (`6 km` T2) radii without single-target ECM
/// optimal-range skill/hull inflation, scaled by hull bonuses on ecmBurstRange (attr 142) such as Scorpion (+25%/lvl).
pub fn compute_burst_jammer_radius_m(
    mod_type: &eve_sde::InvType,
    mod_state: &DogmaItemState,
    sde: &SdeRegistry,
) -> f64 {
    let max_r = mod_state.get_attribute(attr_ids::MAX_RANGE, sde);
    if max_r > 0.0 {
        return max_r;
    }
    let base_burst_span = mod_state
        .base_attributes
        .get(&142)
        .copied()
        .or_else(|| mod_type.default_attributes.get(&142).copied())
        .unwrap_or(0.0);
    if mod_type.group_id == 80 {
        if base_burst_span > 0.0 {
            0.5 * base_burst_span
        } else {
            5_000.0
        }
    } else if base_burst_span > 0.0 {
        base_burst_span
    } else {
        5_000.0
    }
}

/// Returns `true` if `ship_type_id` has a Blood Raider hull bonus overriding the Energy Nosferatu
/// `target_cap > self_cap` limitation (`feat:combat.nosferatu-drain-cap`).
///
/// Derived dynamically from SDE hull data: checks dogma attribute 1945 (`nosOverride` > 0.0)
/// or dogma effect 5854 (`shipBonusNosferatuOverride`).
pub fn is_blood_raider_nosferatu_hull(ship_type_id: TypeId, sde: &SdeRegistry) -> bool {
    if let Some(ship_type) = sde.types.get(&ship_type_id) {
        if ship_type.default_attributes.get(&1945).copied().unwrap_or(0.0) > 0.0 {
            return true;
        }
        if ship_type.effect_ids.contains(&5854) {
            return true;
        }
    }
    false
}

/// Evaluates Energy Nosferatu capacitor vampire transfer with range falloff, absolute GJ comparison
/// (`target_current_cap > source_current_cap`), and Blood Raider hull override (`feat:combat.nosferatu-drain-cap`).
pub fn eval_energy_nosferatu_with_hull(
    module: &DogmaItemState,
    distance: f64,
    source_current_cap: f64,
    source_max_cap: f64,
    target_current_cap: f64,
    blood_raider_override: bool,
    sde: &SdeRegistry,
) -> EnergyNosferatuResult {
    let nominal_transfer = module.get_attribute(90, sde);
    let optimal = module.get_attribute(attr_ids::MAX_RANGE, sde);
    let falloff = get_module_falloff(module, sde);
    let falloff_factor = compute_falloff_with_cutoff(distance, optimal, falloff);
    let can_drain = blood_raider_override || target_current_cap > source_current_cap;
    let actual_transferred = if can_drain {
        (nominal_transfer * falloff_factor)
            .min(target_current_cap.max(0.0))
            .min((source_max_cap - source_current_cap).max(0.0))
    } else {
        0.0
    };
    EnergyNosferatuResult {
        nominal_transfer,
        falloff_factor,
        actual_transferred,
        source_new_cap: (source_current_cap + actual_transferred).min(source_max_cap),
        target_new_cap: (target_current_cap - actual_transferred).max(0.0),
    }
}

/// Evaluates effective signature radius from base signature and a slice of falloff-attenuated
/// percentage bonuses (e.g. `30.0` for `+30%`) using EVE's stacking penalty curve (`feat:combat.target-painter-bloom`).
pub fn eval_target_painters_sig_radius(base_sig: f64, effective_pct_bonuses: &[f64]) -> f64 {
    let mut deltas: Vec<f64> = effective_pct_bonuses
        .iter()
        .copied()
        .filter(|&b| b > 0.0)
        .map(|b| b / 100.0)
        .collect();
    deltas.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let mut sig = base_sig;
    for (i, d) in deltas.into_iter().enumerate() {
        sig *= 1.0 + d * stacking_penalty_factor(i);
    }
    sig
}

/// Computes ECM jam probability against a target sensor strength with range falloff (`DG-1`):
///
/// $$P_{\text{jam}} = \min\left(1.0, \frac{S_{\text{jam}}}{S_{\text{target\_sensor}}} \cdot 0.5^{\left(\frac{\max(0, d - R_{\text{opt}})}{R_{\text{falloff}}}\right)^2}\right)$$
#[inline]
pub fn compute_ecm_jam_chance(
    jam_strength: f64,
    target_sensor_strength: f64,
    distance: f64,
    optimal: f64,
    falloff: f64,
) -> f64 {
    let att = compute_falloff_attenuation(distance, optimal, falloff);
    ((jam_strength / target_sensor_strength.max(1e-6)) * att).clamp(0.0, 1.0)
}

/// Evaluates multiple Stasis Webifier modules on target velocity with range falloff and stacking penalties (`DG-1`).
pub fn eval_stasis_webifier_stacking(
    web_modules: &[&DogmaItemState],
    distances: &[f64],
    target_base_velocity: f64,
    sde: &SdeRegistry,
) -> f64 {
    let mut penalties = Vec::new();
    for (i, &m) in web_modules.iter().enumerate() {
        let dist = distances.get(i).copied().unwrap_or(0.0);
        let speed_factor = m.get_attribute(20, sde);
        let optimal = m.get_attribute(attr_ids::MAX_RANGE, sde);
        let falloff = get_module_falloff(m, sde);
        let att = compute_falloff_attenuation(dist, optimal, falloff);
        let delta = (speed_factor / 100.0) * att;
        penalties.push(delta);
    }
    penalties.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut vel = target_base_velocity;
    for (i, d) in penalties.into_iter().enumerate() {
        vel *= 1.0 + d * stacking_penalty_factor(i);
    }
    vel.max(0.0)
}

/// Evaluates multiple Target Painter modules on target signature radius with range falloff and stacking penalties (`DG-1`).
pub fn eval_target_painter_stacking(
    painter_modules: &[&DogmaItemState],
    distances: &[f64],
    target_base_sig: f64,
    sde: &SdeRegistry,
) -> f64 {
    let mut bonuses = Vec::new();
    for (i, &m) in painter_modules.iter().enumerate() {
        let dist = distances.get(i).copied().unwrap_or(0.0);
        let sig_bonus = m.get_attribute(554, sde);
        let optimal = m.get_attribute(attr_ids::MAX_RANGE, sde);
        let falloff = get_module_falloff(m, sde);
        let att = compute_falloff_attenuation(dist, optimal, falloff);
        let delta = (sig_bonus / 100.0) * att;
        bonuses.push(delta);
    }
    bonuses.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let mut sig = target_base_sig;
    for (i, d) in bonuses.into_iter().enumerate() {
        sig *= 1.0 + d * stacking_penalty_factor(i);
    }
    sig
}

/// Evaluates multiple Remote Sensor Dampener modules on target scan resolution and max target range (`DG-1`).
pub fn eval_sensor_dampener_stacking(
    damp_modules: &[&DogmaItemState],
    distances: &[f64],
    target_base_scan_res: f64,
    target_base_max_range: f64,
    sde: &SdeRegistry,
) -> (f64, f64) {
    let mut scan_res_penalties = Vec::new();
    let mut range_penalties = Vec::new();
    for (i, &m) in damp_modules.iter().enumerate() {
        let dist = distances.get(i).copied().unwrap_or(0.0);
        let scan_res_bonus = m.get_attribute(566, sde);
        let range_bonus = m.get_attribute(309, sde);
        let optimal = m.get_attribute(attr_ids::MAX_RANGE, sde);
        let falloff = get_module_falloff(m, sde);
        let att = compute_falloff_attenuation(dist, optimal, falloff);
        scan_res_penalties.push((scan_res_bonus / 100.0) * att);
        range_penalties.push((range_bonus / 100.0) * att);
    }

    scan_res_penalties.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    range_penalties.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut scan_res = target_base_scan_res;
    for (i, d) in scan_res_penalties.into_iter().enumerate() {
        scan_res *= 1.0 + d * stacking_penalty_factor(i);
    }
    let mut max_range = target_base_max_range;
    for (i, d) in range_penalties.into_iter().enumerate() {
        max_range *= 1.0 + d * stacking_penalty_factor(i);
    }
    (scan_res.max(1.0), max_range.max(1.0))
}

/// Evaluates multiple Tracking Disruptor modules on target turret tracking, optimal, and falloff (`DG-1`).
pub fn eval_tracking_disruptor_stacking(
    disrupt_modules: &[&DogmaItemState],
    distances: &[f64],
    turret_base_tracking: f64,
    turret_base_optimal: f64,
    turret_base_falloff: f64,
    sde: &SdeRegistry,
) -> (f64, f64, f64) {
    let mut track_penalties = Vec::new();
    let mut opt_penalties = Vec::new();
    let mut fall_penalties = Vec::new();

    for (i, &m) in disrupt_modules.iter().enumerate() {
        let dist = distances.get(i).copied().unwrap_or(0.0);
        let track_bonus = m.get_attribute(767, sde);
        let opt_bonus = m.get_attribute(351, sde);
        let fall_bonus = m.get_attribute(349, sde);
        let optimal = m.get_attribute(attr_ids::MAX_RANGE, sde);
        let falloff = get_module_falloff(m, sde);
        let att = compute_falloff_attenuation(dist, optimal, falloff);
        track_penalties.push((track_bonus / 100.0) * att);
        opt_penalties.push((opt_bonus / 100.0) * att);
        fall_penalties.push((fall_bonus / 100.0) * att);
    }

    track_penalties.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    opt_penalties.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    fall_penalties.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut track = turret_base_tracking;
    for (i, d) in track_penalties.into_iter().enumerate() {
        track *= 1.0 + d * stacking_penalty_factor(i);
    }
    let mut opt = turret_base_optimal;
    for (i, d) in opt_penalties.into_iter().enumerate() {
        opt *= 1.0 + d * stacking_penalty_factor(i);
    }
    let mut fall = turret_base_falloff;
    for (i, d) in fall_penalties.into_iter().enumerate() {
        fall *= 1.0 + d * stacking_penalty_factor(i);
    }

    (track.max(0.0), opt.max(0.0), fall.max(0.0))
}

/// Active warp scramble points and MWD suppression state on a target (`DG-1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarpScrambleState {
    pub scramble_strength: i32,
    pub blocks_mwd: bool,
}

/// Evaluates active warp disruptors and scramblers against a target (`DG-1`).
pub fn eval_warp_scrambler_effects(
    modules: &[&DogmaItemState],
    distances: &[f64],
    sde: &SdeRegistry,
) -> WarpScrambleState {
    let mut points = 0i32;
    let mut blocks_mwd = false;

    for (i, &m) in modules.iter().enumerate() {
        let dist = distances.get(i).copied().unwrap_or(0.0);
        let max_range = m.get_attribute(attr_ids::MAX_RANGE, sde);
        if dist <= max_range {
            let pts = {
                let p = m.get_attribute(105, sde);
                if p > 0.0 {
                    p.round() as i32
                } else {
                    m.get_attribute(104, sde).round() as i32
                }
            };
            points += pts.max(1);
            if let Some(t) = sde.types.get(&m.type_id) {
                for &eid in &t.effect_ids {
                    if let Some(eff) = sde.effects.get(&eid) {
                        for spec in &eff.modifiers {
                            if spec.func == "EffectStopper" {
                                blocks_mwd = true;
                            }
                        }
                    }
                }
            }
        }
    }

    WarpScrambleState {
        scramble_strength: points,
        blocks_mwd,
    }

}

/// Result of a Command Burst activation (`DG-1`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandBurstResult {
    pub burst_radius: f64,
    pub duration_s: f64,
    pub affected_fleet_count: usize,
    pub aggregated_bonus: f64,
}

/// Evaluates a Command Burst area-of-effect buff across fleet member distances (`DG-1`).
pub fn eval_command_burst(
    burst_module: &DogmaItemState,
    charge_item: Option<&DogmaItemState>,
    fleet_distances: &[f64],
    sde: &SdeRegistry,
) -> CommandBurstResult {
    let burst_radius = burst_module.get_attribute(1892, sde);
    let effective_radius = if burst_radius > 0.0 {
        burst_radius
    } else {
        35_000.0
    };
    let duration_ms = burst_module.get_attribute(attr_ids::DURATION, sde);
    let duration_s = if duration_ms > 0.0 {
        duration_ms / 1000.0
    } else {
        60.0
    };

    let affected_count = fleet_distances
        .iter()
        .filter(|&&d| d <= effective_radius)
        .count();
    let bonus_val = charge_item
        .map(|c| c.get_attribute(2470, sde))
        .filter(|&v| v > 0.0)
        .unwrap_or(15.0);

    let aggregated = aggregate_dbuff_values("Maximum", &[bonus_val]);

    CommandBurstResult {
        burst_radius: effective_radius,
        duration_s,
        affected_fleet_count: affected_count,
        aggregated_bonus: aggregated,
    }
}

/// Local shield booster or armor repairer cycle recovery result (`feat:modules.shield-booster`, `DG-4`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalRepairCycleResult {
    pub shield_boost_amount: f64,
    pub armor_repair_amount: f64,
    pub capacitor_need: f64,
    pub duration_s: f64,
}

/// Computes shield booster or armor repairer cycle healing amounts and capacitor need (`feat:modules.shield-booster`, `DG-4`).
pub fn compute_local_repair_cycle(
    module: &DogmaItemState,
    overload: bool,
    sde: &SdeRegistry,
) -> LocalRepairCycleResult {
    let duration_ms = module.get_attribute(attr_ids::DURATION, sde);
    let duration_s = if duration_ms > 0.0 { duration_ms / 1000.0 } else { 1.0 };
    let cap_need = module.get_attribute(attr_ids::CAPACITOR_NEED, sde);

    let raw_shield = module.get_attribute(68, sde); // shieldBonus
    let raw_armor = module.get_attribute(84, sde);  // armorDamageAmount

    let overload_mult = if overload { 1.20 } else { 1.0 };

    LocalRepairCycleResult {
        shield_boost_amount: raw_shield * overload_mult,
        armor_repair_amount: raw_armor * overload_mult,
        capacitor_need: cap_need,
        duration_s,
    }
}

/// EVE Online racial sensor strength types (`feat:dogma.targeting-ships-sensor-strength`, `DG-5`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShipSensorType {
    Radar,          // Amarr (attribute 208)
    Ladar,          // Minmatar (attribute 209)
    Magnetometric,  // Gallente (attribute 210)
    Gravimetric,    // Caldari (attribute 211)
}

impl ShipSensorType {
    pub fn attribute_id(&self) -> AttributeId {
        match self {
            Self::Radar => 208,
            Self::Ladar => 209,
            Self::Magnetometric => 210,
            Self::Gravimetric => 211,
        }
    }
}

/// Identifies the dominant racial sensor type and strength of a ship hull (`feat:dogma.targeting-ships-sensor-strength`, `DG-5`).
pub fn get_ship_sensor_strength(ship: &DogmaItemState, sde: &SdeRegistry) -> (ShipSensorType, f64) {
    let radar = ship.get_attribute(208, sde);
    let ladar = ship.get_attribute(209, sde);
    let mag = ship.get_attribute(210, sde);
    let grav = ship.get_attribute(211, sde);

    let candidates = [
        (ShipSensorType::Radar, radar),
        (ShipSensorType::Ladar, ladar),
        (ShipSensorType::Magnetometric, mag),
        (ShipSensorType::Gravimetric, grav),
    ];
    candidates
        .into_iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or((ShipSensorType::Gravimetric, 10.0))
}

/// Computes ECM jam chance matching racial sensor strength against target ship hull (`feat:modules.ecm`, `DG-5`).
pub fn compute_ecm_jam_against_ship(
    ecm_module: &DogmaItemState,
    target_ship: &DogmaItemState,
    distance: f64,
    sde: &SdeRegistry,
) -> f64 {
    let (sensor_type, target_strength) = get_ship_sensor_strength(target_ship, sde);
    let jam_strength = match sensor_type {
        ShipSensorType::Radar => ecm_module.get_attribute(241, sde), // scanRadarStrengthBonus
        ShipSensorType::Ladar => ecm_module.get_attribute(239, sde), // scanLadarStrengthBonus
        ShipSensorType::Magnetometric => ecm_module.get_attribute(240, sde), // scanMagnetometricStrengthBonus
        ShipSensorType::Gravimetric => ecm_module.get_attribute(238, sde), // scanGravimetricStrengthBonus
    };
    let optimal = ecm_module.get_attribute(attr_ids::MAX_RANGE, sde);
    let falloff = get_module_falloff(ecm_module, sde);
    compute_ecm_jam_chance(jam_strength, target_strength, distance, optimal, falloff)
}

/// Evaluates Remote Sensor Dampener multipliers with optional scripting (`feat:modules.sensor-dampener`, `DG-5`).
/// Returns `(scan_resolution_multiplier, max_target_range_multiplier)`.
pub fn eval_scripted_sensor_dampener(
    damp_module: &DogmaItemState,
    script_type_id: Option<u32>,
    distance: f64,
    sde: &SdeRegistry,
) -> (f64, f64) {
    let optimal = damp_module.get_attribute(attr_ids::MAX_RANGE, sde);
    let falloff = get_module_falloff(damp_module, sde);
    let att = compute_falloff_attenuation(distance, optimal, falloff);

    let base_scan_res = damp_module.get_attribute(566, sde); // scanResolutionBonus (negative)
    let base_range = damp_module.get_attribute(309, sde);    // maxTargetRangeBonus (negative)

    // Script 29013 = Scan Resolution Dampening Script (doubles scan res dampening, zeroes range dampening)
    // Script 29015 = Targeting Range Dampening Script (doubles range dampening, zeroes scan res dampening)
    let (eff_scan_pct, eff_range_pct) = match script_type_id {
        Some(29013) => (base_scan_res * 2.0, 0.0),
        Some(29015) => (0.0, base_range * 2.0),
        _ => (base_scan_res, base_range),
    };

    let scan_res_mult = (1.0 + (eff_scan_pct / 100.0) * att).clamp(0.01, 1.0);
    let range_mult = (1.0 + (eff_range_pct / 100.0) * att).clamp(0.01, 1.0);
    (scan_res_mult, range_mult)
}

/// Evaluates Tracking Disruptor multipliers with optional scripting (`feat:modules.tracking-disruptor`, `DG-5`).
/// Returns `(tracking_speed_multiplier, optimal_multiplier, falloff_multiplier)`.
pub fn eval_scripted_tracking_disruptor(
    disrupt_module: &DogmaItemState,
    script_type_id: Option<u32>,
    distance: f64,
    sde: &SdeRegistry,
) -> (f64, f64, f64) {
    let optimal = disrupt_module.get_attribute(attr_ids::MAX_RANGE, sde);
    let falloff = get_module_falloff(disrupt_module, sde);
    let att = compute_falloff_attenuation(distance, optimal, falloff);

    let base_track = disrupt_module.get_attribute(767, sde); // trackingSpeedBonus (negative)
    let base_opt = disrupt_module.get_attribute(351, sde);   // maxRangeBonus (negative)
    let base_fall = disrupt_module.get_attribute(349, sde);  // falloffBonus (negative)

    // Script 29007 = Tracking Speed Disruption Script (doubles tracking disruption, zeroes range)
    // Script 29005 = Optimal Range Disruption Script (doubles range disruption, zeroes tracking)
    let (eff_track, eff_opt, eff_fall) = match script_type_id {
        Some(29007) => (base_track * 2.0, 0.0, 0.0),
        Some(29005) => (0.0, base_opt * 2.0, base_fall * 2.0),
        _ => (base_track, base_opt, base_fall),
    };

    let track_mult = (1.0 + (eff_track / 100.0) * att).clamp(0.01, 1.0);
    let opt_mult = (1.0 + (eff_opt / 100.0) * att).clamp(0.01, 1.0);
    let fall_mult = (1.0 + (eff_fall / 100.0) * att).clamp(0.01, 1.0);
    (track_mult, opt_mult, fall_mult)
}

/// Evaluates Target Painter signature radius multiplier with optional overload (`feat:modules.target-painter`, `DG-5`).
pub fn eval_target_painter_with_overload(
    painter_module: &DogmaItemState,
    overload: bool,
    distance: f64,
    sde: &SdeRegistry,
) -> f64 {
    let optimal = painter_module.get_attribute(attr_ids::MAX_RANGE, sde);
    let falloff = get_module_falloff(painter_module, sde);
    let att = compute_falloff_attenuation(distance, optimal, falloff);

    let base_sig = painter_module.get_attribute(554, sde); // signatureRadiusBonus
    let overload_bonus = if overload {
        let ov = painter_module.get_attribute(1937, sde);
        if ov > 0.0 { ov / 100.0 } else { 0.20 }
    } else {
        0.0
    };
    let eff_bonus = base_sig * (1.0 + overload_bonus);
    1.0 + (eff_bonus / 100.0) * att
}

/// Evaluates capacitor stability and steady-state recharge rate (`feat:dogma.capacitor-stable`, `DG-4`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapacitorStabilityReport {
    pub max_capacitor: f64,
    pub recharge_time_s: f64,
    pub peak_recharge_per_sec: f64,
    pub total_draw_per_sec: f64,
    pub is_stable: bool,
    pub stable_level_pct: Option<f64>,
    pub depletion_time_s: Option<f64>,
}

pub fn evaluate_capacitor_stability(
    max_capacitor: f64,
    recharge_time_s: f64,
    total_draw_per_sec: f64,
) -> CapacitorStabilityReport {
    let peak = 2.5 * max_capacitor / recharge_time_s.max(1e-3);
    if total_draw_per_sec <= peak {
        let rate_norm = total_draw_per_sec / (10.0 * max_capacitor / recharge_time_s.max(1e-3));
        let stable_pct = if rate_norm < 0.25 {
            let root = 0.5 + (0.25 - rate_norm).max(0.0).sqrt();
            (root * root * 100.0).clamp(25.0, 100.0)
        } else {
            25.0
        };
        CapacitorStabilityReport {
            max_capacitor,
            recharge_time_s,
            peak_recharge_per_sec: peak,
            total_draw_per_sec,
            is_stable: true,
            stable_level_pct: Some(stable_pct),
            depletion_time_s: None,
        }
    } else {
        let net_loss = total_draw_per_sec - (peak * 0.5);
        let time = if net_loss > 0.0 { max_capacitor / net_loss } else { 0.0 };
        CapacitorStabilityReport {
            max_capacitor,
            recharge_time_s,
            peak_recharge_per_sec: peak,
            total_draw_per_sec,
            is_stable: false,
            stable_level_pct: None,
            depletion_time_s: Some(time),
        }
    }
}

/// Computes target lock duration from scan resolution and target signature radius (`DG-5`).
pub fn compute_target_lock_time(scan_resolution: f64, target_signature_radius: f64) -> f64 {
    let denom = scan_resolution.max(1.0) * target_signature_radius.max(1.0).asinh().powi(2);
    (40_000.0 / denom).max(0.25)
}

/// Computes effective max locked targets with hull and character skills (`DG-5`).
pub fn compute_effective_max_locked_targets(
    hull_max_targets: usize,
    target_management_level: u8,
    adv_target_management_level: u8,
) -> usize {
    let char_max = 2 + (target_management_level.min(5) as usize) + (adv_target_management_level.min(5) as usize);
    hull_max_targets.min(char_max).clamp(1, 12)
}

impl ShipFittingEngine {
    /// Evaluates incoming Energy Neutralizer (`97` `energyNeutralizerAmount`) or Energy Nosferatu (`90` `powerTransferAmount`)
    /// capacitor drain against this ship, attenuated by range falloff and target `energyWarfareResistance` (`2045`) (`feat:dogma.ewar-resistance-attributes`).
    pub fn evaluate_projected_energy_warfare(
        &self,
        source_module: &DogmaItemState,
        distance: f64,
        sde: &SdeRegistry,
    ) -> f64 {
        let optimal = source_module.get_attribute(attr_ids::MAX_RANGE, sde);
        let falloff = get_module_falloff(source_module, sde);
        let att = compute_falloff_attenuation(distance, optimal, falloff);
        let resist = self.ship.get_attribute(2045, sde).clamp(0.0, 1.0);
        let neut_amt = source_module.get_attribute(97, sde);
        let base_amt = if neut_amt > 0.0 {
            neut_amt
        } else {
            source_module.get_attribute(90, sde)
        };
        base_amt * att * resist
    }

    /// Evaluates incoming Remote Shield Booster (`68`), Remote Armor Repairer (`84`), or Remote Hull Repairer (`83`)
    /// amount applied to this ship, attenuated by range falloff and target `remoteRepairImpedance` (`2116`) (`feat:dogma.ewar-resistance-attributes`).
    pub fn evaluate_projected_remote_repair(
        &self,
        source_module: &DogmaItemState,
        distance: f64,
        sde: &SdeRegistry,
    ) -> f64 {
        let optimal = source_module.get_attribute(attr_ids::MAX_RANGE, sde);
        let falloff = get_module_falloff(source_module, sde);
        let att = compute_falloff_with_cutoff(distance, optimal, falloff);
        let impedance = self.ship.get_attribute(2116, sde).clamp(0.0, 1.0);
        for aid in [attr_ids::SHIELD_BONUS, 84_u16, 83_u16] {
            let v = source_module.get_attribute(aid, sde);
            if v > 0.0 {
                return v * att * impedance;
            }
        }
        0.0
    }

    /// Evaluates incoming Remote Tracking Computer or Remote Sensor Booster assistance bonus (`attr_id`)
    /// applied to this ship, attenuated by range falloff and target `remoteAssistanceImpedance` (`2135`) (`feat:dogma.ewar-resistance-attributes`).
    pub fn evaluate_projected_remote_assistance(
        &self,
        source_module: &DogmaItemState,
        attr_id: AttributeId,
        distance: f64,
        sde: &SdeRegistry,
    ) -> f64 {
        let optimal = source_module.get_attribute(attr_ids::MAX_RANGE, sde);
        let falloff = get_module_falloff(source_module, sde);
        let att = compute_falloff_attenuation(distance, optimal, falloff);
        let impedance = self.ship.get_attribute(2135, sde).clamp(0.0, 1.0);
        source_module.get_attribute(attr_id, sde) * att * impedance
    }
}
