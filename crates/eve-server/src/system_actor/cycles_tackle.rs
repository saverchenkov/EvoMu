//! Module cycle addition and tackle/bubble reconciliation for `system_actor`.

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{BallId, Ballpark, MovementMode};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

/// Returns true when `target_id` is a Sleeper or Drifter NPC immune to capacitor warfare (`#369`).
#[inline]
pub(crate) fn is_sleeper_or_drifter_capacitor_immune(
    ballpark: &Ballpark,
    target_id: BallId,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
) -> bool {
    let Some(b) = ballpark.balls.get(&target_id) else {
        return false;
    };
    if matches!(b.type_id, 30185..=30500 | 37472) {
        return true;
    }
    sde_opt
        .as_deref()
        .and_then(|sde| sde.types.get(&b.type_id))
        .is_some_and(|t| {
            matches!(
                t.group_id,
                959 | 960 | 961 | 982..=987 | 1306 | 1307 | 1529 | 4036
            )
        })
}

pub(crate) fn reconcile_tackle_and_bubbles(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    deactivation_hook: Option<&ModuleDeactivationHook>,
) {
    // 1. Collect all target balls currently subjected to MWD/MJD suppression (Warp Scrambler / Scripted HIC WDFG / Zero-Point Mass Entangler)
    let mut mwd_disabled_targets: Vec<BallId> = Vec::new();
    for cycle in active_module_cycles.iter() {
        if cycle.disables_target_mwd_mjd {
            if let Some(tid) = cycle.target_id {
                if !mwd_disabled_targets.contains(&tid) {
                    mwd_disabled_targets.push(tid);
                }
            }
        }
        if cycle.self_disable_mwd_mjd && !mwd_disabled_targets.contains(&cycle.ship_id) {
            mwd_disabled_targets.push(cycle.ship_id);
        }
    }

    // 2. Sync HIC unscripted warp disruption bubbles to the HIC ship's current position
    for cycle in active_module_cycles.iter() {
        if let Some(radius) = cycle.hic_bubble_radius_m {
            if let Some(ship_ball) = ballpark.balls.get(&cycle.ship_id) {
                ballpark.add_warp_bubble(cycle.module_id, ship_ball.position, radius);
            }
        }
    }

    // 3. Recompute warp_scramble_strength, warp_core_strength, is_mwd_mjd_disabled, disallow_assistance, and is_interdiction_nullified for all balls
    let ball_ids: Vec<BallId> = ballpark.combat_states.keys().copied().collect();
    for ball_id in ball_ids {
        let base_scram = base_scramble_strengths.get(&ball_id).copied().unwrap_or(0);
        let base_core = base_core_strengths.get(&ball_id).copied().unwrap_or(1);

        let mod_scram: i32 = active_module_cycles
            .iter()
            .filter(|c| c.target_id == Some(ball_id))
            .map(|c| c.warp_scramble_points)
            .sum::<i32>()
            + active_module_cycles
                .iter()
                .filter(|c| c.ship_id == ball_id && (c.immobilize_self || c.self_disallow_assistance))
                .map(|_| 100)
                .sum::<i32>();
        let mod_core: i32 = active_module_cycles
            .iter()
            .filter(|c| c.ship_id == ball_id)
            .map(|c| c.warp_core_stab_bonus)
            .sum();
        let mwd_disabled = active_module_cycles
            .iter()
            .any(|c| {
                (c.target_id == Some(ball_id) && c.disables_target_mwd_mjd)
                    || (c.ship_id == ball_id && c.self_disable_mwd_mjd)
            });
        let disallow_assist = active_module_cycles.iter().any(|c| {
            c.ship_id == ball_id && (c.hic_bubble_radius_m.is_some() || c.self_disallow_assistance)
        });
        let ewar_immune = active_module_cycles
            .iter()
            .any(|c| c.ship_id == ball_id && c.self_ewar_immune);
        let nullified_by_mod = active_module_cycles
            .iter()
            .any(|c| c.ship_id == ball_id && c.grants_interdiction_nullification);
        let nullified_by_hull = ballpark
            .ball_slim_meta
            .get(&ball_id)
            .is_some_and(|m| m.group_id == 31);

        if let Some(cs) = ballpark.combat_states.get_mut(&ball_id) {
            let is_protected = cs.is_invulnerable()
                || cs.undock_invulnerable_remaining_ticks > 0
                || cs.gate_cloak_remaining_ticks > 0
                || cs.is_cloaked
                || cs.is_tethered;
            let lance_scram = if cs.lance_disrupted_remaining_s > 0.0 && !is_protected { 100 } else { 0 };
            cs.warp_scramble_strength = base_scram + mod_scram + lance_scram;
            cs.warp_core_strength = base_core + mod_core;
            cs.is_mwd_mjd_disabled = mwd_disabled;
            cs.disallow_assistance = disallow_assist;
            if ewar_immune {
                cs.ecm_jammed_remaining_s = 0.0;
            }
            if nullified_by_mod || nullified_by_hull {
                cs.is_interdiction_nullified = true;
            }
        }
    }

    // 4. Immediately shut down any active MWD or MJD cycle on MWD/MJD-disabled targets (Warp Scrambler / Scripted HIC WDFG / ESS field)
    let reentrant_db = db_opt
        .as_ref()
        .is_some_and(|db| db.is_write_locked_by_current_thread());
    let effective_db = if reentrant_db { &None } else { db_opt };
    let mut aborted_cycles = Vec::new();
    for idx in (0..active_module_cycles.len()).rev() {
        let ship_id = active_module_cycles[idx].ship_id;
        let mjd_or_mwd_disabled = mwd_disabled_targets.contains(&ship_id)
            || ballpark
                .combat_states
                .get(&ship_id)
                .is_some_and(|cs| cs.is_mwd_mjd_disabled);
        let stop_mwd_or_mjd = (active_module_cycles[idx].is_mwd_or_mjd
            || active_module_cycles[idx].mjd_jump_distance_m.is_some())
            && mjd_or_mwd_disabled;
        if stop_mwd_or_mjd {
            let shut_down = active_module_cycles.remove(idx);
            cleanup_removed_module_cycle(ballpark, &shut_down, effective_db);
            aborted_cycles.push(shut_down);
        }
    }
    if !reentrant_db {
        for shut_down in &aborted_cycles {
            emit_cycle_deactivation(shut_down, ballpark, effective_db, deactivation_hook);
        }
    }
    let warp_stop_ids: Vec<BallId> = ballpark
        .balls
        .iter()
        .filter(|(&id, b)| {
            if !matches!(
                b.mode,
                MovementMode::Warp {
                    phase: eve_destiny::WarpPhase::Aligning,
                    ..
                }
            ) {
                return false;
            }
            ballpark.is_ball_scrambled(id)
        })
        .map(|(&id, _)| id)
        .collect();
    for id in warp_stop_ids {
        if let Some(ball) = ballpark.balls.get_mut(&id) {
            ball.velocity = DVec3::ZERO;
        }
        let _ = ballpark.set_mode(id, MovementMode::Stop);
    }

    // 5. Recompute deterministic ship max_velocity from unboosted base velocity,
    //    active self propulsion / HIC WDFG / cloak / immobilizer multipliers,
    //    and stacking-penalized Stasis Webifier / Grappler / Web Probe factors (#438).
    let wubble_mults =
        crate::services::tactical_modules::active_wubble_multipliers_for_ballpark(ballpark);
    let mut vel_ships: std::collections::BTreeSet<BallId> =
        ballpark.base_webbed_max_velocity.keys().copied().collect();
    for &tid in wubble_mults.keys() {
        vel_ships.insert(tid);
    }
    for c in active_module_cycles.iter() {
        if c.immobilize_self
            || c.hic_velocity_multiplier.is_some()
            || c.speed_boost_factor.abs() > 1e-9
        {
            vel_ships.insert(c.ship_id);
        }
        if c.target_velocity_multiplier.is_some() {
            if let Some(tid) = c.target_id {
                vel_ships.insert(tid);
            }
        }
    }
    for sid in vel_ships {
        reconcile_velocity_for_ship(ballpark, sid, active_module_cycles, &wubble_mults);
    }
}

