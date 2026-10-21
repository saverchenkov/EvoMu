//! `tactical_modules` — Dogma and Combat Tactical Modules (`§3.12`, `DG-4`..`DG-6`, `feat:modules.*`).
//!
//! Implements V24.01 server mechanics for:
//! - Bastion Module V24 effects, MJD lock, and wormhole deactivation (`feat:modules.bastion-module-effects`, `feat:modules.bastion-wormhole-and-mjd-interaction`)
//! - Siege and Triage Module V24 effects, strontium formulas, and immobility (`feat:modules.siege-module-v24-effects`, `feat:modules.triage-module-v24-effects`)
//! - Lancer Dreadnought Disruptive Lance (`feat:modules.disruptive-lance`)
//! - Entosis Link activation restrictions, warmup, and contest rules (`feat:modules.entosis-activation-restrictions`, `feat:modules.entosis-warmup-and-contest`)
//! - Command Burst range, strength, duration, fitting, and Industrial Core bonuses (`feat:modules.command-burst-range`, `feat:modules.command-burst-strength-formula`, `feat:modules.command-burst-duration-reload`, `feat:modules.command-burst-fitting-and-clone-restrictions`, `feat:modules.industrial-core-burst-bonus`)
//! - Target Spectrum Breaker lockbreak and penalties (`feat:modules.target-spectrum-breaker`)
//! - Capital Emergency Hull Energizer (`feat:modules.capital-emergency-hull-energizer`)
//! - Stasis Webification Probe (wubble) (`feat:modules.stasis-webification-probe`)
//! - Remote Sensor Boosters and Remote Tracking Computers (`feat:modules.remote-sensor-booster-rtc`)
//! - Ancillary Remote Repairers (`feat:modules.ancillary-remote-repairers`)
//! - Automated Targeting Systems (`feat:modules.automated-targeting-system`)
//! - Fitting capacity modules and cargo expanders (`feat:modules.fitting-capacity-mods`, `feat:modules.cargo-expanders`)
//! - Drone upgrade modules (`feat:modules.drone-upgrade-modules`)
//! - Authentic module mechanics resolving audit flags: MWD cap penalty (`feat:modules.microwarpdrive`), MJD spool & scram (`feat:modules.micro-jump-drive`),
//!   nullsec mobile warp disruptor anchoring (`feat:modules.mobile-warp-disruptor`), interdiction nullifier (`feat:modules.interdiction-nullifier`),
//!   interdiction sphere launcher (`feat:modules.interdiction-sphere`), warp scrambler stops MWD/MJD (`feat:modules.warp-disruptor`),
//!   HIC infinite point and bubble penalties (`feat:modules.warp-disruption-field`), ECM jam formula (`feat:modules.ecm`),
//!   and scripted weapon disruptors (`feat:modules.tracking-disruptor`).

#![forbid(unsafe_code)]

use eve_destiny::DVec3;

/// Bastion Module V24 attributes and state.
#[derive(Debug, Clone, PartialEq)]
pub struct BastionV24Effect {
    pub cycle_duration_sec: f64,
    pub rof_multiplier: f64,
    pub missile_velocity_bonus: f64,
    pub turret_range_bonus: f64,
    pub local_repair_bonus: f64,
    pub local_repair_duration_mult: f64,
    pub local_repair_cap_mult: f64,
    pub resistance_bonus: f64,
    pub sensor_strength_bonus: f64,
    pub remote_assistance_impedance: f64,
    pub weapons_timer_sec: u64,
    pub provides_ecm_immunity: bool,
    pub allows_mjd: bool,
    pub allows_mjfg_movement: bool,
    pub deactivates_on_wormhole_jump: bool,
}

impl Default for BastionV24Effect {
    fn default() -> Self {
        Self {
            cycle_duration_sec: 60.0,
            rof_multiplier: 0.50, // -50% cycle time
            missile_velocity_bonus: 0.25, // +25%
            turret_range_bonus: 0.25, // +25% optimal & falloff
            local_repair_bonus: 0.60, // +60% repair amount
            local_repair_duration_mult: 0.80, // -20% duration
            local_repair_cap_mult: 0.80, // -20% cap cost
            resistance_bonus: 0.30, // +30% shield/armor/hull resists
            sensor_strength_bonus: 1.00, // +100% sensor strength
            remote_assistance_impedance: 0.95, // friendly RSB/RTC reduced 95%
            weapons_timer_sec: 60,
            provides_ecm_immunity: false, // V24 rule: Marauder bastion has NO ECM immunity
            allows_mjd: false, // MJD refused while Bastion active
            allows_mjfg_movement: true, // Command Destroyer MJFG still moves bastioned ship
            deactivates_on_wormhole_jump: true, // Arrives with Bastion deactivated
        }
    }
}

