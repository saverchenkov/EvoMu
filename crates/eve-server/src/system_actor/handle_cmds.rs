//! Mailbox command implementations and `SystemHandle` sync-writer facades (`#1342`).

#![forbid(unsafe_code)]

use super::{
    ball_ops::{self, BallOpsCtx, ACTOR_CMD_TIMEOUT},
    belts, celestials, cycles, cycles_tackle, defer_barrier_cmd_reply, defer_reply, drones,
    ewar_cooldowns, missiles, post_unlock, protection, sync_ball_positions, ActiveNaniteRepair,
    ActorExecCtx, AsteroidState, CharacterId, CooldownPersistGate, ExternalActiveModuleOp,
    PendingCycleAction, PendingDestroyedOutbox, StepFullTickOutput, SystemActorState,
    SystemCommand, SystemHandle, SystemView, TargetLockCompletedHook, TickBroadcast,
    TryRepairCombatOutcome, WarpStateMutOp,
};
use eve_db::lock_rank::{held_lock_ranks, AnyRwLock, RankedRwLock, RANK_LEAF};
use eve_destiny::{Ball, BallId, Ballpark, CombatState, DestinyEvent, MovementMode, SlimItemMeta};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, oneshot, watch};

pub(in crate::system_actor) struct TickEnvRefs<'a> {
    pub(in crate::system_actor) solar_system_id: SolarSystemId,
    pub(in crate::system_actor) tidi: f64,
    pub(in crate::system_actor) ball_positions: &'a Arc<super::BallPositionSyncCache>,
    pub(in crate::system_actor) scrambled_balls: &'a Arc<RankedRwLock<HashSet<BallId>, RANK_LEAF>>,
    pub(in crate::system_actor) active_module_ships: &'a Arc<RankedRwLock<HashSet<BallId>, RANK_LEAF>>,
    pub(in crate::system_actor) last_visible_stamp: &'a Arc<AtomicU64>,
    pub(in crate::system_actor) last_fx_stamp: &'a Arc<AtomicU64>,
    pub(in crate::system_actor) broadcast_seq_gate: Arc<std::sync::Mutex<u64>>,
    pub(in crate::system_actor) event_tx: &'a broadcast::Sender<TickBroadcast>,
    pub(in crate::system_actor) sim_elapsed_secs_bits: &'a Arc<AtomicU64>,
    pub(in crate::system_actor) completed_post_unlock_seq: &'a AtomicU64,
    pub(in crate::system_actor) has_in_flight_post_unlock: bool,
    pub(in crate::system_actor) target_lock_completed_hook: &'a Arc<RankedRwLock<Option<TargetLockCompletedHook>, RANK_LEAF>>,
    pub(in crate::system_actor) raw_deact_hook: Option<super::ModuleDeactivationHook>,
    pub(in crate::system_actor) post_deact_hook: Option<super::PairedModuleDeactivationPostUnlockHook>,
    pub(in crate::system_actor) deactivation_sink_hook: Option<super::ModuleDeactivationHook>,
    pub(in crate::system_actor) cmd_tx_weak: Option<tokio::sync::mpsc::WeakSender<SystemCommand>>,
    pub(in crate::system_actor) pending_destroyed_balls: &'a PendingDestroyedOutbox,
    pub(in crate::system_actor) cooldown_persist_gate: &'a Arc<CooldownPersistGate>,
    pub(in crate::system_actor) tidi_factor_bits: &'a std::sync::atomic::AtomicU64,
    pub(in crate::system_actor) view_tx: &'a watch::Sender<Arc<SystemView>>,
}

impl<'a> TickEnvRefs<'a> {
    fn split_exec_ctx<'b>(
        exec_ctx: &'a mut ActorExecCtx<'b>,
    ) -> (
        Self,
        &'a mut Ballpark,
        &'a mut SystemActorState,
        &'a mut post_unlock::PostUnlockOps,
    ) {
        let env = Self {
            solar_system_id: exec_ctx.solar_system_id,
            tidi: celestials::clamp_tidi_factor_bits(exec_ctx.tidi_factor_bits_actor),
            ball_positions: exec_ctx.ball_positions_actor,
            scrambled_balls: exec_ctx.scrambled_balls_actor,
            active_module_ships: exec_ctx.active_module_ships_actor,
            last_visible_stamp: exec_ctx.last_visible_stamp_actor,
            last_fx_stamp: exec_ctx.last_fx_stamp_actor,
            broadcast_seq_gate: Arc::clone(exec_ctx.broadcast_seq_gate_actor),
            event_tx: exec_ctx.event_tx_actor,
            sim_elapsed_secs_bits: exec_ctx.sim_elapsed_secs_bits_actor,
            completed_post_unlock_seq: exec_ctx.completed_post_unlock_seq_actor,
            has_in_flight_post_unlock: exec_ctx.post_unlock_in_flight_actor.load(Ordering::Acquire) > 0,
            target_lock_completed_hook: exec_ctx.target_lock_completed_hook_actor,
            raw_deact_hook: exec_ctx.raw_deact_hook.clone(),
            post_deact_hook: exec_ctx.post_deact_hook.clone(),
            deactivation_sink_hook: exec_ctx.deactivation_sink_hook.clone(),
            cmd_tx_weak: Some(exec_ctx.cmd_tx_weak.clone()),
            pending_destroyed_balls: exec_ctx.pending_destroyed_actor,
            cooldown_persist_gate: exec_ctx.cooldown_persist_gate_actor,
            tidi_factor_bits: exec_ctx.tidi_factor_bits_actor,
            view_tx: exec_ctx.view_tx_actor,
        };
        (
            env,
            exec_ctx.ballpark,
            exec_ctx.actor_st,
            exec_ctx.post_unlock_ops,
        )
    }

    pub(in crate::system_actor) fn from_handle(
        handle: &'a SystemHandle,
        broadcast_seq_gate: Arc<std::sync::Mutex<u64>>,
    ) -> Self {
        let (raw_deact_hook, post_deact_hook) = ewar_cooldowns::read_deactivation_hooks(
            &handle.module_deactivation_hook,
            &handle.module_deactivation_post_unlock_hook,
        );
        let deactivation_sink_hook = ewar_cooldowns::active_sink_hook(
            raw_deact_hook.as_ref(),
            post_deact_hook.as_ref(),
        );
        Self {
            solar_system_id: handle.solar_system_id,
            tidi: handle.tidi_factor(),
            ball_positions: &handle.ball_positions,
            scrambled_balls: &handle.scrambled_balls,
            active_module_ships: &handle.active_module_ships,
            last_visible_stamp: &handle.last_visible_stamp,
            last_fx_stamp: &handle.last_fx_stamp,
            broadcast_seq_gate,
            event_tx: &handle.event_tx,
            sim_elapsed_secs_bits: &handle.sim_elapsed_secs_bits,
            completed_post_unlock_seq: &handle.completed_post_unlock_seq,
            has_in_flight_post_unlock: handle.has_in_flight_post_unlock(),
            target_lock_completed_hook: &handle.target_lock_completed_hook,
            raw_deact_hook,
            post_deact_hook,
            deactivation_sink_hook,
            cmd_tx_weak: Some(handle.cmd_tx.downgrade()),
            pending_destroyed_balls: &handle.pending_destroyed_balls,
            cooldown_persist_gate: &handle.cooldown_persist_gate,
            tidi_factor_bits: &handle.tidi_factor_bits,
            view_tx: &handle.view_tx,
        }
    }
}

#[inline]
pub(in crate::system_actor) fn defer_maybe_barrier_reply<T: Send + 'static>(
    exec_ctx: &mut ActorExecCtx<'_>,
    wait_for_post_unlock: bool,
    reply: oneshot::Sender<T>,
    value: T,
) {
    if wait_for_post_unlock {
        defer_barrier_cmd_reply(exec_ctx, reply, value);
    } else {
        defer_reply(exec_ctx.deferred_replies, reply, value);
    }
}

#[inline]
fn defer_opt_reply<T: Send + 'static>(
    deferred: &mut Vec<Box<dyn FnOnce() + Send>>,
    reply: Option<oneshot::Sender<T>>,
    value: T,
) {
    if let Some(tx) = reply {
        defer_reply(deferred, tx, value);
    }
}

fn publish_view_snapshot(
    view_tx: &watch::Sender<Arc<SystemView>>,
    tidi_factor_bits: &std::sync::atomic::AtomicU64,
    bp: &Ballpark,
    st: &SystemActorState,
    tidi: f64,
) {
    let next = SystemView::from_ballpark(bp, Some(st), tidi, None);
    celestials::send_view_preserving_atomic_tidi(view_tx, tidi_factor_bits, next);
}

pub(in crate::system_actor) fn refresh_stargate_activation_in_ctx(
    ctx: &mut BallOpsCtx<'_>,
    lazy_enabled: bool,
    loaded_systems: &HashSet<SolarSystemId>,
) {
    let gate_updates: Vec<(BallId, i64)> = ctx
        .ballpark
        .ball_slim_meta
        .iter()
        .filter_map(|(&gate_id, meta)| {
            (meta.group_id == 10).then(|| {
                let active = !lazy_enabled
                    || meta.jump_target.map(|(_, s)| s).is_some_and(|s| loaded_systems.contains(&s));
                let state = if active {
                    eve_destiny::STARGATE_ACTIVATION_STATE_OPEN
                } else {
                    eve_destiny::STARGATE_ACTIVATION_STATE_CLOSED
                };
                (gate_id, state)
            })
        })
        .collect();
    for (gate_id, state) in gate_updates {
        ctx.ballpark.set_stargate_activation_state(gate_id, state, 0, 0);
    }
}

pub(in crate::system_actor) fn activate_stargate_transition_for_dest_in_ctx(
    ctx: &mut BallOpsCtx<'_>,
    dest_sys_id: SolarSystemId,
    now_ms: u64,
) {
    let gate_ids: Vec<BallId> = ctx
        .ballpark
        .ball_slim_meta
        .iter()
        .filter_map(|(&gate_id, meta)| {
            (meta.group_id == 10 && meta.jump_target.is_some_and(|(_, s)| s == dest_sys_id)).then_some(gate_id)
        })
        .collect();
    for gate_id in gate_ids {
        ctx.ballpark.activate_stargate_transition(
            gate_id,
            eve_destiny::STARGATE_ACTIVATION_TRANSITION_MS,
            (now_ms > 0).then_some(now_ms),
        );
    }
}

