//! `system_actor` — Bounded Actor Mailbox and Subscription Handle for Solar System `Ballpark`s.

#![forbid(unsafe_code)]

mod celestials;
pub(in crate::system_actor) use celestials::fast_forward_clock_in_actor_with_epochs_and_seq;
pub use celestials::*;
mod belts;
pub use belts::*;
mod cycles;
pub(crate) use cycles::*;
mod cycles_tackle;
pub(crate) use cycles_tackle::*;
mod disruptors;
pub(crate) use disruptors::*;
mod missiles;
pub(crate) use missiles::*;
mod protection;
mod activation_ops;
mod ball_ops;
pub use ball_ops::*;
mod drones;
pub use drones::*;
#[cfg(test)]
pub(crate) use activation_ops::{
    apply_fit_projection_in_actor, apply_fit_projection_with_cycles_in_actor,
};
pub mod tidi_autoscaler;
pub use tidi_autoscaler::*;
pub mod lifecycle;
pub use lifecycle::{
    ClusterTickWakeReason, LifecycleTransitionRecord, SessionArrivalWakeReason,
    SystemActivationError, SystemContentMaintenanceContext, SystemContentProvider,
    SystemContentProviderMetadata, SystemContentSeedContext, SystemLifecycleState,
    SystemWakeReason,
};
mod lifecycle_sm;
mod types;
pub use types::*;

pub(in crate::system_actor) use belts::{
    build_tick_broadcast, ensure_default_slim_meta, handle_mine_asteroid,
    reconcile_spawned_belt_asteroids, respawn_due_asteroids,
};
pub(in crate::system_actor) use eve_db::lock_rank::{
    held_lock_ranks, AnyRwLock, RankedMutex, RankedRwLock, RANK_ACTOR_STATE, RANK_LEAF,
    RANK_SERVICE_STATE, RANK_SYSTEM_REGISTRY,
};
use eve_destiny::wire::{
    encode_set_state_payload, encode_set_state_update,
};
pub(in crate::system_actor) use eve_destiny::{Ball, BallId, Ballpark, DestinyEvent, SlimItemMeta};
pub(in crate::system_actor) use eve_proto::PyRep;
pub(in crate::system_actor) use eve_sde::{SdeRegistry, SolarSystemId};
pub(in crate::system_actor) use glam::DVec3;
pub(in crate::system_actor) use std::collections::HashMap;
pub(in crate::system_actor) use std::sync::Arc;
pub(in crate::system_actor) use tokio::sync::{broadcast, mpsc};

// parity: plan:P-7, plan:SP-1, plan:SP-2, plan:SP-4, plan:SP-5
// parity: feat:navigation.celestials-balls, feat:navigation.bubbles-grid
// parity: feat:navigation.destiny-updates, feat:serverops.server-tick
// parity: notify:DoDestinyUpdate
// parity: destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall, destiny:OnSlimItemChange

/// Bounded mailbox capacity per solar system actor.
pub const SYSTEM_MAILBOX_CAPACITY: usize = 256;

/// Atomically assigns the next monotonic `TickBroadcast.broadcast_seq` and sends `broadcast`
/// onto `event_tx` while holding `broadcast_seq_gate`, preserving strict FIFO seq ordering (`GH #1172`).
pub(crate) fn send_tick_broadcast_sequenced_owned(
    event_tx: &broadcast::Sender<TickBroadcast>,
    broadcast_seq_gate: &std::sync::Mutex<u64>,
    mut broadcast: TickBroadcast,
) {
    let mut seq = broadcast_seq_gate
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *seq = seq.saturating_add(1);
    broadcast.broadcast_seq = *seq;
    let _ = event_tx.send(broadcast);
}

/// Assigns `broadcast.broadcast_seq` in-place under `broadcast_seq_gate` and sends a clone onto
/// `event_tx` (`GH #1172`).
pub(crate) fn send_tick_broadcast_sequenced(
    event_tx: &broadcast::Sender<TickBroadcast>,
    broadcast_seq_gate: &std::sync::Mutex<u64>,
    broadcast: &mut TickBroadcast,
) {
    let mut seq = broadcast_seq_gate
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *seq = seq.saturating_add(1);
    broadcast.broadcast_seq = *seq;
    let _ = event_tx.send(broadcast.clone());
}

fn drain_observer_updates_in_actor(
    ballpark: &mut Ballpark,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    observer_ball_id: BallId,
    last_visible_stamp_actor: &std::sync::atomic::AtomicU64,
    last_fx_stamp_actor: &std::sync::atomic::AtomicU64,
    broadcast_seq_gate: &std::sync::Mutex<u64>,
) -> (u64, Vec<DestinyEvent>, PyRep, u64, bool) {
    let slim_changed = sync_player_slim_items(ballpark, db_opt);
    let events = ballpark.drain_observer_events(observer_ball_id);
    // `#1169`: `Ballpark::current_tick` increments exclusively inside `Ballpark::step` when `1.0 s`
    // of simulated physics is integrated — never inside `DrainObserverUpdates`.
    let stamp = ballpark.current_tick as u32;
    let fx_floor = last_visible_stamp_actor
        .load(std::sync::atomic::Ordering::Acquire)
        .max(last_fx_stamp_actor.load(std::sync::atomic::Ordering::Acquire))
        as u32;
    let pyrep = belts::encode_destiny_updates_with_fx_floor(
        stamp,
        fx_floor,
        &events,
        Some(ballpark),
        Some(last_fx_stamp_actor),
    );
    if !events.is_empty()
        && !events
            .iter()
            .any(|ev| matches!(ev, DestinyEvent::SetState { .. }))
    {
        last_visible_stamp_actor.fetch_max(
            u64::from(stamp.max(fx_floor)),
            std::sync::atomic::Ordering::AcqRel,
        );
    }
    let broadcast_seq = *broadcast_seq_gate
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    (ballpark.current_tick, events, pyrep, broadcast_seq, slim_changed)
}

/// Builds a `SetState` payload + `DoDestinyUpdate` list for `observer_ball_id` and snapshots the
/// actor's current `TickBroadcast.broadcast_seq` watermark (`#1461`).
fn get_set_state_in_actor(
    ballpark: &Ballpark,
    solar_system_id: SolarSystemId,
    observer_ball_id: BallId,
    last_visible_stamp_actor: &std::sync::atomic::AtomicU64,
    broadcast_seq_gate: &std::sync::Mutex<u64>,
) -> Result<(u64, PyRep, PyRep, u64), String> {
    if ballpark.balls.contains_key(&observer_ball_id) {
        last_visible_stamp_actor.fetch_max(
            ballpark.current_tick,
            std::sync::atomic::Ordering::AcqRel,
        );
        let payload = encode_set_state_payload(ballpark, observer_ball_id);
        let update_list = PyRep::List(vec![encode_set_state_update(
            ballpark,
            observer_ball_id,
        )]);
        let broadcast_seq = *broadcast_seq_gate
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Ok((ballpark.current_tick, payload, update_list, broadcast_seq))
    } else {
        Err(format!(
            "Observer ball {observer_ball_id} not in system {solar_system_id}"
        ))
    }
}

/// DERIVED default maintenance/downtime belt respawn period in seconds (86,400s = 1 day),
/// configurable via `ServerConfig::belt_respawn_interval_secs` (`D-5`, `MN-03`).
/// Per EVE wiki `Asteroids_and_ore.md:69`, belts respawn after downtime only if there are no asteroids left in the belt.
pub const DEFAULT_BELT_RESPAWN_INTERVAL_SECS: u64 = 86_400;

/// Default daily server downtime UTC time of day in seconds (11:00 UTC = 39,600s per EVE wiki `Evergreen_Daily_Login_Rewards.md:21`, `Skillbook_Program.md:43`),
/// configurable via `ServerConfig::downtime_utc_secs_of_day` (`D-5`, `MN-03`).
pub const DEFAULT_DOWNTIME_UTC_SECS_OF_DAY: u64 = 39_600;

/// Computes the strictly next downtime Unix timestamp (`> now_unix`) aligned to `downtime_utc_secs_of_day`.
pub fn next_downtime_unix_secs(now_unix: u64, interval_secs: u64, downtime_utc_secs_of_day: u64) -> u64 {
    let period = interval_secs.max(1);
    let phase = downtime_utc_secs_of_day % period;
    let rem = now_unix % period;
    let delta = if rem < phase { phase - rem } else { period - (rem - phase) };
    now_unix.saturating_add(delta)
}

/// Returns MachoNet wall-clock 100ns FILETIME ticks (subtracting un-rebased `/advancetime` lead) (`#1080`).
pub(crate) fn machonet_wallclock_filetime_ticks(
    clock: &dyn eve_db::ServerClock,
    unrebased_clock_offset_secs: &std::sync::atomic::AtomicU64,
) -> i64 {
    let unrebased_secs = unrebased_clock_offset_secs.load(std::sync::atomic::Ordering::Acquire);
    let unrebased_ticks = i64::try_from(unrebased_secs)
        .unwrap_or(i64::MAX)
        .saturating_mul(10_000_000);
    clock
        .now_blueos_filetime_ticks()
        .saturating_sub(unrebased_ticks)
}

/// Returns wall-clock milliseconds derived from [`machonet_wallclock_filetime_ticks`] (`#1080`).
pub(crate) fn machonet_wallclock_ms(
    clock: &dyn eve_db::ServerClock,
    unrebased_clock_offset_secs: &std::sync::atomic::AtomicU64,
) -> u64 {
    (machonet_wallclock_filetime_ticks(clock, unrebased_clock_offset_secs) / 10_000).max(0) as u64
}

pub(in crate::system_actor) enum StepReply {
    Full(tokio::sync::oneshot::Sender<StepOutput>),
    TickOnly(tokio::sync::oneshot::Sender<StepOutput>),
}

pub(in crate::system_actor) enum PendingActorDelivery {
    Step(StepReply, StepOutput),
    Volley(
        tokio::sync::oneshot::Sender<WeaponVolleyOutput>,
        WeaponVolleyOutput,
    ),
}

#[inline]
pub(in crate::system_actor) fn defer_reply<T: Send + 'static>(
    deferred: &mut Vec<Box<dyn FnOnce() + Send>>,
    reply: tokio::sync::oneshot::Sender<T>,
    value: T,
) {
    deferred.push(Box::new(move || {
        let _ = reply.send(value);
    }));
}

pub(in crate::system_actor) struct ActorExecCtx<'a> {
    pub(in crate::system_actor) solar_system_id: SolarSystemId,
    pub(in crate::system_actor) ballpark: &'a mut Ballpark,
    pub(in crate::system_actor) actor_st: &'a mut SystemActorState,
    pub(in crate::system_actor) ball_spawn_tokens: &'a mut HashMap<BallId, u64>,
    pub(in crate::system_actor) last_active_totals: &'a mut HashMap<BallId, (Option<i32>, Option<i32>)>,
    pub(in crate::system_actor) deferred_replies: &'a mut Vec<Box<dyn FnOnce() + Send>>,
    pub(in crate::system_actor) spawned_checkpoint_ball: &'a mut Option<BallId>,
    pub(in crate::system_actor) removed_checkpoint_ball: &'a mut Option<BallId>,
    pub(in crate::system_actor) ball_positions_actor: &'a Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    pub(in crate::system_actor) scrambled_balls_actor: &'a Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    pub(in crate::system_actor) warp_stopped_at_actor: &'a Arc<RankedRwLock<HashMap<BallId, tokio::time::Instant>, RANK_LEAF>>,
    pub(in crate::system_actor) active_module_ships_actor: &'a Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    pub(in crate::system_actor) module_deactivation_hook_actor: &'a Arc<RankedRwLock<Option<ModuleDeactivationHook>, RANK_LEAF>>,
    pub(in crate::system_actor) target_lock_completed_hook_actor: &'a Arc<RankedRwLock<Option<TargetLockCompletedHook>, RANK_LEAF>>,
    pub(in crate::system_actor) tidi_factor_bits_actor: &'a Arc<std::sync::atomic::AtomicU64>,
    pub(in crate::system_actor) sim_elapsed_secs_bits_actor: &'a Arc<std::sync::atomic::AtomicU64>,
    pub(in crate::system_actor) last_visible_stamp_actor: &'a Arc<std::sync::atomic::AtomicU64>,
    pub(in crate::system_actor) last_fx_stamp_actor: &'a Arc<std::sync::atomic::AtomicU64>,
    pub(in crate::system_actor) broadcast_seq_gate_actor: &'a Arc<std::sync::Mutex<u64>>,
    pub(in crate::system_actor) event_tx_actor: &'a broadcast::Sender<TickBroadcast>,
    pub(in crate::system_actor) pending_destroyed_actor: &'a PendingDestroyedOutbox,
    pub(in crate::system_actor) unrebased_clock_offset_secs_actor: &'a Arc<std::sync::atomic::AtomicU64>,
    pub(in crate::system_actor) explicit_update_state_guards_actor:
        &'a Arc<RankedMutex<HashMap<(u64, i64), types::ExplicitUpdateStateGuard>, RANK_LEAF>>,
    pub(in crate::system_actor) mechanic_rng: &'a Arc<crate::rng::MechanicRng>,
}