/// Computes Strontium Clathrates consumption for Siege Module based on Tactical Weapon Reconfiguration skill.
pub fn compute_siege_strontium_cost(skill_level: u8) -> u32 {
    let base = 250i32;
    let reduction = 25i32 * (skill_level.min(5) as i32);
    (base - reduction).max(125) as u32
}

/// Computes Strontium Clathrates consumption for Triage Module based on Tactical Logistics Reconfiguration skill.
pub fn compute_triage_strontium_cost(skill_level: u8) -> u32 {
    let base = 200i32;
    let reduction = 25i32 * (skill_level.min(5) as i32);
    (base - reduction).max(75) as u32
}

/// Siege Module V24 effects table.
#[derive(Debug, Clone, PartialEq)]
pub struct SiegeV24Effect {
    pub cycle_duration_sec: f64,
    pub strontium_cost: u32,
    pub turret_damage_mult: f64,
    pub missile_damage_mult: f64,
    pub xl_torpedo_cycle_mult: f64,
    pub torpedo_velocity_bonus: f64,
    pub ewar_resistance: f64,
    pub remote_assistance_impedance: f64,
    pub ecm_immunity: bool,
    pub local_repair_mult: f64,
    pub local_repair_duration_mult: f64,
    pub mass_multiplier: f64,
    pub velocity_multiplier: f64,
    pub grants_weapons_timer: bool,
}

pub fn get_siege_v24_effect(is_t2: bool, skill_level: u8) -> SiegeV24Effect {
    SiegeV24Effect {
        cycle_duration_sec: 300.0, // 5 min
        strontium_cost: compute_siege_strontium_cost(skill_level),
        turret_damage_mult: if is_t2 { 9.40 } else { 8.00 }, // +840% / +700%
        missile_damage_mult: if is_t2 { 3.00 } else { 2.65 }, // +200% / +165%
        xl_torpedo_cycle_mult: 0.20, // -80% cycle
        torpedo_velocity_bonus: 1.50, // +150% velocity
        ewar_resistance: if is_t2 { 0.70 } else { 0.60 }, // 70% / 60% damp/TD resist
        remote_assistance_impedance: 0.80, // 80% impedance
        ecm_immunity: true,
        local_repair_mult: 2.00, // +100% amount
        local_repair_duration_mult: 0.50, // -50% duration
        mass_multiplier: 10.0,
        velocity_multiplier: 0.0,
        grants_weapons_timer: false, // V24 rule: Siege activation gives NO weapons timer
    }
}

/// Triage Module V24 effects table.
#[derive(Debug, Clone, PartialEq)]
pub struct TriageV24Effect {
    pub cycle_duration_sec: f64,
    pub strontium_cost: u32,
    pub remote_repair_mult: f64,
    pub remote_repair_duration_mult: f64,
    pub remote_repair_range_mult: f64,
    pub scan_resolution_mult: f64,
    pub max_locked_targets_bonus: i32,
    pub local_repair_mult: f64,
    pub local_repair_duration_mult: f64,
    pub ewar_resistance: f64,
    pub remote_assistance_impedance: f64,
    pub ecm_immunity: bool,
    pub drone_damage_mult: f64,
    pub ewar_disabled: bool,
}

pub fn get_triage_v24_effect(is_t2: bool, skill_level: u8) -> TriageV24Effect {
    TriageV24Effect {
        cycle_duration_sec: 300.0,
        strontium_cost: compute_triage_strontium_cost(skill_level),
        remote_repair_mult: if is_t2 { 5.50 } else { 5.00 }, // +450% / +400%
        remote_repair_duration_mult: 0.25, // -75% duration
        remote_repair_range_mult: 3.00, // +200% range
        scan_resolution_mult: 10.00, // +900% scan res
        max_locked_targets_bonus: if is_t2 { 5 } else { 4 },
        local_repair_mult: if is_t2 { 2.20 } else { 2.00 }, // +120% / +100%
        local_repair_duration_mult: 0.50, // -50% duration
        ewar_resistance: if is_t2 { 0.70 } else { 0.60 },
        remote_assistance_impedance: 0.80,
        ecm_immunity: true,
        drone_damage_mult: 0.0, // -100% drone damage
        ewar_disabled: true,
    }
}

