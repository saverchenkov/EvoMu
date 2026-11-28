//! Per-system in-space EWAR projection/active-start state, deactivation batching,
//! command handling, and `SystemHandle` bridges owned by [`SystemActor`] (`#1541`).

#![forbid(unsafe_code)]

pub use super::module_cooldowns::*;
use super::{
    call_actor_result_with_deadline, defer_reply, dispatch_actor_sync_with_deadline,
    ActiveModuleCycle, ActorExecCtx, BallOpsCtx, CharacterId, ModuleDeactivationHook,
    ModuleDeactivationPostUnlockHook, PairedModuleDeactivationPostUnlockHook, PostUnlockHookAction,
    SystemActorState, SystemCommand, SystemHandle, SystemRegistry, SystemView, ACTOR_CMD_TIMEOUT,
};
use eve_db::lock_rank::{held_lock_ranks, RankedMutex, RankedRwLock, RANK_LEAF, RANK_WORLD_INNER};
use eve_sde::SolarSystemId;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProjectedRemoteAssistance {
    pub source_ship_id: i64,
    pub module_id: i64,
    pub module_type_id: u32,
    pub max_target_range_bonus_pct: f64,
    pub scan_resolution_bonus_pct: f64,
    pub sensor_strength_bonus_pct: f64,
    pub optimal_range_bonus_pct: f64,
    pub falloff_bonus_pct: f64,
    pub tracking_speed_bonus_pct: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActorEwarState {
    pub projected_ewar: HashMap<i64, Vec<eve_dogma::ProjectedEwarEffect>>,
    pub projected_ewar_sources: HashMap<i64, i64>,
    pub active_ewar_starts: HashMap<(i64, i64), Vec<(i64, &'static str)>>,
    pub remote_assistance: HashMap<i64, Vec<ProjectedRemoteAssistance>>,
    pub in_flight_superweapons: HashMap<i64, u64>,
    pub next_in_flight_superweapon_token: u64,
}

impl ActorEwarState {
    pub fn is_empty(&self) -> bool {
        self.projected_ewar.is_empty()
            && self.projected_ewar_sources.is_empty()
            && self.active_ewar_starts.is_empty()
            && self.remote_assistance.is_empty()
            && self.in_flight_superweapons.is_empty()
    }
}

/// Per-system persistence gate rank (`36`), strictly below `RANK_WORLD_INNER` (`40`) and

#[derive(Debug, Clone, PartialEq)]
pub struct FlushedProjectedEwarTarget {
    pub target_ship_id: i64,
    pub source_module_type_id: u32,
    pub charge_type_id: Option<u32>,
    pub effective_module: eve_dogma::DogmaItemState,
}

/// Atomic actor-side EWAR removal snapshot used by `flush_module_ewar_end` (`#1541`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModuleEwarFlushSnapshot {
    pub started_targets: Vec<(i64, &'static str)>,
    pub projected_targets: Vec<FlushedProjectedEwarTarget>,
    pub affected_ships: Vec<i64>,
    pub remote_assistance_affected_ships: Vec<i64>,
}


// ---------------------------------------------------------------------------
// Actor-side mutation helpers
// ---------------------------------------------------------------------------

pub(crate) fn upsert_projected_ewar_in_state(
    ewar: &mut ActorEwarState,
    source_ship_id: i64,
    target_ship_id: i64,
    effect: eve_dogma::ProjectedEwarEffect,
) {
    ewar.projected_ewar_sources
        .insert(effect.source_module_id, source_ship_id);
    let list = ewar.projected_ewar.entry(target_ship_id).or_default();
    if let Some(existing) = list
        .iter_mut()
        .find(|e| e.source_module_id == effect.source_module_id)
    {
        *existing = effect;
    } else {
        list.push(effect);
    }
}

pub(crate) fn remove_projected_ewar_by_module_in_state(
    ewar: &mut ActorEwarState,
    source_ship_id: i64,
    source_module_id: i64,
    allow_db_source_fallback: bool,
) -> Vec<i64> {
    let recorded = ewar.projected_ewar_sources.get(&source_module_id).copied();
    match recorded {
        Some(recorded_ship) if recorded_ship != source_ship_id => return Vec::new(),
        Some(_) => {
            ewar.projected_ewar_sources.remove(&source_module_id);
        }
        None if !allow_db_source_fallback => return Vec::new(),
        None => {}
    }
    let mut affected_ships = Vec::new();
    ewar.projected_ewar.retain(|&ship_id, list| {
        let before = list.len();
        list.retain(|e| e.source_module_id != source_module_id);
        if list.len() < before {
            affected_ships.push(ship_id);
        }
        !list.is_empty()
    });
    affected_ships
}

/// Atomically removes EWAR starts & projected entries for `(source_ship_id, source_module_id)`
/// directly inside `&mut ActorEwarState`, returning the snapshot for out-of-lock notifications (`#1541`).
pub(in crate::system_actor) fn flush_module_ewar_in_state(
    ewar: &mut ActorEwarState,
    source_ship_id: i64,
    source_module_id: i64,
    allow_db_source_fallback: bool,
) -> ModuleEwarFlushSnapshot {
    let started_targets = ewar
        .active_ewar_starts
        .remove(&(source_ship_id, source_module_id))
        .unwrap_or_default();
    let recorded_source = ewar.projected_ewar_sources.get(&source_module_id).copied();
    let source_matches = recorded_source
        .map(|sid| sid == source_ship_id)
        .unwrap_or(allow_db_source_fallback);
    let projected_targets = if started_targets.is_empty() && source_matches {
        let mut out = Vec::new();
        for (&target_ship_id, list) in &ewar.projected_ewar {
            for eff in list {
                if eff.source_module_id == source_module_id {
                    out.push(FlushedProjectedEwarTarget {
                        target_ship_id,
                        source_module_type_id: eff.source_module_type_id,
                        charge_type_id: eff.charge_type_id,
                        effective_module: eff.effective_module.clone(),
                    });
                }
            }
        }
        out
    } else {
        Vec::new()
    };
    let affected_ships = remove_projected_ewar_by_module_in_state(
        ewar,
        source_ship_id,
        source_module_id,
        allow_db_source_fallback,
    );
    let remote_assistance_affected_ships = remove_remote_assistance_by_module_in_state(
        ewar,
        source_ship_id,
        source_module_id,
    );
    ModuleEwarFlushSnapshot {
        started_targets,
        projected_targets,
        affected_ships,
        remote_assistance_affected_ships,
    }
}

fn flush_module_ewar_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    source_ship_id: i64,
    source_module_id: i64,
    allow_db_source_fallback: bool,
) -> Result<ModuleEwarFlushSnapshot, String> {
    Ok(flush_module_ewar_in_state(
        &mut ctx.actor_st.ewar_state,
        source_ship_id,
        source_module_id,
        allow_db_source_fallback,
    ))
}

pub(crate) fn upsert_remote_assistance_in_state(
    ewar: &mut ActorEwarState,
    target_ship_id: i64,
    entry: ProjectedRemoteAssistance,
) {
    let list = ewar.remote_assistance.entry(target_ship_id).or_default();
    list.retain(|e| e.module_id != entry.module_id);
    list.push(entry);
}

pub(crate) fn remove_remote_assistance_by_module_in_state(
    ewar: &mut ActorEwarState,
    source_ship_id: i64,
    source_module_id: i64,
) -> Vec<i64> {
    let mut affected_ships = Vec::new();
    ewar.remote_assistance.retain(|&target_ship_id, list| {
        let before = list.len();
        list.retain(|e| !(e.source_ship_id == source_ship_id && e.module_id == source_module_id));
        if list.len() < before {
            affected_ships.push(target_ship_id);
        }
        !list.is_empty()
    });
    affected_ships
}

pub(crate) fn set_remote_assistance_for_target_in_state(
    ewar: &mut ActorEwarState,
    target_ship_id: i64,
    entries: Vec<ProjectedRemoteAssistance>,
) {
    if entries.is_empty() {
        ewar.remote_assistance.remove(&target_ship_id);
    } else {
        ewar.remote_assistance.insert(target_ship_id, entries);
    }
}



fn read_opt_leaf_lock<T: Clone>(lock: &Arc<RankedRwLock<Option<T>, RANK_LEAF>>) -> Option<T> {
    if held_lock_ranks().iter().any(|&r| r >= RANK_LEAF) {
        lock.read_unranked().unwrap_or_else(|e| e.into_inner()).clone()
    } else {
        lock.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

pub(in crate::system_actor) fn read_deactivation_hooks(
    raw_lock: &Arc<RankedRwLock<Option<ModuleDeactivationHook>, RANK_LEAF>>,
    post_lock: &Arc<RankedRwLock<Option<PairedModuleDeactivationPostUnlockHook>, RANK_LEAF>>,
) -> (
    Option<ModuleDeactivationHook>,
    Option<PairedModuleDeactivationPostUnlockHook>,
) {
    (read_opt_leaf_lock(raw_lock), read_opt_leaf_lock(post_lock))
}

pub(in crate::system_actor) fn active_sink_hook(
    raw_hook: Option<&ModuleDeactivationHook>,
    post_hook: Option<&PairedModuleDeactivationPostUnlockHook>,
) -> Option<ModuleDeactivationHook> {
    raw_hook
        .cloned()
        .or_else(|| post_hook.map(|(paired_raw, _)| Arc::clone(paired_raw)))
}

/// Updates TiDi history, advances per-system `sim_elapsed_secs`, and prunes expired
/// actor cooldowns at tick start, returning the effective simulated `dt` (`cycles.rs` helper).
pub(in crate::system_actor) fn prepare_tick_cooldowns_with_gate(
    solar_system_id: SolarSystemId,
    cooldown_persist_gate: &CooldownPersistGate,
    sim_elapsed_secs_bits: &std::sync::atomic::AtomicU64,
    st: &mut SystemActorState,
    dt: f64,
    tidi: f64,
) -> f64 {
    if tidi < 0.999_999 {
        cooldown_persist_gate.mark_tidi_history();
        st.has_tidi_history = true;
    } else if cooldown_persist_gate.has_tidi_history() {
        st.has_tidi_history = true;
    }
    let dt_sim = if dt > 0.0 && dt.is_finite() { dt * tidi } else { 0.0 };
    if dt_sim > 0.0 {
        st.sim_elapsed_secs += dt_sim;
        sim_elapsed_secs_bits.store(st.sim_elapsed_secs.to_bits(), std::sync::atomic::Ordering::Release);
    }
    let now_unix_f64 = st.db_opt.as_ref().map(|db| db.clock().now_unix_secs() as f64).unwrap_or(0.0);
    if !st.module_cooldowns.reactivation_cooldowns.is_empty()
        && prune_expired_module_cooldowns_in_state(
            &mut st.module_cooldowns, solar_system_id, now_unix_f64,
            st.sim_elapsed_secs, tidi, st.has_tidi_history || tidi < 0.999_999,
        )
    {
        st.module_cooldown_checkpoint_dirty = true;
        st.module_cooldown_checkpoint_seq = st.module_cooldown_checkpoint_seq.saturating_add(1);
    }
    dt_sim
}

pub(in crate::system_actor) fn record_auto_reload_deadlines_from_actions(
    pending_cycle_actions: &[super::PendingCycleAction], sim_elapsed_secs: f64,
    module_cooldowns: &mut ActorModuleCooldownState,
) {
    for action in pending_cycle_actions {
        if let super::PendingCycleAction::AutoReloadStarted { module_ids, sim_deadline_s, .. } = action {
            for &mid in module_ids {
                module_cooldowns.reload_sim_deadlines.insert(mid, super::ModuleReloadSimDeadline {
                    sim_start_s: sim_elapsed_secs, sim_deadline_s: *sim_deadline_s,
                });
            }
        }
    }
}

pub(in crate::system_actor) fn resolve_removed_cycle_char_id(
    char_id_opt: Option<CharacterId>,
    ship_id: eve_destiny::BallId,
    db_opt: Option<&eve_db::WorldRepository>,
    fallback_slim_owner: Option<CharacterId>,
    rank_blocks_snapshot: bool,
) -> Option<CharacterId> {
    char_id_opt
        .or_else(|| {
            if rank_blocks_snapshot {
                return None;
            }
            db_opt.and_then(|db_ref| {
                db_ref.with_snapshot(|s| {
                    s.characters
                        .values()
                        .find(|c| c.active_ship_id == ship_id)
                        .map(|c| c.character_id)
                        .or_else(|| s.items.get(&ship_id).map(|it| it.owner_id))
                })
            })
        })
        .or(fallback_slim_owner)
}

pub(in crate::system_actor) fn resolve_effective_deactivation_hooks(
    raw_hook: Option<ModuleDeactivationHook>,
    post_hook: Option<PairedModuleDeactivationPostUnlockHook>,
) -> (
    Option<ModuleDeactivationHook>,
    Option<ModuleDeactivationPostUnlockHook>,
) {
    match (raw_hook, post_hook) {
        (Some(raw), Some((paired_raw, post))) => {
            let effective_post = Arc::ptr_eq(&raw, &paired_raw).then_some(post);
            (Some(raw), effective_post)
        }
        (None, Some((paired_raw, post))) => (Some(paired_raw), Some(post)),
        (raw, None) => (raw, None),
    }
}

pub(in crate::system_actor) fn cloak_reactivation_delay_for_cycle(
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    cycle: &ActiveModuleCycle,
) -> Option<f64> {
    let is_cloak = sde_opt
        .as_ref()
        .and_then(|sde| sde.types.get(&cycle.module_type_id))
        .is_some_and(|t| t.group_id == 330)
        || cycle.effect_name.to_ascii_lowercase().contains("cloak");
    if !is_cloak {
        return None;
    }
    let delay = if cycle.reactivation_delay_s > 0.0 {
        cycle.reactivation_delay_s
    } else {
        sde_opt
            .as_ref()
            .and_then(|sde| sde.types.get(&cycle.module_type_id))
            .and_then(|t| t.default_attributes.get(&669).copied())
            .map(|ms| ms / 1000.0)
            .unwrap_or(0.0)
    };
    (delay.is_finite() && delay > 0.0).then_some(delay)
}

/// Synchronizes TiDi history and upgrades any collected `PostUnlockIntent::DeactivationHook`
/// intents into [`PostUnlockIntent::CooldownDeactivation`] while actor locks are still held,
/// flushing EWAR and recording cloak reactivation cooldowns before `SystemView` publish and
/// FIFO worker submission (`#1541` + `#1170`).
#[allow(clippy::too_many_arguments)]
pub(in crate::system_actor) fn finalize_post_unlock_ops_deactivations_in_actor(
    solar_system_id: SolarSystemId,
    actor_st: &mut SystemActorState,
    ops: &mut super::post_unlock::PostUnlockOps,
    raw_hook: Option<ModuleDeactivationHook>,
    post_hook: Option<PairedModuleDeactivationPostUnlockHook>,
    cooldown_persist_gate: &Arc<CooldownPersistGate>,
    cmd_tx_weak: Option<&tokio::sync::mpsc::WeakSender<SystemCommand>>,
    tidi_factor: f64,
) -> bool {
    if tidi_factor < 0.999_999 {
        cooldown_persist_gate.mark_tidi_history();
        actor_st.has_tidi_history = true;
    } else if cooldown_persist_gate.has_tidi_history() {
        actor_st.has_tidi_history = true;
    }
    let has_tidi_history = actor_st.has_tidi_history;
    if let Some(ewar_op) = ops.worker_ewar_sync.take() {
        let (mutated, ewar_snap) = match ewar_op {
            super::post_unlock::WorkerEwarSyncOp::FlushModuleEwar {
                source_ship_id,
                source_module_id,
                allow_db_source_fallback,
            } => {
                let snap = flush_module_ewar_in_state(
                    &mut actor_st.ewar_state,
                    source_ship_id,
                    source_module_id,
                    allow_db_source_fallback,
                );
                let mutated = !snap.started_targets.is_empty()
                    || !snap.projected_targets.is_empty()
                    || !snap.affected_ships.is_empty()
                    || !snap.remote_assistance_affected_ships.is_empty();
                (mutated, snap)
            }
            super::post_unlock::WorkerEwarSyncOp::RemoveRemoteAssistanceByModule {
                source_ship_id,
                source_module_id,
            } => {
                let remote_assistance_affected_ships = remove_remote_assistance_by_module_in_state(
                    &mut actor_st.ewar_state,
                    source_ship_id,
                    source_module_id,
                );
                let mutated = !remote_assistance_affected_ships.is_empty();
                (
                    mutated,
                    ModuleEwarFlushSnapshot {
                        remote_assistance_affected_ships,
                        ..Default::default()
                    },
                )
            }
        };
        ops.intents
            .push(super::post_unlock::PostUnlockIntent::CooldownDeactivation(
                Box::new(super::post_unlock::DeferredCooldownDeactOp {
                    solar_system_id,
                    char_id: 0,
                    cycle: ActiveModuleCycle::default(),
                    ewar_snap,
                    post_hook: Arc::new(|_, _, _, _| {}),
                    cooldown_payload: None,
                    cooldown_persist_gate: Arc::clone(cooldown_persist_gate),
                    cmd_tx_weak: cmd_tx_weak.cloned(),
                }),
            ));
        return mutated;
    }
    if !ops.has_deactivation_hook() {
        return false;
    }
    let (raw_eff, post_eff) = resolve_effective_deactivation_hooks(raw_hook, post_hook);
    let Some(post_hook) = post_eff else {
        if raw_eff.is_none() {
            ops.intents
                .retain(|intent| !matches!(intent, super::post_unlock::PostUnlockIntent::DeactivationHook(_)));
        }
        return false;
    };

    let now_unix_f64 = actor_st
        .db_opt
        .as_ref()
        .map(|db| db.clock().now_unix_secs() as f64)
        .unwrap_or(0.0);
    let rank_blocks_snapshot = held_lock_ranks().iter().any(|&r| r >= RANK_WORLD_INNER);
    let mut mutated_actor = false;
    let prev_intents = std::mem::take(&mut ops.intents);
    let mut next_intents = Vec::with_capacity(prev_intents.len());
    for intent in prev_intents {
        match intent {
            super::post_unlock::PostUnlockIntent::DeactivationHook(hook_op) => {
                if hook_op.cycle.module_id < 0
                    || super::cycles_tackle::parse_minion_cycle_parent(&hook_op.cycle.effect_name)
                        .is_some()
                {
                    continue;
                }
                let Some(char_id) = resolve_removed_cycle_char_id(
                    hook_op.char_id,
                    hook_op.cycle.ship_id,
                    actor_st.db_opt.as_deref(),
                    hook_op.fallback_owner_id,
                    rank_blocks_snapshot,
                ) else {
                    if rank_blocks_snapshot {
                        next_intents.push(super::post_unlock::PostUnlockIntent::DeactivationHook(
                            super::post_unlock::DeferredDeactivationHookOp {
                                char_id: None,
                                ..hook_op
                            },
                        ));
                    }
                    continue;
                };
                let allow_db_fallback = !rank_blocks_snapshot
                    && actor_st.db_opt.as_ref().is_some_and(|db| {
                        db.with_snapshot(|s| {
                            s.items
                                .get(&hook_op.cycle.module_id)
                                .is_some_and(|i| i.location_id == hook_op.cycle.ship_id)
                        })
                    });
                let ewar_snap = flush_module_ewar_in_state(
                    &mut actor_st.ewar_state,
                    hook_op.cycle.ship_id,
                    hook_op.cycle.module_id,
                    allow_db_fallback,
                );
                mutated_actor |= !ewar_snap.started_targets.is_empty()
                    || !ewar_snap.projected_targets.is_empty()
                    || !ewar_snap.affected_ships.is_empty()
                    || !ewar_snap.remote_assistance_affected_ships.is_empty();
                let payload_opt = (hook_op.solar_system_id > 0
                    && hook_op.solar_system_id == solar_system_id)
                    .then(|| cloak_reactivation_delay_for_cycle(&actor_st.sde_opt, &hook_op.cycle))
                    .flatten()
                    .and_then(|delay_s| {
                        mutated_actor = true;
                        record_module_cooldown_in_actor_state(
                            solar_system_id,
                            actor_st,
                            hook_op.cycle.ship_id,
                            hook_op.cycle.module_id,
                            hook_op.cycle.module_type_id,
                            now_unix_f64,
                            delay_s,
                            tidi_factor,
                            has_tidi_history,
                        )
                    });
                next_intents.push(super::post_unlock::PostUnlockIntent::CooldownDeactivation(
                    Box::new(super::post_unlock::DeferredCooldownDeactOp {
                        solar_system_id: hook_op.solar_system_id,
                        char_id,
                        cycle: hook_op.cycle,
                        ewar_snap,
                        post_hook: Arc::clone(&post_hook),
                        cooldown_payload: payload_opt,
                        cooldown_persist_gate: Arc::clone(cooldown_persist_gate),
                        cmd_tx_weak: cmd_tx_weak.cloned(),
                    }),
                ));
            }
            other => next_intents.push(other),
        }
    }
    ops.intents = next_intents;
    super::weapon_effects::collect_dirty_weapon_effects_post_unlock_op(solar_system_id, actor_st, ops);
    mutated_actor
}

pub(in crate::system_actor) fn handle_ewar_and_cooldown_command(
    exec_ctx: &mut ActorExecCtx<'_>,
    cmd: SystemCommand,
) -> Option<SystemCommand> {
    match cmd {
        SystemCommand::UpsertProjectedEwar { source_ship_id, target_ship_id, effect, reply } => {
            upsert_projected_ewar_in_state(&mut exec_ctx.actor_st.ewar_state, source_ship_id, target_ship_id, effect);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::RecordProjectedEwarSource { source_module_id, source_ship_id, reply } => {
            exec_ctx.actor_st.ewar_state.projected_ewar_sources.insert(source_module_id, source_ship_id);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::RemoveProjectedEwarByModule { source_ship_id, source_module_id, allow_db_source_fallback, reply } => {
            let affected = remove_projected_ewar_by_module_in_state(
                &mut exec_ctx.actor_st.ewar_state, source_ship_id, source_module_id, allow_db_source_fallback,
            );
            defer_reply(exec_ctx.deferred_replies, reply, Ok(affected));
            None
        }
        SystemCommand::ClearProjectedEwarForTarget { target_ship_id, reply } => {
            exec_ctx.actor_st.ewar_state.projected_ewar.remove(&target_ship_id);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::RecordActiveEwarStart { source_ship_id, module_id, target_ship_id, jamming_type, reply } => {
            exec_ctx.actor_st.ewar_state.active_ewar_starts.entry((source_ship_id, module_id)).or_default().push((target_ship_id, jamming_type));
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::ReplaceActiveEwarStarts { source_ship_id, module_id, targets, reply } => {
            if targets.is_empty() {
                exec_ctx.actor_st.ewar_state.active_ewar_starts.remove(&(source_ship_id, module_id));
            } else {
                exec_ctx.actor_st.ewar_state.active_ewar_starts.insert((source_ship_id, module_id), targets);
            }
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::FlushModuleEwar { source_ship_id, source_module_id, allow_db_source_fallback, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = flush_module_ewar_in_actor(&mut ctx, source_ship_id, source_module_id, allow_db_source_fallback);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ClearEwarState { reply } => {
            exec_ctx.actor_st.ewar_state = ActorEwarState::default();
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::UpsertRemoteAssistance { target_ship_id, entry, reply } => {
            upsert_remote_assistance_in_state(&mut exec_ctx.actor_st.ewar_state, target_ship_id, entry);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::RemoveRemoteAssistanceByModule { source_ship_id, source_module_id, reply } => {
            let affected = remove_remote_assistance_by_module_in_state(
                &mut exec_ctx.actor_st.ewar_state,
                source_ship_id,
                source_module_id,
            );
            defer_reply(exec_ctx.deferred_replies, reply, Ok(affected));
            None
        }
        SystemCommand::SetRemoteAssistanceForTarget { target_ship_id, entries, reply } => {
            set_remote_assistance_for_target_in_state(&mut exec_ctx.actor_st.ewar_state, target_ship_id, entries);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::TryAcquireInFlightSuperweapon { ship_id, reply } => {
            let ewar = &mut exec_ctx.actor_st.ewar_state;
            let token_opt = if ewar.in_flight_superweapons.contains_key(&ship_id) {
                None
            } else {
                ewar.next_in_flight_superweapon_token =
                    ewar.next_in_flight_superweapon_token.wrapping_add(1).max(1);
                let token = ewar.next_in_flight_superweapon_token;
                ewar.in_flight_superweapons.insert(ship_id, token);
                Some(token)
            };
            defer_reply(exec_ctx.deferred_replies, reply, Ok(token_opt));
            None
        }
        SystemCommand::ReleaseInFlightSuperweapon { ship_id, token, reply } => {
            let removed = exec_ctx
                .actor_st
                .ewar_state
                .in_flight_superweapons
                .get(&ship_id)
                .copied()
                == Some(token)
                && exec_ctx
                    .actor_st
                    .ewar_state
                    .in_flight_superweapons
                    .remove(&ship_id)
                    .is_some();
            if let Some(reply) = reply {
                defer_reply(exec_ctx.deferred_replies, reply, Ok(removed));
            }
            None
        }
        SystemCommand::RecordModuleCooldown { ship_id, module_id, module_type_id, now_unix_f64, delay_s, tidi_factor, has_tidi_history, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = record_module_cooldown_in_actor(&mut ctx, RecordModuleCooldownReq {
                ship_id, module_id, module_type_id, now_unix_f64, delay_s, tidi_factor, has_tidi_history,
            });
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::SeedModuleCooldownEntry { key, entry, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = seed_module_cooldown_in_actor(&mut ctx, key, entry);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::RemoveModuleCooldown { key, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = remove_module_cooldown_in_actor(&mut ctx, key);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ClearShipModuleCooldowns { ship_id, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = clear_ship_module_cooldowns_in_actor(&mut ctx, ship_id);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ClearAllModuleCooldowns { reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = clear_all_module_cooldowns_in_actor(&mut ctx);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ReplaceModuleCooldowns {
            cooldowns,
            expected_gate_token,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = replace_module_cooldowns_in_actor(&mut ctx, cooldowns, expected_gate_token);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::PruneExpiredModuleCooldowns { now_unix_f64, tidi_factor, has_tidi_history, force_persist, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = prune_expired_module_cooldowns_in_actor(&mut ctx, now_unix_f64, tidi_factor, has_tidi_history, force_persist);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::SnapshotModuleCooldownCheckpoint { only_if_dirty, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = snapshot_module_cooldown_checkpoint_in_actor(&mut ctx, only_if_dirty);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::RecordModuleReloadSimDeadlines { module_ids, sim_start_s, sim_deadline_s, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = record_module_reload_sim_deadlines_in_actor(&mut ctx, module_ids, sim_start_s, sim_deadline_s);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ClearModuleReloadSimDeadline { module_id, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = clear_module_reload_sim_deadline_in_actor(&mut ctx, module_id);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ApplyLoadingCooldownTransitions { tidi_factor, has_tidi_history, transitions, reply } => {
            let eff_hist = exec_ctx.actor_st.has_tidi_history || has_tidi_history || tidi_factor < 0.999_999;
            let applied = apply_pending_transitions_to_actor_state(
                exec_ctx.solar_system_id, exec_ctx.actor_st, tidi_factor, eff_hist, transitions,
            );
            let payload_opt = exec_ctx.actor_st.module_cooldown_checkpoint_dirty.then(|| {
                build_persist_payload_from_actor_state(exec_ctx.actor_st, tidi_factor, eff_hist || exec_ctx.actor_st.has_tidi_history)
            });
            defer_reply(exec_ctx.deferred_replies, reply, Ok((applied, payload_opt)));
            None
        }
        SystemCommand::ApplyPersistedCooldownCommit { commit, reply } => {
            let _ = apply_persisted_cooldown_commit_to_state(exec_ctx.actor_st, &commit);
            if let Some(reply) = reply {
                defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            }
            None
        }
        other => Some(other),
    }
}

impl SystemView {
    pub fn projected_ewar_for(&self, target_ship_id: i64) -> Option<&[eve_dogma::ProjectedEwarEffect]> { self.ewar.projected_ewar.get(&target_ship_id).map(Vec::as_slice) }
    pub fn projected_ewar_source(&self, source_module_id: i64) -> Option<i64> { self.ewar.projected_ewar_sources.get(&source_module_id).copied() }
    pub fn active_ewar_starts_for(&self, ship_id: i64, module_id: i64) -> Option<&[(i64, &'static str)]> { self.ewar.active_ewar_starts.get(&(ship_id, module_id)).map(Vec::as_slice) }
    pub fn remote_assistance_for(&self, target_ship_id: i64) -> Option<&[ProjectedRemoteAssistance]> { self.ewar.remote_assistance.get(&target_ship_id).map(Vec::as_slice) }
    pub fn has_in_flight_superweapons(&self) -> bool { !self.ewar.in_flight_superweapons.is_empty() }
    pub fn module_reactivation_cooldown(&self, key: &(i64, i64, u32)) -> Option<ModuleCooldownEntry> { self.module_cooldowns.reactivation_cooldowns.get(key).copied() }
    pub fn module_reload_sim_deadline(&self, module_id: i64) -> Option<ModuleReloadSimDeadline> { self.module_cooldowns.reload_sim_deadlines.get(&module_id).copied() }
}

impl SystemHandle {
    /// Reads the latest projected EWAR effects targeting `target_ship_id` lock-free from [`SystemView`].
    pub fn projected_ewar_for(&self, target_ship_id: i64) -> Vec<eve_dogma::ProjectedEwarEffect> { self.view().ewar.projected_ewar.get(&target_ship_id).cloned().unwrap_or_default() }
    /// Reads the published `ActorEwarState` snapshot lock-free from [`SystemView`].
    pub fn ewar_state_snapshot(&self) -> ActorEwarState { self.view().ewar.clone() }
    /// Reads the published module reactivation cooldown map lock-free from [`SystemView`].
    pub fn module_cooldowns_snapshot(&self) -> HashMap<(i64, i64, u32), ModuleCooldownEntry> { self.view().module_cooldowns.reactivation_cooldowns.clone() }
    pub fn module_reactivation_cooldown(&self, key: &(i64, i64, u32)) -> Option<ModuleCooldownEntry> { self.view().module_reactivation_cooldown(key) }

    /// Upserts projected EWAR onto `target_ship_id` inside this solar system actor (`#1541`).
    pub async fn upsert_projected_ewar(&self, source_ship_id: i64, target_ship_id: i64, effect: eve_dogma::ProjectedEwarEffect) -> Result<(), String> {
        call_actor_result_with_deadline(self, "UpsertProjectedEwar", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::UpsertProjectedEwar { source_ship_id, target_ship_id, effect, reply }
        }).await
    }

    /// Synchronous bridge for [`Self::upsert_projected_ewar`].
    pub fn upsert_projected_ewar_sync(&self, source_ship_id: i64, target_ship_id: i64, effect: eve_dogma::ProjectedEwarEffect) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self, "UpsertProjectedEwar", ACTOR_CMD_TIMEOUT, (source_ship_id, target_ship_id, effect),
            |(source_ship_id, target_ship_id, effect), reply| SystemCommand::UpsertProjectedEwar { source_ship_id, target_ship_id, effect, reply },
            |ctx, (s, t, e)| { upsert_projected_ewar_in_state(&mut ctx.actor_st.ewar_state, s, t, e); Ok(()) },
        )
    }

    pub fn record_projected_ewar_source_sync(&self, source_module_id: i64, source_ship_id: i64) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self, "RecordProjectedEwarSource", ACTOR_CMD_TIMEOUT, (source_module_id, source_ship_id),
            |(source_module_id, source_ship_id), reply| SystemCommand::RecordProjectedEwarSource { source_module_id, source_ship_id, reply },
            |ctx, (m, s)| { ctx.actor_st.ewar_state.projected_ewar_sources.insert(m, s); Ok(()) },
        )
    }

    pub async fn remove_projected_ewar_by_module(&self, source_ship_id: i64, source_module_id: i64, allow_db_source_fallback: bool) -> Result<Vec<i64>, String> {
        call_actor_result_with_deadline(self, "RemoveProjectedEwarByModule", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::RemoveProjectedEwarByModule { source_ship_id, source_module_id, allow_db_source_fallback, reply }
        }).await
    }

    pub fn remove_projected_ewar_by_module_sync(
        &self,
        source_ship_id: i64,
        source_module_id: i64,
        allow_db_source_fallback: bool,
    ) -> Result<Vec<i64>, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "RemoveProjectedEwarByModule",
            ACTOR_CMD_TIMEOUT,
            (source_ship_id, source_module_id, allow_db_source_fallback),
            |(source_ship_id, source_module_id, allow_db_source_fallback), reply| {
                SystemCommand::RemoveProjectedEwarByModule {
                    source_ship_id,
                    source_module_id,
                    allow_db_source_fallback,
                    reply,
                }
            },
            |ctx, (source_ship_id, source_module_id, allow_db_source_fallback)| {
                Ok(remove_projected_ewar_by_module_in_state(
                    &mut ctx.actor_st.ewar_state,
                    source_ship_id,
                    source_module_id,
                    allow_db_source_fallback,
                ))
            },
        )
    }

    pub fn clear_projected_ewar_for_target_sync(&self, target_ship_id: i64) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "ClearProjectedEwarForTarget",
            ACTOR_CMD_TIMEOUT,
            target_ship_id,
            |target_ship_id, reply| SystemCommand::ClearProjectedEwarForTarget {
                target_ship_id,
                reply,
            },
            |ctx, target_ship_id| {
                ctx.actor_st.ewar_state.projected_ewar.remove(&target_ship_id);
                Ok(())
            },
        )
    }

    pub async fn record_active_ewar_start(
        &self,
        source_ship_id: i64,
        module_id: i64,
        target_ship_id: i64,
        jamming_type: &'static str,
    ) -> Result<(), String> {
        call_actor_result_with_deadline(self, "RecordActiveEwarStart", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::RecordActiveEwarStart {
                source_ship_id,
                module_id,
                target_ship_id,
                jamming_type,
                reply,
            }
        })
        .await
    }

    pub fn record_active_ewar_start_sync(
        &self,
        source_ship_id: i64,
        module_id: i64,
        target_ship_id: i64,
        jamming_type: &'static str,
    ) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self, "RecordActiveEwarStart", ACTOR_CMD_TIMEOUT,
            (source_ship_id, module_id, target_ship_id, jamming_type),
            |(source_ship_id, module_id, target_ship_id, jamming_type), reply| {
                SystemCommand::RecordActiveEwarStart { source_ship_id, module_id, target_ship_id, jamming_type, reply }
            },
            |ctx, (source_ship_id, module_id, target_ship_id, jamming_type)| {
                ctx.actor_st.ewar_state.active_ewar_starts.entry((source_ship_id, module_id)).or_default().push((target_ship_id, jamming_type));
                Ok(())
            },
        )
    }

    pub fn replace_active_ewar_starts_sync(
        &self,
        source_ship_id: i64,
        module_id: i64,
        targets: Vec<(i64, &'static str)>,
    ) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self, "ReplaceActiveEwarStarts", ACTOR_CMD_TIMEOUT,
            (source_ship_id, module_id, targets),
            |(source_ship_id, module_id, targets), reply| SystemCommand::ReplaceActiveEwarStarts { source_ship_id, module_id, targets, reply },
            |ctx, (source_ship_id, module_id, targets)| {
                if targets.is_empty() {
                    ctx.actor_st.ewar_state.active_ewar_starts.remove(&(source_ship_id, module_id));
                } else {
                    ctx.actor_st.ewar_state.active_ewar_starts.insert((source_ship_id, module_id), targets);
                }
                Ok(())
            },
        )
    }

    pub async fn flush_module_ewar(
        &self,
        source_ship_id: i64,
        source_module_id: i64,
        allow_db_source_fallback: bool,
    ) -> Result<ModuleEwarFlushSnapshot, String> {
        call_actor_result_with_deadline(self, "FlushModuleEwar", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::FlushModuleEwar { source_ship_id, source_module_id, allow_db_source_fallback, reply }
        }).await
    }

    pub(in crate::system_actor) fn is_on_post_unlock_worker_thread(&self) -> bool {
        self.post_unlock_worker_thread_id
            .get()
            .is_some_and(|&tid| tid == std::thread::current().id())
    }

    pub(in crate::system_actor) fn is_on_post_unlock_fallback_thread(&self) -> bool {
        self.post_unlock_fallback.is_current_thread_owner()
    }

    pub(in crate::system_actor) fn is_on_post_unlock_callback_thread(&self) -> bool {
        self.is_on_post_unlock_worker_thread() || self.is_on_post_unlock_fallback_thread()
    }

    fn dispatch_worker_ewar_sync(
        &self,
        op: super::post_unlock::WorkerEwarSyncOp,
    ) -> Result<ModuleEwarFlushSnapshot, String> {
        if self.worker_cb_tx.is_closed() {
            return Err(format!("system {} actor closed", self.solar_system_id));
        }
        let mut ops = super::post_unlock::PostUnlockOps::default();
        ops.worker_ewar_sync = Some(op);
        let Some(mut hooks) = super::post_unlock::call_actor_callback(&self.worker_cb_tx, |reply| {
            super::post_unlock::PostUnlockActorCallback::FinalizeDeactivations {
                ops,
                raw: None,
                post_pair: None,
                reply,
            }
        }) else {
            return Err(format!("system {} actor closed", self.solar_system_id));
        };
        Ok(hooks.pop().map(|h| h.ewar_snap).unwrap_or_default())
    }

    /// Synchronously removes active/projected EWAR state for `(source_ship_id, source_module_id)`
    /// and guarantees [`SystemView`] reflects the removal before returning.
    /// Requires no ranked locks held by the caller.
    pub fn flush_module_ewar_sync(
        &self,
        source_ship_id: i64,
        source_module_id: i64,
        allow_db_source_fallback: bool,
    ) -> Result<ModuleEwarFlushSnapshot, String> {
        if !held_lock_ranks().is_empty() {
            return Err(format!(
                "refusing flush_module_ewar_sync on system {} while holding ranked locks",
                self.solar_system_id
            ));
        }
        if self.is_on_post_unlock_callback_thread() {
            return self.dispatch_worker_ewar_sync(
                super::post_unlock::WorkerEwarSyncOp::FlushModuleEwar {
                    source_ship_id,
                    source_module_id,
                    allow_db_source_fallback,
                },
            );
        }
        dispatch_actor_sync_with_deadline(
            self, "FlushModuleEwar", ACTOR_CMD_TIMEOUT,
            (source_ship_id, source_module_id, allow_db_source_fallback),
            |(source_ship_id, source_module_id, allow_db_source_fallback), reply| {
                SystemCommand::FlushModuleEwar { source_ship_id, source_module_id, allow_db_source_fallback, reply }
            },
            |ctx, (s, m, fb)| flush_module_ewar_in_actor(ctx, s, m, fb),
        )
    }

    pub async fn clear_ewar_state(&self) -> Result<(), String> {
        call_actor_result_with_deadline(self, "ClearEwarState", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::ClearEwarState { reply }
        }).await
    }

    pub fn clear_ewar_state_sync(&self) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self, "ClearEwarState", ACTOR_CMD_TIMEOUT, (),
            |(), reply| SystemCommand::ClearEwarState { reply },
            |ctx, ()| { ctx.actor_st.ewar_state = ActorEwarState::default(); Ok(()) },
        )
    }

    pub fn upsert_remote_assistance_sync(
        &self,
        target_ship_id: i64,
        entry: ProjectedRemoteAssistance,
    ) -> Result<(), String> {
        if !held_lock_ranks().is_empty() {
            return Err(format!(
                "refusing upsert_remote_assistance_sync on system {} while holding ranked locks",
                self.solar_system_id
            ));
        }
        dispatch_actor_sync_with_deadline(
            self,
            "UpsertRemoteAssistance",
            ACTOR_CMD_TIMEOUT,
            (target_ship_id, entry),
            |(target_ship_id, entry), reply| SystemCommand::UpsertRemoteAssistance {
                target_ship_id,
                entry,
                reply,
            },
            |ctx, (target_ship_id, entry)| {
                upsert_remote_assistance_in_state(&mut ctx.actor_st.ewar_state, target_ship_id, entry);
                Ok(())
            },
        )
    }

    pub fn remove_remote_assistance_by_module_sync(
        &self,
        source_ship_id: i64,
        source_module_id: i64,
    ) -> Result<Vec<i64>, String> {
        if !held_lock_ranks().is_empty() {
            return Err(format!(
                "refusing remove_remote_assistance_by_module_sync on system {} while holding ranked locks",
                self.solar_system_id
            ));
        }
        if self.is_on_post_unlock_callback_thread() {
            return self
                .dispatch_worker_ewar_sync(
                    super::post_unlock::WorkerEwarSyncOp::RemoveRemoteAssistanceByModule {
                        source_ship_id,
                        source_module_id,
                    },
                )
                .map(|snap| snap.remote_assistance_affected_ships);
        }
        dispatch_actor_sync_with_deadline(
            self,
            "RemoveRemoteAssistanceByModule",
            ACTOR_CMD_TIMEOUT,
            (source_ship_id, source_module_id),
            |(source_ship_id, source_module_id), reply| {
                SystemCommand::RemoveRemoteAssistanceByModule {
                    source_ship_id,
                    source_module_id,
                    reply,
                }
            },
            |ctx, (source_ship_id, source_module_id)| {
                Ok(remove_remote_assistance_by_module_in_state(
                    &mut ctx.actor_st.ewar_state,
                    source_ship_id,
                    source_module_id,
                ))
            },
        )
    }

    pub fn set_remote_assistance_for_target_sync(
        &self,
        target_ship_id: i64,
        entries: Vec<ProjectedRemoteAssistance>,
    ) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "SetRemoteAssistanceForTarget",
            ACTOR_CMD_TIMEOUT,
            (target_ship_id, entries),
            |(target_ship_id, entries), reply| SystemCommand::SetRemoteAssistanceForTarget {
                target_ship_id,
                entries,
                reply,
            },
            |ctx, (target_ship_id, entries)| {
                set_remote_assistance_for_target_in_state(
                    &mut ctx.actor_st.ewar_state,
                    target_ship_id,
                    entries,
                );
                Ok(())
            },
        )
    }

    pub async fn try_acquire_in_flight_superweapon(
        &self,
        ship_id: i64,
    ) -> Result<Option<u64>, String> {
        call_actor_result_with_deadline(
            self,
            "TryAcquireInFlightSuperweapon",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::TryAcquireInFlightSuperweapon { ship_id, reply },
        )
        .await
    }

    pub async fn release_in_flight_superweapon(
        &self,
        ship_id: i64,
        token: u64,
    ) -> Result<bool, String> {
        call_actor_result_with_deadline(
            self,
            "ReleaseInFlightSuperweapon",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::ReleaseInFlightSuperweapon {
                ship_id,
                token,
                reply: Some(reply),
            },
        )
        .await
    }

    pub fn release_in_flight_superweapon_best_effort(&self, ship_id: i64, token: u64) {
        let cmd = SystemCommand::ReleaseInFlightSuperweapon {
            ship_id,
            token,
            reply: None,
        };
        if let Err(tokio::sync::mpsc::error::TrySendError::Full(cmd)) =
            self.try_send_actor_cmd(cmd)
        {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let tx = self.cmd_tx.clone();
                handle.spawn(async move {
                    let _ = tx.send(cmd).await;
                });
            }
        }
    }

    pub(crate) fn enqueue_post_unlock_action(&self, action: PostUnlockHookAction) {
        self.post_unlock_actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(action);
        if self
            .post_unlock_tx
            .send(super::post_unlock::PostUnlockMsg::FlushActions)
            .is_err()
        {
            self.post_unlock_fallback.ensure_actions_runner(
                self.solar_system_id,
                Arc::clone(&self.post_unlock_actions),
                Arc::clone(&self.post_unlock_actions_in_flight),
                Arc::clone(&self.command_panics),
            );
        }
    }

    pub(in crate::system_actor) fn apply_loading_cooldown_transitions_sync(
        &self,
        tidi_factor: f64,
        has_tidi_history: bool,
        transitions: Vec<PendingCooldownTransition>,
    ) -> Result<(bool, Option<ModuleCooldownPersistPayload>), String> {
        dispatch_actor_sync_with_deadline(
            self, "ApplyLoadingCooldownTransitions", ACTOR_CMD_TIMEOUT, (tidi_factor, has_tidi_history, transitions),
            |(tidi_factor, has_tidi_history, transitions), reply| SystemCommand::ApplyLoadingCooldownTransitions { tidi_factor, has_tidi_history, transitions, reply },
            |ctx, (tidi, hist, tr)| {
                let eff_hist = ctx.actor_st.has_tidi_history || hist || tidi < 0.999_999;
                let applied = apply_pending_transitions_to_actor_state(ctx.solar_system_id, ctx.actor_st, tidi, eff_hist, tr);
                let payload_opt = ctx.actor_st.module_cooldown_checkpoint_dirty.then(|| {
                    build_persist_payload_from_actor_state(ctx.actor_st, tidi, eff_hist || ctx.actor_st.has_tidi_history)
                });
                Ok((applied, payload_opt))
            },
        )
    }

    pub(in crate::system_actor) fn apply_persisted_cooldown_commit_sync(
        &self,
        commit: PersistedCooldownCommit,
    ) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self, "ApplyPersistedCooldownCommit", ACTOR_CMD_TIMEOUT, commit,
            |commit, reply| SystemCommand::ApplyPersistedCooldownCommit { commit, reply: Some(reply) },
            |ctx, commit| { let _ = apply_persisted_cooldown_commit_to_state(ctx.actor_st, &commit); Ok(()) },
        )
    }

    pub(in crate::system_actor) fn persist_cooldown_payload(&self, mut payload: ModuleCooldownPersistPayload) -> bool {
        payload.tidi_factor = self.tidi_factor();
        if payload.tidi_factor < 0.999_999 || self.cooldown_persist_gate.has_tidi_history() {
            payload.has_tidi_history = true;
        }
        let Some(commit) = persist_cooldown_payload_with_gate_in_db(
            self.solar_system_id, &self.cooldown_persist_gate, payload,
        ) else {
            return false;
        };
        let _ = self.apply_persisted_cooldown_commit_sync(commit);
        true
    }

    async fn persist_cooldown_payload_async(&self, payload: ModuleCooldownPersistPayload) -> bool {
        let h = self.clone();
        tokio::task::spawn_blocking(move || h.persist_cooldown_payload(payload)).await.unwrap_or(false)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_module_cooldown(
        &self, ship_id: i64, module_id: i64, module_type_id: u32,
        now_unix_f64: f64, delay_s: f64, tidi_factor: f64, has_tidi_history: bool,
    ) -> Result<(), String> {
        let payload = call_actor_result_with_deadline(self, "RecordModuleCooldown", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::RecordModuleCooldown { ship_id, module_id, module_type_id, now_unix_f64, delay_s, tidi_factor, has_tidi_history, reply }
        }).await?;
        self.persist_cooldown_payload_async(payload).await;
        Ok(())
    }

    pub async fn remove_module_cooldown(&self, key: (i64, i64, u32)) -> Result<bool, String> {
        let payload_opt = call_actor_result_with_deadline(self, "RemoveModuleCooldown", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::RemoveModuleCooldown { key, reply }
        }).await?;
        let removed = payload_opt.is_some();
        if let Some(payload) = payload_opt { self.persist_cooldown_payload_async(payload).await; }
        Ok(removed)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_module_cooldown_sync(
        &self, ship_id: i64, module_id: i64, module_type_id: u32,
        now_unix_f64: f64, delay_s: f64, tidi_factor: f64, has_tidi_history: bool,
    ) -> Result<(), String> {
        let payload = dispatch_actor_sync_with_deadline(
            self, "RecordModuleCooldown", ACTOR_CMD_TIMEOUT,
            RecordModuleCooldownReq { ship_id, module_id, module_type_id, now_unix_f64, delay_s, tidi_factor, has_tidi_history },
            |r, reply| SystemCommand::RecordModuleCooldown {
                ship_id: r.ship_id, module_id: r.module_id, module_type_id: r.module_type_id,
                now_unix_f64: r.now_unix_f64, delay_s: r.delay_s, tidi_factor: r.tidi_factor, has_tidi_history: r.has_tidi_history, reply,
            },
            record_module_cooldown_in_actor,
        )?;
        self.persist_cooldown_payload(payload);
        Ok(())
    }

    pub fn seed_module_cooldown_entry_sync(&self, key: (i64, i64, u32), entry: ModuleCooldownEntry) -> Result<(), String> {
        let payload = dispatch_actor_sync_with_deadline(
            self, "SeedModuleCooldownEntry", ACTOR_CMD_TIMEOUT, (key, entry),
            |(key, entry), reply| SystemCommand::SeedModuleCooldownEntry { key, entry, reply },
            |ctx, (key, entry)| seed_module_cooldown_in_actor(ctx, key, entry),
        )?;
        self.persist_cooldown_payload(payload);
        Ok(())
    }

    pub fn remove_module_cooldown_sync(&self, key: (i64, i64, u32)) -> Result<bool, String> {
        let payload_opt = dispatch_actor_sync_with_deadline(
            self, "RemoveModuleCooldown", ACTOR_CMD_TIMEOUT, key,
            |key, reply| SystemCommand::RemoveModuleCooldown { key, reply }, remove_module_cooldown_in_actor,
        )?;
        let removed = payload_opt.is_some();
        if let Some(payload) = payload_opt { self.persist_cooldown_payload(payload); }
        Ok(removed)
    }

    pub fn clear_ship_module_cooldowns_sync(&self, ship_id: i64) -> Result<bool, String> {
        let payload_opt = dispatch_actor_sync_with_deadline(
            self, "ClearShipModuleCooldowns", ACTOR_CMD_TIMEOUT, ship_id,
            |ship_id, reply| SystemCommand::ClearShipModuleCooldowns { ship_id, reply }, clear_ship_module_cooldowns_in_actor,
        )?;
        let removed = payload_opt.is_some();
        if let Some(payload) = payload_opt { self.persist_cooldown_payload(payload); }
        Ok(removed)
    }

    pub fn clear_all_module_cooldowns_sync(&self) -> Result<(), String> {
        let payload = dispatch_actor_sync_with_deadline(
            self, "ClearAllModuleCooldowns", ACTOR_CMD_TIMEOUT, (),
            |(), reply| SystemCommand::ClearAllModuleCooldowns { reply },
            |ctx, ()| clear_all_module_cooldowns_in_actor(ctx),
        )?;
        self.persist_cooldown_payload(payload);
        Ok(())
    }

    pub fn replace_module_cooldowns_checked_sync(
        &self, cooldowns: HashMap<(i64, i64, u32), ModuleCooldownEntry>, expected_gate_token: Option<(u64, Option<(u64, u64)>)>,
    ) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self, "ReplaceModuleCooldowns", ACTOR_CMD_TIMEOUT, (cooldowns, expected_gate_token),
            |(cooldowns, expected_gate_token), reply| SystemCommand::ReplaceModuleCooldowns { cooldowns, expected_gate_token, reply },
            |ctx, (c, tok)| replace_module_cooldowns_in_actor(ctx, c, tok),
        )
    }

    pub fn replace_module_cooldowns_sync(&self, cooldowns: HashMap<(i64, i64, u32), ModuleCooldownEntry>) -> Result<(), String> {
        self.replace_module_cooldowns_checked_sync(cooldowns, None)
    }

    pub fn prune_expired_module_cooldowns_sync(
        &self, now_unix_f64: f64, tidi_factor: f64, has_tidi_history: bool, force_persist: bool,
    ) -> Result<bool, String> {
        let payload_opt = dispatch_actor_sync_with_deadline(
            self, "PruneExpiredModuleCooldowns", ACTOR_CMD_TIMEOUT,
            (now_unix_f64, tidi_factor, has_tidi_history, force_persist),
            |(now_unix_f64, tidi_factor, has_tidi_history, force_persist), reply| {
                SystemCommand::PruneExpiredModuleCooldowns { now_unix_f64, tidi_factor, has_tidi_history, force_persist, reply }
            },
            |ctx, (n, t, h, f)| prune_expired_module_cooldowns_in_actor(ctx, n, t, h, f),
        )?;
        let pruned = payload_opt.is_some();
        if let Some(payload) = payload_opt { self.persist_cooldown_payload(payload); }
        Ok(pruned)
    }

    /// Flushes this system actor's module cooldowns to per-system SQLite checkpoints
    /// post-unlock (`#1541`). Called on eviction, runtime sync, and checkpoint flushes.
    pub async fn flush_module_cooldown_checkpoint(&self) -> Result<usize, String> {
        let payload_opt = call_actor_result_with_deadline(
            self, "SnapshotModuleCooldownCheckpoint", ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::SnapshotModuleCooldownCheckpoint { only_if_dirty: true, reply },
        ).await?;
        let Some(payload) = payload_opt else { return Ok(0); };
        let count = payload.entries.len();
        self.persist_cooldown_payload_async(payload).await;
        Ok(count)
    }

    fn flush_module_cooldown_checkpoint_sync_inner(&self, only_if_dirty: bool) -> Result<usize, String> {
        let payload_opt = dispatch_actor_sync_with_deadline(
            self, "SnapshotModuleCooldownCheckpoint", ACTOR_CMD_TIMEOUT, only_if_dirty,
            |only_if_dirty, reply| SystemCommand::SnapshotModuleCooldownCheckpoint { only_if_dirty, reply },
            snapshot_module_cooldown_checkpoint_in_actor,
        )?;
        let Some(payload) = payload_opt else { return Ok(0); };
        let count = payload.entries.len();
        self.persist_cooldown_payload(payload);
        Ok(count)
    }

    pub fn flush_module_cooldown_checkpoint_sync(&self) -> Result<usize, String> { self.flush_module_cooldown_checkpoint_sync_inner(false) }
    pub fn flush_dirty_module_cooldown_checkpoint_sync(&self) -> Result<usize, String> { self.flush_module_cooldown_checkpoint_sync_inner(true) }

    pub fn record_module_reload_sim_deadlines_sync(&self, module_ids: Vec<i64>, sim_start_s: f64, sim_deadline_s: f64) -> Result<(), String> {
        if module_ids.is_empty() { return Ok(()); }
        dispatch_actor_sync_with_deadline(
            self, "RecordModuleReloadSimDeadlines", ACTOR_CMD_TIMEOUT, (module_ids, sim_start_s, sim_deadline_s),
            |(module_ids, sim_start_s, sim_deadline_s), reply| SystemCommand::RecordModuleReloadSimDeadlines { module_ids, sim_start_s, sim_deadline_s, reply },
            |ctx, (ids, s, d)| record_module_reload_sim_deadlines_in_actor(ctx, ids, s, d),
        )
    }

    pub fn clear_module_reload_sim_deadline_sync(&self, module_id: i64) -> Result<bool, String> {
        dispatch_actor_sync_with_deadline(
            self, "ClearModuleReloadSimDeadline", ACTOR_CMD_TIMEOUT, module_id,
            |module_id, reply| SystemCommand::ClearModuleReloadSimDeadline { module_id, reply }, clear_module_reload_sim_deadline_in_actor,
        )
    }

    #[cfg(test)]
    pub(crate) fn enqueue_raw_deact_hook_for_test(&self, db: Arc<eve_db::WorldRepository>, sde: Arc<eve_sde::SdeRegistry>, ship_id: i64, module_id: i64, module_type_id: u32, delay_s: f64) -> tokio::sync::oneshot::Receiver<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut ops = super::post_unlock::PostUnlockOps::default();
        ops.intents.push(super::post_unlock::PostUnlockIntent::DeactivationHook(super::post_unlock::DeferredDeactivationHookOp {
            solar_system_id: self.solar_system_id, char_id: None, fallback_owner_id: None,
            cycle: ActiveModuleCycle { ship_id, module_id, module_type_id, reactivation_delay_s: delay_s, ..Default::default() },
        }));
        let item = super::post_unlock::PostUnlockWorkItem { db_opt: Some(db), sde_opt: Some(sde), ops, replies: vec![Box::new(move || { let _ = tx.send(()); })], in_flight_guard: None, work_seq: 0 };
        let _ = self.post_unlock_tx.send(super::post_unlock::PostUnlockMsg::Work(item));
        rx
    }
}