#[derive(Clone, Debug)]
enum SpecialSiteBallSpawnState {
    Spawning(Arc<tokio::sync::Notify>),
    Spawned,
}

type SpecialSiteBallSpawnMap =
    Arc<RankedMutex<HashMap<(SolarSystemId, i64), SpecialSiteBallSpawnState>, RANK_LEAF>>;

/// RAII reservation for spawning a PvE special-site ball once.
/// Dropping without calling [`Self::confirm`] rolls back the in-flight `Spawning` entry and
/// wakes concurrent waiters so transient errors or dropped futures remain recoverable.
pub struct SpecialSiteBallSpawnGuard {
    table: SpecialSiteBallSpawnMap,
    solar_system_id: SolarSystemId,
    ball_id: i64,
    notify: Arc<tokio::sync::Notify>,
    confirmed: bool,
}

impl SpecialSiteBallSpawnGuard {
    /// Marks `(solar_system_id, ball_id)` as `Spawned` and notifies any concurrent waiters.
    pub fn confirm(mut self) {
        self.confirmed = true;
        {
            let mut guard = self.table.lock().unwrap_or_else(|e| e.into_inner());
            guard.insert(
                (self.solar_system_id, self.ball_id),
                SpecialSiteBallSpawnState::Spawned,
            );
        }
        self.notify.notify_waiters();
    }
}

impl Drop for SpecialSiteBallSpawnGuard {
    fn drop(&mut self) {
        if !self.confirmed {
            {
                let mut guard = self.table.lock().unwrap_or_else(|e| e.into_inner());
                if matches!(
                    guard.get(&(self.solar_system_id, self.ball_id)),
                    Some(SpecialSiteBallSpawnState::Spawning(n)) if Arc::ptr_eq(n, &self.notify)
                ) {
                    guard.remove(&(self.solar_system_id, self.ball_id));
                }
            }
            self.notify.notify_waiters();
        }
    }
}

pub struct SystemRegistry {
    pub(crate) systems: RankedRwLock<HashMap<SolarSystemId, lifecycle_sm::SystemLifecycleSlot>, RANK_SYSTEM_REGISTRY>,
    sde: Option<Arc<SdeRegistry>>,
    pub db: Option<Arc<eve_db::WorldRepository>>,
    belt_respawn_interval_secs: Arc<std::sync::atomic::AtomicU64>,
    downtime_utc_secs_of_day: Arc<std::sync::atomic::AtomicU64>,
    /// Game-mechanic RNG (seeded test RNG): OS CSPRNG by default, shared with `ServerContext::mechanic_rng`.
    mechanic_rng: Arc<crate::rng::MechanicRng>,
    pub(crate) npc_death_hook: Arc<RankedRwLock<Option<NpcDeathHook>, RANK_LEAF>>,
    pub(crate) module_deactivation_hook: Arc<RankedRwLock<Option<ModuleDeactivationHook>, RANK_LEAF>>,
    pub(crate) target_lock_completed_hook: Arc<RankedRwLock<Option<TargetLockCompletedHook>, RANK_LEAF>>,
    pub(crate) system_step_hook: Arc<RankedRwLock<Option<SystemStepHook>, RANK_LEAF>>,
    pub(crate) post_unlock_actions: Arc<RankedMutex<Vec<PostUnlockHookAction>, RANK_LEAF>>,
    /// PvE special-site balls (system, ball id) in-flight or already spawned into this registry's
    /// ballparks. Runtime-only (lives and dies with the ballparks), so no sentinel ball is needed.
    special_site_spawned_balls: SpecialSiteBallSpawnMap,
    /// Wormhole ore anomalies (system, anomaly_id) whose initial Sleeper guard wave has been hydrated
    /// in this registry's ballparks, preventing player scan RPCs from respawning killed guards.
    hydrated_wh_ore_anomalies: RankedMutex<std::collections::HashSet<(SolarSystemId, String)>, RANK_LEAF>,
    pub tactical_state: Arc<RankedRwLock<crate::services::tactical_modules::TacticalModulesState, RANK_LEAF>>,
    pub module_reload_sim_deadlines: Arc<RankedRwLock<HashMap<i64, (SolarSystemId, f64, f64)>, RANK_LEAF>>,
    pending_notifications_opt: Option<Arc<RankedRwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>, RANK_LEAF>>>,
    notify_tx_opt: Option<broadcast::Sender<(i64, crate::router::PendingMachoNotification)>>,
    pub cluster_nodes: RankedRwLock<ClusterNodeRegistry, RANK_SERVICE_STATE>,
    /// When true, stargates whose destination solar system is not yet loaded start as `CLOSED (0)`
    /// (`#1080`) and transition `CLOSED -> ACTIVATING -> OPEN` once destination loads.
    lazy_stargate_activation: Arc<std::sync::atomic::AtomicBool>,
    /// Shared `/advancetime` un-rebased clock offset in seconds so gate activation deadlines match MachoNet wallclock (`#1080`).
    unrebased_clock_offset_secs: Arc<std::sync::atomic::AtomicU64>,
    /// Per-system in-flight physics-step atomic flags preventing a wedged system from accumulating overlapping ticks (`#1208`).
    physics_step_flags: RankedMutex<HashMap<SolarSystemId, Arc<std::sync::atomic::AtomicBool>>, RANK_LEAF>,
    /// Per-session `beyonce.UpdateStateRequest` cooldown guards owned by this `ServerContext` (`#1169`).
    pub(crate) explicit_update_state_guards:
        Arc<RankedMutex<HashMap<(u64, i64), types::ExplicitUpdateStateGuard>, RANK_LEAF>>,
    pub(crate) content_providers: lifecycle::LifecycleContentProviders,
    pub(crate) next_lifecycle_generation: std::sync::atomic::AtomicU64,
    pub(crate) lifecycle_telemetry:
        RankedRwLock<lifecycle_sm::LifecycleTelemetryStore, RANK_LEAF>,
    pub(crate) discarded_stale_completions: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    pub(crate) lifecycle_test_hooks: lifecycle_sm::LifecycleTestHooks,
    /// Per-solar-system environment effect overrides owned by this `SystemRegistry` (`#1583` Track 2).
    pub system_wide_effects: RankedRwLock<types::SystemWideEffectsState, RANK_LEAF>,
}

/// Lock-free RAII guard releasing a system's in-flight physics-step slot upon completion or panic.
pub(crate) struct PhysicsStepGuard(Arc<std::sync::atomic::AtomicBool>);

impl Drop for PhysicsStepGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

pub(in crate::system_actor) use lifecycle_sm::PreparedSystemActivation;