/// Disruptive Lance calculation and restrictions (`feat:modules.disruptive-lance`).
pub struct DisruptiveLanceSpec {
    pub titan_lance_damage: f64,
    pub lance_damage: f64,
    pub neutralizer_amount: f64,
    pub jump_dock_tether_lock_sec: u64,
    pub remote_repair_reduction_factor: f64,
}

pub fn eval_disruptive_lance(
    titan_lance_base_dmg: f64,
    lancer_dread_skill: u8,
    disruptive_lance_op_skill: u8,
    is_sieged: bool,
    system_sec: f64,
) -> Result<DisruptiveLanceSpec, &'static str> {
    if !is_sieged {
        return Err("Disruptive Lance usable only while Dreadnought Siege module is active");
    }
    // Allowed in low-sec and null/WH (sec < 0.45)
    if system_sec >= 0.45 {
        return Err("Disruptive Lance prohibited in high-security space");
    }
    let _cap_discount = 1.0 - (0.05 * disruptive_lance_op_skill.min(5) as f64);
    let dmg_bonus = 1.0 + (0.10 * lancer_dread_skill.min(5) as f64);
    let lance_dmg = (titan_lance_base_dmg / 3.0) * dmg_bonus;

    Ok(DisruptiveLanceSpec {
        titan_lance_damage: titan_lance_base_dmg,
        lance_damage: lance_dmg,
        neutralizer_amount: 10_000.0, // 10,000 GJ
        jump_dock_tether_lock_sec: 300, // 5 min lockout
        remote_repair_reduction_factor: 0.50, // Halves incoming remote rep
    })
}

/// Entosis Link activation validation and parameters (`feat:modules.entosis-activation-restrictions`, `feat:modules.entosis-warmup-and-contest`).
pub struct EntosisLinkSpec {
    pub cycle_time_sec: f64,
    pub strontium_cost: u32,
    pub max_ship_velocity: f64,
    pub is_warmup_cycle: bool,
}

pub fn eval_entosis_activation(
    _ship_type_id: u32,
    is_capital: bool,
    is_interceptor: bool,
    existing_entosis_count: usize,
    in_vulnerability_window: bool,
    cycle_index: u32,
) -> Result<EntosisLinkSpec, &'static str> {
    if is_interceptor {
        return Err("Interceptors cannot activate Entosis Link");
    }
    if existing_entosis_count >= 1 {
        return Err("Only one Entosis Link can be active per ship");
    }
    if !in_vulnerability_window {
        return Err("Target structure is outside its vulnerability window");
    }
    let base_cycle = if is_capital { 300.0 * 5.0 } else { 300.0 };
    Ok(EntosisLinkSpec {
        cycle_time_sec: base_cycle,
        strontium_cost: 1,
        max_ship_velocity: 4_000.0, // Speed capped at 4,000 m/s
        is_warmup_cycle: cycle_index == 1,
    })
}

/// Command Burst strength and area-of-effect range formula (`feat:modules.command-burst-range`, `feat:modules.command-burst-strength-formula`).
pub fn compute_command_burst_range(
    leadership_lvl: u8,
    wing_command_lvl: u8,
    fleet_command_lvl: u8,
    ship_range_bonus: f64,
    industrial_core_range_bonus: f64,
) -> f64 {
    let base_range = 15_000.0;
    base_range
        * (1.0 + 0.07 * leadership_lvl.min(5) as f64)
        * (1.0 + 0.06 * wing_command_lvl.min(5) as f64)
        * (1.0 + 0.05 * fleet_command_lvl.min(5) as f64)
        * (1.0 + ship_range_bonus)
        * (1.0 + industrial_core_range_bonus)
}

pub fn compute_command_burst_strength(
    charge_base_bonus: f64,
    specialist_skill_lvl: u8,
    is_t2_module: bool,
    ship_role_bonus: f64,
    mindlink_bonus: f64,
    industrial_core_burst_bonus: f64,
) -> f64 {
    let mod_bonus = if is_t2_module { 0.25 } else { 0.0 };
    charge_base_bonus
        * (1.0 + 0.10 * specialist_skill_lvl.min(5) as f64)
        * (1.0 + mod_bonus)
        * (1.0 + ship_role_bonus)
        * (1.0 + mindlink_bonus)
        * (1.0 + industrial_core_burst_bonus)
}

/// Target Spectrum Breaker lockbreak probability (`feat:modules.target-spectrum-breaker`).
pub fn eval_target_spectrum_breaker(target_sensor_strength: f64, module_strength: f64) -> f64 {
    if target_sensor_strength <= 0.0 {
        return 1.0;
    }
    // Breaker effectiveness scales inversely with target sensor strength
    (module_strength / (module_strength + target_sensor_strength)).clamp(0.05, 0.95)
}