pub(in crate::system_actor) fn repair_combat_state_sync_in_ctx(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    hull_heal: f64,
    armor_heal: f64,
    shield_heal: f64,
) -> Option<CombatState> {
    let updated = super::apply_repair_combat_state_in_ballpark(ctx.ballpark, ball_id, hull_heal, armor_heal, shield_heal)?;
    ctx.sync_positions();
    if ctx.actor_st.db_opt.is_some() {
        ctx.post_unlock_ops.push_ship_combat_hp(post_unlock::CombatHpDbUpdate {
            ball_id,
            combat_state: updated.clone(),
            expected_system: Some(ctx.solar_system_id),
            require_persisted_entity: true,
        });
    }
    Some(updated)
}

pub(in crate::system_actor) fn set_ball_position_sync_in_ctx(ctx: &mut BallOpsCtx<'_>, ball_id: BallId, position: DVec3) {
    let ok = ctx.ballpark.set_ball_position(ball_id, position);
    ctx.sync_positions();
    if ok {
        super::collect_ball_positions_for_post_unlock(
            ctx.ballpark,
            &[ball_id],
            &ctx.actor_st.db_opt,
            &ctx.actor_st.ball_space_entry_epochs,
            ctx.post_unlock_ops,
        );
    }
}

pub(in crate::system_actor) fn stop_ball_in_ctx(ctx: &mut BallOpsCtx<'_>, ball_id: BallId) {
    let _ = ctx.ballpark.force_stop_warp(ball_id);
    ctx.sync_positions();
    ctx.actor_st.warp_state.clear_ball(ball_id, true);
}

pub(in crate::system_actor) fn remove_ball_sync_in_ctx(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    emit_destruction_fx: bool,
    event_rx_count: usize,
) {
    SystemHandle::drain_ship_nanite_repairs_collecting(&mut ctx.actor_st.active_nanite_repairs, ball_id, true, ctx.post_unlock_ops);
    let fallback_slim_owner = ctx.ballpark.ball_slim_meta.get(&ball_id).and_then(|m| (m.owner_id > 0).then_some(m.owner_id));
    let removed_cycles = cycles::remove_all_ship_module_cycles_collecting(
        ctx.solar_system_id,
        ctx.ballpark,
        &mut ctx.actor_st.active_module_cycles,
        &ctx.actor_st.base_scramble_strengths,
        &ctx.actor_st.base_core_strengths,
        &None,
        ball_id,
        None,
        &mut post_unlock::PostUnlockOps::default(),
    );
    ctx.actor_st.external_active_modules.retain(|&(sid, _)| sid != ball_id);
    ctx.actor_st.base_scramble_strengths.remove(&ball_id);
    ctx.actor_st.base_core_strengths.remove(&ball_id);
    ctx.actor_st.protection_registry.remove(&ball_id);
    if emit_destruction_fx {
        let _ = ctx.ballpark.destroy_ball_with_terminal_explosion(ball_id, None);
    } else {
        let _ = ctx.ballpark.remove_ball(ball_id);
    }
    let _ = ctx.actor_st.clear_fighter_ball_and_track(ball_id);
    drones::clear_removed_ball_drones_in_state(ctx.actor_st, ball_id);

    if ctx.actor_st.db_opt.is_some() || ctx.deactivation_sink_hook.is_some() {
        let built = post_unlock::build_removed_ball_post_unlock_ops(
            ctx.solar_system_id,
            ball_id,
            removed_cycles,
            fallback_slim_owner,
            ctx.deactivation_sink_hook.is_some(),
        );
        ctx.post_unlock_ops.intents.extend(built.intents);
        ctx.post_unlock_ops.item_qty_deltas.extend(built.item_qty_deltas);
        ctx.post_unlock_ops.crystal_dmg_deltas.extend(built.crystal_dmg_deltas);
        ctx.post_unlock_ops.overheat_dmg_deltas.extend(built.overheat_dmg_deltas);
    }

    super::remove_ball_checkpoint_tracking(ctx.actor_st, ball_id);
    super::update_system_hibernation_flag(ctx.actor_st, ctx.ballpark, event_rx_count);
    ctx.sync_positions();
}

pub(in crate::system_actor) fn spawn_asteroid_in_ctx(ctx: &mut BallOpsCtx<'_>, asteroid: AsteroidState, mut ball: Ball) {
    let ball_id = ball.id;
    SystemHandle::spawn_asteroid_ball_in_ballpark(ctx.ballpark, &ctx.actor_st.sde_opt, &mut ball);
    ctx.actor_st.asteroid_templates.insert(ball_id, (asteroid.clone(), ball));
    ctx.actor_st.asteroids.insert(ball_id, asteroid);
    ctx.sync_positions();
}

pub(in crate::system_actor) fn sync_asteroid_quantity_in_ctx(
    ctx: &mut BallOpsCtx<'_>,
    asteroid_id: BallId,
    remaining_units: u64,
    due_unix_on_belt_empty: u64,
) {
    let mut belt_id_opt = None;
    if let Some(ast) = ctx.actor_st.asteroids.get_mut(&asteroid_id) {
        ast.remaining_units = remaining_units;
        ast.depleted = remaining_units == 0;
        if remaining_units == 0 {
            belt_id_opt = Some(ast.belt_id);
        }
    }
    if let Some(belt_id) = belt_id_opt {
        if ctx.actor_st.asteroids.values().filter(|a| a.belt_id == belt_id).all(|a| a.remaining_units == 0) {
            for belt_ast in ctx.actor_st.asteroids.values_mut().filter(|a| a.belt_id == belt_id) {
                belt_ast.respawn_at_tick = Some(due_unix_on_belt_empty);
            }
        }
    }
}

fn finish_spawn_ball_accounting(ctx: &mut BallOpsCtx<'_>, ball_id: BallId) {
    ctx.sync_positions();
    protection::sync_ball_protection_in_actor(ctx.ballpark, ctx.actor_st, ball_id);
    let dyn_count = ctx.ballpark.balls.keys().filter(|&&bid| !ctx.ballpark.is_static_ball(bid)).count();
    ctx.actor_st.dynamic_ball_count.store(dyn_count, Ordering::Relaxed);
    if dyn_count > 0 {
        ctx.actor_st.hibernated.store(false, Ordering::Relaxed);
        ctx.actor_st.hibernated_since_unix.store(0, Ordering::Relaxed);
        ctx.actor_st.idle_ticks.store(0, Ordering::Relaxed);
    }
}

pub(in crate::system_actor) fn spawn_ball_with_meta_in_ctx(
    ctx: &mut BallOpsCtx<'_>,
    ball: Ball,
    combat_state: CombatState,
    slim_meta: SlimItemMeta,
) {
    let ball_id = ball.id;
    if ctx.ballpark.balls.contains_key(&ball_id) {
        let _ = ctx.ballpark.remove_ball(ball_id);
    }
    if matches!(ball.mode, MovementMode::Rigid) {
        ctx.ballpark.add_static_ball_with_meta(ball.clone(), slim_meta);
        ctx.ballpark.combat_states.insert(ball_id, combat_state);
        ctx.ballpark.pending_events.push(DestinyEvent::AddBall { ball: ball.clone() });
        ctx.ballpark.queue_event_for_all_grid_observers(Some(ball_id), &DestinyEvent::AddBalls2 { balls: vec![ball] });
    } else {
        if belts::should_pin_stationary_ball(&ball, Some(&slim_meta)) {
            ctx.ballpark.mark_ball_non_free(ball_id, false);
        }
        ctx.ballpark.set_ball_slim_meta(ball_id, slim_meta);
        ctx.ballpark.add_ball(ball);
        ctx.ballpark.combat_states.insert(ball_id, combat_state);
    }
    finish_spawn_ball_accounting(ctx, ball_id);
}

pub(in crate::system_actor) fn spawn_ball_with_combat_in_ctx(ctx: &mut BallOpsCtx<'_>, ball: Ball, combat_state: CombatState) {
    let ball_id = ball.id;
    let existing_meta = ctx.ballpark.ball_slim_meta.get(&ball_id).cloned();
    if ctx.ballpark.balls.contains_key(&ball_id) {
        let _ = ctx.ballpark.remove_ball(ball_id);
    }
    if let Some(meta) = existing_meta {
        ctx.ballpark.set_ball_slim_meta(ball_id, meta);
    } else {
        belts::ensure_default_slim_meta(ctx.ballpark, &ball, &ctx.actor_st.sde_opt);
    }
    if belts::should_pin_stationary_ball(&ball, ctx.ballpark.ball_slim_meta.get(&ball_id)) {
        ctx.ballpark.mark_ball_non_free(ball_id, false);
    }
    ctx.ballpark.add_ball(ball);
    ctx.ballpark.combat_states.insert(ball_id, combat_state);
    finish_spawn_ball_accounting(ctx, ball_id);
}

pub(in crate::system_actor) fn update_combat_state_in_ctx(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    recompute_attrs: bool,
    mutator: Box<dyn FnOnce(&mut CombatState) + Send>,
) -> bool {
    let Some(cs) = ctx.ballpark.combat_states.get_mut(&ball_id) else {
        return false;
    };
    mutator(cs);
    if recompute_attrs {
        ctx.ballpark.recompute_ball_attributes(ball_id);
    }
    ctx.sync_positions();
    protection::sync_ball_protection_in_actor(ctx.ballpark, ctx.actor_st, ball_id);
    true
}

pub(in crate::system_actor) fn apply_direct_combat_damage_in_ctx(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    damage: f64,
) -> Option<bool> {
    let cs = ctx.ballpark.combat_states.get_mut(&ball_id)?;
    let mut rem = damage.max(0.0);
    let ds = rem.min(cs.shield_hp);
    cs.shield_hp -= ds;
    rem -= ds;
    let da = rem.min(cs.armor_hp);
    cs.armor_hp -= da;
    rem -= da;
    cs.structure_hp = (cs.structure_hp - rem).max(0.0);
    let dead = cs.structure_hp <= 0.0;
    ctx.sync_positions();
    protection::sync_ball_protection_in_actor(ctx.ballpark, ctx.actor_st, ball_id);
    Some(dead)
}