/// Computes a ship's deterministic `max_velocity` from its unboosted `base_v` and all active
/// self/targeted velocity modifiers (`#438`).
pub(crate) fn compute_deterministic_max_velocity(
    base_v: f64,
    ship_id: BallId,
    active_module_cycles: &[ActiveModuleCycle],
    wubble_mults: &HashMap<BallId, f64>,
) -> (f64, bool) {
    let mut has_any_modifier = false;
    let mut immobilized = false;
    let mut prop_mult = 1.0_f64;
    let mut hic_mult = 1.0_f64;
    let mut cloak_mult: Option<f64> = None;

    for c in active_module_cycles {
        if c.ship_id != ship_id {
            continue;
        }
        if c.immobilize_self {
            has_any_modifier = true;
            immobilized = true;
        }
        if let Some(hm) = c.hic_velocity_multiplier {
            has_any_modifier = true;
            hic_mult *= hm.max(0.0);
        }
        if c.speed_boost_factor.abs() > 1e-9 {
            has_any_modifier = true;
            let factor = (1.0 + c.speed_boost_factor).max(0.0);
            if c.effect_name.starts_with("cloaking") {
                cloak_mult = Some(cloak_mult.unwrap_or(1.0) * factor);
            } else {
                prop_mult *= factor;
            }
        }
    }

    let target_ewar_immune = active_module_cycles
        .iter()
        .any(|c| c.ship_id == ship_id && c.self_ewar_immune);
    let mut penalties: Vec<f64> = if target_ewar_immune {
        Vec::new()
    } else {
        let mut p_vec: Vec<f64> = active_module_cycles
            .iter()
            .filter_map(|c| {
                if c.target_id == Some(ship_id) {
                    c.target_velocity_multiplier.map(|m| (1.0 - m).max(0.0))
                } else {
                    None
                }
            })
            .collect();
        if let Some(&wm) = wubble_mults.get(&ship_id) {
            p_vec.push((1.0 - wm).max(0.0));
        }
        p_vec
    };

    let web_mult = if penalties.is_empty() {
        1.0
    } else {
        has_any_modifier = true;
        penalties.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let mut combined = 1.0_f64;
        for (rank, p) in penalties.into_iter().enumerate() {
            let sp = (-((rank as f64) / 2.67).powi(2)).exp();
            combined *= (1.0 - p * sp).clamp(0.01, 1.0);
        }
        combined
    };

    let self_mult = if immobilized {
        0.0
    } else if let Some(cm) = cloak_mult {
        cm * hic_mult
    } else {
        prop_mult * hic_mult
    };

    ((base_v * self_mult * web_mult).max(0.0), has_any_modifier)
}