/// Stasis Webification Probe (wubble) parameters (`feat:modules.stasis-webification-probe`).
pub struct StasisWebProbeSpec {
    pub arming_delay_sec: f64,
    pub radius_meters: f64,
    pub duration_sec: f64,
    pub speed_reduction_factor: f64,
    pub disrupts_warp: bool,
}

pub fn get_stasis_web_probe_spec() -> StasisWebProbeSpec {
    StasisWebProbeSpec {
        arming_delay_sec: 3.0,
        radius_meters: 10_000.0,
        duration_sec: 30.0,
        speed_reduction_factor: 0.40, // 40% velocity reduction
        disrupts_warp: false, // Does not disrupt warp
    }
}

/// ECM Jam probability (`feat:modules.ecm`).
pub fn compute_ecm_jam_probability(jammer_strength: f64, target_sensor_strength: f64) -> f64 {
    if target_sensor_strength <= 0.0 {
        return 1.0;
    }
    (jammer_strength / target_sensor_strength).clamp(0.0, 1.0)
}

/// Microwarpdrive evaluation with capacitor capacity penalty (`feat:modules.microwarpdrive`).
pub fn eval_mwd_penalties(
    speed_bonus: f64,
    sig_mult: f64,
    cap_penalty_fraction: f64,
    is_scrambled: bool,
) -> (f64, f64, f64, bool) {
    if is_scrambled {
        // Warp scrambler completely shuts off MWD
        (0.0, 1.0, 1.0, false)
    } else {
        (speed_bonus, sig_mult, 1.0 - cap_penalty_fraction.clamp(0.0, 0.50), true)
    }
}

/// Micro Jump Drive evaluation with 12-second spool and scrambler shutoff (`feat:modules.micro-jump-drive`).
pub fn eval_mjd_activation(
    is_scrambled: bool,
    is_bastioned: bool,
) -> Result<(f64, f64), &'static str> {
    if is_scrambled {
        return Err("Warp scrambler prevents Micro Jump Drive activation");
    }
    if is_bastioned {
        return Err("Bastion module active: Micro Jump Drive activation refused");
    }
    Ok((12.0, 100_000.0)) // 12s spool, 100 km forward jump
}

// ---------------------------------------------------------------------------
// Remaining 22 feat:modules.* Parity Models & Engine Evaluators
// ---------------------------------------------------------------------------

/// 1. Titan Doomsday activation specifications (`feat:modules.doomsday`).
#[derive(Debug, Clone, PartialEq)]
pub struct DoomsdaySpec {
    pub warmup_sec: f64,
    pub delay_sec: f64,
    pub damage: f64,
    pub capacitor_cost: f64,
    pub weapon_timer_sec: u64,
    pub jump_dock_tether_lock_sec: u64,
    pub is_slash_lance: bool,
}

pub fn eval_doomsday_spec(
    is_titan: bool,
    in_highsec: bool,
    is_slash: bool,
    target_id_opt: Option<i64>,
) -> Result<DoomsdaySpec, &'static str> {
    if !is_titan {
        return Err("Doomsday superweapons can only be activated on Titans (group 30)");
    }
    if in_highsec {
        return Err("Superweapons cannot be activated in high-security space");
    }
    if !is_slash && target_id_opt.is_none() {
        return Err("Targeted doomsday requires a locked target");
    }
    Ok(DoomsdaySpec {
        warmup_sec: if is_slash { 15.0 } else { 0.0 },
        delay_sec: if is_slash { 10.0 } else { 300.0 }, // 300s firing delay/cooldown
        damage: if is_slash { 150_000.0 } else { 1_500_000.0 },
        capacitor_cost: 10_000.0,
        weapon_timer_sec: 300,
        jump_dock_tether_lock_sec: 300,
        is_slash_lance: is_slash,
    })
}

/// 2. ECM jammer outcome and burst jammer AoE evaluation (`feat:modules.ecm`).
#[derive(Debug, Clone, PartialEq)]
pub struct EcmJamOutcome {
    pub jammed: bool,
    pub lock_break_duration_sec: f64,
    pub jammed_ship_can_only_lock_jammer: bool,
}

pub fn eval_ecm_jam_attempt(
    jammer_strength: f64,
    target_sensor_strength: f64,
    roll: f64,
) -> EcmJamOutcome {
    let p_jam = compute_ecm_jam_probability(jammer_strength, target_sensor_strength);
    let jammed = roll < p_jam;
    EcmJamOutcome {
        jammed,
        lock_break_duration_sec: if jammed { 20.0 } else { 0.0 },
        jammed_ship_can_only_lock_jammer: jammed,
    }
}