// ---------------------------------------------------------------------------
// SystemRegistry EWAR facades
// ---------------------------------------------------------------------------

impl SystemRegistry {
    /// Finds a currently loaded system handle whose [`SystemView`] contains ball `ship_id` lock-free.
    pub fn find_ship_live_handle(&self, ship_id: i64) -> Option<SystemHandle> {
        self.active_system_handles().into_iter().find(|(_, h)| h.view().balls.contains_key(&ship_id)).map(|(_, h)| h)
    }

    /// Reads projected EWAR targeting `target_ship_id` lock-free across live [`SystemView`]s.
    pub fn ship_ewar_projections(&self, target_ship_id: i64) -> Vec<eve_dogma::ProjectedEwarEffect> {
        if let Some(handle) = self.find_ship_live_handle(target_ship_id) {
            return handle.projected_ewar_for(target_ship_id);
        }
        for (_, handle) in self.active_system_handles() {
            if let Some(list) = handle.view().ewar.projected_ewar.get(&target_ship_id) {
                return list.clone();
            }
        }
        Vec::new()
    }

    /// Reads projected remote assistance targeting `target_ship_id` lock-free across live [`SystemView`]s.
    pub fn ship_remote_assistance(&self, target_ship_id: i64) -> Vec<ProjectedRemoteAssistance> {
        if let Some(handle) = self.find_ship_live_handle(target_ship_id) {
            return handle
                .view()
                .remote_assistance_for(target_ship_id)
                .map(<[ProjectedRemoteAssistance]>::to_vec)
                .unwrap_or_default();
        }
        for (_, handle) in self.active_system_handles() {
            if let Some(list) = handle.view().remote_assistance_for(target_ship_id) {
                return list.to_vec();
            }
        }
        Vec::new()
    }

