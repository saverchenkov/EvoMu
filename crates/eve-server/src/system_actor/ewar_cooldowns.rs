//! Per-system in-space EWAR projection/active-start state, deactivation batching,
//! command handling, and `SystemHandle` bridges owned by [`SystemActor`] (`#1541`).

#![forbid(unsafe_code)]

pub use super::module_cooldowns::*;
use super::celestials::BallparkHandle;
use super::{
    call_actor_result_with_deadline, defer_reply, dispatch_actor_sync_with_deadline,
    ActiveModuleCycle, ActorExecCtx, BallOpsCtx, CharacterId, ModuleDeactivationHook,
    ModuleDeactivationPostUnlockHook, PairedModuleDeactivationPostUnlockHook, PostUnlockHookAction,
    SystemActorState, SystemCommand, SystemHandle, SystemRegistry, SystemView, ACTOR_CMD_TIMEOUT,
};
use eve_db::lock_rank::{
    held_lock_ranks, RankedMutex, RankedRwLock, RANK_ACTOR_STATE, RANK_LEAF, RANK_WORLD_INNER,
};
use eve_sde::SolarSystemId;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActorEwarState {
    pub projected_ewar: HashMap<i64, Vec<eve_dogma::ProjectedEwarEffect>>,
    pub projected_ewar_sources: HashMap<i64, i64>,
    pub active_ewar_starts: HashMap<(i64, i64), Vec<(i64, &'static str)>>,
}

impl ActorEwarState {
    pub fn is_empty(&self) -> bool {
        self.projected_ewar.is_empty()
            && self.projected_ewar_sources.is_empty()
            && self.active_ewar_starts.is_empty()
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
    ModuleEwarFlushSnapshot {
        started_targets,
        projected_targets,
        affected_ships,
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

fn resolve_removed_cycle_char_id(
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
    let effective_post = post_hook.and_then(|(paired_raw, post)| {
        raw_hook
            .as_ref()
            .is_some_and(|raw| Arc::ptr_eq(raw, &paired_raw))
            .then_some(post)
    });
    (raw_hook, effective_post)
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
    ballpark_shared: Option<&Arc<BallparkHandle>>,
    actor_state_shared: Option<&Arc<RankedRwLock<SystemActorState, RANK_ACTOR_STATE>>>,
    tidi_factor: f64,
) -> bool {
    if tidi_factor < 0.999_999 {
        cooldown_persist_gate.mark_tidi_history();
        actor_st.has_tidi_history = true;
    } else if cooldown_persist_gate.has_tidi_history() {
        actor_st.has_tidi_history = true;
    }
    let has_tidi_history = actor_st.has_tidi_history;
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
                    || !ewar_snap.affected_ships.is_empty();
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
                        ballpark_weak: ballpark_shared.map(Arc::downgrade).unwrap_or_default(),
                        actor_state_weak: actor_state_shared.map(Arc::downgrade).unwrap_or_default(),
                    }),
                ));
            }
            other => next_intents.push(other),
        }
    }
    ops.intents = next_intents;
    mutated_actor
}