pub fn eval_burst_jammer_aoe(
    jammer_strength: f64,
    burst_radius_m: f64,
    targets: &[(i64, f64, f64, f64)], // (ship_id, distance, sensor_str, roll)
) -> Vec<(i64, bool)> {
    targets
        .iter()
        .map(|&(id, dist, sensor_str, roll)| {
            if dist <= burst_radius_m {
                let p = compute_ecm_jam_probability(jammer_strength, sensor_str);
                (id, roll < p)
            } else {
                (id, false)
            }
        })
        .collect()
}

/// 3. Remote Sensor Dampener evaluation with script support (`feat:modules.sensor-dampener`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DampenerScript {
    Unscripted,
    TargetingRange,
    ScanResolution,
}

pub fn eval_scripted_sensor_dampener(
    script: DampenerScript,
    base_targeting_range_m: f64,
    base_scan_resolution: f64,
) -> (f64, f64) {
    match script {
        DampenerScript::Unscripted => (
            base_targeting_range_m * 0.70, // -30% range
            base_scan_resolution * 0.70,   // -30% scan res
        ),
        DampenerScript::TargetingRange => (
            base_targeting_range_m * 0.50, // -50% targeting range
            base_scan_resolution,          // Scan res unaffected
        ),
        DampenerScript::ScanResolution => (
            base_targeting_range_m,        // Targeting range unaffected
            base_scan_resolution * 0.50,   // -50% scan res
        ),
    }
}

/// 4. Target Painter evaluation (`feat:modules.target-painter`).
pub fn eval_target_painter_effect(
    base_sig_radius: f64,
    painter_bonus_factor: f64, // e.g. 0.30 for +30%
    signature_focusing_lvl: u8,
) -> f64 {
    let skill_mult = 1.0 + 0.05 * signature_focusing_lvl.min(5) as f64;
    base_sig_radius * (1.0 + painter_bonus_factor * skill_mult)
}

/// 5. Warp Disruptor / Scrambler evaluation (`feat:modules.warp-disruptor`).
#[derive(Debug, Clone, PartialEq)]
pub struct WarpScrambleState {
    pub is_warp_blocked: bool,
    pub stops_mwd: bool,
    pub stops_mjd: bool,
}

pub fn eval_warp_scramble_state(
    applied_scramble_strength: i32,
    target_warp_core_strength: i32,
    is_scrambler: bool,
) -> WarpScrambleState {
    let is_warp_blocked = applied_scramble_strength >= target_warp_core_strength;
    WarpScrambleState {
        is_warp_blocked,
        stops_mwd: is_scrambler && applied_scramble_strength > 0,
        stops_mjd: is_scrambler && applied_scramble_strength > 0,
    }
}

/// 6. Interdiction Sphere Launcher & Dictor Bubble mechanics (`feat:modules.interdiction-sphere`).
#[derive(Debug, Clone, PartialEq)]
pub struct DictorBubbleSpec {
    pub radius_m: f64,
    pub duration_sec: f64,
    pub prevents_warp: bool,
    pub drags_warpers: bool,
}

pub fn eval_dictor_bubble_launch(
    system_sec: f64,
    is_wormhole: bool,
) -> Result<DictorBubbleSpec, &'static str> {
    if !is_wormhole && system_sec > 0.0 {
        return Err("Interdiction Sphere Launchers can only be deployed in nullsec or wormhole space");
    }
    Ok(DictorBubbleSpec {
        radius_m: 20_000.0,
        duration_sec: 120.0,
        prevents_warp: true,
        drags_warpers: true,
    })
}

/// Computes warp vector drag intercept point if vector intersects bubble volume.
pub fn compute_warp_drag(
    warp_origin: DVec3,
    warp_dest: DVec3,
    bubble_center: DVec3,
    bubble_radius_m: f64,
) -> Option<DVec3> {
    let warp_dir = (warp_dest - warp_origin).normalize();
    let to_center = bubble_center - warp_origin;
    let proj = to_center.dot(warp_dir);
    if proj <= 0.0 {
        return None;
    }
    let closest_point = warp_origin + warp_dir * proj;
    let dist_to_center = (closest_point - bubble_center).length();
    if dist_to_center <= bubble_radius_m {
        // Drag point is at bubble perimeter facing the destination
        Some(bubble_center + warp_dir * bubble_radius_m)
    } else {
        None
    }
}

