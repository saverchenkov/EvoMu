//! Atomic module activation, fit projection, and movement/protection actor command handlers (`#1486`).

#![forbid(unsafe_code)]

use super::protection::{
    break_protection_in_actor, is_ship_actively_cloaked_in_actor, sync_ball_protection_in_actor,
};
use super::*;
use eve_destiny::{BallId, Ballpark, DestinyEvent, MovementMode};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

/// Atomically reconciles expired module state, validates warp/cloak/cap/tackle preconditions,
/// commits `request.cycle`, and breaks protection in a single actor turn (`#1486`).
pub(in crate::system_actor) fn handle_activate_module_cycle_atomic(
    exec_ctx: &mut ActorExecCtx<'_>,
    mut request: AtomicActivateCycleRequest,
    max_invuln_seq: Option<u64>,
) -> Result<AtomicActivateCycleOutcome, String> {
    let ship_id = request.cycle.ship_id;
    let module_id = request.cycle.module_id;
    if !exec_ctx.ballpark.balls.contains_key(&ship_id) {
        return Err(format!("Ship {ship_id} is not in space"));
    }

    let deact_guard = super::protection::read_deact_hook(exec_ctx);

    let (_existing_info, cleaned_stale) = reconcile_module_cycle_before_activate_in_actor(
        exec_ctx.solar_system_id,
        exec_ctx.ballpark,
        &mut exec_ctx.actor_st.active_module_cycles,
        &exec_ctx.actor_st.base_scramble_strengths,
        &exec_ctx.actor_st.base_core_strengths,
        &exec_ctx.actor_st.sde_opt,
        &exec_ctx.actor_st.db_opt,
        deact_guard.as_ref(),
        ship_id,
        module_id,
        request.real_now_unix,
    );
    if cleaned_stale {
        super::protection::sync_active_ships_cache(exec_ctx);
        sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ship_id);
    }

    validate_activation_warp_and_cloak(
        exec_ctx,
        ship_id,
        request.allow_while_warping,
        request.allow_while_cloaked,
    )?;

    cycles_tackle::validate_add_module_cycle_preconditions(
        exec_ctx.ballpark,
        &exec_ctx.actor_st.active_module_cycles,
        &mut request.cycle,
        request.replace_existing_cycle,
    )?;

    let is_activating_cloak = super::protection::is_cloak_cycle(&request.cycle);
    let mut protection_events = if request.break_protection && is_activating_cloak {
        // Break any pre-existing gate cloak / invulnerability BEFORE adding a cloak cycle so
        // `deactivate_cloak` clears only prior gate cloak and preserves the new cloak multiplier (`#1486`).
        break_protection_in_actor(
            exec_ctx,
            ship_id,
            Some(request.char_id),
            request.break_reason,
            request.real_now_unix,
            None,
            max_invuln_seq,
        )
    } else {
        ProtectionBreakEvents {
            sim_elapsed_secs: exec_ctx.actor_st.sim_elapsed_secs,
            ..Default::default()
        }
    };

    let pre_snap = (request.break_protection && !is_activating_cloak)
        .then(|| {
            exec_ctx
                .ballpark
                .combat_states
                .get(&ship_id)
                .map(|cs| cs.protection_snapshot())
        })
        .flatten();

    if request.cycle.origin_scene_epoch.is_none() {
        request.cycle.origin_scene_epoch = resolve_active_cycle_origin_epoch(
            exec_ctx.ballpark,
            &exec_ctx.actor_st.db_opt,
            ship_id,
            Some(request.char_id),
        );
    }

    handle_add_module_cycle_with_options(
        exec_ctx.solar_system_id,
        exec_ctx.ballpark,
        &mut exec_ctx.actor_st.active_module_cycles,
        &exec_ctx.actor_st.base_scramble_strengths,
        &exec_ctx.actor_st.base_core_strengths,
        &exec_ctx.actor_st.sde_opt,
        &exec_ctx.actor_st.db_opt,
        deact_guard.as_ref(),
        request.cycle,
        false,
        request.replace_existing_cycle,
    )?;
    exec_ctx
        .actor_st
        .weapon_cycle_start_modules
        .remove(&(ship_id, module_id));

    if request.break_protection && !is_activating_cloak {
        protection_events = break_protection_in_actor(
            exec_ctx,
            ship_id,
            Some(request.char_id),
            request.break_reason,
            request.real_now_unix,
            pre_snap,
            max_invuln_seq,
        );
    } else {
        sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ship_id);
    }

    super::protection::snapshot_tackle_totals_in_actor(exec_ctx);
    super::protection::sync_positions_and_active_ships(exec_ctx);

    let capacitor_after = exec_ctx
        .ballpark
        .combat_states
        .get(&ship_id)
        .map(|cs| cs.capacitor)
        .unwrap_or(0.0);

    Ok(AtomicActivateCycleOutcome {
        protection_events,
        capacitor_after,
    })
}

fn resolve_active_cycle_origin_epoch(
    ballpark: &Ballpark,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ship_id: BallId,
    char_id_hint: Option<CharacterId>,
) -> Option<u64> {
    let db = db_opt.as_ref()?;
    let slim_meta = ballpark.ball_slim_meta.get(&ship_id);
    if char_id_hint.is_none()
        && slim_meta.is_some_and(|m| m.char_id.is_none() && m.category_id != 0 && m.category_id != 6)
    {
        return None;
    }
    let hint_cid = char_id_hint
        .or_else(|| slim_meta.and_then(|m| m.char_id))
        .filter(|&cid| cid > 0);
    let (cid, uid) = db.with_snapshot(|snap| {
        if let Some(cid) = hint_cid {
            snap.characters.get(&cid).map(|c| (c.character_id, c.user_id))
        } else {
            snap.characters
                .values()
                .find(|c| c.active_ship_id == ship_id)
                .map(|c| (c.character_id, c.user_id))
        }
    })?;
    db.active_scene_epoch_for_char_and_user(cid, uid)
}