    /// Returns `true` when any live system actor currently holds an in-flight superweapon guard.
    pub fn has_in_flight_superweapons(&self) -> bool {
        self.active_system_handles()
            .into_iter()
            .any(|(_, h)| h.view().has_in_flight_superweapons())
    }

    /// Clears transient in-space EWAR state across all currently loaded solar system actors.
    pub fn clear_all_ewar_state_sync(&self) {
        for (_, handle) in self.active_system_handles() {
            if !handle.view().ewar.is_empty() { let _ = handle.clear_ewar_state_sync(); }
        }
    }

    pub fn seed_ewar_source_for_test_sync(&self, sys_opt: Option<u32>, source_module_id: i64, source_ship_id: i64) {
        let handle_opt = sys_opt.and_then(|sid| self.get_handle(sid)).or_else(|| self.find_ship_live_handle(source_ship_id));
        if let Some(handle) = handle_opt { let _ = handle.record_projected_ewar_source_sync(source_module_id, source_ship_id); }
    }

    pub fn clear_target_ewar_for_test_sync(&self, target_ship_id: i64) {
        for (_, handle) in self.active_system_handles() {
            if handle.view().ewar.projected_ewar.contains_key(&target_ship_id) { let _ = handle.clear_projected_ewar_for_target_sync(target_ship_id); }
        }
    }