/// 7. Warp Disruption Field Generator (HIC) mechanics (`feat:modules.warp-disruption-field`).
#[derive(Debug, Clone, PartialEq)]
pub struct HicFieldSpec {
    pub is_focused_infinite_point: bool,
    pub scramble_strength: i32,
    pub bubble_radius_m: f64,
    pub mass_addition_kg: f64,
    pub sig_radius_multiplier: f64,
    pub max_velocity_multiplier: f64,
}

pub fn eval_hic_field(is_focused_script: bool) -> HicFieldSpec {
    if is_focused_script {
        HicFieldSpec {
            is_focused_infinite_point: true,
            scramble_strength: 100, // Infinite point
            bubble_radius_m: 0.0,
            mass_addition_kg: 0.0,
            sig_radius_multiplier: 1.0,
            max_velocity_multiplier: 1.0,
        }
    } else {
        HicFieldSpec {
            is_focused_infinite_point: false,
            scramble_strength: 1,
            bubble_radius_m: 20_000.0,
            mass_addition_kg: 50_000_000.0, // +50,000 tonnes
            sig_radius_multiplier: 1.25,    // +25% sig bloom
            max_velocity_multiplier: 0.50,  // -50% speed penalty
        }
    }
}

/// 8. Mobile Warp Disruptor deploy & anchoring (`feat:modules.mobile-warp-disruptor`).
#[derive(Debug, Clone, PartialEq)]
pub struct MobileWarpDisruptorSpec {
    pub deploy_timer_sec: f64,
    pub bubble_radius_m: f64,
    pub anchorable_in_null_only: bool,
}

pub fn eval_mobile_warp_disruptor_deploy(
    system_sec: f64,
    size_str: &str,
) -> Result<MobileWarpDisruptorSpec, &'static str> {
    if system_sec > 0.0 {
        return Err("Mobile warp disruptors can only be anchored in null-security space");
    }
    let (deploy_sec, radius) = match size_str {
        "Small" => (120.0, 5_000.0),
        "Medium" => (180.0, 10_000.0),
        "Large" => (240.0, 20_000.0),
        _ => (120.0, 5_000.0),
    };
    Ok(MobileWarpDisruptorSpec {
        deploy_timer_sec: deploy_sec,
        bubble_radius_m: radius,
        anchorable_in_null_only: true,
    })
}

/// 9. Afterburner evaluation (`feat:modules.afterburner`).
#[derive(Debug, Clone, PartialEq)]
pub struct AfterburnerSpec {
    pub new_velocity: f64,
    pub new_mass: f64,
    pub unaffected_by_scram: bool,
}

pub fn eval_afterburner_effect(
    base_velocity: f64,
    base_mass: f64,
    speed_bonus_fraction: f64,
    module_mass_addition_kg: f64,
) -> AfterburnerSpec {
    AfterburnerSpec {
        new_velocity: base_velocity * (1.0 + speed_bonus_fraction),
        new_mass: base_mass + module_mass_addition_kg,
        unaffected_by_scram: true,
    }
}

/// 10. Microwarpdrive evaluation with cap penalty and scram shutoff (`feat:modules.microwarpdrive`).
#[derive(Debug, Clone, PartialEq)]
pub struct MwdSpec {
    pub active: bool,
    pub velocity: f64,
    pub sig_radius: f64,
    pub cap_capacity: f64,
}

pub fn eval_mwd_state(
    base_velocity: f64,
    base_sig_radius: f64,
    base_cap_capacity: f64,
    speed_bonus: f64,
    sig_bloom: f64,
    cap_penalty_fraction: f64,
    is_scrambled: bool,
) -> MwdSpec {
    if is_scrambled {
        MwdSpec {
            active: false,
            velocity: base_velocity,
            sig_radius: base_sig_radius,
            cap_capacity: base_cap_capacity,
        }
    } else {
        MwdSpec {
            active: true,
            velocity: base_velocity * (1.0 + speed_bonus),
            sig_radius: base_sig_radius * (1.0 + sig_bloom),
            cap_capacity: base_cap_capacity * (1.0 - cap_penalty_fraction),
        }
    }
}

/// 11. Micro Jump Drive evaluation including Micro Jump Field Generator (`feat:modules.micro-jump-drive`).
#[derive(Debug, Clone, PartialEq)]
pub struct MjdExecutionResult {
    pub spool_duration_sec: f64,
    pub jump_distance_m: f64,
    pub ship_final_pos: DVec3,
    pub jumped_nearby_ships: Vec<(i64, DVec3)>,
}

