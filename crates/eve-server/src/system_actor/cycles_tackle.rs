//! Module cycle addition and tackle/bubble reconciliation for `system_actor`.

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{BallId, Ballpark, MovementMode, TargetLock};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) fn reconcile_tackle_and_bubbles(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
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

        if let Some(cs) = ballpark.combat_states.get_mut(&ball_id) {
            cs.warp_scramble_strength = base_scram + mod_scram;
            cs.warp_core_strength = base_core + mod_core;
            cs.is_mwd_mjd_disabled = mwd_disabled;
            cs.disallow_assistance = disallow_assist;
            if ewar_immune {
                cs.ecm_jammed_remaining_s = 0.0;
            }
            if nullified_by_mod {
                cs.is_interdiction_nullified = true;
            }
        }
    }

    // 4. Immediately shut down any active MWD or MJD cycle on scrambled targets (or MJD spool on any bubble/point-scrambled ship)
    let mut idx = 0;
    while idx < active_module_cycles.len() {
        let ship_id = active_module_cycles[idx].ship_id;
        let is_target_scrambled = ballpark.is_ball_scrambled(ship_id)
            || ballpark.combat_states.get(&ship_id).is_some_and(|cs| cs.is_mwd_mjd_disabled);
        let stop_mwd_or_mjd = active_module_cycles[idx].is_mwd_or_mjd
            && (mwd_disabled_targets.contains(&ship_id) || is_target_scrambled);
        let stop_mjd_from_scram = active_module_cycles[idx].mjd_jump_distance_m.is_some()
            && is_target_scrambled;
        if stop_mwd_or_mjd || stop_mjd_from_scram {
            let shut_down = active_module_cycles.remove(idx);
            cleanup_removed_module_cycle(ballpark, &shut_down, &None);
        } else {
            idx += 1;
        }
    }
    let warp_stop_ids: Vec<BallId> = ballpark
        .balls
        .iter()
        .filter(|(&id, b)| matches!(b.mode, MovementMode::Warp { .. }) && ballpark.is_ball_scrambled(id))
        .map(|(&id, _)| id)
        .collect();
    for id in warp_stop_ids {
        if let Some(ball) = ballpark.balls.get_mut(&id) {
            ball.velocity = DVec3::ZERO;
            ball.speed_fraction = 0.0;
            ball.mode = MovementMode::Stop;
        }
    }
}

pub(crate) fn handle_add_module_cycle(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    mut cycle: ActiveModuleCycle,
) -> Result<(), String> {
    if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
        cs.undock_invulnerable_remaining_ticks = 0;
    }
    if cycle.is_mwd_or_mjd {
        let disabled = ballpark
            .combat_states
            .get(&cycle.ship_id)
            .is_some_and(|cs| cs.is_mwd_mjd_disabled)
            || (cycle.mjd_jump_distance_m.is_some()
                && ballpark.is_ball_scrambled(cycle.ship_id));
        if disabled {
            return Err("WarpScrambled: Propulsion module disabled by warp scrambler or warp disruption bubble".to_string());
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
            cs.shield_hp = (cs.shield_hp + cycle.shield_boost_amount)
                .min(cs.max_shield_hp);
        }
    }
    if let Some(target_id) = cycle.target_id {
        let is_sleeper_target = ballpark
            .balls
            .get(&target_id)
            .is_some_and(|b| matches!(b.type_id, 30185..=30500));
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
    if cycle.immobilize_self {
        if let Some(ball) = ballpark.balls.get_mut(&cycle.ship_id) {
            cycle.original_max_velocity = Some(ball.max_velocity);
            ball.max_velocity = 0.0;
            ball.velocity = DVec3::ZERO;
            ball.speed_fraction = 0.0;
            ball.mode = MovementMode::Stop;
        }
    } else if let Some(vel_mult) = cycle.hic_velocity_multiplier {
        if let Some(ball) = ballpark.balls.get_mut(&cycle.ship_id) {
            cycle.original_max_velocity = Some(ball.max_velocity);
            ball.max_velocity *= vel_mult;
        }
    } else if cycle.speed_boost_factor > 0.0 {
        if let Some(ball) = ballpark.balls.get_mut(&cycle.ship_id) {
            if cycle.original_max_velocity.is_none() {
                cycle.original_max_velocity = Some(ball.max_velocity);
                ball.max_velocity *= 1.0 + cycle.speed_boost_factor;
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
    if let Some(tgt_vel_mult) = cycle.target_velocity_multiplier {
        if let Some(target_id) = cycle.target_id {
            let target_ewar_immune = active_module_cycles
                .iter()
                .any(|c| c.ship_id == target_id && c.self_ewar_immune);
            if !target_ewar_immune {
                if let Some(t_ball) = ballpark.balls.get_mut(&target_id) {
                    cycle.target_original_max_velocity = Some(t_ball.max_velocity);
                    t_ball.max_velocity *= tgt_vel_mult;
                }
            }
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
                if let Some(s_item) = snap.items.get_mut(&cycle.ship_id) {
                    for aid in [267, 268, 269, 270, 271, 272, 273, 274, 109, 110, 111, 112] {
                        s_item.dynamic_attributes.insert(aid, 0.25);
                    }
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
                if let Some(s_item) = snap.items.get_mut(&cycle.ship_id) {
                    s_item.dynamic_attributes.insert(113, ehe_res[0]);
                    s_item.dynamic_attributes.insert(111, ehe_res[1]);
                    s_item.dynamic_attributes.insert(109, ehe_res[2]);
                    s_item.dynamic_attributes.insert(110, ehe_res[3]);
                }
            });
        }
    }
    if cycle.is_rah {
        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
            cs.armor_resonance = [0.85; 4];
        }
        if let Some(db_ref) = db_opt {
            db_ref.with_mut_snapshot(|snap| {
                if let Some(m_item) = snap.items.get_mut(&cycle.module_id) {
                    for aid in 267..=270 {
                        m_item.dynamic_attributes.insert(aid, 0.85);
                    }
                }
                if let Some(s_item) = snap.items.get_mut(&cycle.ship_id) {
                    for aid in 267..=270 {
                        s_item.dynamic_attributes.insert(aid, 0.85);
                    }
                }
            });
        }
    }
    if cycle.is_auto_targeter {
        if let Some(ship_ball) = ballpark.balls.get(&cycle.ship_id) {
            let ship_pos = ship_ball.position;
            let existing_locks = ballpark.get_target_locks(cycle.ship_id);
            let mut lock_count = existing_locks.len();
            let candidates: Vec<BallId> = ballpark
                .balls
                .values()
                .filter(|b| {
                    b.id != cycle.ship_id
                        && !ballpark.is_static_ball(b.id)
                        && (b.position - ship_pos).length()
                            <= cycle.auto_target_max_range
                        && !existing_locks.iter().any(|l| l.target_id == b.id)
                })
                .map(|b| b.id)
                .collect();
            for cand_id in candidates {
                if lock_count >= cycle.auto_target_max_locks {
                    break;
                }
                ballpark.add_target_lock(
                    cycle.ship_id,
                    TargetLock {
                        target_id: cand_id,
                        lock_time_ms: 0.0,
                        remaining_lock_ms: 0.0,
                        locked: true,
                        max_range_m: Some(cycle.auto_target_max_range),
                    },
                );
                lock_count += 1;
            }
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
    );
    Ok(())
}