pub(in crate::system_actor) fn sync_ship_ballpark_stats_in_ctx(ctx: &mut BallOpsCtx<'_>, ball_id: BallId, fresh: &CombatState) -> bool {
    let prev_max_vel = ctx.ballpark.balls.get(&ball_id).map(|b| b.max_velocity);
    let Some(cs) = ctx.ballpark.combat_states.get_mut(&ball_id) else {
        return false;
    };
    cs.base_max_velocity = fresh.base_max_velocity;
    cs.base_mass = fresh.base_mass;
    cs.base_inertia = fresh.base_inertia;
    cs.base_signature_radius = fresh.base_signature_radius;
    cs.base_max_shield_hp = fresh.base_max_shield_hp;
    cs.base_max_armor_hp = fresh.base_max_armor_hp;
    cs.base_max_structure_hp = fresh.base_max_structure_hp;
    cs.base_max_capacitor = fresh.base_max_capacitor;
    cs.shield_recharge_time_ms = fresh.shield_recharge_time_ms;
    cs.base_capacitor_recharge_time_ms = fresh.base_capacitor_recharge_time_ms;
    cs.base_shield_resonance = fresh.base_shield_resonance;
    cs.base_armor_resonance = fresh.base_armor_resonance;
    cs.base_structure_resonance = fresh.base_structure_resonance;
    ctx.ballpark.recompute_ball_attributes(ball_id);
    if let (Some(prev), Some(new_v)) = (prev_max_vel, ctx.ballpark.balls.get(&ball_id).map(|b| b.max_velocity)) {
        if (prev - new_v).abs() > 1e-6 {
            ctx.ballpark.emit_event(Some(ball_id), DestinyEvent::SetMaxSpeed { ball_id, max_speed: new_v });
        }
    }
    true
}

pub(in crate::system_actor) fn mutate_external_active_module_in_ctx(ctx: &mut BallOpsCtx<'_>, op: ExternalActiveModuleOp) -> bool {
    match op {
        ExternalActiveModuleOp::Register(ship_id, module_id) => {
            ctx.actor_st.external_active_modules.insert((ship_id, module_id))
        }
        ExternalActiveModuleOp::Unregister(ship_id, module_id) => {
            ctx.actor_st.external_active_modules.remove(&(ship_id, module_id))
        }
        ExternalActiveModuleOp::ClearAll => {
            let had_entries = !ctx.actor_st.external_active_modules.is_empty();
            ctx.actor_st.external_active_modules.clear();
            had_entries
        }
    }
}

pub(in crate::system_actor) fn cancel_nanite_repairs_for_ship_in_ctx(ctx: &mut BallOpsCtx<'_>, ship_id: BallId) -> Vec<ActiveNaniteRepair> {
    let mut cancelled = Vec::new();
    for job in std::mem::take(&mut ctx.actor_st.active_nanite_repairs) {
        if job.ship_id == ship_id {
            cancelled.push(job);
        } else {
            ctx.actor_st.active_nanite_repairs.push(job);
        }
    }
    cancelled
}

pub(in crate::system_actor) fn mutate_warp_state_in_ctx(ctx: &mut BallOpsCtx<'_>, op: WarpStateMutOp) -> bool {
    let w = &mut ctx.actor_st.warp_state;
    match op {
        WarpStateMutOp::SetProvenance { ball_id, provenance } => {
            w.provenance.insert(ball_id, provenance);
            true
        }
        WarpStateMutOp::ClearProvenance { ball_id } => w.provenance.remove(&ball_id).is_some(),
        WarpStateMutOp::TrackPlayerOrFleet { ball_id, char_id } => {
            w.track_player_or_fleet_warp(ball_id, char_id);
            true
        }
        WarpStateMutOp::TrackReturn { ball_id } => {
            w.track_return_warp(ball_id);
            true
        }
        WarpStateMutOp::ClearBall { ball_id } => {
            w.clear_ball(ball_id, true);
            true
        }
        WarpStateMutOp::ClearReturnAndPending { ball_id } => {
            w.return_warps.remove(&ball_id);
            w.pending_warp_finished.remove(&ball_id);
            true
        }
        WarpStateMutOp::ConsumeFinished { ball_id } => w.consume_warp_finished(ball_id),
        WarpStateMutOp::InsertActiveForTest { ball_id, char_id } => {
            w.active_warps.insert(ball_id, char_id);
            true
        }
        WarpStateMutOp::InsertReturnForTest { ball_id } => {
            w.return_warps.insert(ball_id);
            true
        }
        WarpStateMutOp::ClearAllForTest => {
            *w = super::types::ActorWarpState::default();
            true
        }
        WarpStateMutOp::RestoreForTest(state) => {
            *w = *state;
            true
        }
    }
}

pub(in crate::system_actor) fn finish_warp_for_dock_in_ctx(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    landing_pos: Option<DVec3>,
) -> Result<u64, String> {
    if let Some(pos) = landing_pos.filter(|p| p.is_finite()) {
        if let Some(ball) = ctx.ballpark.balls.get_mut(&ball_id) {
            if matches!(ball.mode, MovementMode::Warp { .. }) {
                ball.position = pos;
            }
        }
    }
    ctx.ballpark.force_stop_warp(ball_id)?;
    ctx.sync_positions();
    ctx.actor_st.warp_state.clear_ball(ball_id, true);
    Ok(ctx.ballpark.current_tick)
}

pub(in crate::system_actor) fn respawn_depleted_belts_in_ctx(ctx: &mut BallOpsCtx<'_>) -> usize {
    let super::SystemActorState { asteroids, asteroid_templates, sde_opt, db_opt, .. } = &mut *ctx.actor_st;
    let count = belts::respawn_depleted_belts_on_downtime_inner_collecting(
        ctx.ballpark, asteroids, asteroid_templates, sde_opt, db_opt, ctx.post_unlock_ops,
    );
    if count > 0 {
        ctx.sync_positions();
    }
    count
}

pub(in crate::system_actor) fn try_remove_module_cycle_in_ctx(ctx: &mut BallOpsCtx<'_>, ship_id: BallId, module_id: i64) {
    ctx.actor_st.weapon_cycle_start_modules.remove(&(ship_id, module_id));
    if let Some(totals) = ctx.last_active_totals.as_deref_mut() {
        cycles_tackle::snapshot_active_tackle_totals(ctx.ballpark, &ctx.actor_st.active_module_cycles, totals);
    }
    if let Some(pos) = ctx.actor_st.active_module_cycles.iter().position(|c| c.ship_id == ship_id && c.module_id == module_id) {
        let removed = ctx.actor_st.active_module_cycles.remove(pos);
        cycles::cleanup_removed_module_cycle_collecting(ctx.ballpark, &removed, &ctx.actor_st.db_opt, ctx.post_unlock_ops);
        let deact_guard = ctx.deactivation_sink_hook.clone();
        cycles_tackle::reconcile_tackle_and_bubbles_collecting(
            ctx.solar_system_id,
            ctx.ballpark,
            &mut ctx.actor_st.active_module_cycles,
            &ctx.actor_st.base_scramble_strengths,
            &ctx.actor_st.base_core_strengths,
            &ctx.actor_st.db_opt,
            deact_guard.as_ref(),
            ctx.post_unlock_ops,
        );
        protection::sync_ball_protection_in_actor(ctx.ballpark, ctx.actor_st, ship_id);
    }
    sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
}

pub(in crate::system_actor) fn step_manual_in_ctx(
    env: &TickEnvRefs<'_>,
    bp: &mut Ballpark,
    st: &mut SystemActorState,
    post_unlock_ops: &mut post_unlock::PostUnlockOps,
    dt: f64,
) -> Vec<DestinyEvent> {
    let dt_sim = if dt > 0.0 && dt.is_finite() { dt * env.tidi } else { dt };
    if dt_sim > 0.0 && dt_sim.is_finite() {
        st.sim_elapsed_secs += dt_sim;
        env.sim_elapsed_secs_bits.store(st.sim_elapsed_secs.to_bits(), Ordering::Release);
    }
    let events = bp.step(dt_sim);
    super::collect_landed_warps_for_post_unlock_with_in_flight(
        bp, &events, &st.db_opt, &st.ball_space_entry_epochs, env.has_in_flight_post_unlock, post_unlock_ops,
    );
    super::clear_landed_warp_tracking_from_events(bp, &events, &mut st.warp_state);
    sync_ball_positions(bp, env.ball_positions, env.scrambled_balls);
    protection::sync_all_protection_registry_in_actor(bp, st);
    publish_view_snapshot(env.view_tx, env.tidi_factor_bits, bp, st, env.tidi);
    let fx_floor = env.last_visible_stamp.load(Ordering::Acquire).max(env.last_fx_stamp.load(Ordering::Acquire)).min(u64::from(u32::MAX)) as u32;
    let broadcast = belts::build_tick_broadcast_with_fx_floor(
        env.solar_system_id, bp, events, fx_floor, Some(env.last_visible_stamp), Some(env.last_fx_stamp),
    );
    let warp_events = broadcast.events.clone();
    super::send_tick_broadcast_sequenced_owned(env.event_tx, &env.broadcast_seq_gate, broadcast);
    warp_events
}