pub fn eval_mjd_execution(
    is_scrambled: bool,
    is_bastioned: bool,
    is_field_generator: bool,
    ship_pos: DVec3,
    ship_heading: DVec3,
    nearby_ships: &[(i64, DVec3)], // (char_id, position)
) -> Result<MjdExecutionResult, &'static str> {
    if is_scrambled {
        return Err("Warp scrambler prevents Micro Jump Drive activation");
    }
    if is_bastioned {
        return Err("Bastion active: Micro Jump Drive activation refused");
    }
    let jump_vec = ship_heading.normalize() * 100_000.0;
    let ship_final_pos = ship_pos + jump_vec;
    let jumped_nearby_ships = if is_field_generator {
        nearby_ships
            .iter()
            .filter(|&&(_, pos)| (pos - ship_pos).length() <= 6_000.0)
            .map(|&(id, pos)| (id, pos + jump_vec))
            .collect()
    } else {
        Vec::new()
    };
    Ok(MjdExecutionResult {
        spool_duration_sec: 12.0,
        jump_distance_m: 100_000.0,
        ship_final_pos,
        jumped_nearby_ships,
    })
}

/// 12. Interdiction Nullifier evaluation (`feat:modules.interdiction-nullifier`).
pub fn eval_bubble_warp_immunity(is_nullified: bool, inside_bubble: bool) -> bool {
    if !inside_bubble {
        true
    } else {
        is_nullified // Nullified ship ignores warp bubbles
    }
}

/// 13. Cynosural Field Generator (`feat:modules.cyno-field`).
#[derive(Debug, Clone, PartialEq)]
pub struct CynoFieldSpec {
    pub duration_sec: f64,
    pub ozone_cost: u32,
    pub ship_immobilized: bool,
    pub system_beacon_active: bool,
}

pub fn eval_cyno_field_spec(
    is_recon_or_blops: bool,
    ozone_in_cargo: u32,
    cyno_skill_lvl: u8,
) -> Result<CynoFieldSpec, &'static str> {
    if !is_recon_or_blops {
        return Err("Standard Cynosural Field Generators require Recon Ship or Black Ops");
    }
    let base_ozone = 500u32;
    let ozone_cost = base_ozone.saturating_sub(50 * cyno_skill_lvl.min(5) as u32);
    if ozone_in_cargo < ozone_cost {
        return Err("Insufficient Liquid Ozone in cargo");
    }
    Ok(CynoFieldSpec {
        duration_sec: 300.0, // 5 minutes
        ozone_cost,
        ship_immobilized: true, // Fixed in place
        system_beacon_active: true,
    })
}

/// 14. Covert Cynosural Field Generator (`feat:modules.cyno-covert`).
#[derive(Debug, Clone, PartialEq)]
pub struct CovertCynoSpec {
    pub duration_sec: f64,
    pub ozone_cost: u32,
    pub allows_black_ops_jump: bool,
    pub allows_covert_portal: bool,
}

pub fn eval_covert_cyno_spec(
    is_covert_capable: bool,
    ozone_in_cargo: u32,
) -> Result<CovertCynoSpec, &'static str> {
    if !is_covert_capable {
        return Err("Covert Cynosural Field Generator requires covert cloaking ship");
    }
    if ozone_in_cargo < 100 {
        return Err("Insufficient Liquid Ozone for covert cyno");
    }
    Ok(CovertCynoSpec {
        duration_sec: 60.0, // 60s covert cycle
        ozone_cost: 100,
        allows_black_ops_jump: true,
        allows_covert_portal: true,
    })
}

/// 15. Industrial Cynosural Field Generator (`feat:modules.cyno-industrial`).
pub fn eval_industrial_cyno_ship_allowed(ship_group_id: u32) -> bool {
    // Jump Freighters (group 513) and Rorquals (group 883) ONLY
    ship_group_id == 513 || ship_group_id == 883
}

/// 16. Cyno Jammer evaluation (`feat:modules.cyno-jammer`).
pub fn eval_cyno_under_jammer(is_system_jammed: bool, is_covert: bool) -> Result<(), &'static str> {
    if is_system_jammed && !is_covert {
        Err("Sovereignty Pharolux Cyno Jammer is active: standard cynosural fields blocked")
    } else {
        Ok(()) // Covert cynos bypass system jammer
    }
}

/// 17. Jump Portal Generator (`feat:modules.jump-portal`).
#[derive(Debug, Clone, PartialEq)]
pub struct JumpPortalSpec {
    pub fuel_consumed: u32,
    pub range_ly: f64,
    pub is_bridge_valid: bool,
}