    pub fn ewar_starts_count(&self) -> usize {
        self.active_system_handles().into_iter().map(|(_, h)| h.view().ewar.active_ewar_starts.len()).sum()
    }

    /// Resolves effective `(tidi_factor, has_tidi_history)` for live or cold `solar_system_id`
    /// (`G1A-R3-P1-01`), respecting `#1196`'s `tidi_history_systems` distinction from pure `1.0x` anchors.
    pub(crate) fn resolve_effective_tidi_mode(
        &self,
        solar_system_id: SolarSystemId,
        db_read_safe: bool,
    ) -> (f64, bool) {
        if solar_system_id == 0 {
            return (1.0, false);
        }
        let live_tidi = self.get_handle(solar_system_id).map(|h| h.tidi_factor());
        let held_svc = held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_SERVICE_STATE);
        let (manual_override, cn_tidi_hist) = if held_svc {
            let cn = self
                .cluster_nodes
                .read_unranked()
                .unwrap_or_else(|e| e.into_inner());
            (
                cn.manual_tidi_overrides.get(&solar_system_id).copied(),
                cn.tidi_history_systems.contains(&solar_system_id),
            )
        } else {
            let cn = self.cluster_nodes.read().unwrap_or_else(|e| e.into_inner());
            (
                cn.manual_tidi_overrides.get(&solar_system_id).copied(),
                cn.tidi_history_systems.contains(&solar_system_id),
            )
        };
        let tidi = live_tidi
            .or(manual_override)
            .or_else(|| {
                if db_read_safe && !held_lock_ranks().iter().any(|&r| r >= RANK_WORLD_INNER) {
                    self.db.as_ref().and_then(|db| {
                        db.with_snapshot(|snap| {
                            let nid =
                                -(7_500_000_000_000_i64.saturating_add(solar_system_id as i64));
                            snap.owner_notes
                                .get(&nid)
                                .and_then(|n| serde_json::from_str::<f64>(&n.note).ok())
                        })
                    })
                } else {
                    None
                }
            })
            .filter(|v| v.is_finite())
            .unwrap_or(1.0)
            .clamp(0.10, 1.0);
        let gate_tidi_hist = self
            .get_existing_cooldown_persist_gate(solar_system_id)
            .is_some_and(|gate| gate.has_tidi_history());
        let has_tidi_history = tidi < 0.999_999 || cn_tidi_hist || gate_tidi_hist;
        (tidi, has_tidi_history)
    }

    pub(crate) fn transitioning_system_ids(
        &self,
        exclude_sys_opt: Option<SolarSystemId>,
    ) -> Vec<SolarSystemId> {
        let collect_transitioning =
            |guard: &HashMap<SolarSystemId, super::lifecycle_sm::SystemLifecycleSlot>| {
                guard
                    .iter()
                    .filter(|&(&id, slot)| {
                        Some(id) != exclude_sys_opt && slot.live_handle().is_none()
                    })
                    .map(|(&id, _)| id)
                    .collect::<Vec<_>>()
            };
        let mut ids: Vec<SolarSystemId> = if held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_SYSTEM_REGISTRY)
        {
            let guard = self.systems.read_unranked().unwrap_or_else(|e| e.into_inner());
            collect_transitioning(&guard)
        } else {
            let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
            collect_transitioning(&guard)
        };
        ids.sort_unstable();
        ids
    }
}

pub(in crate::system_actor) fn flush_post_unlock_actions_catching(
    actions: &Arc<RankedMutex<Vec<PostUnlockHookAction>, RANK_LEAF>>,
    solar_system_id: SolarSystemId,
    command_panics: &std::sync::atomic::AtomicU64,
) -> usize {
    if !held_lock_ranks().is_empty() {
        return 0;
    }
    let mut executed = 0usize;
    loop {
        let batch = {
            let mut guard = actions.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_empty() {
                break;
            }
            std::mem::take(&mut *guard)
        };
        for action in batch {
            if let Err(panic_payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action)) {
                let panic_count = command_panics.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                let msg = panic_payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| panic_payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("non-string panic payload");
                tracing::error!(
                    solar_system_id,
                    panic_count,
                    panic = %msg,
                    "SystemActor post_unlock_action panicked; continuing actor loop"
                );
            }
            executed += 1;
        }
    }
    executed
}

#[cfg(test)]
#[path = "ewar_cooldowns_tests.rs"]
mod tests;