pub(in crate::system_actor) fn step_full_tick_in_ctx(
    env: &TickEnvRefs<'_>,
    bp: &mut Ballpark,
    st: &mut SystemActorState,
    post_unlock_ops: &mut post_unlock::PostUnlockOps,
    dt: f64,
    waited_sync: bool,
) -> StepFullTickOutput {
    let (tick_snap_opt, virtual_item_deltas, virtual_crystal_dmg_deltas) =
        post_unlock::begin_tick_charge_reservations(env.solar_system_id, st, env.completed_post_unlock_seq);
    let dt_sim = ewar_cooldowns::prepare_tick_cooldowns_with_gate(
        env.solar_system_id, env.cooldown_persist_gate, env.sim_elapsed_secs_bits, st, dt, env.tidi,
    );
    let SystemActorState {
        active_module_cycles, pending_anchoring_disruptors, active_nanite_repairs, ship_rack_heat,
        rah_incoming_damage, base_scramble_strengths, base_core_strengths, asteroids,
        asteroid_templates, sde_opt, db_opt, mechanic_rng_opt, sim_elapsed_secs,
        pending_cycle_actions, tactical_state, pending_notifications_opt, notify_tx_opt,
        ball_space_entry_epochs, fit_projections, weapon_cycle_start_modules, module_cooldowns,
        module_reload_sim_remaining, pending_missile_launches, pending_missile_deploy_fx,
        active_guided_missiles, next_missile_ball_id, warp_state, external_active_modules,
        active_drone_miners, drone_next_ready_unix, next_post_unlock_seq, post_unlock_shadows,
        broadcast_seq_gate, ..
    } = &mut *st;
    let dt = dt_sim;
    belts::respawn_due_asteroids_collecting(bp, asteroids, asteroid_templates, sde_opt, db_opt, post_unlock_ops);
    belts::sync_player_slim_items(bp, db_opt);
    if let Some(db) = db_opt.as_ref() {
        let now_unix = db.clock().now_unix_secs();
        if now_unix > bp.sim_unix_secs {
            bp.sim_unix_secs = now_unix;
        }
    }
    let pre_tick_caps = cycles::snapshot_player_ship_capacitors(bp, db_opt.as_ref());
    let pre_missile_positions = missiles::snapshot_missile_positions(bp, active_guided_missiles);
    let events = bp.step(dt);
    missiles::flush_pending_missile_deploy_fx(bp, pending_missile_deploy_fx);
    super::collect_landed_warps_for_post_unlock_with_in_flight(
        bp, &events, db_opt, ball_space_entry_epochs,
        env.has_in_flight_post_unlock || !post_unlock_ops.is_empty(), post_unlock_ops,
    );
    super::clear_landed_warp_tracking_from_events(bp, &events, warp_state);
    cycles::flush_target_lost_notifications(
        bp, &events, db_opt.as_ref(),
        pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>), notify_tx_opt.as_ref(),
    );
    if env.target_lock_completed_hook.read().unwrap_or_else(|e| e.into_inner()).is_some() {
        for ev in &events {
            if let DestinyEvent::OnTarget { mode, source_id, target_id } = ev {
                if mode == "locked" {
                    let _ = cycles_tackle::emit_target_lock_completed_collecting(
                        bp, db_opt, *source_id, *target_id, true, post_unlock_ops,
                    );
                }
            }
        }
    }
    let next_pu_seq = next_post_unlock_seq.saturating_add(1);
    let sup_dead = crate::services::pve_abyssal_pylons::step_abyssal_hazards_and_pylons_in_ballpark_for_tick_collecting(
        bp, sde_opt.as_deref(), db_opt.as_ref(), env.solar_system_id,
        Some((&mut post_unlock_shadows.abyssal_boundary_ticks, next_pu_seq)),
        Some((&mut post_unlock_shadows.abyssal_active_clouds, next_pu_seq)), post_unlock_ops,
    );
    let pg_dead = crate::services::pvp_filament_arena::step_proving_grounds_arena_hazards_in_ballpark_for_tick_collecting(
        bp, sde_opt.as_deref(), db_opt.as_ref(), env.solar_system_id,
        pending_notifications_opt.as_ref(), notify_tx_opt.as_ref(),
        Some((&mut post_unlock_shadows.arena_hazard_ticks, next_pu_seq)), post_unlock_ops,
    );
    let step_now_unix = db_opt.as_ref().map(|db| db.clock().now_unix_secs()).unwrap_or(0);
    let deact_hook_guard = env.deactivation_sink_hook.clone();
    for ev in &events {
        if let DestinyEvent::OnTarget { mode, source_id, target_id } = ev {
            if mode == "lost" {
                cycles_tackle::on_target_unlocked_in_actor_collecting(
                    env.solar_system_id, bp, active_module_cycles, base_scramble_strengths,
                    base_core_strengths, db_opt, *source_id, Some(*target_id),
                    deact_hook_guard.as_ref(), post_unlock_ops,
                );
            }
        }
    }
    let fallback_rng = crate::rng::MechanicRng::default();
    let rng_ref = mechanic_rng_opt.as_deref().unwrap_or(&fallback_rng);
    crate::services::mobile_deployables::step_mobile_deployables_in_actor_collecting(
        bp, active_module_cycles, rng_ref, sde_opt.as_deref(), db_opt.as_ref(),
        mechanic_rng_opt.as_deref(), env.solar_system_id, dt, step_now_unix,
        Some((&mut post_unlock_shadows.mmju_spool_fired_at, next_post_unlock_seq.saturating_add(1))),
        Some((&mut post_unlock_shadows.observatory_pings_done, next_post_unlock_seq.saturating_add(1))),
        post_unlock_ops,
    );
    post_unlock::step_module_reloads_sim_clock_collecting(
        module_reload_sim_remaining, Some(&mut module_cooldowns.reload_sim_deadlines),
        db_opt, dt, env.tidi, post_unlock_ops,
    );
    let mut destroyed = Vec::new();
    let pending_destroyed = env.pending_destroyed_balls.clone();
    missiles::step_guided_missiles_in_actor(
        bp, active_guided_missiles, &pre_missile_positions, rah_incoming_damage, sde_opt, db_opt,
        pending_cycle_actions, &mut destroyed, Some(&pending_destroyed), dt,
        Some((&mut post_unlock_shadows.depot_reinforced_until, next_pu_seq)), post_unlock_ops,
    );
    destroyed.extend(cycles::step_cycles_with_reservations_collecting(
        env.solar_system_id, bp, active_module_cycles, pending_anchoring_disruptors,
        active_nanite_repairs, ship_rack_heat, rah_incoming_damage, base_scramble_strengths,
        base_core_strengths, sde_opt, db_opt, ball_space_entry_epochs, fit_projections,
        weapon_cycle_start_modules, pending_missile_launches, mechanic_rng_opt,
        tactical_state.as_ref(), pending_cycle_actions, *sim_elapsed_secs, dt, env.tidi,
        Some(module_reload_sim_remaining), deact_hook_guard.as_ref(), Some(&pending_destroyed),
        tick_snap_opt, virtual_item_deltas, virtual_crystal_dmg_deltas,
        Some((post_unlock_shadows, next_pu_seq)), post_unlock_ops,
    ));
    missiles::flush_pending_missile_launches(
        bp, pending_missile_launches, active_guided_missiles, pending_missile_deploy_fx, next_missile_ball_id, sde_opt,
    );
    ewar_cooldowns::record_auto_reload_deadlines_from_actions(pending_cycle_actions, *sim_elapsed_secs, module_cooldowns);
    cycles::record_player_ship_capacitor_deltas(bp, &pre_tick_caps, pending_cycle_actions);
    cycles::flush_pending_cycle_damage_notifications_with_ballpark(
        bp, sde_opt.as_deref(), db_opt.as_ref(),
        pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>),
        notify_tx_opt.as_ref(), pending_cycle_actions,
    );
    for bid in sup_dead.into_iter().chain(pg_dead) {
        destroyed.push((bid, None));
        pending_destroyed.record_one(bid, None);
    }
    for bid in crate::services::space_navigation_advanced::step_zarzakh_gravity_hazard_with_notifications(
        bp, sde_opt.as_deref(), db_opt.as_ref(), env.solar_system_id,
        pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>), notify_tx_opt.as_ref(),
    ) {
        destroyed.push((bid, None));
        pending_destroyed.record_one(bid, None);
    }
    let dead_ids: Vec<BallId> = destroyed.iter().map(|&(bid, _)| bid).collect();
    if !dead_ids.is_empty() {
        external_active_modules.retain(|(sid, _)| !dead_ids.contains(sid));
        for &did in &dead_ids {
            active_drone_miners.remove(&did);
            drone_next_ready_unix.remove(&did);
        }
    }
    cycles::teardown_attacker_locks_and_cycles_for_destroyed_balls_collecting(
        env.solar_system_id, bp, active_module_cycles, base_scramble_strengths,
        base_core_strengths, db_opt,
        pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>),
        notify_tx_opt.as_ref(), deact_hook_guard.as_ref(), &dead_ids, post_unlock_ops,
    );
    for &(target_ball_id, _) in &destroyed {
        crate::services::tactical_module_mechanics::on_probe_ball_removed_in_actor(
            bp, db_opt, tactical_state.as_ref(), target_ball_id,
        );
        if let Some(db) = db_opt.as_ref() {
            crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_in_ballpark_collecting(
                bp, db, env.solar_system_id, target_ball_id, post_unlock_ops,
            );
        }
    }
    let (drained_destroyed, destroyed_lease) = pending_destroyed.checkout_for_step();
    let has_destroyed = !drained_destroyed.is_empty();
    if db_opt.is_some() {
        for (&bid, cs) in &bp.combat_states {
            if bp.balls.contains_key(&bid) && !bp.is_static_ball(bid) && bp.ball_slim_meta.get(&bid).is_none_or(|m| m.category_id != 11) {
                post_unlock_ops.push_ship_combat_hp(post_unlock::CombatHpDbUpdate {
                    ball_id: bid, combat_state: cs.clone(), expected_system: Some(env.solar_system_id), require_persisted_entity: true,
                });
            }
        }
    }
    sync_ball_positions(bp, env.ball_positions, env.scrambled_balls);
    protection::sync_active_module_ships_cache(active_module_cycles, external_active_modules, env.active_module_ships);
    let tick = bp.current_tick;
    let broadcast_seq_gate_arc = Arc::clone(broadcast_seq_gate);
    let _ = ewar_cooldowns::finalize_post_unlock_ops_deactivations_in_actor(
        env.solar_system_id,
        st,
        post_unlock_ops,
        env.raw_deact_hook.clone(),
        env.post_deact_hook.clone(),
        env.cooldown_persist_gate,
        env.cmd_tx_weak.as_ref(),
        env.tidi,
    );
    celestials::update_system_hibernation_flag(st, bp, env.event_tx.receiver_count());
    let ops_was_empty = post_unlock_ops.is_empty();
    protection::sync_all_protection_registry_in_actor(bp, st);
    publish_view_snapshot(env.view_tx, env.tidi_factor_bits, bp, st, env.tidi);

    let fx_floor = env.last_visible_stamp.load(Ordering::Acquire).max(env.last_fx_stamp.load(Ordering::Acquire)).min(u64::from(u32::MAX)) as u32;
    let broadcast = belts::build_tick_broadcast_with_fx_floor(
        env.solar_system_id, bp, events, fx_floor, Some(env.last_visible_stamp), Some(env.last_fx_stamp),
    );
    let warp_events = broadcast.events.clone();
    super::send_tick_broadcast_sequenced_owned(env.event_tx, &broadcast_seq_gate_arc, broadcast);

    StepFullTickOutput { tick, dt_sim: dt, warp_events, drained_destroyed, destroyed_lease, has_destroyed, ops_was_empty, waited_sync }
}

