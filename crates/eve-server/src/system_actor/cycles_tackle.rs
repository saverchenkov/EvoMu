//! Module cycle addition and tackle/bubble reconciliation for `system_actor`.

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{BallId, Ballpark, CombatState, MovementMode};
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

pub(crate) fn parse_siren_tackle_ids(effect_name: &str) -> Option<(BallId, i64)> {
    let rest = effect_name.strip_prefix("siren_tackle:")?;
    let (parent_s, item_s) = rest.split_once(':')?;
    Some((parent_s.parse().ok()?, item_s.parse().ok()?))
}

pub(crate) fn active_tackle_and_core_deltas(
    ball_id: BallId,
    cs: &CombatState,
    active_module_cycles: &[ActiveModuleCycle],
) -> (i32, i32) {
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
    let is_protected = cs.is_invulnerable()
        || cs.undock_invulnerable_remaining_ticks > 0
        || cs.gate_cloak_remaining_ticks > 0
        || cs.is_cloaked
        || cs.is_tethered;
    let lance_scram = if cs.lance_disrupted_remaining_s > 0.0 && !is_protected {
        100
    } else {
        0
    };
    (mod_scram + lance_scram, mod_core)
}

pub(crate) fn snapshot_active_tackle_totals(
    ballpark: &Ballpark,
    active_module_cycles: &[ActiveModuleCycle],
    last_active_totals: &mut HashMap<BallId, (Option<i32>, Option<i32>)>,
) {
    for (&bid, cs) in &ballpark.combat_states {
        let (scram_d, core_d) = active_tackle_and_core_deltas(bid, cs, active_module_cycles);
        if scram_d != 0 || core_d != 0 {
            let entry = last_active_totals.entry(bid).or_insert((None, None));
            if scram_d != 0 {
                entry.0 = Some(cs.warp_scramble_strength);
            }
            if core_d != 0 {
                entry.1 = Some(cs.warp_core_strength);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn_ball_with_combat_in_actor(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &mut HashMap<BallId, i32>,
    base_core_strengths: &mut HashMap<BallId, i32>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    ball: Ball,
    combat_state: CombatState,
    slim_meta_opt: Option<SlimItemMeta>,
) -> u64 {
    let ball_id = ball.id;
    let had_prev_cs = ballpark.combat_states.contains_key(&ball_id);
    let existing_meta = ballpark.ball_slim_meta.get(&ball_id).cloned();
    if ballpark.balls.contains_key(&ball_id) {
        let _ = ballpark.remove_ball(ball_id);
    }
    if let Some(meta) = slim_meta_opt.or(existing_meta) {
        ballpark.set_ball_slim_meta(ball_id, meta);
    } else {
        ensure_default_slim_meta(ballpark, &ball, sde_opt);
    }
    let (scram_delta, core_delta) = if had_prev_cs {
        active_tackle_and_core_deltas(ball_id, &combat_state, active_module_cycles)
    } else {
        (0, 0)
    };
    base_scramble_strengths.insert(ball_id, (combat_state.warp_scramble_strength - scram_delta).max(0));
    base_core_strengths.insert(ball_id, combat_state.warp_core_strength - core_delta);
    let mut cs = combat_state;
    // If a caller mutated HP/cap/sig stats after `build_ship_combat_state` / `build_type_combat_state`
    // (before spawning with no active runtime multipliers), align baselines to those values (`#584`).
    if cs.env_max_shield_multiplier.is_none()
        && cs.base_max_shield_hp.is_some_and(|b| (cs.max_shield_hp - b).abs() > 1e-6)
    {
        cs.base_max_shield_hp = Some(cs.max_shield_hp);
    }
    if cs.env_max_armor_multiplier.is_none()
        && cs.base_max_armor_hp.is_some_and(|b| (cs.max_armor_hp - b).abs() > 1e-6)
    {
        cs.base_max_armor_hp = Some(cs.max_armor_hp);
    }
    if cs.base_max_structure_hp.is_some_and(|b| (cs.max_structure_hp - b).abs() > 1e-6) {
        cs.base_max_structure_hp = Some(cs.max_structure_hp);
    }
    if cs.env_max_cap_multiplier.is_none()
        && cs.base_max_capacitor.is_some_and(|b| (cs.max_capacitor - b).abs() > 1e-6)
    {
        cs.base_max_capacitor = Some(cs.max_capacitor);
    }
    if cs.cloud_cap_recharge_multiplier.is_none()
        && cs
            .base_capacitor_recharge_time_ms
            .is_some_and(|b| (cs.capacitor_recharge_time_ms - b).abs() > 1e-6)
    {
        cs.base_capacitor_recharge_time_ms = Some(cs.capacitor_recharge_time_ms);
    }
    if cs.base_signature_radius.is_none() {
        let base = if cs.signature_radius > 0.0 {
            cs.signature_radius
        } else {
            sde_opt
                .as_ref()
                .and_then(|s| s.types.get(&ball.type_id)?.default_attributes.get(&552).copied())
                .unwrap_or(ball.radius.max(35.0))
        };
        cs.base_signature_radius = Some(base);
    } else if cs.signature_radius > 0.0
        && (cs.module_sig_multiplier - 1.0).abs() <= 1e-6
        && cs.mjfg_sig_multiplier.is_none()
        && cs.tp_sig_multiplier.is_none()
        && cs.external_sig_multiplier.is_none()
        && cs.cloud_sig_multiplier.is_none()
        && cs.env_sig_multiplier.is_none()
        && cs
            .base_signature_radius
            .is_some_and(|b| (cs.signature_radius - b).abs() > 1e-6)
    {
        cs.base_signature_radius = Some(cs.signature_radius);
    }
    cs.capture_missing_baselines(&ball);
    if sde_opt
        .as_ref()
        .and_then(|s| s.types.get(&ball.type_id)?.default_attributes.get(&1538).copied())
        .unwrap_or(0.0)
        >= 1.0
    {
        cs.is_interdiction_nullified = true;
    }
    let mut ball = ball;
    if !cs.module_immobilized
        && cs.external_velocity_multiplier.is_none()
        && cs.cloud_velocity_multiplier.is_none()
        && cs.cloak_velocity_multiplier.is_none()
        && (cs.prop_velocity_multiplier - 1.0).abs() <= 1e-9
        && (cs.hic_velocity_multiplier - 1.0).abs() <= 1e-9
        && (cs.web_velocity_multiplier - 1.0).abs() <= 1e-9
    {
        if let Some(base_v) = cs.base_max_velocity.filter(|&v| v > 0.0) {
            ball.max_velocity = base_v;
        }
    }
    if matches!(ball.mode, MovementMode::Rigid) {
        ball.velocity = DVec3::ZERO;
        ball.max_velocity = 0.0;
        ball.speed_fraction = 0.0;
        let meta = ballpark
            .ball_slim_meta
            .get(&ball_id)
            .cloned()
            .unwrap_or_default();
        ballpark.add_static_ball_with_meta(ball.clone(), meta);
        ballpark.combat_states.insert(ball_id, cs);
        ballpark
            .pending_events
            .push(eve_destiny::DestinyEvent::AddBall { ball: ball.clone() });
        ballpark.queue_event_for_all_grid_observers(
            Some(ball_id),
            &eve_destiny::DestinyEvent::AddBalls2 { balls: vec![ball] },
        );
    } else {
        if super::should_pin_stationary_ball(&ball, ballpark.ball_slim_meta.get(&ball_id)) {
            ballpark.mark_ball_non_free(ball_id, false);
        }
        ballpark.add_ball(ball);
        ballpark.combat_states.insert(ball_id, cs);
    }
    if !active_module_cycles.is_empty() {
        reconcile_tackle_and_bubbles(
            ballpark,
            active_module_cycles,
            base_scramble_strengths,
            base_core_strengths,
            db_opt,
            deactivation_hook,
        );
    }
    ballpark.current_tick
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn set_combat_state_in_actor(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &mut HashMap<BallId, i32>,
    base_core_strengths: &mut HashMap<BallId, i32>,
    last_active_totals: &mut HashMap<BallId, (Option<i32>, Option<i32>)>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    ball_id: BallId,
    combat_state: CombatState,
) {
    let (scram_delta, core_delta) =
        active_tackle_and_core_deltas(ball_id, &combat_state, active_module_cycles);
    match ballpark.combat_states.get(&ball_id) {
        None => {
            base_scramble_strengths
                .insert(ball_id, (combat_state.warp_scramble_strength - scram_delta).max(0));
            base_core_strengths.insert(ball_id, combat_state.warp_core_strength - core_delta);
        }
        Some(prev_cs) => {
            if combat_state.warp_scramble_strength != prev_cs.warp_scramble_strength {
                let stale_scram = scram_delta == 0
                    && last_active_totals
                        .get(&ball_id)
                        .and_then(|t| t.0)
                        == Some(combat_state.warp_scramble_strength);
                if !stale_scram {
                    base_scramble_strengths.insert(
                        ball_id,
                        (combat_state.warp_scramble_strength - scram_delta).max(0),
                    );
                }
                if let Some(entry) = last_active_totals.get_mut(&ball_id) {
                    entry.0 = None;
                }
            }
            if combat_state.warp_core_strength != prev_cs.warp_core_strength {
                let stale_core = core_delta == 0
                    && last_active_totals
                        .get(&ball_id)
                        .and_then(|t| t.1)
                        == Some(combat_state.warp_core_strength);
                if !stale_core {
                    base_core_strengths
                        .insert(ball_id, combat_state.warp_core_strength - core_delta);
                }
                if let Some(entry) = last_active_totals.get_mut(&ball_id) {
                    entry.1 = None;
                }
            }
        }
    }
    let mut cs = combat_state;
    let prev_opt = ballpark.combat_states.get(&ball_id);
    if cs.env_max_shield_multiplier.is_none()
        && cs.base_max_shield_hp.is_some_and(|b| (cs.max_shield_hp - b).abs() > 1e-6)
    {
        cs.base_max_shield_hp = Some(cs.max_shield_hp.max(0.0));
    } else if let Some(prev_cs) = prev_opt {
        if (cs.max_shield_hp - prev_cs.max_shield_hp).abs() > 1e-6
            && cs.base_max_shield_hp == prev_cs.base_max_shield_hp
        {
            let m = cs.env_max_shield_multiplier.unwrap_or(1.0).max(1e-6);
            cs.base_max_shield_hp = Some((cs.max_shield_hp / m).max(0.0));
        }
    }
    if cs.env_max_armor_multiplier.is_none()
        && cs.base_max_armor_hp.is_some_and(|b| (cs.max_armor_hp - b).abs() > 1e-6)
    {
        cs.base_max_armor_hp = Some(cs.max_armor_hp.max(0.0));
    } else if let Some(prev_cs) = prev_opt {
        if (cs.max_armor_hp - prev_cs.max_armor_hp).abs() > 1e-6
            && cs.base_max_armor_hp == prev_cs.base_max_armor_hp
        {
            let m = cs.env_max_armor_multiplier.unwrap_or(1.0).max(1e-6);
            cs.base_max_armor_hp = Some((cs.max_armor_hp / m).max(0.0));
        }
    }
    if cs.base_max_structure_hp.is_some_and(|b| (cs.max_structure_hp - b).abs() > 1e-6) {
        cs.base_max_structure_hp = Some(cs.max_structure_hp.max(0.0));
    }
    if cs.env_max_cap_multiplier.is_none()
        && cs.base_max_capacitor.is_some_and(|b| (cs.max_capacitor - b).abs() > 1e-6)
    {
        cs.base_max_capacitor = Some(cs.max_capacitor.max(0.0));
    } else if let Some(prev_cs) = prev_opt {
        if (cs.max_capacitor - prev_cs.max_capacitor).abs() > 1e-6
            && cs.base_max_capacitor == prev_cs.base_max_capacitor
        {
            let m = cs.env_max_cap_multiplier.unwrap_or(1.0).max(1e-6);
            cs.base_max_capacitor = Some((cs.max_capacitor / m).max(0.0));
        }
    }
    if cs.cloud_cap_recharge_multiplier.is_none()
        && cs
            .base_capacitor_recharge_time_ms
            .is_some_and(|b| (cs.capacitor_recharge_time_ms - b).abs() > 1e-6)
    {
        cs.base_capacitor_recharge_time_ms = Some(cs.capacitor_recharge_time_ms.max(0.0));
    } else if let Some(prev_cs) = prev_opt {
        if (cs.capacitor_recharge_time_ms - prev_cs.capacitor_recharge_time_ms).abs() > 1e-6
            && cs.base_capacitor_recharge_time_ms == prev_cs.base_capacitor_recharge_time_ms
        {
            let m = cs.cloud_cap_recharge_multiplier.unwrap_or(1.0).max(1e-6);
            cs.base_capacitor_recharge_time_ms = Some((cs.capacitor_recharge_time_ms / m).max(0.0));
        }
    }
    let has_sig_mults = (cs.module_sig_multiplier - 1.0).abs() > 1e-6
        || cs.mjfg_sig_multiplier.is_some()
        || cs.tp_sig_multiplier.is_some()
        || cs.external_sig_multiplier.is_some()
        || cs.cloud_sig_multiplier.is_some()
        || cs.env_sig_multiplier.is_some();
    if !has_sig_mults
        && cs
            .base_signature_radius
            .is_some_and(|b| (cs.signature_radius - b).abs() > 1e-6)
    {
        cs.base_signature_radius = Some(cs.signature_radius.max(1.0));
    } else if let Some(prev_cs) = prev_opt {
        if (cs.signature_radius - prev_cs.signature_radius).abs() > 1e-6
            && cs.base_signature_radius == prev_cs.base_signature_radius
        {
            let sig_m = (cs.module_sig_multiplier
                * cs.mjfg_sig_multiplier.unwrap_or(1.0)
                * cs.tp_sig_multiplier.unwrap_or(1.0)
                * cs.external_sig_multiplier.unwrap_or(1.0)
                * cs.cloud_sig_multiplier.unwrap_or(1.0)
                * cs.env_sig_multiplier.unwrap_or(1.0))
            .max(1e-6);
            cs.base_signature_radius = Some((cs.signature_radius / sig_m).max(1.0));
        }
    }
    if let Some(ball) = ballpark.balls.get(&ball_id) {
        cs.capture_missing_baselines(ball);
    }
    let hp_changed_or_damaged = prev_opt.is_some_and(|p| {
        (p.shield_hp - cs.shield_hp).abs() > 1e-6
            || (p.armor_hp - cs.armor_hp).abs() > 1e-6
            || (p.structure_hp - cs.structure_hp).abs() > 1e-6
    }) || (cs.max_shield_hp > 0.0 && cs.shield_hp < cs.max_shield_hp - 1e-6)
        || (cs.max_armor_hp > 0.0 && cs.armor_hp < cs.max_armor_hp - 1e-6)
        || (cs.max_structure_hp > 0.0 && cs.structure_hp < cs.max_structure_hp - 1e-6);
    if hp_changed_or_damaged {
        if let Some(db) = db_opt {
            crate::sp6_lifecycle::persist_ship_combat_hp_in_db(db, ball_id, &cs);
        }
    }
    ballpark.combat_states.insert(ball_id, cs);
    ballpark.recompute_ball_attributes(ball_id);
    reconcile_tackle_and_bubbles(
        ballpark,
        active_module_cycles,
        base_scramble_strengths,
        base_core_strengths,
        db_opt,
        deactivation_hook,
    );
    snapshot_active_tackle_totals(ballpark, active_module_cycles, last_active_totals);
}

pub(crate) fn reconcile_tackle_and_bubbles(
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    deactivation_hook: Option<&ModuleDeactivationHook>,
) {
    let reentrant_db = db_opt
        .as_ref()
        .is_some_and(|db| db.is_write_locked_by_current_thread());
    let effective_db = if reentrant_db { &None } else { db_opt };
    let mut aborted_cycles = Vec::new();

    // 0. Prune synthetic Support Fighter Siren tackle cycles when the fighter ball, parent carrier,
    //    or target ball leaves space (`CmdAbandonFighter` explicitly strips cycles on abandonment),
    //    and deactivate targeted cycles whose balls have drifted beyond `max_effective_range_m`.
    for idx in (0..active_module_cycles.len()).rev() {
        let is_siren = parse_siren_tackle_ids(&active_module_cycles[idx].effect_name);
        let siren_missing = is_siren.is_some_and(|(parent_ship_id, _fighter_item_id)| {
            !ballpark.balls.contains_key(&active_module_cycles[idx].ship_id)
                || !ballpark.balls.contains_key(&parent_ship_id)
                || !active_module_cycles[idx]
                    .target_id
                    .is_some_and(|tid| ballpark.balls.contains_key(&tid))
        });
        if siren_missing {
            active_module_cycles.remove(idx);
            continue;
        }
        let out_of_range = matches!(
            (
                active_module_cycles[idx]
                    .target_id
                    .and_then(|tid| ballpark.balls.get(&tid)),
                ballpark.balls.get(&active_module_cycles[idx].ship_id),
                active_module_cycles[idx].max_effective_range_m,
            ),
            (Some(tb), Some(sb), Some(max_r)) if (sb.position - tb.position).length() > max_r
        );
        if out_of_range {
            let shut_down = active_module_cycles.remove(idx);
            cleanup_removed_module_cycle(ballpark, &shut_down, effective_db);
            if is_siren.is_none() {
                aborted_cycles.push(shut_down);
            }
        }
    }

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

    // 5. Declaratively recompute ship velocity, mass, signature radius, and resistances from
    //    immutable baselines and active module / EWAR / wubble modifiers (`#584`, `#438`).
    let wubble_mults =
        crate::services::tactical_modules::active_wubble_multipliers_for_ballpark(ballpark);
    let mut dirty_ships: std::collections::BTreeSet<BallId> = ballpark
        .combat_states
        .iter()
        .filter(|(_, cs)| {
            cs.module_immobilized
                || (cs.prop_velocity_multiplier - 1.0).abs() > 1e-9
                || (cs.hic_velocity_multiplier - 1.0).abs() > 1e-9
                || (cs.web_velocity_multiplier - 1.0).abs() > 1e-9
                || (cs.module_mass_multiplier - 1.0).abs() > 1e-9
                || cs.module_mass_addition.abs() > 1e-9
                || (cs.module_sig_multiplier - 1.0).abs() > 1e-9
                || cs.adc_active
                || cs.ehe_hull_resists.is_some()
        })
        .map(|(&bid, _)| bid)
        .collect();
    for &tid in wubble_mults.keys() {
        dirty_ships.insert(tid);
    }
    for c in active_module_cycles.iter() {
        if c.immobilize_self
            || c.hic_velocity_multiplier.is_some()
            || c.speed_boost_factor.abs() > 1e-9
            || c.self_mass_multiplier.is_some()
            || c.hic_mass_addition.is_some()
            || c.hic_sig_multiplier.is_some()
            || c.is_adc
            || c.ehe_hull_resists.is_some()
            || c.is_rah
        {
            dirty_ships.insert(c.ship_id);
        }
        if c.target_velocity_multiplier.is_some() {
            if let Some(tid) = c.target_id {
                dirty_ships.insert(tid);
            }
        }
    }
    for sid in dirty_ships {
        reconcile_velocity_for_ship(ballpark, sid, active_module_cycles, &wubble_mults);
    }
}

/// Declaratively recomputes and applies all active module/EWAR modifiers (`max_velocity`, `mass`,
/// `signature_radius`, `adc_active`, `ehe_hull_resists`, etc.) on `ship_id` from its immutable
/// baselines (`#584`, `#438`).
pub(crate) fn reconcile_velocity_for_ship(
    ballpark: &mut Ballpark,
    ship_id: BallId,
    active_module_cycles: &[ActiveModuleCycle],
    wubble_mults: &HashMap<BallId, f64>,
) {
    if !ballpark.balls.contains_key(&ship_id) {
        return;
    }

    let mut immobilized = false;
    let mut prop_mult = 1.0_f64;
    let mut hic_mult = 1.0_f64;
    let mut cloak_mult: Option<f64> = None;
    let mut mass_mult = 1.0_f64;
    let mut mass_add = 0.0_f64;
    let mut sig_mult = 1.0_f64;
    let mut adc_active = false;
    let mut ehe_resists = None;

    for c in active_module_cycles {
        if c.ship_id != ship_id {
            continue;
        }
        if c.immobilize_self {
            immobilized = true;
        }
        if let Some(hm) = c.hic_velocity_multiplier {
            hic_mult *= hm.max(0.0);
        }
        if c.speed_boost_factor.abs() > 1e-9 || c.effect_name.starts_with("cloaking") {
            let factor = (1.0 + c.speed_boost_factor).max(0.0);
            if c.effect_name.starts_with("cloaking") {
                cloak_mult = Some(cloak_mult.unwrap_or(1.0) * factor);
            } else {
                prop_mult *= factor;
            }
        }
        if let Some(mm) = c.self_mass_multiplier {
            mass_mult *= mm;
        }
        if let Some(ma) = c.hic_mass_addition {
            mass_add += ma;
        }
        if let Some(sm) = c.hic_sig_multiplier {
            sig_mult *= sm;
        }
        if c.is_adc {
            adc_active = true;
        }
        if c.ehe_hull_resists.is_some() {
            ehe_resists = c.ehe_hull_resists;
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
        penalties.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
        let mut combined = 1.0_f64;
        for (rank, p) in penalties.into_iter().enumerate() {
            let sp = (-((rank as f64) / 2.67).powi(2)).exp();
            combined *= (1.0 - p * sp).clamp(0.01, 1.0);
        }
        combined
    };

    if let Some(cs) = ballpark.combat_states.get_mut(&ship_id) {
        cs.module_immobilized = immobilized;
        cs.prop_velocity_multiplier = prop_mult;
        cs.hic_velocity_multiplier = hic_mult;
        cs.web_velocity_multiplier = web_mult;
        if let Some(cm) = cloak_mult {
            cs.cloak_velocity_multiplier = Some(cm);
        }
        cs.module_mass_multiplier = mass_mult;
        cs.module_mass_addition = mass_add;
        cs.module_sig_multiplier = sig_mult;
        cs.adc_active = adc_active;
        cs.ehe_hull_resists = ehe_resists;
    }
    ballpark.recompute_ball_attributes(ship_id);
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
        if (active_module_cycles[idx].ship_id == source_id
            || parse_siren_tackle_ids(&active_module_cycles[idx].effect_name)
                .is_some_and(|(parent_id, _)| parent_id == source_id))
            && matches_target
        {
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
    if (cycle.weapon_is_turret || cycle.weapon_is_missile)
        && cycle.ammo_type_id.is_none()
        && (cycle.damage_vector.em - 20.0).abs() < 1e-6
        && (cycle.damage_vector.thermal - 20.0).abs() < 1e-6
        && (cycle.damage_vector.kinetic - 20.0).abs() < 1e-6
        && (cycle.damage_vector.explosive - 20.0).abs() < 1e-6
    {
        cycle.damage_vector = eve_destiny::DamageVector::new(0.0, 0.0, 0.0, 0.0);
    }
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
    if let (Some(ball), Some(cs)) = (
        ballpark.balls.get(&cycle.ship_id),
        ballpark.combat_states.get_mut(&cycle.ship_id),
    ) {
        cs.capture_missing_baselines(ball);
    }
    if cycle.effect_name.starts_with("cloaking") {
        let cs = ballpark.combat_states.entry(cycle.ship_id).or_default();
        cs.cloak_velocity_multiplier = Some((1.0 + cycle.speed_boost_factor).max(0.0));
    }
    if cycle.immobilize_self {
        let prev_max_v = ballpark.balls.get(&cycle.ship_id).map(|b| b.max_velocity);
        if let Some(ball) = ballpark.balls.get_mut(&cycle.ship_id) {
            ball.velocity = DVec3::ZERO;
            ball.speed_fraction = 0.0;
            ball.mode = MovementMode::Stop;
        }
        if prev_max_v.is_some_and(|v| v > 1e-6) {
            ballpark.set_ball_max_velocity(cycle.ship_id, 0.0);
        }
    }
    if cycle.is_adc {
        if let Some(db_ref) = db_opt {
            db_ref.with_mut_snapshot(|snap| {
                let entry = snap.transient_item_attributes.entry(cycle.ship_id).or_default();
                for aid in [267, 268, 269, 270, 271, 272, 273, 274, 109, 110, 111, 112] {
                    entry.insert(aid, 0.25);
                }
            });
        }
    } else if let Some(ehe_res) = cycle.ehe_hull_resists {
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
            cs.rah_armor_resonance = Some([0.85; 4]);
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
    if cycle.started_at_unix == 0 {
        cycle.started_at_unix = db_opt
            .as_ref()
            .map(|db| db.clock().now_unix_secs())
            .unwrap_or(0);
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
                let old_factor = (1.0 + c.speed_boost_factor).max(1e-6);
                apply_scale(&mut c.speed_boost_factor, ovl_speed);
                let new_factor = (1.0 + c.speed_boost_factor).max(0.0);
                if !c.effect_name.starts_with("cloaking") {
                    if let Some(cs) = ballpark.combat_states.get_mut(&ship_id) {
                        cs.prop_velocity_multiplier =
                            (cs.prop_velocity_multiplier / old_factor) * new_factor;
                    }
                    ballpark.recompute_ball_attributes(ship_id);
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
                if c.weapon_optimal_m > 0.0 {
                    apply_scale(&mut c.weapon_optimal_m, rng_mult);
                }
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
            } => {
                if ballpark.ball_interactive_overrides.get(&id).copied() != Some(is_interactive)
                    && !ballpark.set_ball_interactive(id, is_interactive)
                {
                    ballpark.ball_interactive_overrides.insert(id, is_interactive);
                    ballpark.emit_event(
                        scope,
                        DestinyEvent::SetBallInteractive {
                            ball_id: id,
                            is_interactive,
                        },
                    );
                }
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
                expiry_filetime,
            } if id == ball_id => {
                ballpark.apply_breacher_dot_with_expiry(
                    id,
                    flat_dps,
                    pct_dps,
                    remaining_ticks,
                    expiry_filetime,
                );
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
            DestinyEvent::GotoDirection {
                ball_id: id,
                direction,
            } if id == ball_id => {
                let _ = ballpark.set_mode(
                    id,
                    MovementMode::GotoDirection {
                        direction: DVec3::from_array(direction),
                    },
                );
            }
            DestinyEvent::SetSpeedFraction {
                ball_id: id,
                fraction,
            } if id == ball_id => {
                ballpark.set_speed_fraction(id, fraction);
            }
            DestinyEvent::Stop { ball_id: id } if id == ball_id => {
                let _ = ballpark.set_mode(id, MovementMode::Stop);
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

impl SystemHandle {
    pub fn is_ball_warping_sync(&self, ship_id: BallId) -> bool {
        self.ballpark
            .read()
            .is_ok_and(|bp| bp.balls.get(&ship_id).is_some_and(|b| matches!(b.mode, MovementMode::Warp { .. })))
    }

    pub fn committed_warp_destination_sync(&self, ship_id: BallId) -> Option<DVec3> {
        if let Some(dest) = self
            .committed_warp_balls
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&ship_id)
            .copied()
        {
            return Some(dest);
        }
        self.ballpark.try_read().ok().and_then(|bp| {
            bp.balls.get(&ship_id).and_then(|b| match b.mode {
                MovementMode::Warp { destination, phase, .. }
                    if phase != eve_destiny::WarpPhase::Aligning =>
                {
                    Some(destination)
                }
                _ => None,
            })
        })
    }

    pub fn is_ball_aligning_warp_sync(&self, ship_id: BallId) -> bool {
        self.ballpark.read().is_ok_and(|bp| {
            bp.balls.get(&ship_id).is_some_and(|b| {
                matches!(b.mode, MovementMode::Warp { phase: eve_destiny::WarpPhase::Aligning, .. })
            })
        })
    }

    pub fn max_active_module_cycle_expiry_unix_sync(
        &self,
        ship_id: BallId,
        cyno_expiry_unix: u64,
        now_unix: u64,
    ) -> u64 {
        let st = self.actor_state.read().unwrap_or_else(|e| e.into_inner());
        let mut max_exp = cyno_expiry_unix;
        for c in st
            .active_module_cycles
            .iter()
            .filter(|c| c.ship_id == ship_id && c.duration_s > 0.0)
        {
            if matches!(
                c.effect_name.as_str(),
                "cynosuralGeneration" | "cynosuralGenerationCovert" | "jumpPortalGeneration"
            ) && cyno_expiry_unix > 0
            {
                continue;
            }
            let elapsed = if c.started_at_unix > 0 {
                c.elapsed_s.max(now_unix.saturating_sub(c.started_at_unix) as f64)
            } else {
                c.elapsed_s
            };
            let rem_secs = (c.duration_s - elapsed).max(0.0).ceil() as u64;
            let extra_lance = if matches!(
                c.effect_name.as_str(),
                "superWeaponLanceWarmup" | "superWeaponLanceBeam"
            ) && c.repeat > 0
            {
                c.repeat as u64
            } else {
                0
            };
            let exp = now_unix.saturating_add(rem_secs).saturating_add(extra_lance);
            max_exp = max_exp.max(exp);
        }
        max_exp
    }
}

impl SystemRegistry {
    pub fn is_ball_in_committed_warp_sync(&self, solar_system_id: SolarSystemId, ball_id: BallId) -> bool {
        let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&solar_system_id)
            .is_some_and(|h| h.committed_warp_destination_sync(ball_id).is_some())
    }

    pub fn max_active_module_cycle_expiry_unix_sync(
        &self,
        solar_system_id: SolarSystemId,
        ship_id: BallId,
        cyno_expiry_unix: u64,
        now_unix: u64,
    ) -> u64 {
        let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&solar_system_id)
            .map(|h| h.max_active_module_cycle_expiry_unix_sync(ship_id, cyno_expiry_unix, now_unix))
            .unwrap_or(cyno_expiry_unix)
    }
}

#[cfg(test)]
#[path = "cycles_tackle_tests.rs"]
mod cycles_tackle_tests;