/// Applies an immutable [`eve_dogma::FitProjection`] onto `ship_id` inside the system actor,
/// enforcing monotonic [`eve_dogma::FitRev`] ordering and recomputing derived `Ball` / `CombatState`
/// attributes from the new baselines without overwriting actor-managed multipliers (`#922`, `#834`).
#[cfg(test)]
pub(crate) fn apply_fit_projection_in_actor(
    ballpark: &mut Ballpark,
    fit_projections: &mut HashMap<BallId, eve_dogma::FitProjection>,
    ship_id: BallId,
    projection: eve_dogma::FitProjection,
) -> bool {
    let mut empty_cycles = Vec::new();
    let (empty_scram, empty_core) = (HashMap::new(), HashMap::new());
    apply_fit_projection_with_cycles_in_actor(
        ballpark.solar_system_id, ballpark, fit_projections, &mut empty_cycles, &empty_scram, &empty_core, &None, None, ship_id, projection,
    )
}

/// Applies an immutable [`eve_dogma::FitProjection`] onto `ship_id` inside the system actor,
/// re-syncs active cycles whose overload state just converged via `SyncOverloadState` or whose
/// active propulsion projection matches `(module_id, is_overloaded)`, and reconciles ship
/// kinematics (`prop_velocity_multiplier`, `Ball::max_velocity`, `Ball::mass`, `signature_radius`)
/// (`#922`, `#1364`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_fit_projection_with_cycles_in_actor(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    fit_projections: &mut HashMap<BallId, eve_dogma::FitProjection>,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    ship_id: BallId,
    projection: eve_dogma::FitProjection,
) -> bool {
    if fit_projections.get(&ship_id).is_some_and(|cur| projection.rev < cur.rev) {
        return false;
    }
    let prev_proj = fit_projections.get(&ship_id).cloned();
    let prev_baseline = prev_proj.as_ref().map(|p| p.baseline);
    let hull_changed = prev_proj.as_ref().is_some_and(|p| p.ship_type_id != projection.ship_type_id);
    let baseline = projection.baseline;
    fit_projections.insert(ship_id, projection);
    let shield_boost_mult = {
        let Some(cs) = ballpark.combat_states.get_mut(&ship_id) else { return false; };
        let shield_regen_disabled = cs.shield_recharge_time_ms <= 0.0 || cs.shield_recharge_time_ms >= 1.0e12;
        let cap_regen_disabled = cs.capacitor_recharge_time_ms <= 0.0
            || cs.base_capacitor_recharge_time_ms.is_some_and(|b| b <= 0.0)
            || cs.capacitor_recharge_time_ms >= 1.0e12;
        let keep_prev = |cur_base: Option<f64>, prev_val: Option<f64>, new_val: f64| -> bool {
            !hull_changed && prev_val.is_some_and(|pv| (new_val - pv).abs() <= 1e-2 && cur_base.is_some_and(|cb| (cb - pv).abs() > 1e-2))
        };
        if !keep_prev(cs.base_max_velocity, prev_baseline.map(|p| p.max_velocity), baseline.max_velocity) {
            cs.base_max_velocity = Some(baseline.max_velocity.max(0.0));
        }
        if !keep_prev(cs.base_mass, prev_baseline.map(|p| p.mass), baseline.mass) {
            cs.base_mass = Some(baseline.mass.max(1.0));
        }
        if !keep_prev(cs.base_inertia, prev_baseline.map(|p| p.agility), baseline.agility) {
            cs.base_inertia = Some(baseline.agility.max(0.001));
        }
        if !keep_prev(cs.base_signature_radius, prev_baseline.map(|p| p.signature_radius), baseline.signature_radius) {
            cs.base_signature_radius = Some(baseline.signature_radius.max(0.0));
        }
        if !shield_regen_disabled {
            let sync_hp_base = |cur_hp: f64, base_hp: &mut Option<f64>, new_b: f64, prev_b: Option<f64>, env: f64| {
                let overridden = prev_b.is_some_and(|pb| !hull_changed && (new_b - pb).abs() <= 1e-2 && (cur_hp / env - pb).abs() > 1e-2);
                if overridden { *base_hp = Some((cur_hp / env).max(0.0)); }
                else if !(prev_b.is_none() && base_hp.is_some_and(|cb| cb > new_b + 1e-2)) { *base_hp = Some(new_b.max(0.0)); }
            };
            sync_hp_base(cs.max_shield_hp, &mut cs.base_max_shield_hp, baseline.max_shield_hp, prev_baseline.map(|p| p.max_shield_hp), cs.env_max_shield_multiplier.unwrap_or(1.0).max(1e-6));
            sync_hp_base(cs.max_armor_hp, &mut cs.base_max_armor_hp, baseline.max_armor_hp, prev_baseline.map(|p| p.max_armor_hp), cs.env_max_armor_multiplier.unwrap_or(1.0).max(1e-6));
            sync_hp_base(cs.max_structure_hp, &mut cs.base_max_structure_hp, baseline.max_structure_hp, prev_baseline.map(|p| p.max_structure_hp), 1.0);
            cs.shield_recharge_time_ms = baseline.shield_recharge_time_ms.max(1.0);
        }
        if !cap_regen_disabled {
            let env_c = cs.env_max_cap_multiplier.unwrap_or(1.0).max(1e-6);
            let prev_cap_overridden = prev_baseline.is_some_and(|pb| {
                !hull_changed && (baseline.max_capacitor - pb.max_capacitor).abs() <= 1e-2
                    && ((cs.max_capacitor / env_c - pb.max_capacitor).abs() > 1e-2
                        || cs.base_max_capacitor.is_some_and(|cur_b| (cur_b - pb.max_capacitor).abs() > 1e-2))
            });
            if prev_cap_overridden {
                if (cs.max_capacitor / env_c - prev_baseline.map(|p| p.max_capacitor).unwrap_or(0.0)).abs() > 1e-2 {
                    cs.base_max_capacitor = Some((cs.max_capacitor / env_c).max(0.0));
                }
            } else if prev_baseline.is_none() && cs.base_max_capacitor.is_some_and(|cur_b| (cs.max_capacitor / env_c - cur_b).abs() > 1e-2 && cs.max_capacitor / env_c > baseline.max_capacitor + 1e-2) {
                cs.base_max_capacitor = Some((cs.max_capacitor / env_c).max(0.0));
            } else {
                let init_cap_overridden = prev_baseline.is_none() && cs.base_max_capacitor.is_some_and(|cur_b| {
                    (cur_b - baseline.max_capacitor).abs() > 1e-2 && (cur_b > baseline.max_capacitor + 1e-2
                        || ((cs.capacitor_recharge_time_ms - 120_000.0).abs() < 1e-2 && (baseline.capacitor_recharge_time_ms - 120_000.0).abs() > 1e-2 && (cur_b - 250.0).abs() > 1e-2))
                });
                if !init_cap_overridden {
                    let cap_floor = if hull_changed { cs.capacitor } else { 0.0 };
                    cs.base_max_capacitor = Some(baseline.max_capacitor.max(cap_floor).max(0.0));
                }
            }
            cs.base_capacitor_recharge_time_ms = Some(baseline.capacitor_recharge_time_ms.max(1.0));
        }
        cs.base_shield_resonance = Some(baseline.shield_resonance);
        cs.base_armor_resonance = Some(baseline.armor_resonance);
        cs.base_structure_resonance = Some(baseline.structure_resonance);
        cs.shield_boost_multiplier
    };

    let mut synced_any_cycle = false;
    if let Some(new_proj) = fit_projections.get(&ship_id) {
        for cycle in active_module_cycles.iter_mut().filter(|c| c.ship_id == ship_id) {
            let overload_synced = prev_proj.as_ref().and_then(|p| p.modules.get(&cycle.module_id)).is_some_and(|old_cp| old_cp.is_overloaded != cycle.is_overloaded)
                && new_proj.modules.get(&cycle.module_id).is_some_and(|new_cp| new_cp.is_overloaded == cycle.is_overloaded);
            let active_prop_matches = new_proj.propulsion.as_ref().is_some_and(|prop| prop.module_id == cycle.module_id && prop.is_overloaded == cycle.is_overloaded);
            if (overload_synced || active_prop_matches) && cycles::sync_cycle_from_fit_projection_with_shield_boost_mult(cycle, fit_projections, shield_boost_mult) {
                synced_any_cycle = true;
            }
        }
    }

    if synced_any_cycle {
        reconcile_tackle_and_bubbles(solar_system_id, ballpark, active_module_cycles, base_scramble_strengths, base_core_strengths, db_opt, deactivation_hook);
    }
    ballpark.recompute_ball_attributes(ship_id);
    true
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_set_movement_mode_command(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    ball_id: BallId,
    mode: MovementMode,
    provenance: Option<WarpProvenance>,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &mut HashMap<BallId, i32>,
    base_core_strengths: &mut HashMap<BallId, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    module_deactivation_hook_actor: &Arc<RankedRwLock<Option<ModuleDeactivationHook>, RANK_LEAF>>,
    pending_notifications_opt: &Option<
        Arc<RankedRwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>, RANK_LEAF>>,
    >,
    notify_tx_opt: &Option<
        tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    >,
    ball_positions_actor: &Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    scrambled_balls_actor: &Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    ball_space_entry_epochs: &HashMap<BallId, u64>,
    warp_state: &mut ActorWarpState,
) -> Result<u64, String> {
    if !ballpark.balls.contains_key(&ball_id) {
        return Err(format!("Ball {ball_id} not in system {solar_system_id}"));
    }
    let is_stop = matches!(mode, MovementMode::Stop);
    let is_warp = matches!(mode, MovementMode::Warp { .. });
    let was_warping = matches!(
        ballpark.balls.get(&ball_id).map(|b| &b.mode),
        Some(MovementMode::Warp { .. })
    );
    let pre_ev_len = ballpark.pending_events.len();
    let read_hook = || module_deactivation_hook_actor.read().unwrap_or_else(|e| e.into_inner()).clone();
    match ballpark.set_mode(ball_id, mode) {
        Ok(()) => {
            if ballpark.pending_events.len() > pre_ev_len {
                let new_evs: Vec<DestinyEvent> = ballpark.pending_events[pre_ev_len..].to_vec();
                let deact_guard = read_hook();
                for ev in &new_evs {
                    if let DestinyEvent::OnTarget { mode: m, source_id, target_id } = ev {
                        if m == "lost" {
                            on_target_unlocked_in_actor(
                                solar_system_id, ballpark, active_module_cycles, base_scramble_strengths,
                                base_core_strengths, db_opt, *source_id, Some(*target_id), deact_guard.as_ref(),
                            );
                        }
                    }
                }
                cycles::flush_target_lost_notifications(
                    ballpark, &new_evs, db_opt.as_ref(),
                    pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>), notify_tx_opt.as_ref(),
                );
                let mut idx = pre_ev_len;
                while idx < ballpark.pending_events.len() {
                    let drop_ev = matches!(&ballpark.pending_events[idx], DestinyEvent::TargetLockFailed { .. })
                        || matches!(&ballpark.pending_events[idx], DestinyEvent::OnTarget { mode, .. } if mode == "lost");
                    if drop_ev {
                        ballpark.pending_events.remove(idx);
                    } else {
                        idx += 1;
                    }
                }
            }
            if is_stop && active_module_cycles.iter().any(|c| c.ship_id == ball_id) {
                let deact_guard = read_hook();
                reconcile_tackle_and_bubbles(
                    solar_system_id,
                    ballpark,
                    active_module_cycles,
                    base_scramble_strengths,
                    base_core_strengths,
                    db_opt,
                    deact_guard.as_ref(),
                );
                sync_ball_positions(ballpark, ball_positions_actor, scrambled_balls_actor);
            }
            if is_warp {
                if let Some(prov) = provenance {
                    if !matches!(prov, WarpProvenance::DisconnectedEmergencyWarp) {
                        if let Some(cs) = ballpark.combat_states.get_mut(&ball_id) {
                            cs.is_emergency_warping = false;
                        }
                    } else {
                        warp_state.clear_ball(ball_id, false);
                    }
                    warp_state.provenance.insert(ball_id, prov);
                    warp_state.pending_warp_finished.remove(&ball_id);
                } else {
                    warp_state.provenance.remove(&ball_id);
                }
            } else {
                warp_state.clear_ball(ball_id, was_warping);
            }
            persist_ball_positions_in_db(ballpark, &[ball_id], db_opt, ball_space_entry_epochs);
            Ok(ballpark.current_tick)
        }
        Err(e) => Err(e),
    }
}