pub(in crate::system_actor) fn handle_sync_handle_command(
    exec_ctx: &mut ActorExecCtx<'_>,
    cmd: SystemCommand,
) -> Option<SystemCommand> {
    match cmd {
        SystemCommand::RefreshStargateActivationStates { lazy_enabled, loaded_systems, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            refresh_stargate_activation_in_ctx(&mut ctx, lazy_enabled, &loaded_systems);
            defer_opt_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        SystemCommand::ActivateStargateTransitionForDest { dest_sys_id, now_ms, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            activate_stargate_transition_for_dest_in_ctx(&mut ctx, dest_sys_id, now_ms);
            defer_opt_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        SystemCommand::ActivateStargateTransition { gate_id, transition_ms, now_ms_opt, reply } => {
            let changed = exec_ctx.ballpark.activate_stargate_transition(gate_id, transition_ms, now_ms_opt);
            defer_reply(exec_ctx.deferred_replies, reply, changed);
            None
        }
        SystemCommand::SetStargateActivationState { gate_id, activation_state, transition_remaining_ms, activation_transition_at_ms, reply } => {
            let changed = exec_ctx.ballpark.set_stargate_activation_state(
                gate_id, activation_state, transition_remaining_ms, activation_transition_at_ms,
            );
            defer_reply(exec_ctx.deferred_replies, reply, changed);
            None
        }
        SystemCommand::SettleStargateActivationStates { now_ms, reply } => {
            let opened = exec_ctx.ballpark.settle_stargate_activation_transitions(0.0, Some(now_ms));
            defer_reply(exec_ctx.deferred_replies, reply, opened);
            None
        }
        SystemCommand::MutateExternalActiveModule { op, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = mutate_external_active_module_in_ctx(&mut ctx, op);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::RepairCombatStateSync { ball_id, hull_heal, armor_heal, shield_heal, wait_for_post_unlock, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let updated = repair_combat_state_sync_in_ctx(&mut ctx, ball_id, hull_heal, armor_heal, shield_heal);
            defer_maybe_barrier_reply(exec_ctx, wait_for_post_unlock, reply, updated);
            None
        }
        SystemCommand::SetBallPositionSync { ball_id, position, wait_for_post_unlock, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            set_ball_position_sync_in_ctx(&mut ctx, ball_id, position);
            defer_maybe_barrier_reply(exec_ctx, wait_for_post_unlock, reply, ());
            None
        }
        SystemCommand::StopBall { ball_id, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            stop_ball_in_ctx(&mut ctx, ball_id);
            defer_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        SystemCommand::RemoveBallSync { ball_id, emit_destruction_fx, wait_for_post_unlock, reply } => {
            let rx_count = exec_ctx.event_tx_actor.receiver_count();
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            remove_ball_sync_in_ctx(&mut ctx, ball_id, emit_destruction_fx, rx_count);
            defer_maybe_barrier_reply(exec_ctx, wait_for_post_unlock, reply, ());
            None
        }
        SystemCommand::CancelShipNaniteRepairsSync { ship_id, refund_paste, wait_for_post_unlock, reply } => {
            SystemHandle::drain_ship_nanite_repairs_collecting(
                &mut exec_ctx.actor_st.active_nanite_repairs, ship_id, refund_paste, exec_ctx.post_unlock_ops,
            );
            defer_maybe_barrier_reply(exec_ctx, wait_for_post_unlock, reply, ());
            None
        }
        SystemCommand::CancelNaniteRepairsForShip { ship_id, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let cancelled = cancel_nanite_repairs_for_ship_in_ctx(&mut ctx, ship_id);
            defer_reply(exec_ctx.deferred_replies, reply, cancelled);
            None
        }
        SystemCommand::TrySpawnAsteroid { asteroid, ball, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            spawn_asteroid_in_ctx(&mut ctx, asteroid, ball);
            defer_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        SystemCommand::SyncAsteroidQuantity { asteroid_id, remaining_units, due_unix_on_belt_empty, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            sync_asteroid_quantity_in_ctx(&mut ctx, asteroid_id, remaining_units, due_unix_on_belt_empty);
            defer_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        SystemCommand::TrySpawnBallWithMeta { ball, combat_state, slim_meta, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            spawn_ball_with_meta_in_ctx(&mut ctx, ball, combat_state, slim_meta);
            defer_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        SystemCommand::MarkBallNonFree { ball_id, is_free, reply } => {
            exec_ctx.ballpark.mark_ball_non_free(ball_id, is_free);
            defer_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        SystemCommand::TrackModuleReloadSimSync { module_id, ship_id: _, remaining_sim_s, reply } => {
            if remaining_sim_s.is_finite() && remaining_sim_s > 0.0 {
                let now_unix = exec_ctx.actor_st.db_opt.as_ref().map(|db| db.clock().now_unix_secs()).unwrap_or(0);
                let tidi = celestials::clamp_tidi_factor_bits(exec_ctx.tidi_factor_bits_actor);
                exec_ctx.actor_st.module_reload_sim_remaining.insert(
                    module_id,
                    super::ModuleReloadSimEntry {
                        remaining_sim_s,
                        min_wall_until_unix: now_unix.saturating_add(remaining_sim_s.ceil().max(1.0) as u64),
                        tidi_stretched: tidi < 0.995,
                    },
                );
            } else {
                exec_ctx.actor_st.module_reload_sim_remaining.remove(&module_id);
            }
            defer_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        SystemCommand::TryHandoffInSpaceObserver { old_ball_id, new_ball_id, added_ball_ids, reply } => {
            let ok = exec_ctx.ballpark.queue_in_space_ship_handoff(old_ball_id, new_ball_id, &added_ball_ids);
            defer_reply(exec_ctx.deferred_replies, reply, ok);
            None
        }
        SystemCommand::TrySpawnBallWithCombat { ball, combat_state, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            spawn_ball_with_combat_in_ctx(&mut ctx, ball, combat_state);
            defer_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        SystemCommand::UpdateCombatState { ball_id, recompute_attrs, mutator, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let applied = update_combat_state_in_ctx(&mut ctx, ball_id, recompute_attrs, mutator);
            defer_reply(exec_ctx.deferred_replies, reply, applied);
            None
        }
        SystemCommand::ApplyDirectCombatDamage { ball_id, damage, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let dead_opt = apply_direct_combat_damage_in_ctx(&mut ctx, ball_id, damage);
            defer_reply(exec_ctx.deferred_replies, reply, dead_opt);
            None
        }
        SystemCommand::SyncShipBallparkStats { ball_id, fresh, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let ok = sync_ship_ballpark_stats_in_ctx(&mut ctx, ball_id, &fresh);
            defer_reply(exec_ctx.deferred_replies, reply, ok);
            None
        }
        SystemCommand::RespawnDepletedBeltsOnDowntime { wait_for_post_unlock, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let count = respawn_depleted_belts_in_ctx(&mut ctx);
            defer_maybe_barrier_reply(exec_ctx, wait_for_post_unlock, reply, count);
            None
        }
        SystemCommand::TryStepManual { dt, wait_for_post_unlock, reply } => {
            let (env, bp, st, ops) = TickEnvRefs::split_exec_ctx(exec_ctx);
            let warp_events = step_manual_in_ctx(&env, bp, st, ops, dt);
            defer_maybe_barrier_reply(exec_ctx, wait_for_post_unlock, reply, warp_events);
            None
        }
        SystemCommand::TakePendingCycleActions { reply } => {
            let actions = std::mem::take(&mut exec_ctx.actor_st.pending_cycle_actions);
            defer_reply(exec_ctx.deferred_replies, reply, actions);
            None
        }
        SystemCommand::MutateWarpState { op, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = mutate_warp_state_in_ctx(&mut ctx, op);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::FinishWarpForDock { ball_id, landing_pos, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = finish_warp_for_dock_in_ctx(&mut ctx, ball_id, landing_pos);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::FlushDirtyConditionCheckpointSync { wait_for_post_unlock, reply } => {
            let entries = super::collect_dirty_condition_checkpoint_entries(exec_ctx.actor_st, exec_ctx.ballpark);
            if !entries.is_empty() && exec_ctx.actor_st.db_opt.is_some() {
                let written = Arc::new(AtomicUsize::new(0));
                let written_reply = Arc::clone(&written);
                exec_ctx.post_unlock_ops.push(post_unlock::PostUnlockIntent::ConditionCheckpoint {
                    solar_system_id: exec_ctx.solar_system_id,
                    entries,
                    written_out: Some(written),
                });
                // When `wait_for_post_unlock == false` (e.g. post_unlock_worker / flush thread caller),
                // SQLite persistence still runs asynchronously on the worker and the reply resolves
                // immediately with `0` rather than blocking on the worker barrier.
                let target = if wait_for_post_unlock { &mut *exec_ctx.deferred_db_replies } else { &mut *exec_ctx.deferred_replies };
                target.push(Box::new(move || { let _ = reply.send(written_reply.load(Ordering::Acquire)); }));
            } else {
                defer_maybe_barrier_reply(exec_ctx, wait_for_post_unlock, reply, 0);
            }
            None
        }
        SystemCommand::StepFullTick { dt, wait_for_post_unlock, reply } => {
            let (env, bp, st, ops) = TickEnvRefs::split_exec_ctx(exec_ctx);
            let out = step_full_tick_in_ctx(&env, bp, st, ops, dt, wait_for_post_unlock);
            exec_ctx.handled_view_and_deact = true;
            defer_maybe_barrier_reply(exec_ctx, wait_for_post_unlock, reply, out);
            None
        }
        SystemCommand::TryRemoveModuleCycle { ship_id, module_id, wait_for_post_unlock, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            try_remove_module_cycle_in_ctx(&mut ctx, ship_id, module_id);
            defer_maybe_barrier_reply(exec_ctx, wait_for_post_unlock, reply, ());
            None
        }
        other => Some(other),
    }
}

pub(in crate::system_actor) enum TryQueuedReply<T> {
    Ready(T),
    QueuedTimedOut,
    FullOrClosed,
}

pub(in crate::system_actor) fn dispatch_actor_sync_try_queued_with_timeout<P, T: Send + 'static>(
    handle: &SystemHandle,
    timeout_dur: Duration,
    payload: P,
    make_cmd: impl FnOnce(P, oneshot::Sender<T>) -> SystemCommand,
    direct_fn: impl FnOnce(&mut BallOpsCtx<'_>, P) -> T,
) -> TryQueuedReply<T> {
    if handle.cmd_tx.is_closed() || handle.is_on_actor_flush_thread() {
        return TryQueuedReply::FullOrClosed;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(rt_handle) => match rt_handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::CurrentThread => TryQueuedReply::Ready(
                ball_ops::run_direct_ball_op_on_current_thread(handle, false, |ctx| direct_fn(ctx, payload)),
            ),
            _ => {
                let Ok(permit) = handle.cmd_sender().try_reserve() else {
                    return TryQueuedReply::FullOrClosed;
                };
                let (reply_tx, reply_rx) = oneshot::channel::<T>();
                permit.send(make_cmd(payload, reply_tx));
                let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<TryQueuedReply<T>>(1);
                rt_handle.spawn(async move {
                    let res = match tokio::time::timeout(timeout_dur, reply_rx).await {
                        Ok(Ok(val)) => TryQueuedReply::Ready(val),
                        Ok(Err(_)) => TryQueuedReply::FullOrClosed,
                        Err(_) => TryQueuedReply::QueuedTimedOut,
                    };
                    let _ = done_tx.send(res);
                });
                match tokio::task::block_in_place(|| done_rx.recv_timeout(timeout_dur.saturating_add(Duration::from_millis(50)))) {
                    Ok(reply) => reply,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        TryQueuedReply::QueuedTimedOut
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        TryQueuedReply::FullOrClosed
                    }
                }
            }
        },
        Err(_) => {
            let Ok(permit) = handle.cmd_sender().try_reserve() else {
                return TryQueuedReply::FullOrClosed;
            };
            let (reply_tx, reply_rx) = oneshot::channel::<T>();
            permit.send(make_cmd(payload, reply_tx));
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_time().build() else {
                return TryQueuedReply::QueuedTimedOut;
            };
            match rt.block_on(async { tokio::time::timeout(timeout_dur, reply_rx).await }) {
                Ok(Ok(val)) => TryQueuedReply::Ready(val),
                Ok(Err(_)) => TryQueuedReply::FullOrClosed,
                Err(_) => TryQueuedReply::QueuedTimedOut,
            }
        }
    }
}

pub(in crate::system_actor) fn dispatch_cmd_tx_sync_reply<T: Send + 'static>(
    cmd_tx: &tokio::sync::mpsc::Sender<SystemCommand>,
    solar_system_id: SolarSystemId,
    label: &str,
    timeout_dur: Duration,
    make_cmd: impl FnOnce(oneshot::Sender<T>) -> SystemCommand,
) -> Result<T, String> {
    if cmd_tx.is_closed() {
        return Err(format!("system {solar_system_id} actor closed"));
    }
    let (reply_tx, reply_rx) = oneshot::channel();
    let cmd = make_cmd(reply_tx);
    match tokio::runtime::Handle::try_current() {
        Ok(rt_handle) => match rt_handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::CurrentThread => {
                let _ = cmd_tx.try_send(cmd);
                Err(format!("queued {label} on current_thread"))
            }
            _ => {
                let sender = cmd_tx.clone();
                let lbl = label.to_owned();
                let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<Result<T, String>>(1);
                rt_handle.spawn(async move {
                    let res =
                        ball_ops::await_actor_cmd_with_deadline(&sender, solar_system_id, &lbl, timeout_dur, cmd, reply_rx)
                            .await;
                    let _ = done_tx.send(res);
                });
                let wait_dur = timeout_dur.saturating_mul(2).saturating_add(Duration::from_secs(1));
                tokio::task::block_in_place(|| done_rx.recv_timeout(wait_dur))
                    .unwrap_or_else(|_| Err(format!("timed out awaiting {label}")))
            }
        },
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .map_err(|e| format!("failed runtime build for {label}: {e}"))?;
            rt.block_on(ball_ops::await_actor_cmd_with_deadline(
                cmd_tx,
                solar_system_id,
                label,
                timeout_dur,
                cmd,
                reply_rx,
            ))
        }
    }
}

impl SystemHandle {
    pub(in crate::system_actor) fn is_on_actor_flush_thread(&self) -> bool {
        self.actor_flush_thread.lock().unwrap_or_else(|e| e.into_inner()).is_some_and(|id| id == std::thread::current().id())
    }

    fn dispatch_fire_or_sync<P, T: Send + 'static>(
        &self,
        label: &str,
        default: T,
        payload: P,
        make_cmd: impl FnOnce(P, oneshot::Sender<T>) -> SystemCommand,
        direct_fn: impl FnOnce(&mut BallOpsCtx<'_>, P) -> T,
    ) -> T {
        if !held_lock_ranks().is_empty() || self.is_on_actor_flush_thread() {
            let (tx, _rx) = oneshot::channel();
            let _ = self.cmd_tx.try_send(make_cmd(payload, tx));
            return default;
        }
        ball_ops::dispatch_actor_sync_reply_with_deadline(self, label, ACTOR_CMD_TIMEOUT, payload, make_cmd, direct_fn)
            .unwrap_or(default)
    }

    fn dispatch_fire_or_sync_bool<P>(
        &self,
        label: &str,
        payload: P,
        make_cmd: impl FnOnce(P, oneshot::Sender<bool>) -> SystemCommand,
        direct_fn: impl FnOnce(&mut BallOpsCtx<'_>, P) -> bool,
    ) -> bool {
        if !held_lock_ranks().is_empty() {
            return false;
        }
        if self.is_on_actor_flush_thread() {
            if self.cmd_tx.is_closed() {
                return false;
            }
            return ball_ops::run_direct_ball_op_on_current_thread(self, false, |ctx| {
                direct_fn(ctx, payload)
            });
        }
        ball_ops::dispatch_actor_sync_reply_with_deadline(self, label, ACTOR_CMD_TIMEOUT, payload, make_cmd, direct_fn)
            .unwrap_or(false)
    }

    pub(in crate::system_actor) fn refresh_stargate_activation_sync(&self, lazy_enabled: bool, loaded_systems: HashSet<SolarSystemId>) {
        self.dispatch_fire_or_sync(
            "RefreshStargateActivationStates",
            (),
            (lazy_enabled, loaded_systems),
            |(lazy_enabled, loaded_systems), reply| SystemCommand::RefreshStargateActivationStates { lazy_enabled, loaded_systems, reply: Some(reply) },
            |ctx, (lazy_enabled, loaded_systems)| refresh_stargate_activation_in_ctx(ctx, lazy_enabled, &loaded_systems),
        );
    }

    pub(in crate::system_actor) fn activate_stargate_transition_for_dest_sync(&self, dest_sys_id: SolarSystemId, now_ms: u64) {
        self.dispatch_fire_or_sync(
            "ActivateStargateTransitionForDest",
            (),
            (dest_sys_id, now_ms),
            |(dest_sys_id, now_ms), reply| SystemCommand::ActivateStargateTransitionForDest { dest_sys_id, now_ms, reply: Some(reply) },
            |ctx, (dest_sys_id, now_ms)| activate_stargate_transition_for_dest_in_ctx(ctx, dest_sys_id, now_ms),
        );
    }

    /// Non-blockingly restores up to the priced `(hull_heal, armor_heal, shield_heal)` HP deltas on `ball_id`'s
    /// `CombatState` through the actor mailbox (returning `NeedsAsyncFallback` if mailbox full or locks held,
    /// `QueuedAsync` if actor reply exceeds bounded wait) (`#1167`, `#1342`).
    pub fn try_repair_combat_state_sync(&self, ball_id: BallId, hull_heal: f64, armor_heal: f64, shield_heal: f64) -> TryRepairCombatOutcome {
        let (hull_delta, armor_delta, shield_delta) = (hull_heal.max(0.0), armor_heal.max(0.0), shield_heal.max(0.0));
        if hull_delta <= 0.0 && armor_delta <= 0.0 && shield_delta <= 0.0 {
            return TryRepairCombatOutcome::Applied;
        }
        if self.cmd_tx.is_closed() || self.get_ball_position_sync(ball_id).is_none() {
            return TryRepairCombatOutcome::NotPresent;
        }
        if !held_lock_ranks().is_empty() || self.is_on_actor_flush_thread() {
            return TryRepairCombatOutcome::NeedsAsyncFallback;
        }
        let wait_for_post_unlock = post_unlock::caller_can_wait_for_handle(self);
        match dispatch_actor_sync_try_queued_with_timeout(
            self,
            Duration::from_millis(350),
            (ball_id, hull_delta, armor_delta, shield_delta),
            |(ball_id, hull_heal, armor_heal, shield_heal), reply| SystemCommand::RepairCombatStateSync {
                ball_id, hull_heal, armor_heal, shield_heal, wait_for_post_unlock, reply,
            },
            |ctx, (ball_id, hull_heal, armor_heal, shield_heal)| repair_combat_state_sync_in_ctx(ctx, ball_id, hull_heal, armor_heal, shield_heal),
        ) {
            TryQueuedReply::Ready(Some(_)) => TryRepairCombatOutcome::Applied,
            TryQueuedReply::Ready(None) => TryRepairCombatOutcome::NotPresent,
            TryQueuedReply::QueuedTimedOut => TryRepairCombatOutcome::QueuedAsync,
            TryQueuedReply::FullOrClosed => TryRepairCombatOutcome::NeedsAsyncFallback,
        }
    }

    pub fn try_set_ball_position(&self, ball_id: BallId, position: DVec3) {
        let can_wait = held_lock_ranks().is_empty() && !self.is_on_actor_flush_thread() && post_unlock::caller_can_wait_for_handle(self);
        self.dispatch_fire_or_sync(
            "SetBallPositionSync",
            (),
            (ball_id, position),
            move |(ball_id, position), reply| SystemCommand::SetBallPositionSync { ball_id, position, wait_for_post_unlock: can_wait, reply },
            |ctx, (ball_id, position)| set_ball_position_sync_in_ctx(ctx, ball_id, position),
        );
    }

    pub fn try_stop_ball(&self, ball_id: BallId) {
        self.dispatch_fire_or_sync("StopBall", (), ball_id, |ball_id, reply| SystemCommand::StopBall { ball_id, reply }, stop_ball_in_ctx);
    }

    /// Transitions a `CLOSED` stargate into `ACTIVATING` (`#1080`).
    pub fn activate_stargate_transition(&self, gate_id: BallId, transition_ms: u64, now_ms_opt: Option<u64>) -> bool {
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "ActivateStargateTransition", ACTOR_CMD_TIMEOUT, (gate_id, transition_ms, now_ms_opt),
            |(gate_id, transition_ms, now_ms_opt), reply| SystemCommand::ActivateStargateTransition { gate_id, transition_ms, now_ms_opt, reply },
            |ctx, (gate_id, transition_ms, now_ms_opt)| ctx.ballpark.activate_stargate_transition(gate_id, transition_ms, now_ms_opt),
        ).unwrap_or(false)
    }

    /// Explicitly sets stargate activation state (`#1080`).
    pub fn set_stargate_activation_state(&self, gate_id: BallId, activation_state: i64, transition_remaining_ms: u64, activation_transition_at_ms: u64) -> bool {
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "SetStargateActivationState", ACTOR_CMD_TIMEOUT,
            (gate_id, activation_state, transition_remaining_ms, activation_transition_at_ms),
            |(gate_id, activation_state, transition_remaining_ms, activation_transition_at_ms), reply| SystemCommand::SetStargateActivationState {
                gate_id, activation_state, transition_remaining_ms, activation_transition_at_ms, reply,
            },
            |ctx, (gate_id, activation_state, transition_remaining_ms, activation_transition_at_ms)| {
                ctx.ballpark.set_stargate_activation_state(gate_id, activation_state, transition_remaining_ms, activation_transition_at_ms)
            },
        ).unwrap_or(false)
    }

    /// Settles any elapsed `ACTIVATING` stargates against `now_ms`, returning opened gate IDs (`#1080`).
    pub fn settle_stargate_activation_states(&self, now_ms: u64) -> Vec<BallId> {
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "SettleStargateActivationStates", ACTOR_CMD_TIMEOUT, now_ms,
            |now_ms, reply| SystemCommand::SettleStargateActivationStates { now_ms, reply },
            |ctx, now_ms| ctx.ballpark.settle_stargate_activation_transitions(0.0, Some(now_ms)),
        ).unwrap_or_default()
    }

    pub fn try_remove_ball(&self, ball_id: BallId) {
        self.try_remove_ball_impl(ball_id, false);
    }

    /// Removes `ball_id` and emits a preceding `TerminalExplosion` (`#1082`) if it is still in space.
    pub fn try_remove_destroyed_ball(&self, ball_id: BallId) {
        self.try_remove_ball_impl(ball_id, true);
    }

    fn try_remove_ball_impl(&self, ball_id: BallId, emit_destruction_fx: bool) {
        let can_wait = held_lock_ranks().is_empty() && !self.is_on_actor_flush_thread() && post_unlock::caller_can_wait_for_handle(self);
        let rx_count = self.event_tx.receiver_count();
        self.dispatch_fire_or_sync(
            "RemoveBallSync",
            (),
            (ball_id, emit_destruction_fx),
            move |(ball_id, emit_destruction_fx), reply| SystemCommand::RemoveBallSync {
                ball_id, emit_destruction_fx, wait_for_post_unlock: can_wait, reply,
            },
            move |ctx, (ball_id, emit_destruction_fx)| remove_ball_sync_in_ctx(ctx, ball_id, emit_destruction_fx, rx_count),
        );
    }

    pub fn cancel_ship_nanite_repairs_sync(&self, ship_id: BallId, refund_paste: bool) {
        if !held_lock_ranks().is_empty() || self.is_on_actor_flush_thread() {
            if let Err(err) = self.cmd_tx.try_send(SystemCommand::CancelShipNaniteRepairs { ship_id, refund_paste }) {
                tracing::warn!(solar_system_id = self.solar_system_id, ship_id, error = %err, "Failed to enqueue CancelShipNaniteRepairs under held lock");
            }
            return;
        }
        let wait_for_post_unlock = post_unlock::caller_can_wait_for_handle(self);
        let _ = ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "CancelShipNaniteRepairsSync", ACTOR_CMD_TIMEOUT, (ship_id, refund_paste),
            |(ship_id, refund_paste), reply| SystemCommand::CancelShipNaniteRepairsSync { ship_id, refund_paste, wait_for_post_unlock, reply },
            |ctx, (ship_id, refund_paste)| Self::drain_ship_nanite_repairs_collecting(&mut ctx.actor_st.active_nanite_repairs, ship_id, refund_paste, ctx.post_unlock_ops),
        );
    }

    pub fn try_spawn_asteroid(&self, asteroid: AsteroidState, ball: Ball) {
        self.dispatch_fire_or_sync(
            "TrySpawnAsteroid", (), (asteroid, ball),
            |(asteroid, ball), reply| SystemCommand::TrySpawnAsteroid { asteroid, ball, reply },
            |ctx, (asteroid, ball)| spawn_asteroid_in_ctx(ctx, asteroid, ball),
        );
    }

    pub fn sync_asteroid_quantity(&self, asteroid_id: BallId, remaining_units: u64, due_unix_on_belt_empty: u64) {
        self.dispatch_fire_or_sync(
            "SyncAsteroidQuantity", (), (asteroid_id, remaining_units, due_unix_on_belt_empty),
            |(asteroid_id, remaining_units, due_unix_on_belt_empty), reply| SystemCommand::SyncAsteroidQuantity {
                asteroid_id, remaining_units, due_unix_on_belt_empty, reply,
            },
            |ctx, (asteroid_id, remaining_units, due_unix_on_belt_empty)| sync_asteroid_quantity_in_ctx(ctx, asteroid_id, remaining_units, due_unix_on_belt_empty),
        );
    }

    pub fn try_spawn_ball_with_meta(&self, ball: Ball, combat_state: CombatState, slim_meta: SlimItemMeta) {
        self.dispatch_fire_or_sync(
            "TrySpawnBallWithMeta", (), (ball, combat_state, slim_meta),
            |(ball, combat_state, slim_meta), reply| SystemCommand::TrySpawnBallWithMeta { ball, combat_state, slim_meta, reply },
            |ctx, (ball, combat_state, slim_meta)| spawn_ball_with_meta_in_ctx(ctx, ball, combat_state, slim_meta),
        );
    }

    pub(crate) fn try_mark_ball_non_free(&self, ball_id: BallId, is_free: bool) {
        self.dispatch_fire_or_sync(
            "MarkBallNonFree", (), (ball_id, is_free),
            |(ball_id, is_free), reply| SystemCommand::MarkBallNonFree { ball_id, is_free, reply },
            |ctx, (ball_id, is_free)| ctx.ballpark.mark_ball_non_free(ball_id, is_free),
        );
    }

    pub fn try_push_set_state(&self, observer_ball_id: BallId) -> bool {
        if !held_lock_ranks().is_empty() {
            return false;
        }
        if self.is_on_actor_flush_thread() {
            if self.cmd_tx.is_closed() {
                return false;
            }
            return ball_ops::run_direct_ball_op_on_current_thread(self, false, |ctx| {
                ctx.ballpark.queue_set_state(observer_ball_id)
            });
        }
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self,
            "PushSetState",
            ACTOR_CMD_TIMEOUT,
            observer_ball_id,
            |observer_ball_id, reply| SystemCommand::PushSetState { observer_ball_id, reply },
            |ctx, observer_ball_id| {
                if ctx.ballpark.queue_set_state(observer_ball_id) {
                    Ok(ctx.ballpark.current_tick)
                } else {
                    Err("not present".into())
                }
            },
        )
        .is_ok_and(|res| res.is_ok())
    }

    /// Registers or clears a sim-clock-governed module reload deadline on the system actor (`#1081`).
    pub fn try_track_module_reload_sim(&self, module_id: i64, ship_id: BallId, remaining_sim_s: f64) {
        if !held_lock_ranks().is_empty() || self.is_on_actor_flush_thread() {
            let _ = self.cmd_tx.try_send(SystemCommand::TrackModuleReloadSim { module_id, ship_id, remaining_sim_s });
            return;
        }
        let tidi = self.tidi_factor();
        let _ = ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "TrackModuleReloadSimSync", ACTOR_CMD_TIMEOUT, (module_id, ship_id, remaining_sim_s),
            |(module_id, ship_id, remaining_sim_s), reply| SystemCommand::TrackModuleReloadSimSync { module_id, ship_id, remaining_sim_s, reply },
            |ctx, (module_id, _ship_id, remaining_sim_s)| {
                if remaining_sim_s.is_finite() && remaining_sim_s > 0.0 {
                    let now_unix = ctx.actor_st.db_opt.as_ref().map(|db| db.clock().now_unix_secs()).unwrap_or(0);
                    ctx.actor_st.module_reload_sim_remaining.insert(module_id, super::ModuleReloadSimEntry {
                        remaining_sim_s, min_wall_until_unix: now_unix.saturating_add(remaining_sim_s.ceil().max(1.0) as u64), tidi_stretched: tidi < 0.995,
                    });
                } else {
                    ctx.actor_st.module_reload_sim_remaining.remove(&module_id);
                }
            },
        );
    }

    pub fn try_handoff_in_space_observer(&self, old_ball_id: BallId, new_ball_id: BallId, added_ball_ids: &[BallId]) -> bool {
        self.dispatch_fire_or_sync_bool(
            "TryHandoffInSpaceObserver", (old_ball_id, new_ball_id, added_ball_ids.to_vec()),
            |(old_ball_id, new_ball_id, added_ball_ids), reply| SystemCommand::TryHandoffInSpaceObserver { old_ball_id, new_ball_id, added_ball_ids, reply },
            |ctx, (old_ball_id, new_ball_id, added_ball_ids)| ctx.ballpark.queue_in_space_ship_handoff(old_ball_id, new_ball_id, &added_ball_ids),
        )
    }

    pub fn try_spawn_ball_with_combat(&self, ball: Ball, combat_state: CombatState) {
        self.dispatch_fire_or_sync(
            "TrySpawnBallWithCombat", (), (ball, combat_state),
            |(ball, combat_state), reply| SystemCommand::TrySpawnBallWithCombat { ball, combat_state, reply },
            |ctx, (ball, combat_state)| spawn_ball_with_combat_in_ctx(ctx, ball, combat_state),
        );
    }

    fn dispatch_combat_state_mutator(
        &self,
        ball_id: BallId,
        recompute_attrs: bool,
        mutator: Box<dyn FnOnce(&mut CombatState) + Send>,
    ) -> bool {
        self.dispatch_fire_or_sync_bool(
            "UpdateCombatState",
            (ball_id, recompute_attrs, mutator),
            |(ball_id, recompute_attrs, mutator), reply| SystemCommand::UpdateCombatState {
                ball_id,
                recompute_attrs,
                mutator,
                reply,
            },
            |ctx, (ball_id, recompute_attrs, mutator)| {
                update_combat_state_in_ctx(ctx, ball_id, recompute_attrs, mutator)
            },
        )
    }

    pub fn try_update_combat_state<F: FnOnce(&mut CombatState) + Send + 'static>(&self, ball_id: BallId, f: F) -> bool {
        self.dispatch_combat_state_mutator(ball_id, false, Box::new(f))
    }

    /// Synchronously applies shield/armor/structure `damage` onto `ball_id` and returns
    /// `Some(destroyed)` once actor commit + [`SystemView`] publish complete, or `None` if
    /// absent, actor closed, or called under held locks.
    pub fn try_apply_direct_combat_damage(&self, ball_id: BallId, damage: f64) -> Option<bool> {
        if !held_lock_ranks().is_empty() {
            return None;
        }
        if self.is_on_actor_flush_thread() {
            if self.cmd_tx.is_closed() {
                return None;
            }
            return ball_ops::run_direct_ball_op_on_current_thread(self, false, |ctx| {
                apply_direct_combat_damage_in_ctx(ctx, ball_id, damage)
            });
        }
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self,
            "ApplyDirectCombatDamage",
            ACTOR_CMD_TIMEOUT,
            (ball_id, damage),
            |(ball_id, damage), reply| SystemCommand::ApplyDirectCombatDamage { ball_id, damage, reply },
            |ctx, (ball_id, damage)| apply_direct_combat_damage_in_ctx(ctx, ball_id, damage),
        )
        .ok()
        .flatten()
    }

    pub fn try_add_mobile_warp_disruptor(&self, id: BallId, position: DVec3, radius: f64) {
        self.dispatch_fire_or_sync(
            "AddMobileWarpDisruptor", false, (id, position, radius),
            |(id, position, radius), reply| SystemCommand::AddMobileWarpDisruptor { id, position, radius, reply },
            |ctx, (id, position, radius)| { let ok = ctx.ballpark.add_mobile_warp_disruptor(id, position, radius); ctx.sync_positions(); ok },
        );
    }

    /// Atomically mutates declarative modifiers on `ball_id`'s [`CombatState`] and recomputes
    /// its derived `Ball` and `CombatState` attributes from immutable baselines (`#584`).
    pub fn try_recompute_combat_attributes(&self, ball_id: BallId, f: impl FnOnce(&mut CombatState) + Send + 'static) -> bool {
        self.dispatch_combat_state_mutator(ball_id, true, Box::new(f))
    }

    /// Pushes recomputed ship kinematics & `CombatState` baselines from `ServerContext`
    /// after skill completions, booster expirations, or expert system expirations (`#736`).
    pub fn try_sync_ship_ballpark_stats(&self, ball_id: BallId, fresh: &CombatState) -> bool {
        self.dispatch_fire_or_sync_bool(
            "SyncShipBallparkStats", (ball_id, Box::new(fresh.clone())),
            |(ball_id, fresh), reply| SystemCommand::SyncShipBallparkStats { ball_id, fresh, reply },
            |ctx, (ball_id, fresh)| sync_ship_ballpark_stats_in_ctx(ctx, ball_id, &fresh),
        )
    }

    pub fn try_remove_warp_bubble(&self, id: BallId) {
        self.dispatch_fire_or_sync(
            "RemoveWarpBubble", false, id,
            |id, reply| SystemCommand::RemoveWarpBubble { id, reply },
            |ctx, id| { let removed = ctx.ballpark.remove_warp_bubble(id).is_some(); ctx.sync_positions(); removed },
        );
    }

    /// Synchronously respawns fully depleted SDE ore belts and depleted ice blocks during 11:00 UTC downtime.
    pub fn respawn_depleted_belts_on_downtime(&self) -> usize {
        let wait_for_post_unlock = post_unlock::caller_can_wait_for_handle(self);
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "RespawnDepletedBeltsOnDowntime", ACTOR_CMD_TIMEOUT, (),
            move |(), reply| SystemCommand::RespawnDepletedBeltsOnDowntime { wait_for_post_unlock, reply },
            |ctx, ()| respawn_depleted_belts_in_ctx(ctx),
        ).unwrap_or(0)
    }

    pub fn try_step_manual(&self, dt: f64) {
        let wait_for_post_unlock = post_unlock::caller_can_wait_for_handle(self);
        if let Ok(warp_events) = ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "TryStepManual", ACTOR_CMD_TIMEOUT, dt,
            move |dt, reply| SystemCommand::TryStepManual { dt, wait_for_post_unlock, reply },
            move |ctx, dt| {
                let gate = Arc::clone(&ctx.actor_st.broadcast_seq_gate);
                let env = TickEnvRefs::from_handle(self, gate);
                step_manual_in_ctx(&env, ctx.ballpark, ctx.actor_st, ctx.post_unlock_ops, dt)
            },
        ) {
            self.record_warp_stops_from_events(&warp_events);
        }
    }

    pub fn take_pending_cycle_actions(&self) -> Vec<PendingCycleAction> {
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "TakePendingCycleActions", ACTOR_CMD_TIMEOUT, (),
            |(), reply| SystemCommand::TakePendingCycleActions { reply },
            |ctx, ()| std::mem::take(&mut ctx.actor_st.pending_cycle_actions),
        ).unwrap_or_default()
    }

    fn mutate_warp_state_op(&self, op: WarpStateMutOp) {
        let _ = self.dispatch_fire_or_sync(
            "MutateWarpState", false, op,
            |op, reply| SystemCommand::MutateWarpState { op, reply },
            mutate_warp_state_in_ctx,
        );
    }

    pub fn set_warp_provenance(&self, ball_id: BallId, provenance: super::WarpProvenance) {
        self.mutate_warp_state_op(WarpStateMutOp::SetProvenance { ball_id, provenance });
    }
    pub fn clear_warp_provenance(&self, ball_id: BallId) {
        self.mutate_warp_state_op(WarpStateMutOp::ClearProvenance { ball_id });
    }
    pub fn track_player_or_fleet_warp(&self, ball_id: BallId, char_id: CharacterId) {
        self.mutate_warp_state_op(WarpStateMutOp::TrackPlayerOrFleet { ball_id, char_id });
    }
    pub fn track_return_warp(&self, ball_id: BallId) {
        self.mutate_warp_state_op(WarpStateMutOp::TrackReturn { ball_id });
    }
    pub fn clear_warp_tracking(&self, ball_id: BallId) {
        self.mutate_warp_state_op(WarpStateMutOp::ClearBall { ball_id });
    }
    pub fn clear_return_and_pending_warp(&self, ball_id: BallId) {
        self.mutate_warp_state_op(WarpStateMutOp::ClearReturnAndPending { ball_id });
    }
    pub fn consume_warp_finished(&self, ball_id: BallId) -> bool {
        if !held_lock_ranks().is_empty() || self.is_on_actor_flush_thread() {
            return false;
        }
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self,
            "MutateWarpState",
            ACTOR_CMD_TIMEOUT,
            WarpStateMutOp::ConsumeFinished { ball_id },
            |op, reply| SystemCommand::MutateWarpState { op, reply },
            mutate_warp_state_in_ctx,
        )
        .unwrap_or(false)
    }
    #[cfg(any(test, feature = "testkit"))]
    pub fn insert_active_warp_for_test(&self, ball_id: BallId, char_id: CharacterId) {
        self.mutate_warp_state_op(WarpStateMutOp::InsertActiveForTest { ball_id, char_id });
    }
    #[cfg(any(test, feature = "testkit"))]
    pub fn insert_return_warp_for_test(&self, ball_id: BallId) {
        self.mutate_warp_state_op(WarpStateMutOp::InsertReturnForTest { ball_id });
    }
    #[cfg(any(test, feature = "testkit"))]
    pub fn clear_all_warp_tracking_for_test(&self) {
        self.mutate_warp_state_op(WarpStateMutOp::ClearAllForTest);
    }
    #[cfg(any(test, feature = "testkit"))]
    pub fn snapshot_warp_state_for_test(&self) -> super::types::ActorWarpState {
        let v = self.view();
        super::types::ActorWarpState {
            provenance: v.warp_provenance.clone(),
            active_warps: v.active_warps.clone(),
            return_warps: v.return_warps.clone(),
            pending_warp_finished: v.pending_warp_finished.clone(),
        }
    }
    #[cfg(any(test, feature = "testkit"))]
    pub fn restore_warp_state_for_test(&self, state: super::types::ActorWarpState) {
        let _ = self.mutate_warp_state_op(WarpStateMutOp::RestoreForTest(Box::new(state)));
    }

    /// Synchronously cancels active nanite repairs for `ship_id` and returns removed records.
    ///
    /// Requires no ranked locks held by the caller (panics under held locks).
    pub fn cancel_nanite_repairs_for_ship_sync(&self, ship_id: BallId) -> Vec<ActiveNaniteRepair> {
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "CancelNaniteRepairsForShip", ACTOR_CMD_TIMEOUT, ship_id,
            |ship_id, reply| SystemCommand::CancelNaniteRepairsForShip { ship_id, reply },
            cancel_nanite_repairs_for_ship_in_ctx,
        ).unwrap_or_default()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_disconnected_warp_sync(
        &self, ship_id: BallId, _type_id: u32, _radius: f64, _mass: f64,
        _inertia: f64, _max_velocity: f64, warp_speed_au_s: f64, current_pos: DVec3, dest: DVec3,
    ) {
        let _ = ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "PrepareDisconnectedWarp", ACTOR_CMD_TIMEOUT, (),
            |(), reply| SystemCommand::PrepareDisconnectedWarp {
                ship_id, warp_speed_au_s, current_pos, dest, reply,
            },
            |ctx, ()| {
                ball_ops::prepare_disconnected_warp_in_actor(ctx, ship_id, warp_speed_au_s, current_pos, dest)
            },
        );
    }

    pub fn reconcile_disconnected_warp_sync(&self, ship_id: BallId, dest: DVec3) -> (DVec3, bool) {
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "ReconcileDisconnectedWarp", ACTOR_CMD_TIMEOUT, (),
            |(), reply| SystemCommand::ReconcileDisconnectedWarp { ship_id, dest, reply },
            |ctx, ()| ball_ops::reconcile_disconnected_warp_in_actor(ctx, ship_id, dest),
        )
        .unwrap_or((dest, true))
    }

    /// Removes `ball_id` synchronously with bounded [`ACTOR_CMD_TIMEOUT`] (`#1342`, `ECO-72`).
    ///
    /// Requires no ranked locks held by the caller (panics under held locks).
    pub fn remove_ball_sync(&self, ball_id: BallId) -> Result<Option<Ball>, String> {
        if self.cmd_sender().is_closed() {
            return Err(format!("system {} actor closed", self.solar_system_id));
        }
        ball_ops::dispatch_actor_sync_reply_with_deadline(
            self, "RemoveBall", ACTOR_CMD_TIMEOUT, ball_id,
            |ball_id, reply| SystemCommand::RemoveBall { ball_id, reply },
            |ctx, ball_id| ball_ops::remove_ball_in_actor(ctx, ball_id, false),
        )
    }
}