impl Default for SystemRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemRegistry {
    pub fn new() -> Self {
        Self {
            systems: RankedRwLock::new(HashMap::new()),
            sde: None,
            db: None,
            belt_respawn_interval_secs: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_BELT_RESPAWN_INTERVAL_SECS)),
            downtime_utc_secs_of_day: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_DOWNTIME_UTC_SECS_OF_DAY)),
            mechanic_rng: Arc::new(crate::rng::MechanicRng::os()),
            npc_death_hook: Arc::new(RankedRwLock::new(None)),
            module_deactivation_hook: Arc::new(RankedRwLock::new(None)),
            target_lock_completed_hook: Arc::new(RankedRwLock::new(None)),
            system_step_hook: Arc::new(RankedRwLock::new(None)),
            post_unlock_actions: Arc::new(RankedMutex::new(Vec::new())),
            special_site_spawned_balls: Arc::new(RankedMutex::new(HashMap::new())),
            hydrated_wh_ore_anomalies: RankedMutex::new(std::collections::HashSet::new()),
            tactical_state: Arc::new(RankedRwLock::new(Default::default())),
            module_reload_sim_deadlines: Arc::new(RankedRwLock::new(HashMap::new())),
            pending_notifications_opt: None,
            notify_tx_opt: None,
            cluster_nodes: RankedRwLock::new(ClusterNodeRegistry::default()),
            lazy_stargate_activation: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            unrebased_clock_offset_secs: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            physics_step_flags: RankedMutex::new(HashMap::new()),
            explicit_update_state_guards: Arc::new(RankedMutex::new(HashMap::new())),
            content_providers: lifecycle::LifecycleContentProviders::default(),
            next_lifecycle_generation: std::sync::atomic::AtomicU64::new(1),
            lifecycle_telemetry: lifecycle_sm::new_lifecycle_telemetry(),
            discarded_stale_completions: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            lifecycle_test_hooks: Default::default(),
            system_wide_effects: RankedRwLock::new(types::SystemWideEffectsState::default()),
        }
    }

    /// Reads any explicit system-wide effect override and overlapping list for `solar_system_id`
    /// without blocking on or spawning a solar system actor (`#1583` Track 2).
    pub fn get_system_wide_effects(&self, solar_system_id: SolarSystemId) -> (Option<u32>, Vec<u32>) {
        let guard = self.system_wide_effects.read().unwrap_or_else(|e| e.into_inner());
        (
            guard.effects.get(&solar_system_id).copied(),
            guard.effect_lists.get(&solar_system_id).cloned().unwrap_or_default(),
        )
    }

    /// Sets the explicit system-wide effect `type_id` override for `solar_system_id`, returning any
    /// previous override (`#1583` Track 2).
    pub fn set_system_wide_effect(&self, solar_system_id: SolarSystemId, effect_type_id: u32) -> Option<u32> {
        self.system_wide_effects
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .effects
            .insert(solar_system_id, effect_type_id)
    }

    /// Clears any explicit system-wide effect override and overlapping list for `solar_system_id`,
    /// returning the previous primary override (`#1583` Track 2).
    pub fn clear_system_wide_effect(&self, solar_system_id: SolarSystemId) -> Option<u32> {
        let mut guard = self.system_wide_effects.write().unwrap_or_else(|e| e.into_inner());
        guard.effect_lists.remove(&solar_system_id);
        guard.effects.remove(&solar_system_id)
    }

    /// Executes `f` with mutable access to this registry's `SystemWideEffectsState` (`#1583` Track 2).
    pub fn with_system_wide_effects_mut<R>(
        &self,
        f: impl FnOnce(&mut types::SystemWideEffectsState) -> R,
    ) -> R {
        let mut guard = self.system_wide_effects.write().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    pub fn clear_explicit_update_state_guard(&self, session_id: u64, char_id: i64) {
        self.explicit_update_state_guards
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(session_id, char_id));
    }

    pub fn clear_explicit_update_state_guards_for_session(&self, session_id: u64) {
        self.explicit_update_state_guards
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(sid, _), _| *sid != session_id);
    }

    /// Acquires the exclusive per-system physics-step slot if no prior step for `solar_system_id` is still in flight.
    pub(crate) fn try_begin_physics_step(&self, solar_system_id: SolarSystemId) -> Option<PhysicsStepGuard> {
        let flag = {
            let mut flags = self.physics_step_flags.lock().unwrap_or_else(|e| e.into_inner());
            flags
                .entry(solar_system_id)
                .or_insert_with(|| Arc::new(std::sync::atomic::AtomicBool::new(false)))
                .clone()
        };
        if flag
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok()
        {
            Some(PhysicsStepGuard(flag))
        } else {
            None
        }
    }

    /// Returns whether lazy stargate activation (`CLOSED -> ACTIVATING -> OPEN`) is enabled (`#1080`).
    pub fn lazy_stargate_activation(&self) -> bool {
        self.lazy_stargate_activation
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Enables or disables lazy stargate activation (`#1080`), updating already-loaded systems' gates.
    pub fn set_lazy_stargate_activation(&self, enabled: bool) {
        self.lazy_stargate_activation
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        let handles = self.active_system_handles();
        let loaded_systems: std::collections::HashSet<SolarSystemId> = handles
            .iter()
            .filter_map(|&(sys_id, ref h)| (!h.cmd_tx.is_closed()).then_some(sys_id))
            .collect();
        for &(_, ref handle) in &handles {
            if let Ok(mut bp) = handle.ballpark.write() {
                let gate_targets: Vec<(BallId, SolarSystemId)> = bp
                    .ball_slim_meta
                    .iter()
                    .filter_map(|(&bid, meta)| {
                        (meta.group_id == 10)
                            .then_some(meta.jump_target.map(|(_, dest_sys)| (bid, dest_sys)))
                            .flatten()
                    })
                    .collect();
                for (gate_id, dest_sys) in gate_targets {
                    let target_state = if !enabled || loaded_systems.contains(&dest_sys) {
                        eve_destiny::STARGATE_ACTIVATION_STATE_OPEN
                    } else {
                        eve_destiny::STARGATE_ACTIVATION_STATE_CLOSED
                    };
                    bp.set_stargate_activation_state(gate_id, target_state, 0, 0);
                }
            }
        }
    }

    /// Returns `true` if a live `SystemActor` for `solar_system_id` is currently loaded (`#1080`).
    pub fn is_system_loaded(&self, solar_system_id: SolarSystemId) -> bool {
        let is_live = |guard: &HashMap<SolarSystemId, lifecycle_sm::SystemLifecycleSlot>| {
            guard
                .get(&solar_system_id)
                .and_then(lifecycle_sm::SystemLifecycleSlot::live_handle)
                .is_some()
        };
        if held_lock_ranks().iter().any(|&r| r >= RANK_SYSTEM_REGISTRY) {
            let guard = self.systems.read_unranked().unwrap_or_else(|e| e.into_inner());
            is_live(&guard)
        } else {
            let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
            is_live(&guard)
        }
    }

    /// Refreshes stargate activation states across currently loaded systems when `loaded_sys_id` loads (`#1080`).
    pub(crate) fn on_system_loaded_refresh_stargates(
        &self,
        loaded_sys_id: SolarSystemId,
        now_ms: u64,
    ) {
        if !self.lazy_stargate_activation()
            || held_lock_ranks()
                .iter()
                .any(|&r| r >= eve_db::lock_rank::RANK_BALLPARK)
        {
            return;
        }
        let handles = self.active_system_handles();
        for (sys_id, handle) in handles {
            if sys_id == loaded_sys_id {
                continue;
            }
            if let Ok(mut bp) = handle.ballpark.write() {
                let matching_gates: Vec<BallId> = bp
                    .ball_slim_meta
                    .iter()
                    .filter_map(|(&bid, meta)| {
                        (meta.group_id == 10
                            && meta.jump_target.is_some_and(|(_, dest)| dest == loaded_sys_id))
                        .then_some(bid)
                    })
                    .collect();
                for gate_id in matching_gates {
                    bp.activate_stargate_transition(
                        gate_id,
                        eve_destiny::STARGATE_ACTIVATION_TRANSITION_MS,
                        (now_ms > 0).then_some(now_ms),
                    );
                }
            }
        }
    }

    /// Lazily loads `dest_sys_id` if needed and begins transitioning the pointing gate from
    /// `CLOSED` -> `ACTIVATING` (`STARGATE_ACTIVATION_TRANSITION_MS`) (`#1080`).
    pub fn ensure_stargate_destination_loaded_for_intent(
        &self,
        source_sys_id: SolarSystemId,
        gate_id: BallId,
        now_ms: u64,
    ) -> Option<SolarSystemId> {
        let sde = self.sde.as_ref()?;
        let jump = sde.jumps.get(&gate_id)?;
        if jump.from_solar_system_id != source_sys_id {
            return None;
        }
        let dest_sys_id = jump.to_solar_system_id;
        if !self.lazy_stargate_activation() {
            return Some(dest_sys_id);
        }
        let was_loaded = self.is_system_loaded(dest_sys_id);
        let _dest_handle = match self
            .wake_for_session_arrival(dest_sys_id, SessionArrivalWakeReason::Jump)
        {
            Ok(handle) => handle,
            Err(err) => {
                tracing::warn!(
                    source_sys_id,
                    dest_sys_id,
                    gate_id,
                    %err,
                    "failed to wake stargate destination system"
                );
                return None;
            }
        };
        if let Some(src_handle) = self.get_handle(source_sys_id) {
            if !was_loaded {
                src_handle.activate_stargate_transition(
                    gate_id,
                    eve_destiny::STARGATE_ACTIVATION_TRANSITION_MS,
                    (now_ms > 0).then_some(now_ms),
                );
            }
        }
        Some(dest_sys_id)
    }

    pub fn with_sde(sde: Arc<SdeRegistry>) -> Self {
        Self { sde: Some(sde), ..Self::new() }
    }

    pub fn with_sde_and_db(sde: Arc<SdeRegistry>, db: Arc<eve_db::WorldRepository>) -> Self {
        Self { sde: Some(sde), db: Some(db), ..Self::new() }
    }

    pub fn with_notifications(
        mut self,
        pending_notifications: Arc<RankedRwLock<HashMap<i64, Vec<crate::router::PendingMachoNotification>>, RANK_LEAF>>,
        notify_tx: broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    ) -> Self {
        self.pending_notifications_opt = Some(pending_notifications);
        self.notify_tx_opt = Some(notify_tx);
        self
    }

    pub fn with_tactical_state(
        mut self,
        tactical_state: Arc<RankedRwLock<crate::services::tactical_modules::TacticalModulesState, RANK_LEAF>>,
    ) -> Self {
        self.tactical_state = tactical_state;
        self
    }

    pub fn set_npc_death_hook(&self, hook: NpcDeathHook) {
        *self.npc_death_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    /// Invokes the registered `npc_death_hook` for an NPC ball destroyed outside
    /// standard damage ticks (e.g., GM `/kill <npc_ball_id>`, `#1003`).
    pub async fn invoke_npc_death_hook(
        &self,
        solar_system_id: u32,
        npc_ball_id: i64,
        killer_char_id: Option<i64>,
    ) {
        let hook = self
            .npc_death_hook
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(h) = hook {
            h(solar_system_id, npc_ball_id, killer_char_id).await;
        }
    }

    pub fn set_module_deactivation_hook(&self, hook: ModuleDeactivationHook) {
        *self.module_deactivation_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    pub fn set_target_lock_completed_hook(&self, hook: TargetLockCompletedHook) {
        *self.target_lock_completed_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    pub fn set_system_step_hook(&self, hook: SystemStepHook) {
        *self.system_step_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    pub(crate) fn enqueue_post_unlock_action(&self, action: PostUnlockHookAction) {
        self.post_unlock_actions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(action);
    }

    /// Shares `rng` as this registry's game-mechanic RNG (seeded test RNG).
    pub fn with_mechanic_rng(mut self, rng: Arc<crate::rng::MechanicRng>) -> Self {
        self.mechanic_rng = rng;
        self
    }

    /// Shares the server's `/advancetime` un-rebased clock offset atomic with [`SystemRegistry`] (`#1080`).
    pub fn with_unrebased_clock_offset_secs(
        mut self,
        unrebased_clock_offset_secs: Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        self.unrebased_clock_offset_secs = unrebased_clock_offset_secs;
        self
    }

    pub fn belt_respawn_interval_secs(&self) -> u64 {
        self.belt_respawn_interval_secs
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_belt_respawn_interval_secs(&self, secs: u64) {
        self.belt_respawn_interval_secs
            .store(secs.clamp(1, 604_800), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn downtime_utc_secs_of_day(&self) -> u64 {
        self.downtime_utc_secs_of_day
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_downtime_utc_secs_of_day(&self, secs: u64) {
        self.downtime_utc_secs_of_day
            .store(secs % 86_400, std::sync::atomic::Ordering::Relaxed);
    }
}

impl SystemRegistry {
    pub fn get_ball_position_sync(
        &self,
        solar_system_id: SolarSystemId,
        ball_id: BallId,
    ) -> Option<DVec3> {
        self.get_handle(solar_system_id)
            .and_then(|h| h.get_ball_position_sync(ball_id))
    }

    pub fn is_ball_scrambled_sync(
        &self,
        solar_system_id: SolarSystemId,
        ball_id: BallId,
    ) -> bool {
        self.get_handle(solar_system_id)
            .is_some_and(|h| h.is_ball_scrambled_sync(ball_id))
    }

    pub fn is_ball_tethered_sync(
        &self,
        solar_system_id: SolarSystemId,
        ball_id: BallId,
    ) -> bool {
        self.get_handle(solar_system_id)
            .is_some_and(|h| h.is_ball_tethered_sync(ball_id))
    }

    pub fn has_active_modules_or_cloak_sync(
        &self,
        solar_system_id: SolarSystemId,
        ball_id: BallId,
    ) -> bool {
        self.get_handle(solar_system_id)
            .is_some_and(|h| h.has_active_modules_or_cloak_sync(ball_id))
    }

    pub fn find_ball_position_sync(&self, ball_id: BallId) -> Option<DVec3> {
        for (_, handle) in self.active_system_handles() {
            if let Some(pos) = handle.get_ball_position_sync(ball_id) {
                return Some(pos);
            }
        }
        None
    }

    pub fn find_ball_system_sync(&self, ball_id: BallId) -> Option<SolarSystemId> {
        for (sys_id, handle) in self.active_system_handles() {
            if handle.get_ball_position_sync(ball_id).is_some() {
                return Some(sys_id);
            }
        }
        None
    }

    pub fn get_system_sim_elapsed(&self, solar_system_id: SolarSystemId) -> f64 {
        self.get_handle(solar_system_id)
            .map(|h| h.sim_elapsed_secs())
            .unwrap_or(0.0)
    }

    pub fn get_system_tidi_factor(&self, solar_system_id: SolarSystemId) -> f64 {
        self.get_handle(solar_system_id)
            .map(|h| h.tidi_factor())
            .unwrap_or(1.0)
    }

    pub fn record_module_reload_sim_deadlines(
        &self,
        module_ids: &[i64],
        solar_system_id: SolarSystemId,
        sim_start_s: f64,
        sim_deadline_s: f64,
    ) {
        let mut guard = self
            .module_reload_sim_deadlines
            .write()
            .unwrap_or_else(|e| e.into_inner());
        for &mid in module_ids {
            guard.insert(mid, (solar_system_id, sim_start_s, sim_deadline_s));
        }
    }

    pub fn clear_module_reload_sim_deadline(&self, module_id: i64) {
        self.module_reload_sim_deadlines
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&module_id);
    }

    pub fn check_module_reload_sim_deadline(
        &self,
        module_id: i64,
    ) -> Option<(SolarSystemId, f64, f64)> {
        self.module_reload_sim_deadlines
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&module_id)
            .copied()
    }

    pub fn take_pending_cycle_actions(&self, solar_system_id: SolarSystemId) -> Vec<PendingCycleAction> {
        self.get_handle(solar_system_id)
            .map(|h| h.take_pending_cycle_actions())
            .unwrap_or_default()
    }

    pub fn active_systems_count(&self) -> usize {
        let count_live = |guard: &HashMap<SolarSystemId, lifecycle_sm::SystemLifecycleSlot>| {
            guard
                .values()
                .filter(|slot| slot.live_handle().is_some())
                .count()
        };
        if held_lock_ranks().iter().any(|&r| r >= RANK_SYSTEM_REGISTRY) {
            let guard = self.systems.read_unranked().unwrap_or_else(|e| e.into_inner());
            count_live(&guard)
        } else {
            let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
            count_live(&guard)
        }
    }

    pub fn get_handle(&self, solar_system_id: SolarSystemId) -> Option<SystemHandle> {
        if held_lock_ranks().iter().any(|&r| r >= RANK_SYSTEM_REGISTRY) {
            let guard = self.systems.read_unranked().unwrap_or_else(|e| e.into_inner());
            guard
                .get(&solar_system_id)
                .and_then(lifecycle_sm::SystemLifecycleSlot::live_handle)
                .cloned()
        } else {
            let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
            guard
                .get(&solar_system_id)
                .and_then(lifecycle_sm::SystemLifecycleSlot::live_handle)
                .cloned()
        }
    }

    pub fn active_system_handles(&self) -> Vec<(SolarSystemId, SystemHandle)> {
        let collect_live = |guard: &HashMap<SolarSystemId, lifecycle_sm::SystemLifecycleSlot>| {
            guard
                .iter()
                .filter_map(|(&id, slot)| slot.live_handle().map(|h| (id, h.clone())))
                .collect::<Vec<_>>()
        };
        let mut handles: Vec<(SolarSystemId, SystemHandle)> =
            if held_lock_ranks().iter().any(|&r| r >= RANK_SYSTEM_REGISTRY) {
                let guard = self.systems.read_unranked().unwrap_or_else(|e| e.into_inner());
                collect_live(&guard)
            } else {
                let guard = self.systems.read().unwrap_or_else(|e| e.into_inner());
                collect_live(&guard)
            };
        handles.sort_by_key(|(id, _)| *id);
        handles
    }

    pub fn cancel_nanite_repairs_for_ship_sync(&self, ship_id: BallId) -> Vec<ActiveNaniteRepair> {
        self.active_system_handles()
            .into_iter()
            .flat_map(|(_, h)| h.cancel_nanite_repairs_for_ship_sync(ship_id))
            .collect()
    }

    /// Records that PvE special-site ball `ball_id` has been spawned in `solar_system_id`.
    /// Returns `true` only the first time, so destroyed site NPCs are never re-spawned.
    pub fn claim_special_site_ball(&self, solar_system_id: SolarSystemId, ball_id: i64) -> bool {
        use std::collections::hash_map::Entry;
        let mut guard = self
            .special_site_spawned_balls
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match guard.entry((solar_system_id, ball_id)) {
            Entry::Vacant(entry) => {
                entry.insert(SpecialSiteBallSpawnState::Spawned);
                true
            }
            Entry::Occupied(_) => false,
        }
    }

    /// Returns `true` when `(solar_system_id, ball_id)` has reached confirmed `Spawned` state.
    pub fn is_special_site_ball_spawned(&self, solar_system_id: SolarSystemId, ball_id: i64) -> bool {
        matches!(
            self.special_site_spawned_balls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&(solar_system_id, ball_id)),
            Some(SpecialSiteBallSpawnState::Spawned)
        )
    }

    /// Acquires an exclusive RAII spawn slot for `(solar_system_id, ball_id)`.
    /// Concurrent callers wait until any in-flight spawner confirms or drops; returns `None` once
    /// the ball is already `Spawned`.
    pub async fn begin_special_site_ball_spawn(
        &self,
        solar_system_id: SolarSystemId,
        ball_id: i64,
    ) -> Option<SpecialSiteBallSpawnGuard> {
        use std::collections::hash_map::Entry;
        loop {
            let notify = {
                let mut guard = self
                    .special_site_spawned_balls
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                match guard.entry((solar_system_id, ball_id)) {
                    Entry::Occupied(entry) => match entry.get() {
                        SpecialSiteBallSpawnState::Spawned => return None,
                        SpecialSiteBallSpawnState::Spawning(notify) => Arc::clone(notify),
                    },
                    Entry::Vacant(entry) => {
                        let notify = Arc::new(tokio::sync::Notify::new());
                        entry.insert(SpecialSiteBallSpawnState::Spawning(Arc::clone(&notify)));
                        return Some(SpecialSiteBallSpawnGuard {
                            table: Arc::clone(&self.special_site_spawned_balls),
                            solar_system_id,
                            ball_id,
                            notify,
                            confirmed: false,
                        });
                    }
                }
            };
            // Pin and register on `Notify` before re-checking the map under the mutex so a
            // concurrent `confirm()` or `Drop` cannot fire `notify_waiters()` in between.
            let notified = notify.notified();
            tokio::pin!(notified);
            let _registered = notified.as_mut().enable();
            let still_waiting = {
                let guard = self
                    .special_site_spawned_balls
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                matches!(
                    guard.get(&(solar_system_id, ball_id)),
                    Some(SpecialSiteBallSpawnState::Spawning(current))
                        if Arc::ptr_eq(current, &notify)
                )
            };
            if still_waiting {
                notified.await;
            }
        }
    }

    /// Marks wormhole ore `anomaly_id` in `solar_system_id` as hydrated for this ballpark registry.
    /// Returns `true` only on first hydration so player scan RPCs never respawn killed Sleeper guards.
    pub fn claim_wh_ore_anomaly_hydration(&self, solar_system_id: SolarSystemId, anomaly_id: &str) -> bool {
        self.hydrated_wh_ore_anomalies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((solar_system_id, anomaly_id.to_string()))
    }

    /// Returns `true` when wormhole ore `anomaly_id` in `solar_system_id` is already hydrated.
    pub fn is_wh_ore_anomaly_hydrated(&self, solar_system_id: SolarSystemId, anomaly_id: &str) -> bool {
        self.hydrated_wh_ore_anomalies
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|(sid, aid)| *sid == solar_system_id && aid == anomaly_id)
    }

    pub(crate) fn reclaim_transient_items_in_db(&self, solar_system_id: SolarSystemId) {
        if let Some(ref db) = self.db {
            if !db.is_write_locked_by_current_thread() {
                let _ = db.tx("reclaim_transient_items", |wtx| {
                    wtx.snap.reclaim_transient_items_in_system(solar_system_id);
                    Ok::<_, eve_db::TxError>(())
                });
            }
        }
    }

    /// Synchronously flushes dirty condition checkpoints across all active systems to `self.db`
    /// (`RFC-1 E-4`, `T-2`, `T-5`, `#919`).
    pub fn flush_all_condition_checkpoints_to_db(&self) -> usize {
        let Some(ref db) = self.db else {
            return 0;
        };
        if db.is_write_locked_by_current_thread() {
            return 0;
        }
        let mut total = 0usize;
        for (sys_id, handle) in self.active_system_handles() {
            let entries = handle.collect_dirty_condition_checkpoint_sync();
            if !entries.is_empty() {
                total += crate::sp6_lifecycle::apply_condition_checkpoint_for_system_in_db(
                    db, sys_id, &entries,
                );
            }
        }
        total
    }

    /// Retrieves a running Solar System Actor handle, or spawns one only when a Tokio runtime
    /// reactor is currently active (`None` on headless non-Tokio teardown/unit threads).
    pub fn get_or_spawn_if_runtime(&self, solar_system_id: SolarSystemId) -> Option<SystemHandle> {
        self.legacy_activate_system_if_runtime(solar_system_id)
    }

    /// Spawns or retrieves a running Solar System Actor for `solar_system_id`.
    /// Populates static SDE celestials, stargates, and stations (`SP-1`), re-materializes persisted
    /// space objects (`SP-7`), and hibernates (`LOD 0`) with zero timer wakes until dynamic balls exist.
    pub fn get_or_spawn(&self, solar_system_id: SolarSystemId) -> SystemHandle {
        self.legacy_activate_system(solar_system_id)
    }

    pub(crate) fn prepare_system_activation(
        &self,
        solar_system_id: SolarSystemId,
        wake_reason: &SystemWakeReason,
    ) -> Result<PreparedSystemActivation, SystemActivationError> {
        let sde_opt = self.sde.clone();
        let db_opt = self.db.clone();
        let belt_interval_atomic = self.belt_respawn_interval_secs.clone();
        let downtime_utc_atomic = self.downtime_utc_secs_of_day.clone();
        let mechanic_rng = self.mechanic_rng.clone();
        let mut initial_ballpark = Ballpark::new(solar_system_id);
        if let Some(sde) = &sde_opt {
            populate_system_celestials(&mut initial_ballpark, solar_system_id, sde);
        }
        let lazy_gates = self.lazy_stargate_activation();
        let gate_dests: Vec<(BallId, SolarSystemId)> = initial_ballpark
            .ball_slim_meta
            .iter()
            .filter_map(|(&bid, meta)| {
                (meta.group_id == 10)
                    .then_some(meta.jump_target.map(|(_, dest_sys)| (bid, dest_sys)))
                    .flatten()
            })
            .collect();
        for (gate_id, dest_sys) in gate_dests {
            let state = if !lazy_gates || self.is_system_loaded(dest_sys) {
                eve_destiny::STARGATE_ACTIVATION_STATE_OPEN
            } else {
                eve_destiny::STARGATE_ACTIVATION_STATE_CLOSED
            };
            initial_ballpark.init_stargate_activation_state(gate_id, state);
        }
        let db_read_safe = db_opt.is_some()
            && !held_lock_ranks().iter().any(|&r| r >= eve_db::lock_rank::RANK_WORLD_INNER);
        if db_read_safe {
            crate::services::cluster_macho_misc_services::populate_shipcaster_balls_from_db(
                solar_system_id,
                &mut initial_ballpark,
                db_opt.as_ref(),
            );
        }
        let mut initial_asteroids = HashMap::new();
        let mut initial_asteroid_templates = HashMap::new();
        let mut initial_warp_state = types::ActorWarpState::default();
        let initial_pending_disruptors = if let (Some(sde), Some(db)) = (&sde_opt, &db_opt) {
            if db_read_safe {
                let first_sde_belt_ast_id =
                    crate::system_actor::belts::belt_asteroid_base_id(solar_system_id);
                let has_persisted_belt_asteroids =
                    db.with_snapshot(|s| s.asteroids.contains_key(&first_sde_belt_ast_id));
                if has_persisted_belt_asteroids {
                    let spawned = populate_asteroid_belt_entities(&mut initial_ballpark, solar_system_id, sde);
                    let _ = reconcile_spawned_belt_asteroids(
                        &mut initial_ballpark,
                        solar_system_id,
                        spawned,
                        &mut initial_asteroids,
                        &mut initial_asteroid_templates,
                        &db_opt,
                        belt_interval_atomic.load(std::sync::atomic::Ordering::Relaxed),
                        downtime_utc_atomic.load(std::sync::atomic::Ordering::Relaxed),
                    );
                }
                let pending = crate::sp6_lifecycle::populate_persisted_space_objects_with_warps(
                    &mut initial_ballpark,
                    solar_system_id,
                    sde,
                    db,
                    &mut initial_warp_state,
                );
                crate::services::cluster_macho_misc_services::populate_shipcaster_balls_with_sde(
                    solar_system_id,
                    &mut initial_ballpark,
                    Some(sde),
                    Some(db),
                );
                pending
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let ball_positions: Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>> =
            Arc::new(RankedRwLock::new(HashMap::new()));
        let scrambled_balls: Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>> =
            Arc::new(RankedRwLock::new(std::collections::HashSet::new()));
        let tethered_balls: Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>> =
            Arc::new(RankedRwLock::new(std::collections::HashSet::new()));
        let committed_warp_balls: Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>> =
            Arc::new(RankedRwLock::new(HashMap::new()));
        let warp_stopped_at: Arc<RankedRwLock<HashMap<BallId, tokio::time::Instant>, RANK_LEAF>> =
            Arc::new(RankedRwLock::new(HashMap::new()));
        let active_module_ships: Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>> =
            Arc::new(RankedRwLock::new(std::collections::HashSet::new()));
        let hibernated = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hibernated_since_unix = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let idle_ticks = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let dynamic_ball_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pending_destroyed_balls = PendingDestroyedOutbox::default();
        let mut initial_actor_state = SystemActorState {
            asteroids: initial_asteroids,
            asteroid_templates: initial_asteroid_templates,
            pending_anchoring_disruptors: initial_pending_disruptors,
            sde_opt,
            db_opt,
            mechanic_rng_opt: Some(mechanic_rng.clone()),
            tactical_state: self.tactical_state.clone(),
            module_reload_sim_deadlines: self.module_reload_sim_deadlines.clone(),
            pending_notifications_opt: self.pending_notifications_opt.clone(),
            notify_tx_opt: self.notify_tx_opt.clone(),
            hibernated: Arc::clone(&hibernated),
            hibernated_since_unix: Arc::clone(&hibernated_since_unix),
            idle_ticks: Arc::clone(&idle_ticks),
            dynamic_ball_count: Arc::clone(&dynamic_ball_count),
            pending_destroyed_balls: pending_destroyed_balls.clone(),
            warp_state: initial_warp_state,
            ..Default::default()
        };
        let broadcast_seq_gate_actor = Arc::clone(&initial_actor_state.broadcast_seq_gate);
        if db_read_safe {
            let dyn_ids: Vec<BallId> = initial_ballpark
                .balls
                .keys()
                .copied()
                .filter(|&bid| !initial_ballpark.is_static_ball(bid))
                .collect();
            for bid in dyn_ids {
                celestials::record_spawned_ball_checkpoint_baseline(
                    &mut initial_actor_state,
                    &initial_ballpark,
                    bid,
                );
            }
        }
        let staged_claims = {
            let mut seed_ctx = SystemContentSeedContext {
                solar_system_id,
                wake_reason,
                registry: self,
                ballpark: &mut initial_ballpark,
                actor_state: &mut initial_actor_state,
                sde: self.sde.as_ref(),
                db: self.db.as_ref(),
                db_read_safe,
                belt_respawn_interval_secs: belt_interval_atomic
                    .load(std::sync::atomic::Ordering::Relaxed),
                downtime_utc_secs_of_day: downtime_utc_atomic
                    .load(std::sync::atomic::Ordering::Relaxed),
                mechanic_rng: &mechanic_rng,
                staged_special_site_balls: std::collections::HashSet::new(),
                staged_wh_ore_anomalies: std::collections::HashSet::new(),
            };
            self.content_providers.seed_all(&mut seed_ctx)?;
            lifecycle_sm::StagedSeedClaims {
                special_site_balls: seed_ctx.staged_special_site_balls,
                wh_ore_anomalies: seed_ctx.staged_wh_ore_anomalies,
            }
        };
        register_committed_warp_cache(&ball_positions, &committed_warp_balls);
        register_tethered_balls_cache(&ball_positions, &tethered_balls);
        sync_ball_positions(&initial_ballpark, &ball_positions, &scrambled_balls);
        celestials::update_system_hibernation_flag(&initial_actor_state, &initial_ballpark, 0);
        protection::sync_all_protection_registry_in_actor(&initial_ballpark, &mut initial_actor_state);
        let initial_tidi = self.cluster_nodes.read().ok()
            .and_then(|cn| cn.manual_tidi_overrides.get(&solar_system_id).copied())
            .or_else(|| self.db.as_ref().and_then(|db| db.with_snapshot(|snap| {
                let nid = -(7_500_000_000_000_i64.saturating_add(solar_system_id as i64));
                snap.owner_notes.get(&nid).and_then(|n| serde_json::from_str::<f64>(&n.note).ok())
            })))
            .unwrap_or(1.0).clamp(0.10, 1.0);
        let initial_view = SystemView::from_ballpark(
            &initial_ballpark,
            Some(&initial_actor_state),
            initial_tidi,
            None,
        );
        let (view_tx, view_rx) = tokio::sync::watch::channel(Arc::new(initial_view));
        let view_tx = Arc::new(view_tx);
        let tidi_factor_bits = Arc::new(std::sync::atomic::AtomicU64::new(initial_tidi.to_bits()));
        let ballpark_shared: Arc<BallparkHandle> = Arc::new(BallparkHandle::new(
            initial_ballpark,
            Arc::clone(&view_tx),
            Arc::clone(&tidi_factor_bits),
        ));
        let actor_state_shared: Arc<RankedRwLock<SystemActorState, RANK_ACTOR_STATE>> =
            Arc::new(RankedRwLock::new(initial_actor_state));
        ballpark_shared.attach_actor_state(&actor_state_shared);
        Ok(PreparedSystemActivation {
            solar_system_id,
            lazy_gates,
            ball_positions,
            scrambled_balls,
            tethered_balls,
            committed_warp_balls,
            warp_stopped_at,
            active_module_ships,
            hibernated,
            hibernated_since_unix,
            idle_ticks,
            dynamic_ball_count,
            pending_destroyed_balls,
            broadcast_seq_gate_actor,
            tidi_factor_bits,
            view_tx,
            view_rx,
            ballpark_shared,
            actor_state_shared,
            staged_claims,
        })
    }

    pub(crate) fn spawn_prepared_system_actor(
        &self,
        rt_handle: &tokio::runtime::Handle,
        prepared: PreparedSystemActivation,
    ) -> (SystemHandle, bool) {
        let PreparedSystemActivation {
            solar_system_id,
            lazy_gates,
            ball_positions,
            scrambled_balls,
            tethered_balls,
            committed_warp_balls,
            warp_stopped_at,
            active_module_ships,
            hibernated,
            hibernated_since_unix,
            idle_ticks,
            dynamic_ball_count,
            pending_destroyed_balls,
            broadcast_seq_gate_actor,
            tidi_factor_bits,
            view_tx,
            view_rx,
            ballpark_shared,
            actor_state_shared,
            staged_claims: _,
        } = prepared;
        let (cmd_tx, cmd_rx) = mpsc::channel::<SystemCommand>(SYSTEM_MAILBOX_CAPACITY);
        let (event_tx, _) = broadcast::channel::<TickBroadcast>(4096);
        let event_tx_actor = event_tx.clone();
        let belt_interval_atomic = self.belt_respawn_interval_secs.clone();
        let downtime_utc_atomic = self.downtime_utc_secs_of_day.clone();
        let mechanic_rng = self.mechanic_rng.clone();
        let module_deactivation_hook_actor = self.module_deactivation_hook.clone();
        let target_lock_completed_hook_actor = self.target_lock_completed_hook.clone();
        let post_unlock_actions_actor = self.post_unlock_actions.clone();
        let ball_positions_actor = ball_positions.clone();
        let scrambled_balls_actor = scrambled_balls.clone();
        let warp_stopped_at_actor = warp_stopped_at.clone();
        let active_module_ships_actor = active_module_ships.clone();
        let pending_destroyed_actor = pending_destroyed_balls.clone();
        let view_tx_actor = Arc::clone(&view_tx);
        let tidi_factor_bits_actor = tidi_factor_bits.clone();
        let ballpark_actor = ballpark_shared.clone();
        let actor_state_actor = actor_state_shared.clone();
        let last_auto_stepped_unix = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let sim_elapsed_secs_bits = Arc::new(std::sync::atomic::AtomicU64::new(0.0_f64.to_bits()));
        let sim_elapsed_secs_bits_actor = sim_elapsed_secs_bits.clone();
        let last_visible_stamp = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let last_visible_stamp_actor = last_visible_stamp.clone();
        let last_fx_stamp = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let last_fx_stamp_actor = last_fx_stamp.clone();
        let unrebased_clock_offset_secs_actor = Arc::clone(&self.unrebased_clock_offset_secs);
        let explicit_update_state_guards_actor = Arc::clone(&self.explicit_update_state_guards);
        let command_panics = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let command_panics_actor = command_panics.clone();
        #[cfg(test)]
        let panic_next_command_for_test = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(test)]
        let panic_next_command_actor = panic_next_command_for_test.clone();
        #[cfg(test)]
        let panic_on_command_countdown_for_test = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        #[cfg(test)]
        let panic_on_command_countdown_actor = panic_on_command_countdown_for_test.clone();
        #[cfg(test)]
        let panic_after_destruction_for_test = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(test)]
        let panic_after_destruction_actor = panic_after_destruction_for_test.clone();
        #[cfg(test)]
        let before_step_reply_hook_for_test: Arc<
            RankedMutex<Option<Box<dyn FnOnce() + Send>>, RANK_LEAF>,
        > = Arc::new(RankedMutex::new(None));
        #[cfg(test)]
        let before_step_reply_hook_actor = before_step_reply_hook_for_test.clone();

        struct ShutdownReplyGuard(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for ShutdownReplyGuard {
            fn drop(&mut self) {
                if let Some(reply) = self.0.take() {
                    let _ = reply.send(());
                }
            }
        }

        rt_handle.spawn(async move {
            // Drop actor `Arc`s before closing `cmd_rx` and sending `shutdown_reply` (`#1542` R1 P2).
            let mut shutdown_reply = ShutdownReplyGuard(None);
            let mut cmd_rx = cmd_rx;
            let (ballpark_actor, actor_state_actor, view_tx_actor, ball_positions_actor, scrambled_balls_actor, warp_stopped_at_actor, active_module_ships_actor, pending_destroyed_actor, event_tx_actor) =
                (ballpark_actor, actor_state_actor, view_tx_actor, ball_positions_actor, scrambled_balls_actor, warp_stopped_at_actor, active_module_ships_actor, pending_destroyed_actor, event_tx_actor);
            let mut last_active_totals: HashMap<BallId, (Option<i32>, Option<i32>)> = HashMap::new();
            let mut ball_spawn_tokens: HashMap<BallId, u64> = HashMap::new();
            // parity: feat:lifecycle.world-unobserved-system-sim, plan:SP-7
            const MAX_ACTOR_COMMAND_BATCH: usize = 64;
            let is_batch_barrier_cmd = |cmd: &SystemCommand| -> bool {
                matches!(
                    cmd,
                    SystemCommand::StepManual { .. }
                        | SystemCommand::StepManualTickOnly { .. }
                        | SystemCommand::FireWeaponCycleVolley { .. }
                        | SystemCommand::FlushConditionCheckpoint { .. }
                )
            };
            while let Some(first_cmd) = cmd_rx.recv().await {
                if let SystemCommand::ShutdownActor { reply } = first_cmd {
                    shutdown_reply.0 = Some(reply);
                    break;
                }
                let mut shutdown_requested = false;
                let stop_after_first = is_batch_barrier_cmd(&first_cmd);
                let mut cmd_batch = Vec::with_capacity(1);
                cmd_batch.push(first_cmd);
                if !stop_after_first {
                    while cmd_batch.len() < MAX_ACTOR_COMMAND_BATCH {
                        match cmd_rx.try_recv() {
                            Ok(SystemCommand::ShutdownActor { reply }) => {
                                shutdown_reply.0 = Some(reply);
                                shutdown_requested = true;
                                break;
                            }
                            Ok(next_cmd) => {
                                let stop_after = is_batch_barrier_cmd(&next_cmd);
                                cmd_batch.push(next_cmd);
                                if stop_after {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }

                let mut batch_view_dirty = false;
                let mut batch_deferred_replies: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
                let mut batch_delivery: Option<PendingActorDelivery> = None;

                for cmd in cmd_batch {
                let cmd_may_mutate_view = cmd.may_mutate_system_view();
                let cmd_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                #[cfg(test)]
                let countdown_hit = {
                    let rem = panic_on_command_countdown_actor
                        .load(std::sync::atomic::Ordering::SeqCst);
                    if rem > 0 {
                        panic_on_command_countdown_actor
                            .store(rem - 1, std::sync::atomic::Ordering::SeqCst);
                    }
                    rem == 1
                };
                #[cfg(test)]
                let should_panic =
                    panic_next_command_actor.swap(false, std::sync::atomic::Ordering::SeqCst)
                        || countdown_hit;
                #[cfg(test)]
                if should_panic {
                    panic!("injected SystemActor command panic for #841 supervision test");
                }
                let mut deferred_replies: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
                let mut cmd_slim_changed = false;
                let actor_delivery_out: Option<PendingActorDelivery> = {
                let mut ballpark = ballpark_actor.write_untracked().unwrap_or_else(|e| e.into_inner());
                let mut actor_st = actor_state_actor.write().unwrap_or_else(|e| e.into_inner());
                let out = (|| -> Option<PendingActorDelivery> {
                let mut pending_step: Option<(f64, StepReply)> = None;
                let mut pending_volley: Option<(
                    tokio::sync::oneshot::Sender<WeaponVolleyOutput>,
                    WeaponVolleyOutput,
                )> = None;
                let mut spawned_checkpoint_ball: Option<BallId> = None;
                let mut removed_checkpoint_ball: Option<BallId> = None;
                let mut flush_checkpoint_reply: Option<
                    tokio::sync::oneshot::Sender<Vec<ConditionCheckpointEntry>>,
                > = None;
                let mut exec_ctx = ActorExecCtx {
                    solar_system_id,
                    ballpark: &mut ballpark,
                    actor_st: &mut actor_st,
                    ball_spawn_tokens: &mut ball_spawn_tokens,
                    last_active_totals: &mut last_active_totals,
                    deferred_replies: &mut deferred_replies,
                    spawned_checkpoint_ball: &mut spawned_checkpoint_ball,
                    removed_checkpoint_ball: &mut removed_checkpoint_ball,
                    ball_positions_actor: &ball_positions_actor,
                    scrambled_balls_actor: &scrambled_balls_actor,
                    warp_stopped_at_actor: &warp_stopped_at_actor,
                    active_module_ships_actor: &active_module_ships_actor,
                    module_deactivation_hook_actor: &module_deactivation_hook_actor,
                    target_lock_completed_hook_actor: &target_lock_completed_hook_actor,
                    tidi_factor_bits_actor: &tidi_factor_bits_actor,
                    sim_elapsed_secs_bits_actor: &sim_elapsed_secs_bits_actor,
                    last_visible_stamp_actor: &last_visible_stamp_actor,
                    last_fx_stamp_actor: &last_fx_stamp_actor,
                    broadcast_seq_gate_actor: &broadcast_seq_gate_actor,
                    event_tx_actor: &event_tx_actor,
                    pending_destroyed_actor: &pending_destroyed_actor,
                    unrebased_clock_offset_secs_actor: &unrebased_clock_offset_secs_actor,
                    explicit_update_state_guards_actor: &explicit_update_state_guards_actor,
                    mechanic_rng: &mechanic_rng,
                };
                if let Some(cmd) = protection::handle_protection_or_lifecycle_command(&mut exec_ctx, cmd) {
                    if let Some(cmd) = activation_ops::handle_activation_and_movement_command(&mut exec_ctx, cmd) {
                        if let Some(cmd) = cycles_tackle::handle_tackle_or_damage_command(&mut exec_ctx, cmd) {
                            if let Some(cmd) = ball_ops::handle_ball_ops_command(&mut exec_ctx, cmd) {
                            if let Some(cmd) = drones::handle_drone_runtime_command(&mut exec_ctx, cmd) {
                            let mut sync_protection_ball: Option<BallId> = None;
                            {
                                let ballpark = &mut *exec_ctx.ballpark;
                                let deferred_replies = &mut *exec_ctx.deferred_replies;
                                let ball_spawn_tokens = &mut *exec_ctx.ball_spawn_tokens;
                                let spawned_checkpoint_ball = &mut *exec_ctx.spawned_checkpoint_ball;
                                let ball_positions_actor = exec_ctx.ball_positions_actor;
                                let scrambled_balls_actor = exec_ctx.scrambled_balls_actor;
                                let module_deactivation_hook_actor = exec_ctx.module_deactivation_hook_actor;
                                let last_visible_stamp_actor = exec_ctx.last_visible_stamp_actor;
                                let last_fx_stamp_actor = exec_ctx.last_fx_stamp_actor;
                                let broadcast_seq_gate_actor = exec_ctx.broadcast_seq_gate_actor;
                                let event_tx_actor = exec_ctx.event_tx_actor;
                                let pending_destroyed_actor = exec_ctx.pending_destroyed_actor;
                                let mechanic_rng = exec_ctx.mechanic_rng;
                                let SystemActorState {
                                    active_module_cycles,
                                    active_nanite_repairs,
                                    rah_incoming_damage,
                                    base_scramble_strengths,
                                    base_core_strengths,
                                    asteroids,
                                    asteroid_templates,
                                    sde_opt,
                                    db_opt,
                                    tactical_state,
                                    pending_notifications_opt,
                                    notify_tx_opt,
                                    weapon_cycle_start_modules,
                                    pending_missile_launches,
                                    warp_state,
                                    external_active_modules,
                                    protection_registry,
                                    active_drone_miners,
                                    drone_next_ready_unix,
                                    ..
                                } = &mut *exec_ctx.actor_st;
                                match cmd {
                            SystemCommand::SpawnBall { ball, reply } => {
                                let ball_id = ball.id;
                                ball_spawn_tokens.remove(&ball_id);
                                let was_warping = matches!(
                                    ballpark.balls.get(&ball_id).map(|b| &b.mode),
                                    Some(eve_destiny::MovementMode::Warp { .. })
                                );
                                let is_warp = matches!(ball.mode, eve_destiny::MovementMode::Warp { .. });
                                let existing_meta = ballpark.ball_slim_meta.get(&ball_id).cloned();
                                if ballpark.balls.contains_key(&ball_id) {
                                    let _ = ballpark.remove_ball(ball_id);
                                }
                                if let Some(meta) = existing_meta {
                                    ballpark.set_ball_slim_meta(ball_id, meta);
                                } else {
                                    ensure_default_slim_meta(ballpark, &ball, sde_opt);
                                }
                                if should_pin_stationary_ball(&ball, ballpark.ball_slim_meta.get(&ball_id)) {
                                    ballpark.mark_ball_non_free(ball_id, false);
                                }
                                ballpark.add_ball(ball);
                                if was_warping && !is_warp {
                                    warp_state.clear_ball(ball_id, true);
                                }
                                sync_protection_ball = Some(ball_id);
                                *spawned_checkpoint_ball = Some(ball_id);
                                sync_ball_positions(ballpark, ball_positions_actor, scrambled_balls_actor);
                                defer_reply(deferred_replies, reply, Ok(ballpark.current_tick));
                            }
                            SystemCommand::SetBallMaxVelocity { ball_id, max_velocity, reply } => {
                                defer_reply(deferred_replies, reply, ballpark.set_ball_max_velocity(ball_id, max_velocity));
                            }
                            SystemCommand::SetBallMaxVelocityCap { ball_id, cap, reply } => {
                                defer_reply(deferred_replies, reply, ballpark.set_ball_max_velocity_cap(ball_id, cap));
                            }
                            SystemCommand::ChangeSlimItem {
                                ball_id,
                                slim_meta,
                                reply,
                            } => {
                                let changed = ballpark.change_slim_item(ball_id, slim_meta);
                                if changed {
                                    sync_protection_ball = Some(ball_id);
                                    *spawned_checkpoint_ball = Some(ball_id);
                                }
                                defer_reply(deferred_replies, reply, changed);
                            }
                            SystemCommand::SetBallWormholeStages {
                                ball_id,
                                wormhole_age,
                                wormhole_size,
                                reply,
                            } => {
                                let changed = ballpark.set_ball_wormhole_stages(
                                    ball_id,
                                    wormhole_age,
                                    wormhole_size,
                                );
                                defer_reply(deferred_replies, reply, changed);
                            }
                            SystemCommand::GetBallSlimItem { ball_id, reply } => {
                                let slim = ballpark.balls.get(&ball_id).map(|b| {
                                    let meta = eve_destiny::wire::resolve_slim_item_meta(
                                        Some(ballpark),
                                        b,
                                    );
                                    eve_destiny::wire::encode_slim_item_with_park(
                                        Some(ballpark),
                                        b,
                                        &meta,
                                    )
                                });
                                defer_reply(deferred_replies, reply, slim);
                            }
                            SystemCommand::SpawnAsteroid { asteroid, ball, reply } => {
                                let ball_id = asteroid.asteroid_id;
                                if asteroids.contains_key(&ball_id) {
                                    defer_reply(deferred_replies, reply, Ok(ball_id));
                                    return None;
                                }
                                let mut ast_ball = ball;
                                SystemHandle::spawn_asteroid_ball_in_ballpark(
                                    ballpark,
                                    &sde_opt,
                                    &mut ast_ball,
                                );
                                if let Some(db) = &db_opt {
                                    if db.get_asteroid(ball_id).is_none() {
                                        let _ = db.spawn_asteroid(eve_db::AsteroidRecord {
                                            asteroid_id: ball_id,
                                            belt_id: asteroid.belt_id,
                                            type_id: ast_ball.type_id,
                                            solar_system_id,
                                            position: (ast_ball.position.x, ast_ball.position.y, ast_ball.position.z),
                                            quantity: i64::try_from(asteroid.remaining_units).unwrap_or(i64::MAX),
                                            initial_quantity: i64::try_from(asteroid.initial_units).unwrap_or(i64::MAX),
                                            radius: ast_ball.radius,
                                        });
                                    }
                                }
                                asteroid_templates.insert(ball_id, (asteroid.clone(), ast_ball));
                                asteroids.insert(ball_id, asteroid);
                                sync_ball_positions(ballpark, ball_positions_actor, scrambled_balls_actor);
                                defer_reply(deferred_replies, reply, Ok(ball_id));
                            }
                            SystemCommand::MineAsteroid {
                                asteroid_id,
                                requested_units,
                                waste_probability,
                                persist_db,
                                reply,
                            } => {
                                let res = handle_mine_asteroid(
                                    solar_system_id,
                                    asteroid_id,
                                    requested_units,
                                    waste_probability,
                                    persist_db,
                                    ballpark,
                                    asteroids,
                                    mechanic_rng,
                                    db_opt,
                                    &belt_interval_atomic,
                                    &downtime_utc_atomic,
                                );
                                sync_ball_positions(ballpark, ball_positions_actor, scrambled_balls_actor);
                                defer_reply(deferred_replies, reply, res);
                            }
                            SystemCommand::GetAsteroidState { asteroid_id, reply } => {
                                defer_reply(deferred_replies, reply, asteroids.get(&asteroid_id).cloned());
                            }
                            SystemCommand::PopulateAsteroids { reply } => {
                                let first_sde_belt_ast_id =
                                    crate::system_actor::belts::belt_asteroid_base_id(solar_system_id);
                                let mut populated = if !asteroid_templates.contains_key(&first_sde_belt_ast_id) {
                                    if let Some(sde) = &sde_opt {
                                        let spawned = populate_asteroid_belt_entities(ballpark, solar_system_id, sde);
                                        let reconciled = reconcile_spawned_belt_asteroids(
                                            ballpark,
                                            solar_system_id,
                                            spawned,
                                            asteroids,
                                            asteroid_templates,
                                            db_opt,
                                            belt_interval_atomic.load(std::sync::atomic::Ordering::Relaxed),
                                            downtime_utc_atomic.load(std::sync::atomic::Ordering::Relaxed),
                                        );
                                        let newly_added_balls: Vec<Ball> = reconciled
                                            .iter()
                                            .filter(|a| !a.depleted)
                                            .filter_map(|a| ballpark.balls.get(&a.asteroid_id).cloned())
                                            .collect();
                                        if !newly_added_balls.is_empty() {
                                            ballpark.queue_event_for_all_grid_observers(
                                                None,
                                                &DestinyEvent::AddBalls2 {
                                                    balls: newly_added_balls,
                                                },
                                            );
                                        }
                                        reconciled
                                    } else {
                                        Vec::new()
                                    }
                                } else {
                                    Vec::new()
                                };
                                for ast in asteroids.values() {
                                    if !populated.iter().any(|p| p.asteroid_id == ast.asteroid_id) {
                                        populated.push(ast.clone());
                                    }
                                }
                                populated.sort_by_key(|a| a.asteroid_id);
                                sync_ball_positions(ballpark, ball_positions_actor, scrambled_balls_actor);
                                defer_reply(deferred_replies, reply, populated);
                            }
                            SystemCommand::GetSetState {
                                observer_ball_id,
                                reply,
                            } => {
                                let res = get_set_state_in_actor(
                                    ballpark,
                                    solar_system_id,
                                    observer_ball_id,
                                    last_visible_stamp_actor,
                                    broadcast_seq_gate_actor,
                                )
                                .map(|(tick, payload, update_list, _seq)| {
                                    (tick, payload, update_list)
                                });
                                defer_reply(deferred_replies, reply, res);
                            }
                            SystemCommand::GetSetStateWithSeq {
                                observer_ball_id,
                                reply,
                            } => {
                                let res = get_set_state_in_actor(
                                    ballpark,
                                    solar_system_id,
                                    observer_ball_id,
                                    last_visible_stamp_actor,
                                    broadcast_seq_gate_actor,
                                );
                                defer_reply(deferred_replies, reply, res);
                            }
                            SystemCommand::DrainObserverUpdates {
                                observer_ball_id,
                                reply,
                            } => {
                                let (tick, events, pyrep, _seq, slim_changed) =
                                    drain_observer_updates_in_actor(
                                        ballpark,
                                        db_opt,
                                        observer_ball_id,
                                        last_visible_stamp_actor,
                                        last_fx_stamp_actor,
                                        broadcast_seq_gate_actor,
                                    );
                                cmd_slim_changed |= slim_changed;
                                defer_reply(deferred_replies, reply, (tick, events, pyrep));
                            }
                            SystemCommand::DrainObserverUpdatesWithSeq {
                                observer_ball_id,
                                reply,
                            } => {
                                let (tick, events, pyrep, seq, slim_changed) =
                                    drain_observer_updates_in_actor(
                                        ballpark,
                                        db_opt,
                                        observer_ball_id,
                                        last_visible_stamp_actor,
                                        last_fx_stamp_actor,
                                        broadcast_seq_gate_actor,
                                    );
                                cmd_slim_changed |= slim_changed;
                                defer_reply(deferred_replies, reply, (tick, events, pyrep, seq));
                            }
                            SystemCommand::StepManual { dt, reply } => {
                                pending_step = Some((dt, StepReply::Full(reply)));
                            }
                            SystemCommand::StepManualTickOnly { dt, reply } => {
                                pending_step = Some((dt, StepReply::TickOnly(reply)));
                            }
                            SystemCommand::FlushConditionCheckpoint { reply } => {
                                flush_checkpoint_reply = Some(reply);
                            }
                            SystemCommand::SetCapacitor { ball_id, capacitor, reply } => {
                                if !ballpark.balls.contains_key(&ball_id) {
                                    defer_reply(deferred_replies, reply, Err(format!("Ball {ball_id} is not in space")));
                                    return None;
                                }
                                let Some(cs) = ballpark.combat_states.get_mut(&ball_id) else {
                                    defer_reply(deferred_replies, reply, Err(format!("Ball {ball_id} has no combat state")));
                                    return None;
                                };
                                cs.capacitor = capacitor;
                                if let Some(db) = &db_opt {
                                    let cloned = cs.clone();
                                    crate::sp6_lifecycle::persist_existing_ship_combat_hp_for_system_in_db(
                                        db,
                                        solar_system_id,
                                        std::iter::once((ball_id, &cloned)),
                                    );
                                }
                                defer_reply(deferred_replies, reply, Ok(()));
                            }
                            SystemCommand::HealCombatState { ball_id, reply } => {
                                if !ballpark.balls.contains_key(&ball_id) {
                                    defer_reply(deferred_replies, reply, Err(format!("Ball {ball_id} is not in space")));
                                    return None;
                                }
                                let grid_id = ballpark.ball_grid_ids.get(&ball_id).copied();
                                let Some(cs) = ballpark.combat_states.get_mut(&ball_id) else {
                                    defer_reply(deferred_replies, reply, Err(format!("Ball {ball_id} has no combat state")));
                                    return None;
                                };
                                cs.shield_hp = cs.max_shield_hp;
                                cs.armor_hp = cs.max_armor_hp;
                                cs.structure_hp = cs.max_structure_hp;
                                cs.capacitor = cs.max_capacitor;
                                let cloned = cs.clone();
                                // Queue authoritative Destiny `OnDamageStateChange` / `OnFleetDamageStateChange`
                                // (`[1.0, 1.0, 1.0]`) in the ballpark so V24 in-space clients receive `DoDestinyUpdate`
                                // when `/heal` restores ship HP (standalone MachoNet `OnDamageStateChange` is suppressed).
                                ballpark.emit_damage_and_destruction_events(
                                    ball_id, grid_id, 1.0, 1.0, 1.0, false,
                                );
                                if let Some(db) = &db_opt {
                                    let not_stale = db.with_snapshot(|snap| {
                                        !crate::sp6_lifecycle::is_ship_stale_for_in_space_write_in_snap(
                                            snap,
                                            ball_id,
                                            Some(solar_system_id),
                                            None,
                                        )
                                    });
                                    if not_stale {
                                        crate::sp6_lifecycle::persist_ship_combat_hp_in_db(db, ball_id, &cloned);
                                    }
                                }
                                defer_reply(deferred_replies, reply, Ok(cloned));
                            }
                            SystemCommand::RepairCombatState {
                                ball_id,
                                hull_heal,
                                armor_heal,
                                shield_heal,
                                reply,
                            } => {
                                let updated = apply_repair_combat_state_in_ballpark(
                                    ballpark,
                                    ball_id,
                                    hull_heal,
                                    armor_heal,
                                    shield_heal,
                                );
                                if updated.is_some() {
                                    sync_ball_positions(
                                        ballpark,
                                        ball_positions_actor,
                                        scrambled_balls_actor,
                                    );
                                }
                                if let (Some(db), Some(ref ucs)) = (&db_opt, &updated) {
                                    crate::sp6_lifecycle::persist_existing_ship_combat_hp_for_system_in_db(
                                        db,
                                        solar_system_id,
                                        std::iter::once((ball_id, ucs)),
                                    );
                                }
                                defer_reply(deferred_replies, reply, updated);
                            }
                            SystemCommand::FireWeaponCycleVolley { ship_id, module_id, reply } => {
                                let mut volley_destroyed = Vec::new();
                                let mut volley_actions = Vec::new();
                                if let Some(cycle) = active_module_cycles
                                    .iter()
                                    .find(|c| c.ship_id == ship_id && c.module_id == module_id)
                                    .cloned()
                                {
                                    weapon_cycle_start_modules.insert((ship_id, module_id));
                                    if cycle.weapon_is_guided_missile {
                                        missiles::enqueue_missile_launches_for_cycle(
                                            &cycle,
                                            ballpark,
                                            db_opt,
                                            pending_missile_launches,
                                        );
                                    } else {
                                        cycles::apply_weapon_cycle_damage(
                                            &cycle,
                                            ballpark,
                                            rah_incoming_damage,
                                            sde_opt,
                                            db_opt,
                                            &mut volley_actions,
                                            &mut volley_destroyed,
                                            Some(pending_destroyed_actor),
                                        );
                                    }
                                    cycles::flush_pending_cycle_damage_notifications_with_ballpark(
                                        ballpark,
                                        sde_opt.as_deref(),
                                        db_opt.as_ref(),
                                        pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>),
                                        notify_tx_opt.as_ref(),
                                        &mut volley_actions,
                                    );
                                    if !volley_destroyed.is_empty() {
                                        let dead_ids: Vec<BallId> =
                                            volley_destroyed.iter().map(|&(bid, _)| bid).collect();
                                        let deact_guard = module_deactivation_hook_actor
                                            .read()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .clone();
                                        cycles::teardown_attacker_locks_and_cycles_for_destroyed_balls(
                                            solar_system_id, ballpark,
                                            active_module_cycles,
                                            base_scramble_strengths,
                                            base_core_strengths,
                                            db_opt,
                                            pending_notifications_opt
                                                .as_deref()
                                                .map(|r| r as &dyn AnyRwLock<_>),
                                            notify_tx_opt.as_ref(),
                                            deact_guard.as_ref(),
                                            &dead_ids,
                                        );
                                        for &dead_id in &dead_ids {
                                            external_active_modules.retain(|&(sid, _)| sid != dead_id);
                                            protection_registry.remove(&dead_id);
                                            active_drone_miners.remove(&dead_id);
                                            drone_next_ready_unix.remove(&dead_id);
                                        }
                                        ballpark.pending_events.retain(|ev| match ev {
                                            DestinyEvent::TargetLockFailed { target_id, .. } => {
                                                !dead_ids.contains(target_id)
                                            }
                                            DestinyEvent::OnTarget { mode, target_id, .. }
                                                if mode == "lost" =>
                                            {
                                                !dead_ids.contains(target_id)
                                            }
                                            _ => true,
                                        });
                                    }
                                    if let Some(db) = db_opt.as_ref() {
                                        for &(target_ball_id, _) in &volley_destroyed {
                                            crate::services::tactical_module_mechanics::on_probe_ball_removed_in_actor(
                                                ballpark,
                                                db_opt,
                                                tactical_state.as_ref(),
                                                target_ball_id,
                                            );
                                            crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_in_ballpark(
                                                ballpark,
                                                db,
                                                solar_system_id,
                                                target_ball_id,
                                            );
                                        }
                                    }
                                    sync_ball_positions(ballpark, ball_positions_actor, scrambled_balls_actor);
                                }
                                let (checked_out_destroyed, destroyed_lease) =
                                    pending_destroyed_actor.checkout_entries(&volley_destroyed);
                                pending_volley = Some((
                                    reply,
                                    WeaponVolleyOutput {
                                        destroyed: checked_out_destroyed,
                                        destroyed_lease,
                                        actions: volley_actions,
                                    },
                                ));
                            }
                            SystemCommand::StartNaniteRepair { repair, reply } => {
                                if !active_nanite_repairs.iter().any(|r| r.ship_id == repair.ship_id && r.module_id == repair.module_id) {
                                    active_nanite_repairs.push(repair);
                                }
                                defer_reply(deferred_replies, reply, ());
                            }
                            SystemCommand::StopNaniteRepair { ship_id, module_id, reply } => {
                                let mut rem = None;
                                if let Some(pos) = active_nanite_repairs.iter().position(|r| r.ship_id == ship_id && r.module_id == module_id) {
                                    let r = active_nanite_repairs.remove(pos);
                                    let frac = (r.elapsed_s / r.duration_s.max(0.001)).clamp(0.0, 1.0);
                                    let rem_dmg = (r.initial_damage_hp * (1.0 - frac)).max(0.0);
                                    let used_units = if r.elapsed_s <= 1e-6 {
                                        0
                                    } else {
                                        ((r.paste_total_needed.max(1) as f64) * frac - 1e-9)
                                            .ceil()
                                            .clamp(0.0, r.paste_consumed_so_far as f64) as i32
                                    };
                                    let paste_refund = r.paste_consumed_so_far.saturating_sub(used_units);
                                    rem = Some((rem_dmg, paste_refund));
                                }
                                defer_reply(deferred_replies, reply, rem);
                            }
                            SystemCommand::RecordIncomingArmorDamage { ship_id, damage, reply } => {
                                let entry = rah_incoming_damage.entry(ship_id).or_insert([0.0; 4]);
                                for (dst, src) in entry.iter_mut().zip(damage.iter()) {
                                    *dst += *src;
                                }
                                defer_reply(deferred_replies, reply, ());
                            }
                            SystemCommand::FlushPeerObserverUpdates {
                                exclude_ball_id,
                                reply,
                            } => {
                                let mut flushed = 0usize;
                                if event_tx_actor.receiver_count() > 0 {
                                    cmd_slim_changed |= sync_player_slim_items(ballpark, db_opt);
                                    let stamp = ballpark.current_tick as u32;
                                    let ready_obs_ids: Vec<BallId> = ballpark
                                        .observer_events
                                        .iter()
                                        .filter_map(|(&obs_id, evs)| {
                                            if Some(obs_id) == exclude_ball_id
                                                || evs.is_empty()
                                                || ballpark
                                                    .pending_handoff_observers
                                                    .contains(&obs_id)
                                            {
                                                return None;
                                            }
                                            if evs
                                                .iter()
                                                .any(|ev| matches!(ev, DestinyEvent::SetState { .. }))
                                            {
                                                return None;
                                            }
                                            Some(obs_id)
                                        })
                                        .collect();
                                    if !ready_obs_ids.is_empty() {
                                        let mut observer_events = std::collections::BTreeMap::new();
                                        let mut observer_updates_pyrep =
                                            std::collections::BTreeMap::new();
                                        let mut flushed_max_stamp = u64::from(stamp);
                                        for obs_id in ready_obs_ids {
                                            let obs_evs = ballpark.drain_observer_events(obs_id);
                                            if !obs_evs.is_empty() {
                                                let fx_floor = last_visible_stamp_actor
                                                    .load(std::sync::atomic::Ordering::Acquire)
                                                    .max(last_fx_stamp_actor.load(std::sync::atomic::Ordering::Acquire))
                                                    as u32;
                                                flushed_max_stamp =
                                                    flushed_max_stamp.max(u64::from(stamp.max(fx_floor)));
                                                observer_updates_pyrep.insert(
                                                    obs_id,
                                                    belts::encode_destiny_updates_with_fx_floor(
                                                        stamp,
                                                        fx_floor,
                                                        &obs_evs,
                                                        Some(ballpark),
                                                        Some(last_fx_stamp_actor),
                                                    ),
                                                );
                                                observer_events.insert(obs_id, obs_evs);
                                            }
                                        }
                                        flushed = observer_events.len();
                                        if flushed > 0 {
                                            last_visible_stamp_actor.fetch_max(
                                                flushed_max_stamp,
                                                std::sync::atomic::Ordering::AcqRel,
                                            );
                                            send_tick_broadcast_sequenced_owned(
                                                event_tx_actor,
                                                broadcast_seq_gate_actor,
                                                TickBroadcast {
                                                    solar_system_id,
                                                    tick: ballpark.current_tick,
                                                    broadcast_seq: 0,
                                                    events: Vec::new(),
                                                    observer_events,
                                                    observer_updates_pyrep,
                                                },
                                            );
                                        }
                                    }
                                }
                                defer_reply(deferred_replies, reply, flushed);
                            }
                            _ => unreachable!("handled by system_actor command submodules"),
                                }
                            }
                            if let Some(bid) = sync_protection_ball {
                                protection::sync_ball_protection_in_actor(
                                    exec_ctx.ballpark,
                                    exec_ctx.actor_st,
                                    bid,
                                );
                            }
                            }
                            }
                        }
                    }
                }
                if let Some((dt, step_reply)) = pending_step {
                    Some(cycles_tackle::execute_pending_step_in_actor(
                        &mut exec_ctx,
                        dt,
                        step_reply,
                    ))
                } else {
                    if cmd_may_mutate_view {
                        protection::sync_active_module_ships_cache(
                            &exec_ctx.actor_st.active_module_cycles,
                            &exec_ctx.actor_st.external_active_modules,
                            exec_ctx.active_module_ships_actor,
                        );
                        if let Some(bid) = *exec_ctx.spawned_checkpoint_ball {
                            celestials::record_spawned_ball_checkpoint_baseline(
                                exec_ctx.actor_st,
                                exec_ctx.ballpark,
                                bid,
                            );
                        }
                        if let Some(bid) = *exec_ctx.removed_checkpoint_ball {
                            celestials::remove_ball_checkpoint_tracking(exec_ctx.actor_st, bid);
                        }
                    }
                    if let Some(reply) = flush_checkpoint_reply {
                        let entries = celestials::collect_all_condition_checkpoint_entries(
                            exec_ctx.actor_st,
                            exec_ctx.ballpark,
                        );
                        defer_reply(exec_ctx.deferred_replies, reply, entries);
                    }
                    if cmd_may_mutate_view {
                        celestials::update_system_hibernation_flag(
                            exec_ctx.actor_st,
                            exec_ctx.ballpark,
                            exec_ctx.event_tx_actor.receiver_count(),
                        );
                    }
                    pending_volley.map(|(reply, volley_out)| {
                        PendingActorDelivery::Volley(reply, volley_out)
                    })
                }
                })();
                out
                };
                (cmd_slim_changed, deferred_replies, actor_delivery_out)
                }));
                match cmd_outcome {
                    Ok((cmd_slim_changed, deferred_replies, actor_delivery_out)) => {
                        if cmd_may_mutate_view || cmd_slim_changed {
                            batch_view_dirty = true;
                        }
                        batch_deferred_replies.extend(deferred_replies);
                        if let Some(delivery) = actor_delivery_out {
                            batch_delivery = Some(delivery);
                        }
                    }
                    Err(panic_payload) => {
                        let panic_count = command_panics_actor.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        let msg = panic_payload
                            .downcast_ref::<&str>()
                            .copied()
                            .or_else(|| panic_payload.downcast_ref::<String>().map(String::as_str))
                            .unwrap_or("non-string panic payload");
                        tracing::error!(
                            solar_system_id,
                            panic_count,
                            panic = %msg,
                            "SystemActor caught panic while handling command; continuing actor loop"
                        );
                    }
                }
                }
                if batch_view_dirty {
                    let next_view = {
                        let ballpark = ballpark_actor.read().unwrap_or_else(|e| e.into_inner());
                        let actor_st = actor_state_actor.read().unwrap_or_else(|e| e.into_inner());
                        let cur_tidi = celestials::clamp_tidi_factor_bits(&tidi_factor_bits_actor);
                        let prev_view = { view_tx_actor.borrow().clone() };
                        SystemView::from_ballpark(
                            &ballpark,
                            Some(&actor_st),
                            cur_tidi,
                            Some(prev_view.as_ref()),
                        )
                    };
                    let _ = view_tx_actor.send(Arc::new(next_view));
                }
                for send_reply in batch_deferred_replies {
                    send_reply();
                }
                if let Some(actor_delivery) = batch_delivery {
                    let delivery_outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        #[cfg(test)]
                        if panic_after_destruction_actor
                            .swap(false, std::sync::atomic::Ordering::SeqCst)
                        {
                            panic!("injected post-destruction SystemActor step panic for #1168 test");
                        }
                        #[cfg(test)]
                        {
                            let hook = before_step_reply_hook_actor
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .take();
                            if let Some(hook) = hook {
                                hook();
                            }
                        }
                        match actor_delivery {
                            PendingActorDelivery::Step(step_reply, step_out) => match step_reply {
                                StepReply::Full(reply) | StepReply::TickOnly(reply) => {
                                    if let Err(unsent_step_out) = reply.send(step_out) {
                                        let retained_count = unsent_step_out.destroyed_balls.len();
                                        let step_tick = unsent_step_out.tick;
                                        drop(unsent_step_out);
                                        if retained_count > 0 {
                                            tracing::warn!(
                                                solar_system_id,
                                                tick = step_tick,
                                                retained_destroyed_balls = retained_count,
                                                "SystemActor step reply receiver dropped; destroyed_balls lease rolled back to pending outbox"
                                            );
                                        }
                                    }
                                }
                            },
                            PendingActorDelivery::Volley(reply, volley_out) => {
                                if let Err(unsent_volley_out) = reply.send(volley_out) {
                                    let retained_count = unsent_volley_out.destroyed.len();
                                    drop(unsent_volley_out);
                                    if retained_count > 0 {
                                        tracing::warn!(
                                            solar_system_id,
                                            retained_destroyed_balls = retained_count,
                                            "SystemActor volley reply receiver dropped; destroyed_balls lease rolled back to pending outbox"
                                        );
                                    }
                                }
                            }
                        }
                    }));
                    if let Err(panic_payload) = delivery_outcome {
                        let panic_count = command_panics_actor.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        let msg = panic_payload
                            .downcast_ref::<&str>()
                            .copied()
                            .or_else(|| panic_payload.downcast_ref::<String>().map(String::as_str))
                            .unwrap_or("non-string panic payload");
                        tracing::error!(
                            solar_system_id,
                            panic_count,
                            panic = %msg,
                            "SystemActor caught panic while delivering reply; continuing actor loop"
                        );
                    }
                }
                flush_post_unlock_actions(&post_unlock_actions_actor);
                if shutdown_requested {
                    break;
                }
            }
            drop((
                ballpark_actor, actor_state_actor, view_tx_actor, ball_positions_actor,
                scrambled_balls_actor, warp_stopped_at_actor, active_module_ships_actor,
                pending_destroyed_actor, event_tx_actor,
            ));
            cmd_rx.close();
            drop(cmd_rx);
            drop(shutdown_reply);
        });

        let handle = SystemHandle {
            solar_system_id,
            cmd_tx,
            event_tx,
            ball_positions,
            scrambled_balls,
            tethered_balls,
            committed_warp_balls,
            warp_stopped_at,
            active_module_ships,
            ballpark: ballpark_shared,
            actor_state: actor_state_shared,
            view_tx,
            view_rx,
            last_auto_stepped_unix,
            tidi_factor_bits,
            sim_elapsed_secs_bits,
            last_visible_stamp,
            last_fx_stamp,
            command_panics,
            pending_destroyed_balls,
            hibernated,
            hibernated_since_unix,
            idle_ticks,
            dynamic_ball_count,
            #[cfg(test)]
            panic_next_command_for_test,
            #[cfg(test)]
            panic_on_command_countdown_for_test,
            #[cfg(test)]
            panic_after_destruction_for_test,
            #[cfg(test)]
            before_step_reply_hook_for_test,
            npc_death_hook: Arc::clone(&self.npc_death_hook),
            module_deactivation_hook: Arc::clone(&self.module_deactivation_hook),
            target_lock_completed_hook: Arc::clone(&self.target_lock_completed_hook),
            system_step_hook: Arc::clone(&self.system_step_hook),
            post_unlock_actions: Arc::clone(&self.post_unlock_actions),
        };
        (handle, lazy_gates)
    }
}

pub(crate) fn apply_repair_combat_state_in_ballpark(
    bp: &mut Ballpark,
    ball_id: BallId,
    hull_heal: f64,
    armor_heal: f64,
    shield_heal: f64,
) -> Option<eve_destiny::CombatState> {
    let grid_id = bp.ball_grid_ids.get(&ball_id).copied();
    let cs = bp.combat_states.get_mut(&ball_id)?;
    let prev_s = cs.shield_hp;
    let prev_a = cs.armor_hp;
    let prev_h = cs.structure_hp;
    cs.shield_hp = (cs.shield_hp + shield_heal.max(0.0)).min(cs.max_shield_hp);
    cs.armor_hp = (cs.armor_hp + armor_heal.max(0.0)).min(cs.max_armor_hp);
    cs.structure_hp = (cs.structure_hp + hull_heal.max(0.0)).min(cs.max_structure_hp);
    let hp_changed = (cs.shield_hp - prev_s).abs() > 1e-6
        || (cs.armor_hp - prev_a).abs() > 1e-6
        || (cs.structure_hp - prev_h).abs() > 1e-6;
    let cloned = cs.clone();
    if hp_changed {
        let ratio = |cur: f64, max: f64| {
            if max > 0.0 {
                (cur / max).clamp(0.0, 1.0)
            } else {
                1.0
            }
        };
        bp.emit_damage_and_destruction_events(
            ball_id,
            grid_id,
            ratio(cloned.shield_hp, cloned.max_shield_hp),
            ratio(cloned.armor_hp, cloned.max_armor_hp),
            ratio(cloned.structure_hp, cloned.max_structure_hp),
            false,
        );
    }
    Some(cloned)
}

pub(crate) fn flush_post_unlock_actions(
    actions: &Arc<RankedMutex<Vec<PostUnlockHookAction>, RANK_LEAF>>,
) {
    if !held_lock_ranks().is_empty() {
        return;
    }
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
        }
    }
}

#[cfg(test)] #[path = "system_actor_tests.rs"] mod system_actor_tests;
#[cfg(test)] #[path = "system_actor_extra_tests.rs"] mod system_actor_extra_tests;
#[cfg(test)] #[path = "system_actor/cycles_extra_unit_tests.rs"] mod cycles_extra_unit_tests;
#[cfg(test)] #[path = "system_actor/lifecycle_tests.rs"] mod lifecycle_tests;
#[cfg(test)] #[path = "system_actor/drones_tests.rs"] mod drones_tests;
#[cfg(test)] #[path = "system_actor_combat_ai_deep_extra_unit_tests.rs"] mod system_actor_combat_ai_deep_extra_unit_tests;