fn run_set_movement_mode_from_ctx(
    exec_ctx: &mut ActorExecCtx<'_>,
    ball_id: BallId,
    mode: MovementMode,
    provenance: Option<WarpProvenance>,
) -> Result<u64, String> {
    apply_set_movement_mode_command(
        exec_ctx.solar_system_id,
        exec_ctx.ballpark,
        ball_id,
        mode,
        provenance,
        &mut exec_ctx.actor_st.active_module_cycles,
        &mut exec_ctx.actor_st.base_scramble_strengths,
        &mut exec_ctx.actor_st.base_core_strengths,
        &exec_ctx.actor_st.db_opt,
        exec_ctx.module_deactivation_hook_actor,
        &exec_ctx.actor_st.pending_notifications_opt,
        &exec_ctx.actor_st.notify_tx_opt,
        exec_ctx.ball_positions_actor,
        exec_ctx.scrambled_balls_actor,
        &exec_ctx.actor_st.ball_space_entry_epochs,
        &mut exec_ctx.actor_st.warp_state,
    )
}

fn run_movement_breaking_protection(
    exec_ctx: &mut ActorExecCtx<'_>,
    ball_id: BallId,
    char_id: Option<CharacterId>,
    reason: ProtectionBreakReason,
    real_now_unix: u64,
    max_invuln_seq: Option<u64>,
    op: impl FnOnce(&mut ActorExecCtx<'_>) -> Result<u64, String>,
) -> Result<(u64, ProtectionBreakEvents), String> {
    let pre_snap = exec_ctx
        .ballpark
        .combat_states
        .get(&ball_id)
        .map(|cs| cs.protection_snapshot());
    let tick = op(exec_ctx)?;
    Ok((
        tick,
        break_protection_in_actor(
            exec_ctx,
            ball_id,
            char_id,
            reason,
            real_now_unix,
            pre_snap,
            max_invuln_seq,
        ),
    ))
}

fn commit_combat_state_update(
    exec_ctx: &mut ActorExecCtx<'_>,
    ball_id: BallId,
    combat_state: eve_destiny::CombatState,
) {
    let deact_guard = super::protection::read_deact_hook(exec_ctx);
    cycles_tackle::set_combat_state_in_actor(
        exec_ctx.solar_system_id,
        exec_ctx.ballpark,
        &mut exec_ctx.actor_st.active_module_cycles,
        &mut exec_ctx.actor_st.base_scramble_strengths,
        &mut exec_ctx.actor_st.base_core_strengths,
        exec_ctx.last_active_totals,
        &exec_ctx.actor_st.db_opt,
        deact_guard.as_ref(),
        ball_id,
        combat_state,
    );
    sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ball_id);
    super::protection::sync_positions_in_actor(exec_ctx);
}