#[allow(clippy::too_many_arguments)]
pub(in crate::system_actor) fn build_and_finalize_removed_ball_ops(
    handle: &SystemHandle,
    st: &mut SystemActorState,
    ball_id: eve_destiny::BallId,
    removed_cycles: Vec<(ActiveModuleCycle, Option<CharacterId>)>,
    fallback_slim_owner: Option<CharacterId>,
    raw_hook: Option<ModuleDeactivationHook>,
    post_hook: Option<PairedModuleDeactivationPostUnlockHook>,
    tidi_factor: f64,
) -> super::post_unlock::PostUnlockOps {
    let include_deact = raw_hook.is_some() || post_hook.is_some();
    if st.db_opt.is_none() && !include_deact {
        return super::post_unlock::PostUnlockOps::default();
    }
    let mut ops = super::post_unlock::build_removed_ball_post_unlock_ops(
        handle.solar_system_id,
        ball_id,
        removed_cycles,
        fallback_slim_owner,
        include_deact,
    );
    let _ = finalize_post_unlock_ops_deactivations_in_actor(
        handle.solar_system_id,
        st,
        &mut ops,
        raw_hook,
        post_hook,
        &handle.cooldown_persist_gate,
        Some(&handle.ballpark),
        Some(&handle.actor_state),
        tidi_factor,
    );
    ops
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
        SystemCommand::PruneExpiredModuleCooldowns {
            now_unix_f64,
            tidi_factor,
            has_tidi_history,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = prune_expired_module_cooldowns_in_actor(
                &mut ctx,
                now_unix_f64,
                tidi_factor,
                has_tidi_history,
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::SnapshotModuleCooldownCheckpoint {
            only_if_dirty,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = snapshot_module_cooldown_checkpoint_in_actor(&mut ctx, only_if_dirty);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::RecordModuleReloadSimDeadlines {
            module_ids,
            sim_start_s,
            sim_deadline_s,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = record_module_reload_sim_deadlines_in_actor(
                &mut ctx,
                module_ids,
                sim_start_s,
                sim_deadline_s,
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ClearModuleReloadSimDeadline { module_id, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = clear_module_reload_sim_deadline_in_actor(&mut ctx, module_id);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        other => Some(other),
    }
}

// ---------------------------------------------------------------------------
// Lock-free SystemView readers
// ---------------------------------------------------------------------------

impl SystemView {
    pub fn projected_ewar_for(&self, target_ship_id: i64) -> Option<&[eve_dogma::ProjectedEwarEffect]> {
        self.ewar.projected_ewar.get(&target_ship_id).map(Vec::as_slice)
    }

    pub fn projected_ewar_source(&self, source_module_id: i64) -> Option<i64> {
        self.ewar.projected_ewar_sources.get(&source_module_id).copied()
    }

    pub fn active_ewar_starts_for(&self, ship_id: i64, module_id: i64) -> Option<&[(i64, &'static str)]> {
        self.ewar
            .active_ewar_starts
            .get(&(ship_id, module_id))
            .map(Vec::as_slice)
    }

    pub fn module_reactivation_cooldown(&self, key: &(i64, i64, u32)) -> Option<ModuleCooldownEntry> {
        self.module_cooldowns.reactivation_cooldowns.get(key).copied()
    }

    pub fn module_reload_sim_deadline(&self, module_id: i64) -> Option<ModuleReloadSimDeadline> {
        self.module_cooldowns.reload_sim_deadlines.get(&module_id).copied()
    }
}

// ---------------------------------------------------------------------------
// SystemHandle async / sync / post-unlock APIs
// ---------------------------------------------------------------------------

impl SystemHandle {
    /// Reads the latest projected EWAR effects targeting `target_ship_id` lock-free from [`SystemView`].
    pub fn projected_ewar_for(&self, target_ship_id: i64) -> Vec<eve_dogma::ProjectedEwarEffect> {
        self.view()
            .ewar
            .projected_ewar
            .get(&target_ship_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Reads the published `ActorEwarState` snapshot lock-free from [`SystemView`].
    pub fn ewar_state_snapshot(&self) -> ActorEwarState {
        self.view().ewar.clone()
    }

    /// Reads the published module reactivation cooldown map lock-free from [`SystemView`].
    pub fn module_cooldowns_snapshot(&self) -> HashMap<(i64, i64, u32), ModuleCooldownEntry> {
        self.view().module_cooldowns.reactivation_cooldowns.clone()
    }

    pub fn module_reactivation_cooldown(&self, key: &(i64, i64, u32)) -> Option<ModuleCooldownEntry> {
        self.view().module_reactivation_cooldown(key)
    }

    /// Upserts projected EWAR onto `target_ship_id` inside this solar system actor (`#1541`).
    pub async fn upsert_projected_ewar(
        &self,
        source_ship_id: i64,
        target_ship_id: i64,
        effect: eve_dogma::ProjectedEwarEffect,
    ) -> Result<(), String> {
        call_actor_result_with_deadline(self, "UpsertProjectedEwar", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::UpsertProjectedEwar {
                source_ship_id,
                target_ship_id,
                effect,
                reply,
            }
        })
        .await
    }

    /// Synchronous bridge for [`Self::upsert_projected_ewar`].
    pub fn upsert_projected_ewar_sync(
        &self,
        source_ship_id: i64,
        target_ship_id: i64,
        effect: eve_dogma::ProjectedEwarEffect,
    ) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "UpsertProjectedEwar",
            ACTOR_CMD_TIMEOUT,
            (source_ship_id, target_ship_id, effect),
            |(source_ship_id, target_ship_id, effect), reply| {
                SystemCommand::UpsertProjectedEwar {
                    source_ship_id,
                    target_ship_id,
                    effect,
                    reply,
                }
            },
            |ctx, (source_ship_id, target_ship_id, effect)| {
                upsert_projected_ewar_in_state(
                    &mut ctx.actor_st.ewar_state,
                    source_ship_id,
                    target_ship_id,
                    effect,
                );
                Ok(())
            },
        )
    }

    pub fn record_projected_ewar_source_sync(
        &self,
        source_module_id: i64,
        source_ship_id: i64,
    ) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "RecordProjectedEwarSource",
            ACTOR_CMD_TIMEOUT,
            (source_module_id, source_ship_id),
            |(source_module_id, source_ship_id), reply| {
                SystemCommand::RecordProjectedEwarSource {
                    source_module_id,
                    source_ship_id,
                    reply,
                }
            },
            |ctx, (source_module_id, source_ship_id)| {
                ctx.actor_st
                    .ewar_state
                    .projected_ewar_sources
                    .insert(source_module_id, source_ship_id);
                Ok(())
            },
        )
    }

    pub async fn remove_projected_ewar_by_module(
        &self,
        source_ship_id: i64,
        source_module_id: i64,
        allow_db_source_fallback: bool,
    ) -> Result<Vec<i64>, String> {
        call_actor_result_with_deadline(
            self,
            "RemoveProjectedEwarByModule",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::RemoveProjectedEwarByModule {
                source_ship_id,
                source_module_id,
                allow_db_source_fallback,
                reply,
            },
        )
        .await
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

    pub fn flush_module_ewar_sync(
        &self,
        source_ship_id: i64,
        source_module_id: i64,
        allow_db_source_fallback: bool,
    ) -> Result<ModuleEwarFlushSnapshot, String> {
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

    pub(crate) fn enqueue_post_unlock_action(&self, action: PostUnlockHookAction) {
        self.post_unlock_actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(action);
    }

    pub(in crate::system_actor) fn persist_cooldown_payload(&self, mut payload: ModuleCooldownPersistPayload) -> bool {
        payload.tidi_factor = self.tidi_factor();
        if payload.tidi_factor < 0.999_999 || self.cooldown_persist_gate.has_tidi_history() {
            payload.has_tidi_history = true;
        }
        persist_cooldown_payload_with_gate(
            self.solar_system_id, &self.cooldown_persist_gate, Some(&self.ballpark), Some(&self.actor_state), payload,
        )
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
        &self,
        cooldowns: HashMap<(i64, i64, u32), ModuleCooldownEntry>,
        expected_gate_token: Option<(u64, Option<(u64, u64)>)>,
    ) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "ReplaceModuleCooldowns",
            ACTOR_CMD_TIMEOUT,
            (cooldowns, expected_gate_token),
            |(cooldowns, expected_gate_token), reply| SystemCommand::ReplaceModuleCooldowns {
                cooldowns,
                expected_gate_token,
                reply,
            },
            |ctx, (cooldowns, expected_gate_token)| {
                replace_module_cooldowns_in_actor(ctx, cooldowns, expected_gate_token)
            },
        )
    }

    pub fn replace_module_cooldowns_sync(
        &self,
        cooldowns: HashMap<(i64, i64, u32), ModuleCooldownEntry>,
    ) -> Result<(), String> {
        self.replace_module_cooldowns_checked_sync(cooldowns, None)
    }

    pub fn prune_expired_module_cooldowns_sync(
        &self,
        now_unix_f64: f64,
        tidi_factor: f64,
        has_tidi_history: bool,
    ) -> Result<bool, String> {
        let payload_opt = dispatch_actor_sync_with_deadline(
            self,
            "PruneExpiredModuleCooldowns",
            ACTOR_CMD_TIMEOUT,
            (now_unix_f64, tidi_factor, has_tidi_history),
            |(now_unix_f64, tidi_factor, has_tidi_history), reply| {
                SystemCommand::PruneExpiredModuleCooldowns {
                    now_unix_f64,
                    tidi_factor,
                    has_tidi_history,
                    reply,
                }
            },
            |ctx, (now_unix_f64, tidi_factor, has_tidi_history)| {
                prune_expired_module_cooldowns_in_actor(
                    ctx,
                    now_unix_f64,
                    tidi_factor,
                    has_tidi_history,
                )
            },
        )?;
        let pruned = payload_opt.is_some();
        if let Some(payload) = payload_opt {
            self.persist_cooldown_payload(payload);
        }
        Ok(pruned)
    }

    /// Flushes this system actor's module cooldowns to per-system SQLite checkpoints
    /// post-unlock (`#1541`). Called on eviction, runtime sync, and checkpoint flushes.
    pub async fn flush_module_cooldown_checkpoint(&self) -> Result<usize, String> {
        let payload_opt = call_actor_result_with_deadline(
            self,
            "SnapshotModuleCooldownCheckpoint",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::SnapshotModuleCooldownCheckpoint {
                only_if_dirty: true,
                reply,
            },
        )
        .await?;
        let Some(payload) = payload_opt else {
            return Ok(0);
        };
        let count = payload.entries.len();
        self.persist_cooldown_payload_async(payload).await;
        Ok(count)
    }

    pub fn flush_module_cooldown_checkpoint_sync(&self) -> Result<usize, String> {
        let payload_opt = dispatch_actor_sync_with_deadline(
            self,
            "SnapshotModuleCooldownCheckpoint",
            ACTOR_CMD_TIMEOUT,
            false,
            |only_if_dirty, reply| SystemCommand::SnapshotModuleCooldownCheckpoint {
                only_if_dirty,
                reply,
            },
            snapshot_module_cooldown_checkpoint_in_actor,
        )?;
        let Some(payload) = payload_opt else {
            return Ok(0);
        };
        let count = payload.entries.len();
        self.persist_cooldown_payload(payload);
        Ok(count)
    }

    pub fn record_module_reload_sim_deadlines_sync(
        &self,
        module_ids: Vec<i64>,
        sim_start_s: f64,
        sim_deadline_s: f64,
    ) -> Result<(), String> {
        if module_ids.is_empty() {
            return Ok(());
        }
        dispatch_actor_sync_with_deadline(
            self,
            "RecordModuleReloadSimDeadlines",
            ACTOR_CMD_TIMEOUT,
            (module_ids, sim_start_s, sim_deadline_s),
            |(module_ids, sim_start_s, sim_deadline_s), reply| {
                SystemCommand::RecordModuleReloadSimDeadlines {
                    module_ids,
                    sim_start_s,
                    sim_deadline_s,
                    reply,
                }
            },
            |ctx, (module_ids, sim_start_s, sim_deadline_s)| {
                record_module_reload_sim_deadlines_in_actor(
                    ctx,
                    module_ids,
                    sim_start_s,
                    sim_deadline_s,
                )
            },
        )
    }

    pub fn clear_module_reload_sim_deadline_sync(&self, module_id: i64) -> Result<bool, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "ClearModuleReloadSimDeadline",
            ACTOR_CMD_TIMEOUT,
            module_id,
            |module_id, reply| SystemCommand::ClearModuleReloadSimDeadline { module_id, reply },
            clear_module_reload_sim_deadline_in_actor,
        )
    }
}

// ---------------------------------------------------------------------------
// SystemRegistry EWAR facades
// ---------------------------------------------------------------------------

impl SystemRegistry {
    /// Finds a currently loaded system handle whose [`SystemView`] contains ball `ship_id` lock-free.
    pub fn find_ship_live_handle(&self, ship_id: i64) -> Option<SystemHandle> {
        self.active_system_handles()
            .into_iter()
            .find(|(_, handle)| handle.view().balls.contains_key(&ship_id))
            .map(|(_, handle)| handle)
    }

    /// Reads projected EWAR targeting `target_ship_id` lock-free across live [`SystemView`]s.
    pub fn ship_ewar_projections(&self, target_ship_id: i64) -> Vec<eve_dogma::ProjectedEwarEffect> {
        if let Some(handle) = self.find_ship_live_handle(target_ship_id) {
            return handle.projected_ewar_for(target_ship_id);
        }
        for (_, handle) in self.active_system_handles() {
            let view = handle.view();
            if let Some(list) = view.ewar.projected_ewar.get(&target_ship_id) {
                return list.clone();
            }
        }
        Vec::new()
    }

    /// Clears transient in-space EWAR state across all currently loaded solar system actors.
    pub fn clear_all_ewar_state_sync(&self) {
        for (_, handle) in self.active_system_handles() {
            if !handle.view().ewar.is_empty() {
                let _ = handle.clear_ewar_state_sync();
            }
        }
    }

    pub fn seed_ewar_source_for_test_sync(
        &self,
        sys_opt: Option<u32>,
        source_module_id: i64,
        source_ship_id: i64,
    ) {
        let handle_opt = sys_opt
            .and_then(|sid| self.get_handle(sid))
            .or_else(|| self.find_ship_live_handle(source_ship_id));
        if let Some(handle) = handle_opt {
            let _ = handle.record_projected_ewar_source_sync(source_module_id, source_ship_id);
        }
    }

    pub fn clear_target_ewar_for_test_sync(&self, target_ship_id: i64) {
        for (_, handle) in self.active_system_handles() {
            if handle
                .view()
                .ewar
                .projected_ewar
                .contains_key(&target_ship_id)
            {
                let _ = handle.clear_projected_ewar_for_target_sync(target_ship_id);
            }
        }
    }

    pub fn ewar_starts_count(&self) -> usize {
        self.active_system_handles()
            .into_iter()
            .map(|(_, h)| h.view().ewar.active_ewar_starts.len())
            .sum()
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

pub(crate) fn flush_post_unlock_actions(
    actions: &Arc<RankedMutex<Vec<PostUnlockHookAction>, RANK_LEAF>>,
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
            action();
            executed += 1;
        }
    }
    executed
}

#[cfg(test)]
#[path = "ewar_cooldowns_tests.rs"]
mod tests;
