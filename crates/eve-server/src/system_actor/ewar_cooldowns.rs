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



pub(in crate::system_actor) fn read_effective_deactivation_hooks(
    raw_lock: &Arc<RankedRwLock<Option<ModuleDeactivationHook>, RANK_LEAF>>,
    post_lock: &Arc<RankedRwLock<Option<PairedModuleDeactivationPostUnlockHook>, RANK_LEAF>>,
) -> (
    Option<ModuleDeactivationHook>,
    Option<ModuleDeactivationPostUnlockHook>,
) {
    fn read_opt<T: Clone>(lock: &Arc<RankedRwLock<Option<T>, RANK_LEAF>>) -> Option<T> {
        if held_lock_ranks().iter().any(|&r| r >= RANK_LEAF) {
            lock.read_unranked().unwrap_or_else(|e| e.into_inner()).clone()
        } else {
            lock.read().unwrap_or_else(|e| e.into_inner()).clone()
        }
    }
    resolve_effective_deactivation_hooks(read_opt(raw_lock), read_opt(post_lock))
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

pub(in crate::system_actor) fn finalize_removed_ball_deactivations_in_actor(
    handle: &SystemHandle,
    st: &mut SystemActorState,
    tidi_cur: f64,
    post_hook_opt: Option<&ModuleDeactivationPostUnlockHook>,
    removed_cycles: &[(ActiveModuleCycle, Option<CharacterId>)],
    fallback_slim_owner: Option<CharacterId>,
) -> bool {
    let Some(post_hook) = post_hook_opt else {
        return false;
    };
    if removed_cycles.is_empty() {
        return false;
    }
    let rank_blocks_snapshot = held_lock_ranks().iter().any(|&r| r >= RANK_WORLD_INNER);
    let mut collected = Vec::with_capacity(removed_cycles.len());
    for (removed, char_id_opt) in removed_cycles {
        if removed.module_id < 0
            || super::cycles_tackle::parse_minion_cycle_parent(&removed.effect_name).is_some()
        {
            continue;
        }
        if let Some(cid) = resolve_removed_cycle_char_id(
            *char_id_opt,
            removed.ship_id,
            st.db_opt.as_deref(),
            fallback_slim_owner,
            rank_blocks_snapshot,
        ) {
            collected.push((handle.solar_system_id, cid, removed.clone()));
        }
    }
    if collected.is_empty() {
        return false;
    }
    let has_tidi = st.has_tidi_history || tidi_cur < 0.999_999;
    finalize_collected_deactivations_in_actor(
        handle.solar_system_id,
        st,
        &handle.post_unlock_actions,
        &handle.cooldown_persist_gate,
        Some(&handle.ballpark),
        Some(&handle.actor_state),
        tidi_cur,
        has_tidi,
        None,
        Some(post_hook),
        collected,
    );
    true
}

pub(in crate::system_actor) fn finish_removed_ball_deactivation_hooks(
    handle: &SystemHandle,
    db_opt: Option<&eve_db::WorldRepository>,
    post_unlock_enqueued: bool,
    deact_guard: Option<&ModuleDeactivationHook>,
    removed_cycles: &[(ActiveModuleCycle, Option<CharacterId>)],
    fallback_slim_owner: Option<CharacterId>,
) {
    if post_unlock_enqueued {
        if held_lock_ranks().is_empty() {
            super::flush_post_unlock_actions(&handle.post_unlock_actions);
        }
        return;
    }
    let (Some(hook), Some(db_ref)) = (deact_guard, db_opt) else {
        return;
    };
    if db_ref.is_write_locked_by_current_thread()
        || held_lock_ranks().iter().any(|&r| r >= RANK_WORLD_INNER)
    {
        return;
    }
    for (removed, char_id_opt) in removed_cycles {
        if let Some(cid) = resolve_removed_cycle_char_id(
            *char_id_opt,
            removed.ship_id,
            Some(db_ref),
            fallback_slim_owner,
            false,
        ) {
            hook(handle.solar_system_id, cid, removed);
        }
    }
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

/// Collects cycle deactivations emitted while `ballpark`/`actor_state` are locked, mutates
/// actor-owned EWAR & cloak cooldown state synchronously BEFORE unlocking, and queues
/// out-of-lock DB/notification/fit-projection actions onto the owning actor's
/// per-actor `post_unlock_actions` queue (`#1541`).
pub(in crate::system_actor) struct ActorDeactivationBatch {
    collected: Arc<RankedMutex<Vec<(SolarSystemId, CharacterId, ActiveModuleCycle)>, RANK_LEAF>>,
    collector_hook: Option<ModuleDeactivationHook>,
    raw_hook: Option<ModuleDeactivationHook>,
    post_unlock_hook: Option<ModuleDeactivationPostUnlockHook>,
}

impl ActorDeactivationBatch {
    pub(in crate::system_actor) fn new(
        raw_hook: Option<ModuleDeactivationHook>,
        post_unlock_hook: Option<PairedModuleDeactivationPostUnlockHook>,
    ) -> Self {
        let (raw_hook, post_unlock_hook) =
            resolve_effective_deactivation_hooks(raw_hook, post_unlock_hook);
        let collected = Arc::new(RankedMutex::new(Vec::new()));
        let collector_hook = if raw_hook.is_some() || post_unlock_hook.is_some() {
            let sink = Arc::clone(&collected);
            Some(Arc::new(
                move |sys_id: SolarSystemId, cid: CharacterId, cycle: &ActiveModuleCycle| {
                    sink.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((sys_id, cid, cycle.clone()));
                },
            ) as ModuleDeactivationHook)
        } else {
            None
        };
        Self {
            collected,
            collector_hook,
            raw_hook,
            post_unlock_hook,
        }
    }

    pub(in crate::system_actor) fn collector_hook(&self) -> Option<&ModuleDeactivationHook> {
        self.collector_hook.as_ref()
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::system_actor) fn finalize_in_actor(
        &self,
        solar_system_id: SolarSystemId,
        actor_st: &mut SystemActorState,
        post_unlock_actions: &Arc<RankedMutex<Vec<PostUnlockHookAction>, RANK_LEAF>>,
        cooldown_persist_gate: &Arc<CooldownPersistGate>,
        ballpark_shared: Option<&Arc<BallparkHandle>>,
        actor_state_shared: Option<&Arc<RankedRwLock<SystemActorState, RANK_ACTOR_STATE>>>,
        tidi_factor: f64,
        has_tidi_history: bool,
    ) -> bool {
        let items = {
            let mut guard = self.collected.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_empty() {
                return false;
            }
            std::mem::take(&mut *guard)
        };
        finalize_collected_deactivations_in_actor(
            solar_system_id,
            actor_st,
            post_unlock_actions,
            cooldown_persist_gate,
            ballpark_shared,
            actor_state_shared,
            tidi_factor,
            has_tidi_history,
            self.raw_hook.as_ref(),
            self.post_unlock_hook.as_ref(),
            items,
        )
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

#[allow(clippy::too_many_arguments)]
pub(in crate::system_actor) fn finalize_collected_deactivations_in_actor(
    solar_system_id: SolarSystemId,
    actor_st: &mut SystemActorState,
    post_unlock_actions: &Arc<RankedMutex<Vec<PostUnlockHookAction>, RANK_LEAF>>,
    cooldown_persist_gate: &Arc<CooldownPersistGate>,
    ballpark_shared: Option<&Arc<BallparkHandle>>,
    actor_state_shared: Option<&Arc<RankedRwLock<SystemActorState, RANK_ACTOR_STATE>>>,
    tidi_factor: f64,
    has_tidi_history: bool,
    raw_hook: Option<&ModuleDeactivationHook>,
    post_unlock_hook: Option<&ModuleDeactivationPostUnlockHook>,
    items: Vec<(SolarSystemId, CharacterId, ActiveModuleCycle)>,
) -> bool {
    if items.is_empty() {
        return false;
    }
    let mut mutated_actor = false;
    if let Some(post_hook) = post_unlock_hook {
        let now_unix_f64 = actor_st
            .db_opt
            .as_ref()
            .map(|db| db.clock().now_unix_secs() as f64)
            .unwrap_or(0.0);
        let rank_blocks_snapshot = held_lock_ranks()
            .iter()
            .any(|&r| r >= RANK_WORLD_INNER);
        let mut actions_to_push: Vec<PostUnlockHookAction> = Vec::with_capacity(items.len());
        for (deact_sys_id, char_id, cycle) in items {
            let allow_db_fallback = !rank_blocks_snapshot
                && actor_st.db_opt.as_ref().is_some_and(|db| {
                    db.with_snapshot(|s| {
                        s.items
                            .get(&cycle.module_id)
                            .is_some_and(|i| i.location_id == cycle.ship_id)
                    })
                });
            let ewar_snap = flush_module_ewar_in_state(
                &mut actor_st.ewar_state,
                cycle.ship_id,
                cycle.module_id,
                allow_db_fallback,
            );
            if !ewar_snap.started_targets.is_empty()
                || !ewar_snap.projected_targets.is_empty()
                || !ewar_snap.affected_ships.is_empty()
            {
                mutated_actor = true;
            }
            let payload_opt = if deact_sys_id > 0 && deact_sys_id == solar_system_id {
                if let Some(delay_s) = cloak_reactivation_delay_for_cycle(&actor_st.sde_opt, &cycle)
                {
                    mutated_actor = true;
                    record_module_cooldown_in_actor_state(
                        solar_system_id,
                        actor_st,
                        cycle.ship_id,
                        cycle.module_id,
                        cycle.module_type_id,
                        now_unix_f64,
                        delay_s,
                        tidi_factor,
                        has_tidi_history,
                    )
                } else {
                    None
                }
            } else {
                None
            };
            let post_hook_c = Arc::clone(post_hook);
            let gate_c = Arc::clone(cooldown_persist_gate);
            let ballpark_w = ballpark_shared.map(Arc::downgrade);
            let actor_state_w = actor_state_shared.map(Arc::downgrade);
            actions_to_push.push(Box::new(move || {
                if let Some(payload) = payload_opt {
                    if let Ok(rt) = tokio::runtime::Handle::try_current() {
                        let _ = rt.spawn_blocking(move || {
                            let bp_up = ballpark_w.and_then(|w| w.upgrade());
                            let st_up = actor_state_w.and_then(|w| w.upgrade());
                            let _ = persist_cooldown_payload_with_gate(
                                deact_sys_id,
                                &gate_c,
                                bp_up.as_deref(),
                                st_up.as_deref(),
                                payload,
                            );
                        });
                    } else {
                        let bp_up = ballpark_w.and_then(|w| w.upgrade());
                        let st_up = actor_state_w.and_then(|w| w.upgrade());
                        let _ = persist_cooldown_payload_with_gate(
                            deact_sys_id,
                            &gate_c,
                            bp_up.as_deref(),
                            st_up.as_deref(),
                            payload,
                        );
                    }
                }
                post_hook_c(deact_sys_id, char_id, cycle, ewar_snap);
            }));
        }
        post_unlock_actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(actions_to_push);
    } else if let Some(hook) = raw_hook {
        let mut actions_to_push: Vec<PostUnlockHookAction> = Vec::with_capacity(items.len());
        for (deact_sys_id, char_id, cycle) in items {
            let hook_c = Arc::clone(hook);
            actions_to_push.push(Box::new(move || {
                hook_c(deact_sys_id, char_id, &cycle);
            }));
        }
        post_unlock_actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(actions_to_push);
    }
    mutated_actor
}



pub(in crate::system_actor) fn handle_ewar_and_cooldown_command(
    exec_ctx: &mut ActorExecCtx<'_>,
    cmd: SystemCommand,
) -> Option<SystemCommand> {
    match cmd {
        SystemCommand::UpsertProjectedEwar {
            source_ship_id,
            target_ship_id,
            effect,
            reply,
        } => {
            upsert_projected_ewar_in_state(
                &mut exec_ctx.actor_st.ewar_state,
                source_ship_id,
                target_ship_id,
                effect,
            );
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::RecordProjectedEwarSource {
            source_module_id,
            source_ship_id,
            reply,
        } => {
            exec_ctx
                .actor_st
                .ewar_state
                .projected_ewar_sources
                .insert(source_module_id, source_ship_id);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::RemoveProjectedEwarByModule {
            source_ship_id,
            source_module_id,
            allow_db_source_fallback,
            reply,
        } => {
            let affected = remove_projected_ewar_by_module_in_state(
                &mut exec_ctx.actor_st.ewar_state,
                source_ship_id,
                source_module_id,
                allow_db_source_fallback,
            );
            defer_reply(exec_ctx.deferred_replies, reply, Ok(affected));
            None
        }
        SystemCommand::ClearProjectedEwarForTarget {
            target_ship_id,
            reply,
        } => {
            exec_ctx
                .actor_st
                .ewar_state
                .projected_ewar
                .remove(&target_ship_id);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::RecordActiveEwarStart {
            source_ship_id,
            module_id,
            target_ship_id,
            jamming_type,
            reply,
        } => {
            exec_ctx
                .actor_st
                .ewar_state
                .active_ewar_starts
                .entry((source_ship_id, module_id))
                .or_default()
                .push((target_ship_id, jamming_type));
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::ReplaceActiveEwarStarts {
            source_ship_id,
            module_id,
            targets,
            reply,
        } => {
            if targets.is_empty() {
                exec_ctx
                    .actor_st
                    .ewar_state
                    .active_ewar_starts
                    .remove(&(source_ship_id, module_id));
            } else {
                exec_ctx
                    .actor_st
                    .ewar_state
                    .active_ewar_starts
                    .insert((source_ship_id, module_id), targets);
            }
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::FlushModuleEwar {
            source_ship_id,
            source_module_id,
            allow_db_source_fallback,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = flush_module_ewar_in_actor(
                &mut ctx,
                source_ship_id,
                source_module_id,
                allow_db_source_fallback,
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ClearEwarState { reply } => {
            exec_ctx.actor_st.ewar_state = ActorEwarState::default();
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::RecordModuleCooldown {
            ship_id,
            module_id,
            module_type_id,
            now_unix_f64,
            delay_s,
            tidi_factor,
            has_tidi_history,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = record_module_cooldown_in_actor(
                &mut ctx,
                RecordModuleCooldownReq {
                    ship_id,
                    module_id,
                    module_type_id,
                    now_unix_f64,
                    delay_s,
                    tidi_factor,
                    has_tidi_history,
                },
            );
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
        SystemCommand::ReplaceModuleCooldowns { cooldowns, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = replace_module_cooldowns_in_actor(&mut ctx, cooldowns);
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
            self,
            "RecordActiveEwarStart",
            ACTOR_CMD_TIMEOUT,
            (source_ship_id, module_id, target_ship_id, jamming_type),
            |(source_ship_id, module_id, target_ship_id, jamming_type), reply| {
                SystemCommand::RecordActiveEwarStart {
                    source_ship_id,
                    module_id,
                    target_ship_id,
                    jamming_type,
                    reply,
                }
            },
            |ctx, (source_ship_id, module_id, target_ship_id, jamming_type)| {
                ctx.actor_st
                    .ewar_state
                    .active_ewar_starts
                    .entry((source_ship_id, module_id))
                    .or_default()
                    .push((target_ship_id, jamming_type));
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
            self,
            "ReplaceActiveEwarStarts",
            ACTOR_CMD_TIMEOUT,
            (source_ship_id, module_id, targets),
            |(source_ship_id, module_id, targets), reply| SystemCommand::ReplaceActiveEwarStarts {
                source_ship_id,
                module_id,
                targets,
                reply,
            },
            |ctx, (source_ship_id, module_id, targets)| {
                if targets.is_empty() {
                    ctx.actor_st
                        .ewar_state
                        .active_ewar_starts
                        .remove(&(source_ship_id, module_id));
                } else {
                    ctx.actor_st
                        .ewar_state
                        .active_ewar_starts
                        .insert((source_ship_id, module_id), targets);
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
            SystemCommand::FlushModuleEwar {
                source_ship_id,
                source_module_id,
                allow_db_source_fallback,
                reply,
            }
        })
        .await
    }

    pub fn flush_module_ewar_sync(
        &self,
        source_ship_id: i64,
        source_module_id: i64,
        allow_db_source_fallback: bool,
    ) -> Result<ModuleEwarFlushSnapshot, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "FlushModuleEwar",
            ACTOR_CMD_TIMEOUT,
            (source_ship_id, source_module_id, allow_db_source_fallback),
            |(source_ship_id, source_module_id, allow_db_source_fallback), reply| {
                SystemCommand::FlushModuleEwar {
                    source_ship_id,
                    source_module_id,
                    allow_db_source_fallback,
                    reply,
                }
            },
            |ctx, (source_ship_id, source_module_id, allow_db_source_fallback)| {
                flush_module_ewar_in_actor(
                    ctx,
                    source_ship_id,
                    source_module_id,
                    allow_db_source_fallback,
                )
            },
        )
    }

    pub async fn clear_ewar_state(&self) -> Result<(), String> {
        call_actor_result_with_deadline(self, "ClearEwarState", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::ClearEwarState { reply }
        })
        .await
    }

    pub fn clear_ewar_state_sync(&self) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "ClearEwarState",
            ACTOR_CMD_TIMEOUT,
            (),
            |(), reply| SystemCommand::ClearEwarState { reply },
            |ctx, ()| {
                ctx.actor_st.ewar_state = ActorEwarState::default();
                Ok(())
            },
        )
    }

    pub(crate) fn enqueue_post_unlock_action(&self, action: PostUnlockHookAction) {
        self.post_unlock_actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(action);
    }

    fn persist_cooldown_payload(&self, mut payload: ModuleCooldownPersistPayload) -> bool {
        payload.tidi_factor = self.tidi_factor();
        if payload.tidi_factor < 0.999_999 {
            payload.has_tidi_history = true;
        }
        persist_cooldown_payload_with_gate(
            self.solar_system_id,
            &self.cooldown_persist_gate,
            Some(&self.ballpark),
            Some(&self.actor_state),
            payload,
        )
    }

    async fn persist_cooldown_payload_async(&self, payload: ModuleCooldownPersistPayload) -> bool {
        let h = self.clone();
        tokio::task::spawn_blocking(move || h.persist_cooldown_payload(payload))
            .await
            .unwrap_or(false)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_module_cooldown(
        &self,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        now_unix_f64: f64,
        delay_s: f64,
        tidi_factor: f64,
        has_tidi_history: bool,
    ) -> Result<(), String> {
        let payload = call_actor_result_with_deadline(
            self,
            "RecordModuleCooldown",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::RecordModuleCooldown {
                ship_id,
                module_id,
                module_type_id,
                now_unix_f64,
                delay_s,
                tidi_factor,
                has_tidi_history,
                reply,
            },
        )
        .await?;
        self.persist_cooldown_payload_async(payload).await;
        Ok(())
    }

    pub async fn remove_module_cooldown(&self, key: (i64, i64, u32)) -> Result<bool, String> {
        let payload_opt = call_actor_result_with_deadline(
            self,
            "RemoveModuleCooldown",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::RemoveModuleCooldown { key, reply },
        )
        .await?;
        let removed = payload_opt.is_some();
        if let Some(payload) = payload_opt {
            self.persist_cooldown_payload_async(payload).await;
        }
        Ok(removed)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_module_cooldown_sync(
        &self,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        now_unix_f64: f64,
        delay_s: f64,
        tidi_factor: f64,
        has_tidi_history: bool,
    ) -> Result<(), String> {
        let payload = dispatch_actor_sync_with_deadline(
            self,
            "RecordModuleCooldown",
            ACTOR_CMD_TIMEOUT,
            RecordModuleCooldownReq {
                ship_id,
                module_id,
                module_type_id,
                now_unix_f64,
                delay_s,
                tidi_factor,
                has_tidi_history,
            },
            |req, reply| SystemCommand::RecordModuleCooldown {
                ship_id: req.ship_id,
                module_id: req.module_id,
                module_type_id: req.module_type_id,
                now_unix_f64: req.now_unix_f64,
                delay_s: req.delay_s,
                tidi_factor: req.tidi_factor,
                has_tidi_history: req.has_tidi_history,
                reply,
            },
            record_module_cooldown_in_actor,
        )?;
        self.persist_cooldown_payload(payload);
        Ok(())
    }

    pub fn seed_module_cooldown_entry_sync(
        &self,
        key: (i64, i64, u32),
        entry: ModuleCooldownEntry,
    ) -> Result<(), String> {
        let payload = dispatch_actor_sync_with_deadline(
            self,
            "SeedModuleCooldownEntry",
            ACTOR_CMD_TIMEOUT,
            (key, entry),
            |(key, entry), reply| SystemCommand::SeedModuleCooldownEntry { key, entry, reply },
            |ctx, (key, entry)| seed_module_cooldown_in_actor(ctx, key, entry),
        )?;
        self.persist_cooldown_payload(payload);
        Ok(())
    }

    pub fn remove_module_cooldown_sync(&self, key: (i64, i64, u32)) -> Result<bool, String> {
        let payload_opt = dispatch_actor_sync_with_deadline(
            self,
            "RemoveModuleCooldown",
            ACTOR_CMD_TIMEOUT,
            key,
            |key, reply| SystemCommand::RemoveModuleCooldown { key, reply },
            remove_module_cooldown_in_actor,
        )?;
        let removed = payload_opt.is_some();
        if let Some(payload) = payload_opt {
            self.persist_cooldown_payload(payload);
        }
        Ok(removed)
    }

    pub fn clear_ship_module_cooldowns_sync(&self, ship_id: i64) -> Result<bool, String> {
        let payload_opt = dispatch_actor_sync_with_deadline(
            self,
            "ClearShipModuleCooldowns",
            ACTOR_CMD_TIMEOUT,
            ship_id,
            |ship_id, reply| SystemCommand::ClearShipModuleCooldowns { ship_id, reply },
            clear_ship_module_cooldowns_in_actor,
        )?;
        let removed = payload_opt.is_some();
        if let Some(payload) = payload_opt {
            self.persist_cooldown_payload(payload);
        }
        Ok(removed)
    }

    pub fn clear_all_module_cooldowns_sync(&self) -> Result<(), String> {
        let payload = dispatch_actor_sync_with_deadline(
            self,
            "ClearAllModuleCooldowns",
            ACTOR_CMD_TIMEOUT,
            (),
            |(), reply| SystemCommand::ClearAllModuleCooldowns { reply },
            |ctx, ()| clear_all_module_cooldowns_in_actor(ctx),
        )?;
        self.persist_cooldown_payload(payload);
        Ok(())
    }

    pub fn replace_module_cooldowns_sync(
        &self,
        cooldowns: HashMap<(i64, i64, u32), ModuleCooldownEntry>,
    ) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "ReplaceModuleCooldowns",
            ACTOR_CMD_TIMEOUT,
            cooldowns,
            |cooldowns, reply| SystemCommand::ReplaceModuleCooldowns { cooldowns, reply },
            replace_module_cooldowns_in_actor,
        )
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
}

#[cfg(test)]
#[path = "ewar_cooldowns_tests.rs"]
mod tests;