fn validate_activation_warp_and_cloak(
    exec_ctx: &ActorExecCtx<'_>,
    ship_id: BallId,
    allow_while_warping: bool,
    allow_while_cloaked: bool,
) -> Result<(), String> {
    if !allow_while_warping {
        if let Some(ship_ball) = exec_ctx.ballpark.balls.get(&ship_id) {
            if matches!(
                ship_ball.mode,
                MovementMode::Warp { phase, .. } if phase != eve_destiny::WarpPhase::Aligning
            ) {
                return Err("CantActivateInWarp: Cannot activate modules while in warp".to_string());
            }
        }
    }

    if !allow_while_cloaked
        && is_ship_actively_cloaked_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ship_id)
    {
        return Err("CannotActivateCloaked: Cannot activate module while cloaked".to_string());
    }

    Ok(())
}

fn effective_cap_max_for_ball(
    actor_st: &SystemActorState,
    ball_id: BallId,
    cs: &eve_destiny::CombatState,
) -> Option<f64> {
    actor_st
        .fit_projections
        .get(&ball_id)
        .map(|fp| fp.baseline.max_capacitor * cs.env_max_cap_multiplier.unwrap_or(1.0).max(1e-6))
        .filter(|&m| m > 0.0 && ((cs.max_capacitor - 250.0).abs() <= 1e-6 || cs.max_capacitor <= m + 1e-2))
}

fn handle_commit_lancer_activation_atomic(
    exec_ctx: &mut ActorExecCtx<'_>,
    request: AtomicLancerActivationRequest,
    max_invuln_seq: Option<u64>,
) -> Result<AtomicActivateCycleOutcome, String> {
    let ship_id = request.ship_id;
    let module_id = request.module_id;
    if !exec_ctx.ballpark.balls.contains_key(&ship_id) {
        return Err(format!("Ship {ship_id} is not in space"));
    }

    let deact_guard = super::protection::read_deact_hook(exec_ctx);
    let (_existing_info, cleaned_stale) = reconcile_module_cycle_before_activate_in_actor(
        exec_ctx.solar_system_id,
        exec_ctx.ballpark,
        &mut exec_ctx.actor_st.active_module_cycles,
        &exec_ctx.actor_st.base_scramble_strengths,
        &exec_ctx.actor_st.base_core_strengths,
        &exec_ctx.actor_st.sde_opt,
        &exec_ctx.actor_st.db_opt,
        deact_guard.as_ref(),
        ship_id,
        module_id,
        request.real_now_unix,
    );
    if cleaned_stale {
        super::protection::sync_active_ships_cache(exec_ctx);
        sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ship_id);
    }

    if exec_ctx
        .actor_st
        .active_module_cycles
        .iter()
        .any(|c| c.ship_id == ship_id && c.module_id == module_id)
    {
        return Err(format!(
            "ModuleAlreadyActive: Module {module_id} is already active in a cycle"
        ));
    }

    validate_activation_warp_and_cloak(exec_ctx, ship_id, false, false)?;

    let cap_cost = request.cap_cost;
    if cap_cost > 0.0 {
        if let Some(cs) = exec_ctx.ballpark.combat_states.get(&ship_id) {
            let eff_cap_max = effective_cap_max_for_ball(exec_ctx.actor_st, ship_id, cs);
            let (proj_cap, proj_max_cap) =
                cycles_tackle::projected_capacitor_values(cs, eff_cap_max);
            if proj_cap + 1e-4 < cap_cost {
                return Err(
                    "NotEnoughCapacitor: Insufficient capacitor to activate module".to_string(),
                );
            }
            if let Some(cs_mut) = exec_ctx.ballpark.combat_states.get_mut(&ship_id) {
                if eff_cap_max.is_some() {
                    cs_mut.max_capacitor = proj_max_cap;
                }
                cs_mut.capacitor = (proj_cap - cap_cost).max(0.0);
            }
        }
    }

    let pre_snap = exec_ctx
        .ballpark
        .combat_states
        .get(&ship_id)
        .map(|cs| cs.protection_snapshot());
    let protection_events = break_protection_in_actor(
        exec_ctx,
        ship_id,
        Some(request.char_id),
        ProtectionBreakReason::ModuleActivation,
        request.real_now_unix,
        pre_snap,
        max_invuln_seq,
    );
    super::protection::sync_positions_and_active_ships(exec_ctx);

    let capacitor_after = exec_ctx
        .ballpark
        .combat_states
        .get(&ship_id)
        .map(|cs| cs.capacitor)
        .unwrap_or(0.0);
    Ok(AtomicActivateCycleOutcome {
        protection_events,
        capacitor_after,
    })
}