/// Recomputes and applies `ball.max_velocity` for `ship_id` from its true unboosted base velocity
/// stored in `ballpark.base_webbed_max_velocity` (`#438`).
pub(crate) fn reconcile_velocity_for_ship(
    ballpark: &mut Ballpark,
    ship_id: BallId,
    active_module_cycles: &[ActiveModuleCycle],
    wubble_mults: &HashMap<BallId, f64>,
) {
    let Some(current_v) = ballpark.balls.get(&ship_id).map(|b| b.max_velocity) else {
        ballpark.base_webbed_max_velocity.remove(&ship_id);
        return;
    };
    let cloak_orig = ballpark
        .combat_states
        .get(&ship_id)
        .and_then(|cs| cs.cloak_original_max_velocity)
        .filter(|&v| v > 0.0);
    let cycle_orig = active_module_cycles
        .iter()
        .filter(|c| c.ship_id == ship_id)
        .filter_map(|c| c.original_max_velocity)
        .find(|&v| v > 0.0);
    let base_v = *ballpark
        .base_webbed_max_velocity
        .entry(ship_id)
        .and_modify(|v| {
            if *v <= 0.0 {
                if let Some(valid) = cloak_orig
                    .or(cycle_orig)
                    .or((current_v > 0.0).then_some(current_v))
                {
                    *v = valid;
                }
            }
        })
        .or_insert_with(|| cloak_orig.or(cycle_orig).unwrap_or(current_v));

    let (target_v, has_any_modifier) =
        compute_deterministic_max_velocity(base_v, ship_id, active_module_cycles, wubble_mults);

    if let Some(ball) = ballpark.balls.get_mut(&ship_id) {
        ball.max_velocity = target_v;
    }
    if !has_any_modifier {
        ballpark.base_webbed_max_velocity.remove(&ship_id);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn on_target_unlocked_in_actor(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    source_id: BallId,
    target_id_opt: Option<BallId>,
    deactivation_hook: Option<&ModuleDeactivationHook>,
) {
    let mut removed_cycles = Vec::new();
    for idx in (0..active_module_cycles.len()).rev() {
        let matches_target = match target_id_opt {
            Some(tid) => active_module_cycles[idx].target_id == Some(tid),
            None => active_module_cycles[idx].target_id.is_some(),
        };
        if active_module_cycles[idx].ship_id == source_id && matches_target {
            let removed = active_module_cycles.swap_remove(idx);
            cleanup_removed_module_cycle(ballpark, &removed, db_opt);
            removed_cycles.push(removed);
        }
    }
    if !removed_cycles.is_empty() {
        reconcile_tackle_and_bubbles(
            ballpark,
            active_module_cycles,
            base_scramble_strengths,
            base_core_strengths,
            db_opt,
            deactivation_hook,
        );
        for removed in &removed_cycles {
            emit_cycle_deactivation(removed, ballpark, db_opt, deactivation_hook);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_add_module_cycle(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    mut cycle: ActiveModuleCycle,
) -> Result<(), String> {
    if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
        cs.undock_invulnerable_remaining_ticks = 0;
    }
    if cycle.is_mwd_or_mjd || cycle.mjd_jump_distance_m.is_some() {
        let disabled = ballpark
            .combat_states
            .get(&cycle.ship_id)
            .is_some_and(|cs| cs.is_mwd_mjd_disabled);
        if disabled {
            return Err("WarpScrambled: Propulsion module disabled by warp scrambler".to_string());
        }
    }
    // Remove any existing cycle for this (ship_id, module_id) before re-adding
    // Covers: mutadaptive spool preservation, emergency hull energizer, stasis grappler
    if let Some(pos) = active_module_cycles
        .iter()
        .position(|c| c.ship_id == cycle.ship_id && c.module_id == cycle.module_id)
    {
        let prev = active_module_cycles.remove(pos);
        if prev.mutadaptive_spool_step > 0.0 && prev.target_id == cycle.target_id {
            cycle.current_spool_cycles = prev.current_spool_cycles;
        }
        cleanup_removed_module_cycle(ballpark, &prev, db_opt);
    }
    if let Some(eff_cap_max) = cycle.effective_max_capacitor {
        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
            cs.max_capacitor = eff_cap_max;
            cs.capacitor = cs.capacitor.min(cs.max_capacitor);
        }
    }
    if cycle.cap_need_per_cycle > 0.0 {
        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
            if cs.capacitor < cycle.cap_need_per_cycle {
                return Err("Insufficient capacitor to activate module".to_string());
            }
            cs.capacitor = (cs.capacitor - cycle.cap_need_per_cycle).max(0.0);
        }
    }
    if cycle.cap_booster_inject_amount > 0.0 {
        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
            cs.capacitor = (cs.capacitor + cycle.cap_booster_inject_amount)
                .min(cs.max_capacitor);
        }
    }
    if cycle.shield_boost_amount > 0.0 {
        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
            let boost_mult = if cs.shield_boost_multiplier > 0.0 { cs.shield_boost_multiplier } else { 1.0 };
            cs.shield_hp = (cs.shield_hp + cycle.shield_boost_amount * boost_mult)
                .min(cs.max_shield_hp);
        }
    }
    if let Some(target_id) = cycle.target_id {
        let is_sleeper_target =
            is_sleeper_or_drifter_capacitor_immune(ballpark, target_id, sde_opt);
        if let Some(t_cs) = ballpark.combat_states.get_mut(&target_id) {
            let mut att = 1.0;
            if cycle.remote_falloff > 0.0 || cycle.remote_optimal > 0.0 {
                if let (Some(sb), Some(tb)) = (ballpark.balls.get(&cycle.ship_id), ballpark.balls.get(&target_id)) {
                    let dist = (sb.position - tb.position).length();
                    att = eve_dogma::compute_falloff_with_cutoff(dist, cycle.remote_optimal, cycle.remote_falloff);
                }
            }
            if cycle.cap_drain_target > 0.0 && !is_sleeper_target {
                t_cs.capacitor =
                    (t_cs.capacitor - cycle.cap_drain_target * att).max(0.0);
            }
            if !t_cs.disallow_assistance {
                if cycle.remote_shield_amount > 0.0 {
                    let impedance = if is_capital_remote_repair_module(cycle.module_type_id, sde_opt) {
                        let cap_rank = active_module_cycles
                            .iter()
                            .filter(|c| {
                                c.target_id == Some(target_id)
                                    && c.remote_shield_amount > 0.0
                                    && is_capital_remote_repair_module(c.module_type_id, sde_opt)
                            })
                            .count();
                        1.0 / (1.0 + 0.25 * (cap_rank as f64))
                    } else {
                        1.0
                    };
                    t_cs.shield_hp = (t_cs.shield_hp + cycle.remote_shield_amount * att * impedance).min(t_cs.max_shield_hp);
                }
                if cycle.remote_cap_amount > 0.0 {
                    t_cs.capacitor = (t_cs.capacitor + cycle.remote_cap_amount * att).min(t_cs.max_capacitor);
                }
            }
        }
    }
    let has_vel_mod = cycle.immobilize_self
        || cycle.hic_velocity_multiplier.is_some()
        || cycle.speed_boost_factor.abs() > 1e-9;
    if has_vel_mod {
        let cloak_orig = ballpark
            .combat_states
            .get(&cycle.ship_id)
            .and_then(|cs| cs.cloak_original_max_velocity)
            .filter(|&v| v > 0.0);
        let existing_cycle_orig = active_module_cycles
            .iter()
            .filter(|c| c.ship_id == cycle.ship_id)
            .filter_map(|c| c.original_max_velocity)
            .find(|&v| v > 0.0);
        let ball_v_opt = ballpark
            .balls
            .get(&cycle.ship_id)
            .map(|b| b.max_velocity)
            .filter(|&v| v > 0.0);
        if let Some(resolved_v) = cloak_orig.or(existing_cycle_orig).or(ball_v_opt) {
            let base_v = *ballpark
                .base_webbed_max_velocity
                .entry(cycle.ship_id)
                .and_modify(|v| {
                    if *v <= 0.0 {
                        *v = resolved_v;
                    }
                })
                .or_insert(resolved_v);
            if cycle.original_max_velocity.is_none_or(|v| v <= 0.0) {
                cycle.original_max_velocity = Some(base_v);
            }
            if cycle.effect_name.starts_with("cloaking") {
                ballpark
                    .combat_states
                    .entry(cycle.ship_id)
                    .or_default()
                    .cloak_original_max_velocity = Some(base_v);
            }
        }
        if cycle.immobilize_self {
            if let Some(ball) = ballpark.balls.get_mut(&cycle.ship_id) {
                ball.max_velocity = 0.0;
                ball.velocity = DVec3::ZERO;
                ball.speed_fraction = 0.0;
                ball.mode = MovementMode::Stop;
            }
        }
    }
    if let Some(mass_mult) = cycle.self_mass_multiplier {
        if let Some(ball) = ballpark.balls.get_mut(&cycle.ship_id) {
            cycle.original_mass = Some(ball.mass);
            ball.mass *= mass_mult;
        }
    } else if let Some(mass_add) = cycle.hic_mass_addition {
        if let Some(ball) = ballpark.balls.get_mut(&cycle.ship_id) {
            cycle.original_mass = Some(ball.mass);
            ball.mass += mass_add;
        }
    }
    if let Some(sig_mult) = cycle.hic_sig_multiplier {
        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
            cycle.original_signature_radius = Some(cs.signature_radius);
            cs.signature_radius *= sig_mult;
        }
    }
    if cycle.is_adc {
        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
            cycle.adc_saved_resists =
                Some((cs.shield_resonance, cs.armor_resonance, cs.structure_resonance));
            cs.shield_resonance = [0.25; 4];
            cs.armor_resonance = [0.25; 4];
            cs.structure_resonance = [0.25; 4];
        }
        if let Some(db_ref) = db_opt {
            db_ref.with_mut_snapshot(|snap| {
                let entry = snap.transient_item_attributes.entry(cycle.ship_id).or_default();
                for aid in [267, 268, 269, 270, 271, 272, 273, 274, 109, 110, 111, 112] {
                    entry.insert(aid, 0.25);
                }
            });
        }
    } else if let Some(ehe_res) = cycle.ehe_hull_resists {
        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
            cycle.adc_saved_resists =
                Some((cs.shield_resonance, cs.armor_resonance, cs.structure_resonance));
            cs.structure_resonance = ehe_res;
        }
        if let Some(db_ref) = db_opt {
            db_ref.with_mut_snapshot(|snap| {
                let entry = snap.transient_item_attributes.entry(cycle.ship_id).or_default();
                entry.insert(113, ehe_res[0]);
                entry.insert(111, ehe_res[1]);
                entry.insert(109, ehe_res[2]);
                entry.insert(110, ehe_res[3]);
            });
        }
    }
    if cycle.is_rah {
        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
            cs.armor_resonance = [0.85; 4];
        }
        if let Some(db_ref) = db_opt {
            db_ref.with_mut_snapshot(|snap| {
                let m_entry = snap.transient_item_attributes.entry(cycle.module_id).or_default();
                for aid in 267..=270 {
                    m_entry.insert(aid, 0.85);
                }
                let s_entry = snap.transient_item_attributes.entry(cycle.ship_id).or_default();
                for aid in 267..=270 {
                    s_entry.insert(aid, 0.85);
                }
            });
        }
    }
    if cycle.grants_interdiction_nullification {
        ballpark.set_interdiction_nullified(cycle.ship_id, true);
    }
    active_module_cycles.push(cycle);
    reconcile_tackle_and_bubbles(
        ballpark,
        active_module_cycles,
        base_scramble_strengths,
        base_core_strengths,
        db_opt,
        deactivation_hook,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_update_module_overload(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
    _sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    ship_id: BallId,
    module_id: i64,
    is_overloaded: bool,
) -> bool {
    let mut updated = false;
    for c in active_module_cycles.iter_mut() {
        if c.ship_id != ship_id || c.module_id != module_id {
            continue;
        }
        // Per EVE Overheating rules (`Overheating.md`), toggling overload on an already-active
        // module takes effect at the next cycle boundary rather than mid-cycle.
        c.pending_overloaded = if c.is_overloaded == is_overloaded {
            None
        } else {
            Some(is_overloaded)
        };
        updated = true;
    }
    if updated {
        reconcile_tackle_and_bubbles(
            ballpark,
            active_module_cycles,
            base_scramble_strengths,
            base_core_strengths,
            db_opt,
            deactivation_hook,
        );
    }
    updated
}

pub(crate) fn apply_overload_transition_to_cycle(
    c: &mut ActiveModuleCycle,
    is_overloaded: bool,
    ballpark: &mut Ballpark,
    sde_opt: &Option<Arc<SdeRegistry>>,
) {
    let ship_id = c.ship_id;
    if c.is_overloaded != is_overloaded {
            let mt_opt = sde_opt.as_ref().and_then(|sde| sde.types.get(&c.module_type_id));
            let has_cat5 = match (sde_opt.as_ref(), mt_opt) {
                (Some(sde), Some(mt)) => mt
                    .effect_ids
                    .iter()
                    .any(|eid| sde.effects.get(eid).is_some_and(|e| e.effect_category == 5)),
                _ => false,
            };
            let apply_scale = |val: &mut f64, mult: f64| {
                if (mult - 1.0).abs() > 1e-9 && mult > 0.0 {
                    if is_overloaded {
                        *val *= mult;
                    } else {
                        *val /= mult;
                    }
                }
            };

            if c.speed_boost_factor > 0.0 {
                let ovl_speed = mt_opt
                    .and_then(|t| t.default_attributes.get(&1223).copied())
                    .map(|pct| (1.0 + pct / 100.0).max(0.1))
                    .unwrap_or(1.50);
                apply_scale(&mut c.speed_boost_factor, ovl_speed);
                if !ballpark.base_webbed_max_velocity.contains_key(&ship_id) {
                    if let Some(orig_v) = c.original_max_velocity {
                        if let Some(ball) = ballpark.balls.get_mut(&ship_id) {
                            ball.max_velocity = orig_v * (1.0 + c.speed_boost_factor);
                        }
                    }
                }
            }

            // SDE cycle-duration overload attributes: `1206` (`overloadSelfDurationBonus`, `-15%` repairers),
            // `1205` (`overloadRofBonus`, `-15%` turrets/launchers), `1181` (`overloadDurationBonus`).
            if let Some(dur_pct) = mt_opt.and_then(|t| {
                t.default_attributes
                    .get(&1206)
                    .copied()
                    .or_else(|| t.default_attributes.get(&1205).copied())
                    .or_else(|| t.default_attributes.get(&1181).copied())
            }) {
                let dur_mult = (1.0 + dur_pct / 100.0).max(0.1);
                apply_scale(&mut c.duration_s, dur_mult);
            }

            // SDE `1230` (`overloadArmorDamageAmount`, `+10%`) for armor / remote-armor / AAR repairers,
            // `1231` (`overloadShieldBonus`, `+10%`) for shield / remote-shield boosters,
            // and `1.20` fallback only when no category-5 overload Dogma effect exists on the module.
            let armor_mult = mt_opt
                .and_then(|t| {
                    t.default_attributes.get(&1230).copied().or_else(|| {
                        t.default_attributes
                            .get(&1210)
                            .copied()
                            .filter(|_| matches!(t.group_id, 62 | 325 | 2018 | 1199 | 1698))
                    })
                })
                .map(|pct| (1.0 + pct / 100.0).max(0.1))
                .unwrap_or(if has_cat5 { 1.0 } else { 1.20 });
            let shield_mult = mt_opt
                .and_then(|t| {
                    t.default_attributes.get(&1231).copied().or_else(|| {
                        t.default_attributes
                            .get(&1208)
                            .copied()
                            .filter(|_| matches!(t.group_id, 40 | 41 | 1156 | 1697))
                    })
                })
                .map(|pct| (1.0 + pct / 100.0).max(0.1))
                .unwrap_or(if has_cat5 { 1.0 } else { 1.20 });
            let hull_mult = mt_opt
                .and_then(|t| t.default_attributes.get(&1230).copied())
                .map(|pct| (1.0 + pct / 100.0).max(0.1))
                .unwrap_or(if has_cat5 { 1.0 } else { 1.20 });

            apply_scale(&mut c.armor_repair_amount, armor_mult);
            apply_scale(&mut c.remote_armor_amount, armor_mult);
            apply_scale(&mut c.aar_base_armor_repair, armor_mult);
            apply_scale(&mut c.shield_boost_amount, shield_mult);
            apply_scale(&mut c.remote_shield_amount, shield_mult);
            apply_scale(&mut c.hull_repair_amount, hull_mult);
            apply_scale(&mut c.remote_hull_amount, hull_mult);

            // SDE `1210` (`overloadDamageModifier`, e.g. `+15%` turrets) on weapon volleys
            if let Some(dmg_pct) = mt_opt.and_then(|t| t.default_attributes.get(&1210).copied()) {
                let wep_mult = (1.0 + dmg_pct / 100.0).max(0.1);
                apply_scale(&mut c.damage_vector.em, wep_mult);
                apply_scale(&mut c.damage_vector.thermal, wep_mult);
                apply_scale(&mut c.damage_vector.kinetic, wep_mult);
                apply_scale(&mut c.damage_vector.explosive, wep_mult);
            }

            // SDE `1222` (`overloadRangeBonus`, e.g. `+300%` Grappler, `+30%` Web, `+20%` Scram)
            if let Some(rng_pct) = mt_opt.and_then(|t| t.default_attributes.get(&1222).copied()) {
                let rng_mult = (1.0 + rng_pct / 100.0).max(0.1);
                if c.grappler_optimal_m > 0.0 {
                    apply_scale(&mut c.grappler_optimal_m, rng_mult);
                    c.max_effective_range_m =
                        Some(c.grappler_optimal_m + 3.0 * c.grappler_falloff_m.max(0.0));
                    if let Some(tid) = c.target_id {
                        if let (Some(sb), Some(tb)) = (ballpark.balls.get(&ship_id), ballpark.balls.get(&tid)) {
                            let dist = (sb.position - tb.position).length();
                            let att = eve_dogma::compute_falloff_attenuation(
                                dist,
                                c.grappler_optimal_m,
                                c.grappler_falloff_m,
                            );
                            let eff_sf = if dist <= c.grappler_optimal_m + 3.0 * c.grappler_falloff_m.max(1.0) {
                                c.grappler_speed_factor_pct * att
                            } else {
                                0.0
                            };
                            c.target_velocity_multiplier =
                                Some((1.0 + eff_sf / 100.0).clamp(0.05, 1.0));
                        }
                    }
                } else if let Some(ref mut max_r) = c.max_effective_range_m {
                    if c.remote_falloff == 0.0 {
                        apply_scale(max_r, rng_mult);
                    }
                }
                if c.remote_optimal > 0.0 {
                    apply_scale(&mut c.remote_optimal, rng_mult);
                }
            }
        }
        c.is_overloaded = is_overloaded;
}

pub(super) fn apply_destiny_event_in_actor(
    ballpark: &mut Ballpark,
    ball_id: BallId,
    event: DestinyEvent,
) -> bool {
    let known = ballpark.balls.contains_key(&ball_id)
        || matches!(event, DestinyEvent::OnSovereigntyChanged { .. });
    if known {
        let scope = ballpark.balls.contains_key(&ball_id).then_some(ball_id);
        match event {
            DestinyEvent::SetBallFree {
                ball_id: id,
                is_free,
            } if id == ball_id => {
                ballpark.set_ball_free(id, is_free);
            }
            DestinyEvent::SetBallHarmonic {
                ball_id: id,
                harmonic,
            } if id == ball_id => {
                ballpark.set_ball_harmonic(id, harmonic);
            }
            DestinyEvent::SetBallInteractive {
                ball_id: id,
                is_interactive,
            } if id == ball_id => {
                ballpark.set_ball_interactive(id, is_interactive);
            }
            DestinyEvent::SetBallRadius {
                ball_id: id,
                radius,
            } if id == ball_id => {
                ballpark.set_ball_radius(id, radius);
            }
            DestinyEvent::OnSovereigntyChanged { alliance_id, .. } => {
                ballpark.set_sovereignty(alliance_id);
            }
            DestinyEvent::OnClientControllerEvent {
                ball_id: id,
                target_id,
                event_name,
                value,
                delay,
            } if id == ball_id => {
                ballpark.emit_client_controller_event(id, target_id, event_name, value, delay);
            }
            DestinyEvent::OnShipStateUpdate { module_states } => {
                ballpark.update_ship_module_states(ball_id, module_states);
            }
            DestinyEvent::OnDbuffUpdated {
                ball_id: id,
                dbuffs,
            } if id == ball_id => {
                ballpark.update_dbuffs(id, dbuffs);
            }
            DestinyEvent::OnDotVictimUpdated {
                ball_id: id,
                flat_dps,
                pct_dps,
                remaining_ticks,
            } if id == ball_id => {
                ballpark.apply_breacher_dot(id, flat_dps, pct_dps, remaining_ticks);
            }
            DestinyEvent::OnDroneStateChange {
                drone_id,
                owner_id,
                controller_id,
                activity_state,
                type_id,
                controller_owner_id,
                target_id,
            } if drone_id == ball_id => {
                ballpark.update_drone_state(
                    drone_id,
                    owner_id,
                    controller_id,
                    activity_state,
                    type_id,
                    controller_owner_id,
                    target_id,
                );
            }
            other => {
                if let DestinyEvent::OnSlimItemChange { ball_id: slim_id } = &other {
                    if let Some(meta) = ballpark.ball_slim_meta.get_mut(slim_id) {
                        meta.char_id = (*slim_id == ball_id).then_some(meta.owner_id);
                    }
                }
                ballpark.emit_event(scope, other);
            }
        }
    }
    known
}

#[cfg(test)]
#[path = "cycles_tackle_tests.rs"]
mod cycles_tackle_tests;