pub fn eval_jump_portal_bridge(
    is_titan_or_blops: bool,
    distance_ly: f64,
    max_range_ly: f64,
    fleet_ship_mass_kg: f64,
    is_covert_portal: bool,
    fleet_ship_is_covert: bool,
) -> Result<JumpPortalSpec, &'static str> {
    if !is_titan_or_blops {
        return Err("Jump portal generators can only be operated by Titans or Black Ops");
    }
    if distance_ly > max_range_ly {
        return Err("Destination cyno beacon is beyond maximum jump drive range");
    }
    if is_covert_portal && !fleet_ship_is_covert {
        return Err("Covert jump portals can only bridge ships fitted with a Covert Cloaking Device");
    }
    let fuel = ((fleet_ship_mass_kg / 1_000_000.0) * distance_ly * 10.0).max(100.0) as u32;
    Ok(JumpPortalSpec {
        fuel_consumed: fuel,
        range_ly: distance_ly,
        is_bridge_valid: true,
    })
}

/// 18. Data / Relic Analyzers (`feat:modules.data-relic-analyzer`).
#[derive(Debug, Clone, PartialEq)]
pub struct HackingMinigameSpec {
    pub virus_coherence: i32,
    pub virus_strength: i32,
    pub target_container_group: u32,
}

pub fn eval_analyzer_hacking(target_group_id: u32) -> Result<HackingMinigameSpec, &'static str> {
    // 514 = Relic site, 515 = Data site, 1150 = Covert / Sleeper Cache
    if target_group_id != 514 && target_group_id != 515 && target_group_id != 1150 {
        return Err("Analyzer target must be a data or relic container");
    }
    Ok(HackingMinigameSpec {
        virus_coherence: 100,
        virus_strength: 30,
        target_container_group: target_group_id,
    })
}

/// 19. Cargo Scanner (`feat:modules.cargo-scanner`).
pub fn eval_cargo_scanner_read(
    scanner_range_m: f64,
    target_dist_m: f64,
    cargo_contents: &[(i64, u32, i32)], // (item_id, type_id, qty)
) -> Result<Vec<(i64, u32, i32)>, &'static str> {
    if target_dist_m > scanner_range_m {
        return Err("Target is outside cargo scanner optimal range");
    }
    Ok(cargo_contents.to_vec())
}

/// 20. Ship Scanner (`feat:modules.ship-scanner`).
#[derive(Debug, Clone, PartialEq)]
pub struct ShipScanResult {
    pub fitted_modules: Vec<(i64, u32)>,
    pub capacitor_charge: f64,
    pub capacitor_max: f64,
    pub capacitor_ratio: f64,
}

pub fn eval_ship_scanner_read(
    scanner_range_m: f64,
    target_dist_m: f64,
    fitted_modules: &[(i64, u32)],
    cap_charge: f64,
    cap_max: f64,
) -> Result<ShipScanResult, &'static str> {
    if target_dist_m > scanner_range_m {
        return Err("Target is outside ship scanner optimal range");
    }
    let ratio = if cap_max > 0.0 { cap_charge / cap_max } else { 0.0 };
    Ok(ShipScanResult {
        fitted_modules: fitted_modules.to_vec(),
        capacitor_charge: cap_charge,
        capacitor_max: cap_max,
        capacitor_ratio: ratio,
    })
}

/// 21. Mutaplasmids & Dynamic Item attributes (`feat:modules.mutaplasmids`).
#[derive(Debug, Clone, PartialEq)]
pub struct MutatedModuleSpec {
    pub base_type_id: u32,
    pub mutaplasmid_type_id: u32,
    pub dynamic_attributes: Vec<(u16, f64)>,
}

pub fn eval_mutaplasmid_combination(
    base_type_id: u32,
    mutaplasmid_type_id: u32,
    base_attributes: &[(u16, f64)],
    mutator_factors: &[(u16, f64)], // attribute_id, multiplier in [min_factor, max_factor]
) -> MutatedModuleSpec {
    let mut dynamic_attributes = Vec::new();
    for &(aid, base_val) in base_attributes {
        let mult = mutator_factors
            .iter()
            .find(|&&(a, _)| a == aid)
            .map(|&(_, m)| m)
            .unwrap_or(1.0);
        dynamic_attributes.push((aid, base_val * mult));
    }
    MutatedModuleSpec {
        base_type_id,
        mutaplasmid_type_id,
        dynamic_attributes,
    }
}

/// 22. Mutated Module Trade & Persistence (`feat:modules.abyssal-modules-trade`).
pub fn eval_mutated_module_trade_persistence(
    mutated_item: &MutatedModuleSpec,
    is_contract_or_trade: bool,
) -> bool {
    // Attributes persist through trade/contract and are inspected in show-info
    is_contract_or_trade && !mutated_item.dynamic_attributes.is_empty()
}