fn handle_add_capacitor_clamped(
    exec_ctx: &mut ActorExecCtx<'_>,
    ball_id: BallId,
    delta: f64,
) -> Result<f64, String> {
    if !exec_ctx.ballpark.balls.contains_key(&ball_id) {
        return Err(format!("Ball {ball_id} is not in space"));
    }
    let eff_cap_max = exec_ctx
        .ballpark
        .combat_states
        .get(&ball_id)
        .and_then(|cs| effective_cap_max_for_ball(exec_ctx.actor_st, ball_id, cs));
    let Some(cs) = exec_ctx.ballpark.combat_states.get_mut(&ball_id) else {
        return Err(format!("Ball {ball_id} has no combat state"));
    };
    let (_, proj_max_cap) = cycles_tackle::projected_capacitor_values(cs, eff_cap_max);
    if eff_cap_max.is_some() {
        cs.max_capacitor = proj_max_cap;
    }
    let ceiling = proj_max_cap.max(cs.capacitor).max(0.0);
    if delta > 0.0 {
        cs.capacitor = (cs.capacitor + delta).min(ceiling);
    }
    let updated_cap = cs.capacitor;
    if let Some(db) = &exec_ctx.actor_st.db_opt {
        let cloned = cs.clone();
        crate::sp6_lifecycle::persist_existing_ship_combat_hp_for_system_in_db(
            db,
            exec_ctx.solar_system_id,
            std::iter::once((ball_id, &cloned)),
        );
    }
    Ok(updated_cap)
}

pub(in crate::system_actor) fn handle_activation_and_movement_command(
    exec_ctx: &mut ActorExecCtx<'_>,
    cmd: SystemCommand,
) -> Option<SystemCommand> {
    match cmd {
        SystemCommand::SetMovementMode { ball_id, mode, reply } => {
            let res = run_set_movement_mode_from_ctx(exec_ctx, ball_id, mode, None);
            sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ball_id);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::SetWarpMovementMode { ball_id, mode, provenance, reply } => {
            let res = run_set_movement_mode_from_ctx(exec_ctx, ball_id, mode, Some(provenance));
            sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ball_id);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::SetMovementModeBreakingProtection {
            ball_id,
            char_id,
            mode,
            reason,
            real_now_unix,
            max_invuln_seq,
            reply,
        } => {
            let res = run_movement_breaking_protection(
                exec_ctx,
                ball_id,
                char_id,
                reason,
                real_now_unix,
                max_invuln_seq,
                |ctx| run_set_movement_mode_from_ctx(ctx, ball_id, mode, None),
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::SetWarpMovementModeBreakingProtection {
            ball_id,
            char_id,
            mode,
            provenance,
            reason,
            real_now_unix,
            max_invuln_seq,
            reply,
        } => {
            let res = run_movement_breaking_protection(
                exec_ctx,
                ball_id,
                char_id,
                reason,
                real_now_unix,
                max_invuln_seq,
                |ctx| run_set_movement_mode_from_ctx(ctx, ball_id, mode, Some(provenance)),
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::GotoPoint { ball_id, point, reply } => {
            let res = exec_ctx.ballpark.goto_point(ball_id, point).map(|()| exec_ctx.ballpark.current_tick);
            if res.is_ok() {
                persist_ball_positions_in_db(
                    exec_ctx.ballpark,
                    &[ball_id],
                    &exec_ctx.actor_st.db_opt,
                    &exec_ctx.actor_st.ball_space_entry_epochs,
                );
                sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ball_id);
            }
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::GotoPointBreakingProtection {
            ball_id,
            char_id,
            point,
            reason,
            real_now_unix,
            max_invuln_seq,
            reply,
        } => {
            let res = run_movement_breaking_protection(
                exec_ctx,
                ball_id,
                char_id,
                reason,
                real_now_unix,
                max_invuln_seq,
                |ctx| {
                    ctx.ballpark.goto_point(ball_id, point)?;
                    persist_ball_positions_in_db(
                        ctx.ballpark,
                        &[ball_id],
                        &ctx.actor_st.db_opt,
                        &ctx.actor_st.ball_space_entry_epochs,
                    );
                    Ok(ctx.ballpark.current_tick)
                },
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::TrackModuleReloadSim { module_id, ship_id: _, remaining_sim_s } => {
            if remaining_sim_s.is_finite() && remaining_sim_s > 0.0 {
                let now_unix = exec_ctx.actor_st.db_opt.as_ref().map(|db| db.clock().now_unix_secs()).unwrap_or(0);
                let raw_tidi = f64::from_bits(exec_ctx.tidi_factor_bits_actor.load(std::sync::atomic::Ordering::Relaxed));
                let tidi = if raw_tidi.is_finite() { raw_tidi.clamp(0.10, 1.0) } else { 1.0 };
                exec_ctx.actor_st.module_reload_sim_remaining.insert(
                    module_id,
                    ModuleReloadSimEntry {
                        remaining_sim_s,
                        min_wall_until_unix: now_unix.saturating_add(remaining_sim_s.ceil().max(1.0) as u64),
                        tidi_stretched: tidi < 0.995,
                    },
                );
            } else {
                exec_ctx.actor_st.module_reload_sim_remaining.remove(&module_id);
            }
            None
        }
        SystemCommand::SetCombatState { ball_id, combat_state, reply } => {
            commit_combat_state_update(exec_ctx, ball_id, combat_state);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::GetCombatState { ball_id, reply } => {
            defer_reply(exec_ctx.deferred_replies, reply, exec_ctx.ballpark.combat_states.get(&ball_id).cloned());
            None
        }
        SystemCommand::ModifyCombatState { ball_id, f, reply } => {
            let Some(mut cs) = exec_ctx.ballpark.balls.contains_key(&ball_id).then(|| exec_ctx.ballpark.combat_states.get(&ball_id).cloned()).flatten() else {
                defer_reply(exec_ctx.deferred_replies, reply, None);
                return None;
            };
            f(&mut cs);
            commit_combat_state_update(exec_ctx, ball_id, cs);
            let updated = exec_ctx.ballpark.combat_states.get(&ball_id).cloned();
            if let (Some(db), Some(ref ucs)) = (&exec_ctx.actor_st.db_opt, &updated) {
                crate::sp6_lifecycle::persist_existing_ship_combat_hp_for_system_in_db(db, exec_ctx.solar_system_id, std::iter::once((ball_id, ucs)));
            }
            defer_reply(exec_ctx.deferred_replies, reply, updated);
            None
        }
        SystemCommand::AddModuleCycle { mut cycle, reply } => {
            let cyc_key = (cycle.ship_id, cycle.module_id);
            let ship_id = cycle.ship_id;
            if cycle.origin_scene_epoch.is_none() {
                cycle.origin_scene_epoch = resolve_active_cycle_origin_epoch(
                    exec_ctx.ballpark,
                    &exec_ctx.actor_st.db_opt,
                    ship_id,
                    cycle.owner_char_id,
                );
            }
            let deact_guard = super::protection::read_deact_hook(exec_ctx);
            let res = handle_add_module_cycle(
                exec_ctx.solar_system_id, exec_ctx.ballpark, &mut exec_ctx.actor_st.active_module_cycles, &exec_ctx.actor_st.base_scramble_strengths,
                &exec_ctx.actor_st.base_core_strengths, &exec_ctx.actor_st.sde_opt, &exec_ctx.actor_st.db_opt, deact_guard.as_ref(), cycle,
            );
            if res.is_ok() {
                exec_ctx.actor_st.weapon_cycle_start_modules.remove(&cyc_key);
                sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ship_id);
            }
            super::protection::snapshot_tackle_totals_in_actor(exec_ctx);
            super::protection::sync_positions_in_actor(exec_ctx);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ActivateModuleCycleAtomic {
            request,
            max_invuln_seq,
            reply,
        } => {
            let res = handle_activate_module_cycle_atomic(exec_ctx, request, max_invuln_seq);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::CommitLancerActivationAtomic {
            request,
            max_invuln_seq,
            reply,
        } => {
            let res = handle_commit_lancer_activation_atomic(exec_ctx, request, max_invuln_seq);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::AddCapacitorClamped { ball_id, delta, reply } => {
            let res = handle_add_capacitor_clamped(exec_ctx, ball_id, delta);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::RemoveModuleCycle { ship_id, module_id, reply } => {
            let mut remaining_cooldown = None;
            exec_ctx.actor_st.weapon_cycle_start_modules.remove(&(ship_id, module_id));
            super::protection::snapshot_tackle_totals_in_actor(exec_ctx);
            if let Some(pos) = exec_ctx.actor_st.active_module_cycles.iter().position(|c| c.ship_id == ship_id && c.module_id == module_id) {
                let removed = exec_ctx.actor_st.active_module_cycles.remove(pos);
                exec_ctx
                    .active_cycle_systems_actor
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&(ship_id, module_id));
                remaining_cooldown = Some((removed.duration_s - removed.elapsed_s).max(0.0) + removed.reactivation_delay_s);
                cleanup_removed_module_cycle(exec_ctx.ballpark, &removed, &exec_ctx.actor_st.db_opt);
                let deact_guard = super::protection::read_deact_hook(exec_ctx);
                reconcile_tackle_and_bubbles(
                    exec_ctx.solar_system_id, exec_ctx.ballpark, &mut exec_ctx.actor_st.active_module_cycles, &exec_ctx.actor_st.base_scramble_strengths,
                    &exec_ctx.actor_st.base_core_strengths, &exec_ctx.actor_st.db_opt, deact_guard.as_ref(),
                );
                sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ship_id);
            }
            super::protection::sync_positions_and_active_ships(exec_ctx);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(remaining_cooldown));
            None
        }
        SystemCommand::RequestCycleDeactivation { ship_id, module_id, reply } => {
            let updated = if let Some(cycle) = exec_ctx.actor_st.active_module_cycles.iter_mut().find(|c| c.ship_id == ship_id && c.module_id == module_id) {
                let is_sde_paid_group = exec_ctx.actor_st.sde_opt.as_ref().and_then(|s| s.types.get(&cycle.module_type_id))
                    .is_some_and(|t| matches!(t.group_id, 62 | 63 | 68 | 201 | 325 | 585 | 1199 | 1698));
                if cycle.has_paid_end_of_cycle_effect() || is_sde_paid_group { cycle.repeat = 0; true } else { false }
            } else { false };
            defer_reply(exec_ctx.deferred_replies, reply, updated);
            None
        }
        SystemCommand::HasModuleCycle { ship_id, module_id, reply } => {
            let exists = exec_ctx.actor_st.active_module_cycles.iter().any(|c| c.ship_id == ship_id && c.module_id == module_id);
            defer_reply(exec_ctx.deferred_replies, reply, exists);
            None
        }
        SystemCommand::ReconcileModuleCycleBeforeActivate { ship_id, module_id, now_unix, reply } => {
            let deact_guard = super::protection::read_deact_hook(exec_ctx);
            let res = reconcile_module_cycle_before_activate_in_actor(
                exec_ctx.solar_system_id, exec_ctx.ballpark, &mut exec_ctx.actor_st.active_module_cycles, &exec_ctx.actor_st.base_scramble_strengths,
                &exec_ctx.actor_st.base_core_strengths, &exec_ctx.actor_st.sde_opt, &exec_ctx.actor_st.db_opt,
                deact_guard.as_ref(), ship_id, module_id, now_unix,
            );
            if res.1 {
                super::protection::sync_active_ships_cache(exec_ctx);
                sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ship_id);
            }
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::UpdateModuleOverload { ship_id, module_id, is_overloaded, reply } => {
            let deact_guard = super::protection::read_deact_hook(exec_ctx);
            let updated = handle_update_module_overload(
                exec_ctx.solar_system_id, exec_ctx.ballpark, &mut exec_ctx.actor_st.active_module_cycles, &exec_ctx.actor_st.base_scramble_strengths,
                &exec_ctx.actor_st.base_core_strengths, &exec_ctx.actor_st.sde_opt, &exec_ctx.actor_st.db_opt,
                deact_guard.as_ref(), ship_id, module_id, is_overloaded,
            );
            defer_reply(exec_ctx.deferred_replies, reply, updated);
            None
        }
        SystemCommand::StopNonWarpModuleCycles { ship_id, reply } => {
            let deact_guard = super::protection::read_deact_hook(exec_ctx);
            remove_non_warp_ship_module_cycles(
                exec_ctx.solar_system_id, exec_ctx.ballpark, &mut exec_ctx.actor_st.active_module_cycles, &exec_ctx.actor_st.base_scramble_strengths,
                &exec_ctx.actor_st.base_core_strengths, &exec_ctx.actor_st.db_opt, ship_id, deact_guard.as_ref(),
            );
            let cycles = &exec_ctx.actor_st.active_module_cycles;
            exec_ctx.actor_st.weapon_cycle_start_modules.retain(|&(s_id, m_id)| cycles.iter().any(|c| c.ship_id == s_id && c.module_id == m_id));
            sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ship_id);
            super::protection::sync_positions_and_active_ships(exec_ctx);
            defer_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        SystemCommand::ApplyFitProjection { ship_id, projection, reply } => {
            let deact_guard = super::protection::read_deact_hook(exec_ctx);
            let applied = apply_fit_projection_with_cycles_in_actor(
                exec_ctx.solar_system_id, exec_ctx.ballpark, &mut exec_ctx.actor_st.fit_projections, &mut exec_ctx.actor_st.active_module_cycles,
                &exec_ctx.actor_st.base_scramble_strengths, &exec_ctx.actor_st.base_core_strengths, &exec_ctx.actor_st.db_opt,
                deact_guard.as_ref(), ship_id, projection,
            );
            sync_ball_protection_in_actor(exec_ctx.ballpark, exec_ctx.actor_st, ship_id);
            if let Some(tx) = reply { defer_reply(exec_ctx.deferred_replies, tx, applied); }
            None
        }
        SystemCommand::CurrentTick { reply } => {
            defer_reply(exec_ctx.deferred_replies, reply, exec_ctx.ballpark.current_tick);
            None
        }
        other => Some(other),
    }
}

impl SystemHandle {
    /// Queries the actor's authoritative `Ballpark::current_tick` without locking `ballpark` externally (`#1169`, `#1173`).
    pub(crate) async fn query_current_tick(&self) -> Result<u64, String> {
        super::protection::call_actor_reply(self, "CurrentTick", |reply| {
            SystemCommand::CurrentTick { reply }
        }).await
    }

    /// Pushes an immutable [`eve_dogma::FitProjection`] to the system actor and awaits completion (`#922`).
    pub async fn apply_fit_projection(
        &self,
        ship_id: BallId,
        projection: eve_dogma::FitProjection,
    ) -> bool {
        super::protection::call_actor_reply(self, "ApplyFitProjection", |tx| {
            SystemCommand::ApplyFitProjection { ship_id, projection, reply: Some(tx) }
        })
        .await
        .unwrap_or(false)
    }

    /// Non-blocking enqueue of [`SystemCommand::ApplyFitProjection`] for synchronous fit-mutation paths (`#922`).
    pub fn try_send_fit_projection(
        &self,
        ship_id: BallId,
        projection: eve_dogma::FitProjection,
    ) -> bool {
        self.cmd_sender()
            .try_send(SystemCommand::ApplyFitProjection { ship_id, projection, reply: None })
            .is_ok()
    }

    /// Reads the latest [`eve_dogma::FitProjection`] stored in actor state for `ship_id`, if any.
    pub fn latest_fit_projection(&self, ship_id: BallId) -> Option<eve_dogma::FitProjection> {
        self.actor_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .fit_projections
            .get(&ship_id)
            .cloned()
    }

    /// Atomically validates preconditions, commits a module cycle, and breaks protection (`#1486`).
    pub fn activate_module_cycle_atomic(
        &self,
        request: AtomicActivateCycleRequest,
    ) -> impl std::future::Future<Output = Result<AtomicActivateCycleOutcome, String>> + '_ {
        let max_invuln_seq = self.current_invuln_seq_if_safe();
        self.activate_module_cycle_atomic_at_seq(request, max_invuln_seq)
    }

    /// Sequence-bounded variant of [`Self::activate_module_cycle_atomic`] (`#1486`).
    pub async fn activate_module_cycle_atomic_at_seq(
        &self,
        request: AtomicActivateCycleRequest,
        max_invuln_seq: Option<u64>,
    ) -> Result<AtomicActivateCycleOutcome, String> {
        super::protection::call_actor_result(self, "ActivateModuleCycleAtomic", |reply| {
            SystemCommand::ActivateModuleCycleAtomic {
                request,
                max_invuln_seq,
                reply,
            }
        })
        .await
    }

    /// Sets movement mode and atomically breaks protection in the same actor turn (`#1486`).
    pub fn set_movement_mode_breaking_protection(
        &self,
        ball_id: BallId,
        char_id: Option<CharacterId>,
        mode: MovementMode,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
    ) -> impl std::future::Future<Output = Result<(u64, ProtectionBreakEvents), String>> + '_ {
        let max_invuln_seq = self.current_invuln_seq_if_safe();
        self.set_movement_mode_breaking_protection_at_seq(
            ball_id,
            char_id,
            mode,
            reason,
            max_invuln_seq,
            real_now_unix,
        )
    }

    /// Sequence-bounded variant of [`Self::set_movement_mode_breaking_protection`] (`#1486`).
    pub async fn set_movement_mode_breaking_protection_at_seq(
        &self,
        ball_id: BallId,
        char_id: Option<CharacterId>,
        mode: MovementMode,
        reason: ProtectionBreakReason,
        max_invuln_seq: Option<u64>,
        real_now_unix: u64,
    ) -> Result<(u64, ProtectionBreakEvents), String> {
        super::protection::call_actor_result(
            self,
            "SetMovementModeBreakingProtection",
            |reply| SystemCommand::SetMovementModeBreakingProtection {
                ball_id,
                char_id,
                mode,
                reason,
                real_now_unix,
                max_invuln_seq,
                reply,
            },
        )
        .await
    }

    /// Atomically verifies ship presence, non-active lance cycle, non-warp, non-active-cloak,
    /// and sufficient capacitor, deducts `cap_cost`, and breaks ship protection inside the actor (`#1486`).
    pub fn commit_lancer_activation_atomic(
        &self,
        request: AtomicLancerActivationRequest,
    ) -> impl std::future::Future<Output = Result<AtomicActivateCycleOutcome, String>> + '_ {
        let max_invuln_seq = self.current_invuln_seq_if_safe();
        self.commit_lancer_activation_atomic_at_seq(request, max_invuln_seq)
    }

    /// Sequence-bounded variant of [`Self::commit_lancer_activation_atomic`] (`#1486`).
    pub async fn commit_lancer_activation_atomic_at_seq(
        &self,
        request: AtomicLancerActivationRequest,
        max_invuln_seq: Option<u64>,
    ) -> Result<AtomicActivateCycleOutcome, String> {
        super::protection::call_actor_result(self, "CommitLancerActivationAtomic", |reply| {
            SystemCommand::CommitLancerActivationAtomic {
                request,
                max_invuln_seq,
                reply,
            }
        })
        .await
    }

    /// Adds a non-negative capacitor delta clamped at the ship's effective maximum capacitor (`#1486`).
    pub async fn add_capacitor_clamped(&self, ball_id: BallId, delta: f64) -> Result<f64, String> {
        super::protection::call_actor_result(self, "AddCapacitorClamped", |reply| {
            SystemCommand::AddCapacitorClamped { ball_id, delta, reply }
        })
        .await
    }

    /// Sets warp movement mode and atomically breaks non-covert cloak/invuln in one turn (`#1486`).
    pub fn set_warp_movement_mode_breaking_protection(
        &self,
        ball_id: BallId,
        char_id: Option<CharacterId>,
        mode: MovementMode,
        provenance: WarpProvenance,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
    ) -> impl std::future::Future<Output = Result<(u64, ProtectionBreakEvents), String>> + '_ {
        let max_invuln_seq = self.current_invuln_seq_if_safe();
        self.set_warp_movement_mode_breaking_protection_at_seq(
            ball_id,
            char_id,
            mode,
            provenance,
            reason,
            max_invuln_seq,
            real_now_unix,
        )
    }

    /// Sequence-bounded variant of [`Self::set_warp_movement_mode_breaking_protection`] (`#1486`).
    #[allow(clippy::too_many_arguments)]
    pub async fn set_warp_movement_mode_breaking_protection_at_seq(
        &self,
        ball_id: BallId,
        char_id: Option<CharacterId>,
        mode: MovementMode,
        provenance: WarpProvenance,
        reason: ProtectionBreakReason,
        max_invuln_seq: Option<u64>,
        real_now_unix: u64,
    ) -> Result<(u64, ProtectionBreakEvents), String> {
        super::protection::call_actor_result(
            self,
            "SetWarpMovementModeBreakingProtection",
            |reply| SystemCommand::SetWarpMovementModeBreakingProtection {
                ball_id,
                char_id,
                mode,
                provenance,
                reason,
                real_now_unix,
                max_invuln_seq,
                reply,
            },
        )
        .await
    }

    /// Issues `goto_point` and atomically breaks protection in the same actor turn (`#1486`).
    pub fn goto_point_breaking_protection(
        &self,
        ball_id: BallId,
        char_id: Option<CharacterId>,
        point: DVec3,
        reason: ProtectionBreakReason,
        real_now_unix: u64,
    ) -> impl std::future::Future<Output = Result<(u64, ProtectionBreakEvents), String>> + '_ {
        let max_invuln_seq = self.current_invuln_seq_if_safe();
        self.goto_point_breaking_protection_at_seq(
            ball_id,
            char_id,
            point,
            reason,
            max_invuln_seq,
            real_now_unix,
        )
    }

    /// Sequence-bounded variant of [`Self::goto_point_breaking_protection`] (`#1486`).
    pub async fn goto_point_breaking_protection_at_seq(
        &self,
        ball_id: BallId,
        char_id: Option<CharacterId>,
        point: DVec3,
        reason: ProtectionBreakReason,
        max_invuln_seq: Option<u64>,
        real_now_unix: u64,
    ) -> Result<(u64, ProtectionBreakEvents), String> {
        super::protection::call_actor_result(self, "GotoPointBreakingProtection", |reply| {
            SystemCommand::GotoPointBreakingProtection {
                ball_id,
                char_id,
                point,
                reason,
                real_now_unix,
                max_invuln_seq,
                reply,
            }
        })
        .await
    }

    /// Canonical read-only check of whether `(ship_id, module_id)` has an active cycle (`#1486`).
    pub fn is_module_active_sync(&self, ship_id: BallId, module_id: ItemId) -> bool {
        self.actor_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .active_module_cycles
            .iter()
            .any(|c| c.ship_id == ship_id && c.module_id == module_id)
    }
}

impl SystemRegistry {
    /// Canonical read-only check of whether `(ship_id, module_id)` has an active cycle (`#1486`).
    pub fn is_module_active_sync(
        &self,
        solar_system_id: SolarSystemId,
        ship_id: BallId,
        module_id: ItemId,
    ) -> bool {
        self.get_handle(solar_system_id)
            .is_some_and(|h| h.is_module_active_sync(ship_id, module_id))
    }
}
