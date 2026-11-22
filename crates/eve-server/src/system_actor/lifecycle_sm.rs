//! Single-map generational lifecycle state machine for [`SystemRegistry`] (`#1542`).
//!
//! All per-system runtime residency sits in one [`SystemRegistry::systems`] slot under
//! `RANK_SYSTEM_REGISTRY`:
//! - Absent entry => [`SystemLifecycleState::Cold`]
//! - [`SystemLifecycleSlot::Loading`] => single-flight activation in progress with monotonic `generation`
//! - [`SystemLifecycleSlot::Live`] => live actor handle (hibernation queried live from the actor)
//! - [`SystemLifecycleSlot::Stopping`] => single-flight teardown/eviction in progress before `Cold`

#![forbid(unsafe_code)]

use super::celestials::{
    register_db_system_ballpark_with_generation, unregister_db_system_ballpark_if_generation,
};
use super::lifecycle::{
    ClusterTickWakeReason, LifecycleTransitionRecord, SessionArrivalWakeReason,
    SystemActivationError, SystemLifecycleState, SystemWakeReason,
};
#[cfg(test)]
use super::RankedMutex;
use super::{
    machonet_wallclock_ms, BallId, BallparkHandle, DVec3, PendingDestroyedOutbox,
    SystemActorState, SystemCommand, SystemHandle, SystemRegistry, SystemView, RANK_ACTOR_STATE,
};
use eve_db::lock_rank::{
    held_lock_ranks, RankedRwLock, RankedRwLockReadGuard, RankedRwLockWriteGuard, RANK_BALLPARK,
    RANK_LEAF, RANK_SYSTEM_REGISTRY,
};
use eve_sde::SolarSystemId;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

const DEFAULT_GATE_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TRANSITION_HISTORY: usize = 256;

pub(crate) struct PreparedSystemActivation {
    pub(in crate::system_actor) solar_system_id: SolarSystemId,
    pub(in crate::system_actor) lazy_gates: bool,
    pub(in crate::system_actor) ball_positions: Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    pub(in crate::system_actor) scrambled_balls: Arc<RankedRwLock<HashSet<BallId>, RANK_LEAF>>,
    pub(in crate::system_actor) tethered_balls: Arc<RankedRwLock<HashSet<BallId>, RANK_LEAF>>,
    pub(in crate::system_actor) committed_warp_balls: Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    pub(in crate::system_actor) warp_stopped_at:
        Arc<RankedRwLock<HashMap<BallId, tokio::time::Instant>, RANK_LEAF>>,
    pub(in crate::system_actor) active_module_ships: Arc<RankedRwLock<HashSet<BallId>, RANK_LEAF>>,
    pub(in crate::system_actor) hibernated: Arc<AtomicBool>,
    pub(in crate::system_actor) hibernated_since_unix: Arc<std::sync::atomic::AtomicU64>,
    pub(in crate::system_actor) idle_ticks: Arc<std::sync::atomic::AtomicU64>,
    pub(in crate::system_actor) dynamic_ball_count: Arc<std::sync::atomic::AtomicUsize>,
    pub(in crate::system_actor) pending_destroyed_balls: PendingDestroyedOutbox,
    pub(in crate::system_actor) broadcast_seq_gate_actor: Arc<std::sync::Mutex<u64>>,
    pub(in crate::system_actor) tidi_factor_bits: Arc<std::sync::atomic::AtomicU64>,
    pub(in crate::system_actor) view_tx: Arc<tokio::sync::watch::Sender<Arc<SystemView>>>,
    pub(in crate::system_actor) view_rx: tokio::sync::watch::Receiver<Arc<SystemView>>,
    pub(in crate::system_actor) ballpark_shared: Arc<BallparkHandle>,
    pub(in crate::system_actor) actor_state_shared:
        Arc<RankedRwLock<SystemActorState, RANK_ACTOR_STATE>>,
}

#[cfg(test)]
pub(crate) struct LifecycleTestHooks {
    pub(in crate::system_actor) before_publish_hook:
        RankedMutex<Option<Arc<dyn Fn(SolarSystemId, u64) + Send + Sync>>, RANK_LEAF>,
    pub(in crate::system_actor) before_evict_commit_hook:
        RankedMutex<Option<Arc<dyn Fn(SolarSystemId, u64) + Send + Sync>>, RANK_LEAF>,
    pub(in crate::system_actor) before_stop_finish_hook:
        RankedMutex<Option<Arc<dyn Fn(SolarSystemId, u64) + Send + Sync>>, RANK_LEAF>,
    pub(in crate::system_actor) force_publish_wouldblock: AtomicBool,
    pub(in crate::system_actor) gate_wait_timeout: RankedMutex<Option<Duration>, RANK_LEAF>,
}

#[cfg(test)]
impl Default for LifecycleTestHooks {
    fn default() -> Self {
        Self {
            before_publish_hook: RankedMutex::new(None),
            before_evict_commit_hook: RankedMutex::new(None),
            before_stop_finish_hook: RankedMutex::new(None),
            force_publish_wouldblock: AtomicBool::new(false),
            gate_wait_timeout: RankedMutex::new(None),
        }
    }
}

/// Single registry slot representation for [`SystemRegistry::systems`] (`#1542`).
/// Absent entry denotes [`SystemLifecycleState::Cold`].
pub(crate) enum SystemLifecycleSlot {
    Loading {
        generation: u64,
        gate: Arc<LifecycleGate>,
    },
    Live {
        generation: u64,
        handle: SystemHandle,
    },
    Stopping {
        generation: u64,
        gate: Arc<LifecycleGate>,
    },
}

impl SystemLifecycleSlot {
    pub(crate) fn generation(&self) -> u64 {
        match self {
            Self::Loading { generation, .. }
            | Self::Live { generation, .. }
            | Self::Stopping { generation, .. } => *generation,
        }
    }

    pub(crate) fn live_handle(&self) -> Option<&SystemHandle> {
        match self {
            Self::Live { handle, .. } if !handle.cmd_tx.is_closed() => Some(handle),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GateKind {
    Loading,
    Stopping,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GateResolution {
    Pending,
    Ready,
    SettledToCold,
    Aborted,
}

/// Gate coordinating concurrent callers while a single-flight `Loading` or `Stopping`
/// transition finishes outside the registry lock (`#1542`).
///
/// Stores only coordination state (never a [`SystemHandle`]) so `BallparkHandle` strong
/// refcounts are unaffected and stale handles can never be handed off after a concurrent stop (`R3-01`).
pub(crate) struct LifecycleGate {
    kind: GateKind,
    owner_thread: ThreadId,
    state: Mutex<GateResolution>,
    cvar: Condvar,
    async_notify: tokio::sync::Notify,
}

impl LifecycleGate {
    fn new(kind: GateKind) -> Self {
        Self {
            kind,
            owner_thread: std::thread::current().id(),
            state: Mutex::new(GateResolution::Pending),
            cvar: Condvar::new(),
            async_notify: tokio::sync::Notify::new(),
        }
    }

    fn resolve(&self, resolution: GateResolution) {
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*guard, GateResolution::Pending) {
            *guard = resolution;
            drop(guard);
            self.cvar.notify_all();
            self.async_notify.notify_waiters();
        }
    }

    /// Blocks a synchronous wake waiter until this gate settles or `timeout` expires.
    /// Must only be invoked AFTER releasing `SystemRegistry::systems` and never while holding
    /// ranked locks or DB write locks (`R1-03`).
    fn wait_for_slot_with_timeout(
        &self,
        solar_system_id: SolarSystemId,
        db_write_locked: bool,
        timeout: Duration,
    ) -> Result<GateResolution, SystemActivationError> {
        let current_thread = std::thread::current().id();
        if current_thread == self.owner_thread {
            return Err(match self.kind {
                GateKind::Loading => SystemActivationError::ReentrantLoading { solar_system_id },
                GateKind::Stopping => {
                    SystemActivationError::ConcurrentActivationAborted { solar_system_id }
                }
            });
        }
        if !held_lock_ranks().is_empty() || db_write_locked {
            return Err(SystemActivationError::WaitRejectedWhileHoldingLocks {
                solar_system_id,
            });
        }

        let deadline = Instant::now() + timeout;
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if !matches!(*guard, GateResolution::Pending) {
                return Ok(*guard);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(SystemActivationError::ConcurrentActivationAborted { solar_system_id });
            }
            let remaining = deadline.saturating_duration_since(now);
            let (next_guard, wait_res) = self
                .cvar
                .wait_timeout(guard, remaining)
                .unwrap_or_else(|e| e.into_inner());
            guard = next_guard;
            if wait_res.timed_out() && matches!(*guard, GateResolution::Pending) {
                return Err(SystemActivationError::ConcurrentActivationAborted { solar_system_id });
            }
        }
    }

    async fn wait_async_until_settled(&self) -> GateResolution {
        loop {
            let notified = self.async_notify.notified();
            tokio::pin!(notified);
            let _registered = notified.as_mut().enable();
            let current = *self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !matches!(current, GateResolution::Pending) {
                return current;
            }
            notified.await;
        }
    }
}

#[derive(Clone)]
struct WakeReasonEntry {
    generation: u64,
    reason: SystemWakeReason,
}

/// Generational wake-reason and transition telemetry owned inside [`SystemRegistry`] (`#1542`).
/// Mutated under `RANK_SYSTEM_REGISTRY` -> `RANK_LEAF` and independently gated by monotonic
/// `generation` so older or retired generations can never clobber newer entries (`R2-02`).
#[derive(Default)]
pub(crate) struct LifecycleTelemetryStore {
    wake_reasons: HashMap<SolarSystemId, WakeReasonEntry>,
    retired_generations: HashMap<SolarSystemId, u64>,
    transitions: VecDeque<LifecycleTransitionRecord>,
}

impl LifecycleTelemetryStore {
    fn push_transition(&mut self, record: LifecycleTransitionRecord) {
        if self.transitions.len() >= MAX_TRANSITION_HISTORY {
            self.transitions.pop_front();
        }
        self.transitions.push_back(record);
    }

    fn is_retired(&self, solar_system_id: SolarSystemId, generation: u64) -> bool {
        self.retired_generations
            .get(&solar_system_id)
            .is_some_and(|&retired| retired >= generation)
    }

    fn record_claim_loading(
        &mut self,
        solar_system_id: SolarSystemId,
        generation: u64,
        from: SystemLifecycleState,
        wake_reason: &SystemWakeReason,
        detail: &'static str,
    ) {
        let should_update = !self.is_retired(solar_system_id, generation)
            && self
                .wake_reasons
                .get(&solar_system_id)
                .is_none_or(|cur| generation >= cur.generation);
        if should_update {
            self.wake_reasons.insert(
                solar_system_id,
                WakeReasonEntry {
                    generation,
                    reason: wake_reason.clone(),
                },
            );
        }
        self.push_transition(LifecycleTransitionRecord {
            solar_system_id,
            generation,
            from,
            to: SystemLifecycleState::Loading,
            wake_reason: Some(wake_reason.clone()),
            detail,
        });
    }

    fn record_publish_live(
        &mut self,
        solar_system_id: SolarSystemId,
        generation: u64,
        wake_reason: &SystemWakeReason,
    ) -> bool {
        if self.is_retired(solar_system_id, generation) {
            return false;
        }
        if let Some(cur) = self.wake_reasons.get(&solar_system_id) {
            if cur.generation > generation {
                return false;
            }
        }
        self.wake_reasons.insert(
            solar_system_id,
            WakeReasonEntry {
                generation,
                reason: wake_reason.clone(),
            },
        );
        self.push_transition(LifecycleTransitionRecord {
            solar_system_id,
            generation,
            from: SystemLifecycleState::Loading,
            to: SystemLifecycleState::Live,
            wake_reason: Some(wake_reason.clone()),
            detail: "loading_published_live",
        });
        true
    }

    fn record_discard_stale(
        &mut self,
        solar_system_id: SolarSystemId,
        generation: u64,
        to: SystemLifecycleState,
        wake_reason: &SystemWakeReason,
    ) {
        self.mark_retired_floor(solar_system_id, generation);
        self.push_transition(LifecycleTransitionRecord {
            solar_system_id,
            generation,
            from: SystemLifecycleState::Loading,
            to,
            wake_reason: Some(wake_reason.clone()),
            detail: "stale_loading_discarded",
        });
    }

    fn record_aborted_loading(
        &mut self,
        solar_system_id: SolarSystemId,
        generation: u64,
        detail: &'static str,
    ) {
        self.mark_retired_floor(solar_system_id, generation);
        if self
            .wake_reasons
            .get(&solar_system_id)
            .is_some_and(|entry| entry.generation <= generation)
        {
            self.wake_reasons.remove(&solar_system_id);
        }
        self.push_transition(LifecycleTransitionRecord {
            solar_system_id,
            generation,
            from: SystemLifecycleState::Loading,
            to: SystemLifecycleState::Cold,
            wake_reason: None,
            detail,
        });
    }

    fn record_begin_stopping(
        &mut self,
        solar_system_id: SolarSystemId,
        generation: u64,
        from: SystemLifecycleState,
        detail: &'static str,
    ) {
        self.mark_retired_floor(solar_system_id, generation);
        if self
            .wake_reasons
            .get(&solar_system_id)
            .is_some_and(|entry| entry.generation <= generation)
        {
            self.wake_reasons.remove(&solar_system_id);
        }
        self.push_transition(LifecycleTransitionRecord {
            solar_system_id,
            generation,
            from,
            to: SystemLifecycleState::Stopping,
            wake_reason: None,
            detail,
        });
    }

    fn record_settled_cold(
        &mut self,
        solar_system_id: SolarSystemId,
        generation: u64,
        from: SystemLifecycleState,
        detail: &'static str,
    ) {
        self.mark_retired_floor(solar_system_id, generation);
        if self
            .wake_reasons
            .get(&solar_system_id)
            .is_some_and(|entry| entry.generation <= generation)
        {
            self.wake_reasons.remove(&solar_system_id);
        }
        self.push_transition(LifecycleTransitionRecord {
            solar_system_id,
            generation,
            from,
            to: SystemLifecycleState::Cold,
            wake_reason: None,
            detail,
        });
    }

    fn mark_retired_floor(&mut self, solar_system_id: SolarSystemId, generation: u64) {
        let floor = self.retired_generations.entry(solar_system_id).or_insert(0);
        *floor = (*floor).max(generation);
    }
}

pub(crate) fn new_lifecycle_telemetry() -> RankedRwLock<LifecycleTelemetryStore, RANK_LEAF> {
    RankedRwLock::new(LifecycleTelemetryStore::default())
}

struct LoadingSlotGuard<'a> {
    registry: &'a SystemRegistry,
    solar_system_id: SolarSystemId,
    generation: u64,
    gate: Arc<LifecycleGate>,
    armed: bool,
}

impl<'a> LoadingSlotGuard<'a> {
    fn new(
        registry: &'a SystemRegistry,
        solar_system_id: SolarSystemId,
        generation: u64,
        gate: Arc<LifecycleGate>,
    ) -> Self {
        Self {
            registry,
            solar_system_id,
            generation,
            gate,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for LoadingSlotGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(ref db) = self.registry.db {
            let _ = unregister_db_system_ballpark_if_generation(
                db,
                self.solar_system_id,
                self.generation,
            );
        }
        let mut cleared = false;
        let held = held_lock_ranks();
        if held.iter().any(|&r| r >= RANK_SYSTEM_REGISTRY) {
            if let Ok(mut guard) = self.registry.systems.try_write_unranked() {
                cleared = remove_matching_loading_slot(
                    &mut guard,
                    &self.registry.lifecycle_telemetry,
                    self.solar_system_id,
                    self.generation,
                );
            }
        } else {
            let mut guard = self
                .registry
                .systems
                .write()
                .unwrap_or_else(|e| e.into_inner());
            cleared = remove_matching_loading_slot(
                &mut guard,
                &self.registry.lifecycle_telemetry,
                self.solar_system_id,
                self.generation,
            );
        }
        if !cleared {
            let mut telem = self
                .registry
                .lifecycle_telemetry
                .write()
                .unwrap_or_else(|e| e.into_inner());
            telem.mark_retired_floor(self.solar_system_id, self.generation);
        }
        self.gate.resolve(GateResolution::Aborted);
    }
}

fn remove_matching_loading_slot(
    guard: &mut HashMap<SolarSystemId, SystemLifecycleSlot>,
    telemetry: &RankedRwLock<LifecycleTelemetryStore, RANK_LEAF>,
    solar_system_id: SolarSystemId,
    generation: u64,
) -> bool {
    let matches_gen = matches!(
        guard.get(&solar_system_id),
        Some(SystemLifecycleSlot::Loading { generation: slot_gen, .. }) if *slot_gen == generation
    );
    if !matches_gen {
        return false;
    }
    guard.remove(&solar_system_id);
    let mut telem = telemetry.write().unwrap_or_else(|e| e.into_inner());
    telem.record_aborted_loading(solar_system_id, generation, "loading_aborted_rollback");
    true
}

struct StoppingSlotGuard<'a> {
    registry: &'a SystemRegistry,
    solar_system_id: SolarSystemId,
    generation: u64,
    gate: Arc<LifecycleGate>,
    armed: bool,
}

impl<'a> StoppingSlotGuard<'a> {
    fn new(
        registry: &'a SystemRegistry,
        solar_system_id: SolarSystemId,
        generation: u64,
        gate: Arc<LifecycleGate>,
    ) -> Self {
        Self {
            registry,
            solar_system_id,
            generation,
            gate,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StoppingSlotGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(ref db) = self.registry.db {
            let _ = unregister_db_system_ballpark_if_generation(
                db,
                self.solar_system_id,
                self.generation,
            );
        }
        let held = held_lock_ranks();
        if held.iter().any(|&r| r >= RANK_SYSTEM_REGISTRY) {
            if let Ok(mut guard) = self.registry.systems.try_write_unranked() {
                remove_matching_stopping_slot(
                    &mut guard,
                    &self.registry.lifecycle_telemetry,
                    self.solar_system_id,
                    self.generation,
                    "stopping_cancelled_to_cold",
                );
            }
        } else {
            let mut guard = self
                .registry
                .systems
                .write()
                .unwrap_or_else(|e| e.into_inner());
            remove_matching_stopping_slot(
                &mut guard,
                &self.registry.lifecycle_telemetry,
                self.solar_system_id,
                self.generation,
                "stopping_cancelled_to_cold",
            );
        }
        self.gate.resolve(GateResolution::SettledToCold);
    }
}

fn remove_matching_stopping_slot(
    guard: &mut HashMap<SolarSystemId, SystemLifecycleSlot>,
    telemetry: &RankedRwLock<LifecycleTelemetryStore, RANK_LEAF>,
    solar_system_id: SolarSystemId,
    generation: u64,
    detail: &'static str,
) -> bool {
    let matches_gen = matches!(
        guard.get(&solar_system_id),
        Some(SystemLifecycleSlot::Stopping { generation: slot_gen, .. }) if *slot_gen == generation
    );
    if !matches_gen {
        return false;
    }
    guard.remove(&solar_system_id);
    let mut telem = telemetry.write().unwrap_or_else(|e| e.into_inner());
    telem.record_settled_cold(
        solar_system_id,
        generation,
        SystemLifecycleState::Stopping,
        detail,
    );
    true
}

struct UnregisteredHandleGuard<'a> {
    registry: &'a SystemRegistry,
    handle: Option<SystemHandle>,
}

impl<'a> UnregisteredHandleGuard<'a> {
    fn new(registry: &'a SystemRegistry, handle: SystemHandle) -> Self {
        Self {
            registry,
            handle: Some(handle),
        }
    }

    fn take(&mut self) -> Option<SystemHandle> {
        self.handle.take()
    }
}

impl Drop for UnregisteredHandleGuard<'_> {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.registry.shutdown_unregistered_handle(handle);
        }
    }
}

enum SlotStep {
    Live(SystemHandle),
    Wait(Arc<LifecycleGate>),
    ClaimedLoading {
        generation: u64,
        gate: Arc<LifecycleGate>,
        rt_handle: tokio::runtime::Handle,
    },
}

enum StopBeginAction {
    AlreadyCold,
    PreemptedLoading,
    WaitOnStopping { gate: Arc<LifecycleGate> },
    BeginShutdown {
        generation: u64,
        gate: Arc<LifecycleGate>,
        handle: SystemHandle,
    },
}

enum SystemsReadGuard<'a> {
    Ranked(RankedRwLockReadGuard<'a, HashMap<SolarSystemId, SystemLifecycleSlot>, { RANK_SYSTEM_REGISTRY }>),
    Unranked(std::sync::RwLockReadGuard<'a, HashMap<SolarSystemId, SystemLifecycleSlot>>),
}

impl std::ops::Deref for SystemsReadGuard<'_> {
    type Target = HashMap<SolarSystemId, SystemLifecycleSlot>;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Ranked(g) => g,
            Self::Unranked(g) => g,
        }
    }
}

enum SystemsWriteGuard<'a> {
    Ranked(RankedRwLockWriteGuard<'a, HashMap<SolarSystemId, SystemLifecycleSlot>, { RANK_SYSTEM_REGISTRY }>),
    Unranked(std::sync::RwLockWriteGuard<'a, HashMap<SolarSystemId, SystemLifecycleSlot>>),
}

impl std::ops::Deref for SystemsWriteGuard<'_> {
    type Target = HashMap<SolarSystemId, SystemLifecycleSlot>;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Ranked(g) => g,
            Self::Unranked(g) => g,
        }
    }
}

impl std::ops::DerefMut for SystemsWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Ranked(g) => g,
            Self::Unranked(g) => g,
        }
    }
}

impl SystemRegistry {
    /// Wakes `solar_system_id` on behalf of a client session arrival (`#1542`).
    pub fn wake_for_session_arrival(
        &self,
        solar_system_id: SolarSystemId,
        reason: SessionArrivalWakeReason,
    ) -> Result<SystemHandle, SystemActivationError> {
        self.wake_with_reason(solar_system_id, SystemWakeReason::SessionArrival(reason))
    }

    /// Wakes `solar_system_id` on behalf of a background cluster tick work item (`#1542`).
    pub fn wake_for_cluster_tick(
        &self,
        solar_system_id: SolarSystemId,
        reason: ClusterTickWakeReason,
    ) -> Result<SystemHandle, SystemActivationError> {
        self.wake_with_reason(solar_system_id, SystemWakeReason::ClusterTick(reason))
    }

    /// Wakes `solar_system_id` on behalf of an explicit GM command (`#1542`).
    /// Rejects empty or whitespace-only command strings without transitioning from `Cold`.
    pub fn wake_for_gm_command(
        &self,
        solar_system_id: SolarSystemId,
        command: impl Into<String>,
    ) -> Result<SystemHandle, SystemActivationError> {
        let cmd = command.into();
        if cmd.trim().is_empty() {
            return Err(SystemActivationError::InvalidGmCommand { solar_system_id });
        }
        self.wake_with_reason(solar_system_id, SystemWakeReason::GmCommand(cmd))
    }

    /// Wakes or retrieves a running solar system actor with an explicit [`SystemWakeReason`] (`#1542`).
    pub fn wake_with_reason(
        &self,
        solar_system_id: SolarSystemId,
        wake_reason: SystemWakeReason,
    ) -> Result<SystemHandle, SystemActivationError> {
        if let SystemWakeReason::GmCommand(ref cmd) = wake_reason {
            if cmd.trim().is_empty() {
                return Err(SystemActivationError::InvalidGmCommand { solar_system_id });
            }
        }

        loop {
            match self.inspect_or_claim_loading(solar_system_id, &wake_reason)? {
                SlotStep::Live(handle) => return Ok(handle),
                SlotStep::Wait(gate) => {
                    let resolution = gate.wait_for_slot_with_timeout(
                        solar_system_id,
                        self.is_db_write_locked_by_current_thread(),
                        self.gate_wait_timeout(),
                    )?;
                    if matches!(resolution, GateResolution::Aborted) {
                        return Err(SystemActivationError::ConcurrentActivationAborted {
                            solar_system_id,
                        });
                    }
                }
                SlotStep::ClaimedLoading {
                    generation,
                    gate,
                    rt_handle,
                } => {
                    return self.finish_loading_slot(
                        solar_system_id,
                        generation,
                        gate,
                        rt_handle,
                        wake_reason,
                    );
                }
            }
        }
    }

    /// Wakes `solar_system_id` when already `Live` or when a Tokio runtime reactor is active;
    /// returns `Ok(None)` cleanly when `Cold` with no active Tokio runtime (`#1542`).
    pub fn try_wake_if_runtime(
        &self,
        solar_system_id: SolarSystemId,
        wake_reason: SystemWakeReason,
    ) -> Result<Option<SystemHandle>, SystemActivationError> {
        if let Some(handle) = self.get_handle(solar_system_id) {
            return Ok(Some(handle));
        }
        match self.wake_with_reason(solar_system_id, wake_reason) {
            Ok(handle) => Ok(Some(handle)),
            Err(SystemActivationError::NoTokioRuntime { .. }) => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn legacy_activate_system(&self, solar_system_id: SolarSystemId) -> SystemHandle {
        self.wake_with_reason(solar_system_id, SystemWakeReason::LegacyGetOrSpawn)
            .unwrap_or_else(|err| {
                panic!("failed to activate solar system {solar_system_id}: {err}")
            })
    }

    pub(crate) fn legacy_activate_system_if_runtime(
        &self,
        solar_system_id: SolarSystemId,
    ) -> Option<SystemHandle> {
        self.try_wake_if_runtime(solar_system_id, SystemWakeReason::LegacyGetOrSpawn)
            .unwrap_or_else(|err| {
                panic!("failed to activate solar system {solar_system_id} on runtime: {err}")
            })
    }

    /// Returns the explicit runtime lifecycle state of `solar_system_id` from the single registry
    /// slot, querying hibernation dynamically from the live actor (`#1542`).
    pub fn system_lifecycle_state(&self, solar_system_id: SolarSystemId) -> SystemLifecycleState {
        enum Snapshot {
            Cold,
            Loading,
            Stopping,
            Live(SystemHandle),
        }

        let snap = {
            let guard = self.read_systems_slot_map();
            match guard.get(&solar_system_id) {
                None => Snapshot::Cold,
                Some(SystemLifecycleSlot::Loading { .. }) => Snapshot::Loading,
                Some(SystemLifecycleSlot::Stopping { .. }) => Snapshot::Stopping,
                Some(SystemLifecycleSlot::Live { handle, .. }) => Snapshot::Live(handle.clone()),
            }
        };

        match snap {
            Snapshot::Cold => SystemLifecycleState::Cold,
            Snapshot::Loading => SystemLifecycleState::Loading,
            Snapshot::Stopping => SystemLifecycleState::Stopping,
            Snapshot::Live(handle) => {
                if handle.cmd_tx.is_closed() {
                    return SystemLifecycleState::Cold;
                }
                if self.is_handle_hibernated_safe(&handle) {
                    SystemLifecycleState::Hibernating
                } else {
                    SystemLifecycleState::Live
                }
            }
        }
    }

    /// Returns the monotonic generation currently occupying `solar_system_id`'s slot (`None` if `Cold`).
    pub fn current_system_generation(&self, solar_system_id: SolarSystemId) -> Option<u64> {
        self.read_systems_slot_map()
            .get(&solar_system_id)
            .map(SystemLifecycleSlot::generation)
    }

    fn read_systems_slot_map(&self) -> SystemsReadGuard<'_> {
        if held_lock_ranks()
            .iter()
            .any(|&r| r >= RANK_SYSTEM_REGISTRY)
        {
            SystemsReadGuard::Unranked(
                self.systems
                    .read_unranked()
                    .unwrap_or_else(|e| e.into_inner()),
            )
        } else {
            SystemsReadGuard::Ranked(self.systems.read().unwrap_or_else(|e| e.into_inner()))
        }
    }

    fn write_systems_for_publish(
        &self,
        solar_system_id: SolarSystemId,
    ) -> Result<SystemsWriteGuard<'_>, SystemActivationError> {
        if held_lock_ranks()
            .iter()
            .any(|&r| r >= RANK_SYSTEM_REGISTRY)
        {
            match self.systems.try_write_unranked() {
                Ok(g) => Ok(SystemsWriteGuard::Unranked(g)),
                Err(std::sync::TryLockError::WouldBlock) => {
                    Err(SystemActivationError::WaitRejectedWhileHoldingLocks {
                        solar_system_id,
                    })
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    Err(SystemActivationError::PoisonedLock {
                        solar_system_id: Some(solar_system_id),
                        lock: "systems",
                    })
                }
            }
        } else {
            self.systems
                .write()
                .map(SystemsWriteGuard::Ranked)
                .map_err(|_| SystemActivationError::PoisonedLock {
                    solar_system_id: Some(solar_system_id),
                    lock: "systems",
                })
        }
    }

    /// Returns the wake reason recorded for the current active/loading generation of `solar_system_id`.
    pub fn last_wake_reason(&self, solar_system_id: SolarSystemId) -> Option<SystemWakeReason> {
        self.lifecycle_telemetry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .wake_reasons
            .get(&solar_system_id)
            .map(|entry| entry.reason.clone())
    }

    /// Returns `(generation, wake_reason)` recorded for `solar_system_id`, if active or loading.
    pub fn last_wake_reason_with_generation(
        &self,
        solar_system_id: SolarSystemId,
    ) -> Option<(u64, SystemWakeReason)> {
        self.lifecycle_telemetry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .wake_reasons
            .get(&solar_system_id)
            .map(|entry| (entry.generation, entry.reason.clone()))
    }

    /// Returns a chronological snapshot of recent lifecycle slot transitions (`#1542`).
    pub fn recent_lifecycle_transitions(&self) -> Vec<LifecycleTransitionRecord> {
        self.lifecycle_telemetry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .transitions
            .iter()
            .cloned()
            .collect()
    }

    /// Gracefully stops an active or loading solar system actor task and transitions its slot
    /// to `Cold` via `Stopping` (`#1541`, `#1542`).
    pub async fn stop_system(&self, solar_system_id: SolarSystemId) -> bool {
        loop {
            match self.begin_stop_slot(solar_system_id) {
                StopBeginAction::AlreadyCold => return false,
                StopBeginAction::PreemptedLoading => return true,
                StopBeginAction::WaitOnStopping { gate } => {
                    let _ = gate.wait_async_until_settled().await;
                }
                StopBeginAction::BeginShutdown {
                    generation,
                    gate,
                    handle,
                } => {
                    let mut stop_guard =
                        StoppingSlotGuard::new(self, solar_system_id, generation, Arc::clone(&gate));
                    if let Some(ref db) = self.db {
                        let _ = unregister_db_system_ballpark_if_generation(
                            db,
                            solar_system_id,
                            generation,
                        );
                    }
                    shutdown_actor_gracefully(handle).await;
                    #[cfg(test)]
                    self.run_before_stop_finish_hook_for_test(solar_system_id, generation);
                    self.finish_stopping_slot(
                        solar_system_id,
                        generation,
                        &gate,
                        "stop_system_to_cold",
                        &mut stop_guard,
                    );
                    return true;
                }
            }
        }
    }

    /// Evicts hibernated systems that have been idle for at least `min_idle_secs` after flushing
    /// a final condition checkpoint (`RFC-1 E-4`, `#919`, `#866`, `#1542`).
    pub async fn evict_idle_systems(&self, min_idle_secs: u64) -> Vec<SolarSystemId> {
        self.evict_idle_systems_with_min_ticks(min_idle_secs, 0).await
    }

    /// Evicts hibernated systems that have reached both `min_idle_secs` and `min_idle_ticks`,
    /// provided no external [`SystemHandle`] clones are still held outside the registry/actor.
    pub async fn evict_idle_systems_with_min_ticks(
        &self,
        min_idle_secs: u64,
        min_idle_ticks: u64,
    ) -> Vec<SolarSystemId> {
        let now_unix = self
            .db
            .as_ref()
            .map(|db| db.clock().now_unix_secs())
            .unwrap_or(1);
        let candidates: Vec<(SolarSystemId, u64, SystemHandle)> = self
            .active_live_slots()
            .into_iter()
            // Refcounts during filter: `self.systems` (1) + actor task (1) + vec element (1) = 3.
            .filter(|(_, _, h)| Self::can_evict_handle(h, now_unix, min_idle_secs, min_idle_ticks, 3))
            .collect();

        let mut evicted = Vec::new();
        for (sys_id, candidate_gen, handle) in candidates {
            let _ = handle.flush_condition_checkpoint().await;
            if !self.is_handle_hibernated_safe(&handle) {
                continue;
            }
            #[cfg(test)]
            self.run_before_evict_commit_hook_for_test(sys_id, candidate_gen);

            let maybe_gate = self.try_begin_evict_stopping(
                sys_id,
                candidate_gen,
                &handle,
                now_unix,
                min_idle_secs,
                min_idle_ticks,
            );
            let Some(gate) = maybe_gate else {
                continue;
            };

            let mut stop_guard =
                StoppingSlotGuard::new(self, sys_id, candidate_gen, Arc::clone(&gate));
            if let Some(ref db) = self.db {
                let _ = unregister_db_system_ballpark_if_generation(db, sys_id, candidate_gen);
            }
            shutdown_actor_gracefully(handle).await;
            self.reclaim_transient_items_in_db(sys_id);
            #[cfg(test)]
            self.run_before_stop_finish_hook_for_test(sys_id, candidate_gen);
            self.finish_stopping_slot(
                sys_id,
                candidate_gen,
                &gate,
                "evict_idle_to_cold",
                &mut stop_guard,
            );
            evicted.push(sys_id);
        }
        evicted
    }

    fn can_evict_handle(
        h: &SystemHandle,
        now_unix: u64,
        min_idle_secs: u64,
        min_idle_ticks: u64,
        max_strong: usize,
    ) -> bool {
        if !h.is_hibernated()
            || h.pending_destroyed_balls.is_non_empty()
            || Arc::strong_count(&h.ballpark) > max_strong
        {
            return false;
        }
        if min_idle_ticks > 0 && h.idle_ticks.load(Ordering::Relaxed) < min_idle_ticks {
            return false;
        }
        if min_idle_secs == 0 {
            return true;
        }
        let since = h.hibernated_since_unix();
        since > 0 && now_unix.saturating_sub(since) >= min_idle_secs
    }

    fn is_handle_hibernated_safe(&self, handle: &SystemHandle) -> bool {
        if held_lock_ranks().iter().any(|&r| r >= RANK_BALLPARK) {
            handle.event_tx.receiver_count() == 0
                && handle.dynamic_ball_count.load(Ordering::Relaxed) == 0
                && handle.hibernated.load(Ordering::Relaxed)
        } else {
            handle.is_hibernated()
        }
    }

    fn active_live_slots(&self) -> Vec<(SolarSystemId, u64, SystemHandle)> {
        let mut slots: Vec<(SolarSystemId, u64, SystemHandle)> = self
            .read_systems_slot_map()
            .iter()
            .filter_map(|(&id, slot)| match slot {
                SystemLifecycleSlot::Live { generation, handle } if !handle.cmd_tx.is_closed() => {
                    Some((id, *generation, handle.clone()))
                }
                _ => None,
            })
            .collect();
        slots.sort_by_key(|(id, _, _)| *id);
        slots
    }

    fn inspect_or_claim_loading(
        &self,
        solar_system_id: SolarSystemId,
        wake_reason: &SystemWakeReason,
    ) -> Result<SlotStep, SystemActivationError> {
        let held_above_registry = held_lock_ranks()
            .iter()
            .any(|&r| r >= RANK_SYSTEM_REGISTRY);
        if held_above_registry {
            {
                let guard = self
                    .systems
                    .read_unranked()
                    .map_err(|_| SystemActivationError::PoisonedLock {
                        solar_system_id: Some(solar_system_id),
                        lock: "systems",
                    })?;
                if let Some(slot) = guard.get(&solar_system_id) {
                    match slot {
                        SystemLifecycleSlot::Live { handle, .. } if !handle.cmd_tx.is_closed() => {
                            return Ok(SlotStep::Live(handle.clone()));
                        }
                        SystemLifecycleSlot::Loading { gate, .. }
                        | SystemLifecycleSlot::Stopping { gate, .. } => {
                            return Ok(SlotStep::Wait(Arc::clone(gate)));
                        }
                        SystemLifecycleSlot::Live { .. } => {}
                    }
                }
            }
            let rt_handle = tokio::runtime::Handle::try_current()
                .map_err(|_| SystemActivationError::NoTokioRuntime { solar_system_id })?;
            let mut guard = match self.systems.try_write_unranked() {
                Ok(g) => g,
                Err(std::sync::TryLockError::WouldBlock) => {
                    return Err(SystemActivationError::WaitRejectedWhileHoldingLocks {
                        solar_system_id,
                    });
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(SystemActivationError::PoisonedLock {
                        solar_system_id: Some(solar_system_id),
                        lock: "systems",
                    });
                }
            };
            return self.claim_loading_under_write_lock(
                &mut guard,
                solar_system_id,
                wake_reason,
                rt_handle,
            );
        }

        // Fast read probe so already-`Live` callers only take a shared lock and never fail with
        // `NoTokioRuntime` when a system is already active.
        {
            let guard = self
                .systems
                .read()
                .map_err(|_| SystemActivationError::PoisonedLock {
                    solar_system_id: Some(solar_system_id),
                    lock: "systems",
                })?;
            if let Some(slot) = guard.get(&solar_system_id) {
                match slot {
                    SystemLifecycleSlot::Live { handle, .. } if !handle.cmd_tx.is_closed() => {
                        return Ok(SlotStep::Live(handle.clone()));
                    }
                    SystemLifecycleSlot::Loading { gate, .. }
                    | SystemLifecycleSlot::Stopping { gate, .. } => {
                        return Ok(SlotStep::Wait(Arc::clone(gate)));
                    }
                    SystemLifecycleSlot::Live { .. } => {}
                }
            }
        }

        let rt_handle = tokio::runtime::Handle::try_current()
            .map_err(|_| SystemActivationError::NoTokioRuntime { solar_system_id })?;
        let mut guard = self
            .systems
            .write()
            .map_err(|_| SystemActivationError::PoisonedLock {
                solar_system_id: Some(solar_system_id),
                lock: "systems",
            })?;
        self.claim_loading_under_write_lock(&mut guard, solar_system_id, wake_reason, rt_handle)
    }

    fn claim_loading_under_write_lock(
        &self,
        guard: &mut HashMap<SolarSystemId, SystemLifecycleSlot>,
        solar_system_id: SolarSystemId,
        wake_reason: &SystemWakeReason,
        rt_handle: tokio::runtime::Handle,
    ) -> Result<SlotStep, SystemActivationError> {
        let from_state = match guard.get(&solar_system_id) {
            None => SystemLifecycleState::Cold,
            Some(SystemLifecycleSlot::Live { handle, .. }) if !handle.cmd_tx.is_closed() => {
                return Ok(SlotStep::Live(handle.clone()));
            }
            Some(SystemLifecycleSlot::Loading { gate, .. })
            | Some(SystemLifecycleSlot::Stopping { gate, .. }) => {
                return Ok(SlotStep::Wait(Arc::clone(gate)));
            }
            Some(SystemLifecycleSlot::Live {
                generation: dead_gen,
                ..
            }) => {
                let dead_gen = *dead_gen;
                if let Some(ref db) = self.db {
                    let _ = unregister_db_system_ballpark_if_generation(
                        db,
                        solar_system_id,
                        dead_gen,
                    );
                }
                let mut telem = self
                    .lifecycle_telemetry
                    .write()
                    .map_err(|_| SystemActivationError::PoisonedLock {
                        solar_system_id: Some(solar_system_id),
                        lock: "lifecycle_telemetry",
                    })?;
                telem.mark_retired_floor(solar_system_id, dead_gen);
                tracing::warn!(
                    solar_system_id,
                    dead_gen,
                    "replacing terminated SystemActor handle with new lifecycle generation"
                );
                SystemLifecycleState::Live
            }
        };

        let generation = self.next_lifecycle_generation.fetch_add(1, Ordering::SeqCst);
        let gate = Arc::new(LifecycleGate::new(GateKind::Loading));
        {
            let mut telem = self
                .lifecycle_telemetry
                .write()
                .map_err(|_| SystemActivationError::PoisonedLock {
                    solar_system_id: Some(solar_system_id),
                    lock: "lifecycle_telemetry",
                })?;
            telem.record_claim_loading(
                solar_system_id,
                generation,
                from_state,
                wake_reason,
                "claim_loading",
            );
        }
        guard.insert(
            solar_system_id,
            SystemLifecycleSlot::Loading {
                generation,
                gate: Arc::clone(&gate),
            },
        );
        Ok(SlotStep::ClaimedLoading {
            generation,
            gate,
            rt_handle,
        })
    }

    fn finish_loading_slot(
        &self,
        solar_system_id: SolarSystemId,
        generation: u64,
        gate: Arc<LifecycleGate>,
        rt_handle: tokio::runtime::Handle,
        wake_reason: SystemWakeReason,
    ) -> Result<SystemHandle, SystemActivationError> {
        let mut loading_guard =
            LoadingSlotGuard::new(self, solar_system_id, generation, Arc::clone(&gate));

        let prepared = self.prepare_system_activation(solar_system_id, &wake_reason)?;
        let (handle, lazy_gates) = self.spawn_prepared_system_actor(&rt_handle, prepared);
        let mut handle_guard = UnregisteredHandleGuard::new(self, handle);

        #[cfg(test)]
        self.run_before_publish_hook_for_test(solar_system_id, generation);

        let handle = handle_guard
            .take()
            .expect("unregistered handle guard must still own handle before publish");
        let published = self.publish_loading_slot(
            solar_system_id,
            generation,
            &gate,
            handle,
            &wake_reason,
            &mut loading_guard,
        )?;

        if lazy_gates && !held_lock_ranks().iter().any(|&r| r >= RANK_SYSTEM_REGISTRY) {
            self.on_system_loaded_refresh_stargates(solar_system_id, self.current_wallclock_ms());
        }
        Ok(published)
    }

    fn publish_loading_slot(
        &self,
        solar_system_id: SolarSystemId,
        generation: u64,
        gate: &Arc<LifecycleGate>,
        handle: SystemHandle,
        wake_reason: &SystemWakeReason,
        loading_guard: &mut LoadingSlotGuard<'_>,
    ) -> Result<SystemHandle, SystemActivationError> {
        #[cfg(test)]
        if self
            .lifecycle_test_hooks
            .force_publish_wouldblock
            .swap(false, Ordering::AcqRel)
        {
            self.shutdown_unregistered_handle(handle);
            return Err(SystemActivationError::ConcurrentActivationAborted { solar_system_id });
        }

        let mut guard = match self.write_systems_for_publish(solar_system_id) {
            Ok(g) => g,
            Err(err) => {
                self.shutdown_unregistered_handle(handle);
                return Err(err);
            }
        };

        let slot_matches = matches!(
            guard.get(&solar_system_id),
            Some(SystemLifecycleSlot::Loading { generation: slot_gen, .. }) if *slot_gen == generation
        );
        if !slot_matches {
            let observed_state = match guard.get(&solar_system_id) {
                None => SystemLifecycleState::Cold,
                Some(SystemLifecycleSlot::Loading { .. }) => SystemLifecycleState::Loading,
                Some(SystemLifecycleSlot::Live { .. }) => SystemLifecycleState::Live,
                Some(SystemLifecycleSlot::Stopping { .. }) => SystemLifecycleState::Stopping,
            };
            self.discarded_stale_completions
                .fetch_add(1, Ordering::Relaxed);
            if let Some(ref db) = self.db {
                let _ =
                    unregister_db_system_ballpark_if_generation(db, solar_system_id, generation);
            }
            if let Ok(mut telem) = self.lifecycle_telemetry.write() {
                telem.record_discard_stale(solar_system_id, generation, observed_state, wake_reason);
            }
            drop(guard);
            loading_guard.disarm();
            gate.resolve(GateResolution::Aborted);
            self.shutdown_unregistered_handle(handle);
            return Err(SystemActivationError::ConcurrentActivationAborted { solar_system_id });
        }

        if let Some(ref db) = self.db {
            let registered = register_db_system_ballpark_with_generation(
                db,
                solar_system_id,
                generation,
                &handle,
            );
            if !registered {
                drop(guard);
                self.shutdown_unregistered_handle(handle);
                return Err(SystemActivationError::ConcurrentActivationAborted { solar_system_id });
            }
        }

        let telem_ok = self
            .lifecycle_telemetry
            .write()
            .map(|mut telem| telem.record_publish_live(solar_system_id, generation, wake_reason))
            .unwrap_or(false);
        if !telem_ok {
            if let Some(ref db) = self.db {
                let _ =
                    unregister_db_system_ballpark_if_generation(db, solar_system_id, generation);
            }
            drop(guard);
            self.shutdown_unregistered_handle(handle);
            return Err(SystemActivationError::ConcurrentActivationAborted { solar_system_id });
        }

        guard.insert(
            solar_system_id,
            SystemLifecycleSlot::Live {
                generation,
                handle: handle.clone(),
            },
        );
        loading_guard.disarm();
        drop(guard);

        gate.resolve(GateResolution::Ready);
        Ok(handle)
    }

    fn begin_stop_slot(&self, solar_system_id: SolarSystemId) -> StopBeginAction {
        let Ok(mut guard) = self.systems.write() else {
            return StopBeginAction::AlreadyCold;
        };
        match guard.get(&solar_system_id) {
            None => StopBeginAction::AlreadyCold,
            Some(SystemLifecycleSlot::Stopping { gate, .. }) => StopBeginAction::WaitOnStopping {
                gate: Arc::clone(gate),
            },
            Some(SystemLifecycleSlot::Loading { generation, gate }) => {
                let generation = *generation;
                let gate = Arc::clone(gate);
                guard.remove(&solar_system_id);
                if let Some(ref db) = self.db {
                    let _ =
                        unregister_db_system_ballpark_if_generation(db, solar_system_id, generation);
                }
                if let Ok(mut telem) = self.lifecycle_telemetry.write() {
                    telem.record_aborted_loading(
                        solar_system_id,
                        generation,
                        "stop_preempted_loading",
                    );
                }
                drop(guard);
                gate.resolve(GateResolution::Aborted);
                StopBeginAction::PreemptedLoading
            }
            Some(SystemLifecycleSlot::Live { .. }) => {
                let Some(SystemLifecycleSlot::Live { generation, handle }) =
                    guard.remove(&solar_system_id)
                else {
                    return StopBeginAction::AlreadyCold;
                };
                let gate = Arc::new(LifecycleGate::new(GateKind::Stopping));
                let from_state = if self.is_handle_hibernated_safe(&handle) {
                    SystemLifecycleState::Hibernating
                } else {
                    SystemLifecycleState::Live
                };
                if let Ok(mut telem) = self.lifecycle_telemetry.write() {
                    telem.record_begin_stopping(
                        solar_system_id,
                        generation,
                        from_state,
                        "stop_system_begin",
                    );
                }
                guard.insert(
                    solar_system_id,
                    SystemLifecycleSlot::Stopping {
                        generation,
                        gate: Arc::clone(&gate),
                    },
                );
                StopBeginAction::BeginShutdown {
                    generation,
                    gate,
                    handle,
                }
            }
        }
    }

    fn try_begin_evict_stopping(
        &self,
        solar_system_id: SolarSystemId,
        candidate_gen: u64,
        candidate_handle: &SystemHandle,
        now_unix: u64,
        min_idle_secs: u64,
        min_idle_ticks: u64,
    ) -> Option<Arc<LifecycleGate>> {
        let mut guard = self.systems.write().unwrap_or_else(|e| e.into_inner());
        let matches_candidate = matches!(
            guard.get(&solar_system_id),
            Some(SystemLifecycleSlot::Live { generation, handle: slot_handle })
                if *generation == candidate_gen
                    && Arc::ptr_eq(&slot_handle.ballpark, &candidate_handle.ballpark)
                    && Self::can_evict_handle(slot_handle, now_unix, min_idle_secs, min_idle_ticks, 3)
        );
        if !matches_candidate {
            return None;
        }
        let gate = Arc::new(LifecycleGate::new(GateKind::Stopping));
        if let Ok(mut telem) = self.lifecycle_telemetry.write() {
            telem.record_begin_stopping(
                solar_system_id,
                candidate_gen,
                SystemLifecycleState::Hibernating,
                "evict_idle_begin",
            );
        }
        guard.insert(
            solar_system_id,
            SystemLifecycleSlot::Stopping {
                generation: candidate_gen,
                gate: Arc::clone(&gate),
            },
        );
        Some(gate)
    }

    fn finish_stopping_slot(
        &self,
        solar_system_id: SolarSystemId,
        generation: u64,
        gate: &Arc<LifecycleGate>,
        detail: &'static str,
        stop_guard: &mut StoppingSlotGuard<'_>,
    ) {
        {
            let mut guard = self.systems.write().unwrap_or_else(|e| e.into_inner());
            remove_matching_stopping_slot(
                &mut guard,
                &self.lifecycle_telemetry,
                solar_system_id,
                generation,
                detail,
            );
            stop_guard.disarm();
        }
        gate.resolve(GateResolution::SettledToCold);
    }

    pub(crate) fn current_wallclock_ms(&self) -> u64 {
        self.db
            .as_ref()
            .map(|db| machonet_wallclock_ms(db.clock().as_ref(), &self.unrebased_clock_offset_secs))
            .unwrap_or(0)
    }

    pub(crate) fn shutdown_unregistered_handle(&self, handle: SystemHandle) {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let _ = handle.cmd_tx.try_send(SystemCommand::ShutdownActor { reply: tx });
        drop(handle);
    }

    fn gate_wait_timeout(&self) -> Duration {
        #[cfg(test)]
        {
            if let Some(custom) = *self
                .lifecycle_test_hooks
                .gate_wait_timeout
                .lock()
                .unwrap_or_else(|e| e.into_inner())
            {
                return custom;
            }
        }
        DEFAULT_GATE_WAIT_TIMEOUT
    }

    #[cfg(test)]
    fn run_before_publish_hook_for_test(&self, solar_system_id: SolarSystemId, generation: u64) {
        let hook = self
            .lifecycle_test_hooks
            .before_publish_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook(solar_system_id, generation);
        }
    }

    #[cfg(test)]
    fn run_before_evict_commit_hook_for_test(
        &self,
        solar_system_id: SolarSystemId,
        generation: u64,
    ) {
        let hook = self
            .lifecycle_test_hooks
            .before_evict_commit_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook(solar_system_id, generation);
        }
    }

    #[cfg(test)]
    fn run_before_stop_finish_hook_for_test(
        &self,
        solar_system_id: SolarSystemId,
        generation: u64,
    ) {
        let hook = self
            .lifecycle_test_hooks
            .before_stop_finish_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook(solar_system_id, generation);
        }
    }

    #[cfg(test)]
    pub(crate) fn set_before_publish_hook_for_test(
        &self,
        hook: Option<Arc<dyn Fn(SolarSystemId, u64) + Send + Sync>>,
    ) {
        *self
            .lifecycle_test_hooks
            .before_publish_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = hook;
    }

    #[cfg(test)]
    pub(crate) fn set_before_evict_commit_hook_for_test(
        &self,
        hook: Option<Arc<dyn Fn(SolarSystemId, u64) + Send + Sync>>,
    ) {
        *self
            .lifecycle_test_hooks
            .before_evict_commit_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = hook;
    }

    #[cfg(test)]
    pub(crate) fn set_before_stop_finish_hook_for_test(
        &self,
        hook: Option<Arc<dyn Fn(SolarSystemId, u64) + Send + Sync>>,
    ) {
        *self
            .lifecycle_test_hooks
            .before_stop_finish_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = hook;
    }

    #[cfg(test)]
    pub(crate) fn set_force_publish_wouldblock_for_test(&self, enable: bool) {
        self.lifecycle_test_hooks
            .force_publish_wouldblock
            .store(enable, Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn set_gate_wait_timeout_for_test(&self, timeout: Option<Duration>) {
        *self
            .lifecycle_test_hooks
            .gate_wait_timeout
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = timeout;
    }

    #[cfg(test)]
    pub(crate) fn discarded_stale_completions_for_test(&self) -> u64 {
        self.discarded_stale_completions.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn replace_live_handle_for_test(
        &self,
        solar_system_id: SolarSystemId,
        handle: SystemHandle,
    ) {
        let mut guard = self.systems.write().unwrap_or_else(|e| e.into_inner());
        let generation = guard
            .get(&solar_system_id)
            .map(SystemLifecycleSlot::generation)
            .unwrap_or_else(|| self.next_lifecycle_generation.fetch_add(1, Ordering::SeqCst));
        guard.insert(
            solar_system_id,
            SystemLifecycleSlot::Live { generation, handle },
        );
    }
}

async fn shutdown_actor_gracefully(handle: SystemHandle) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let sent = handle
        .cmd_tx
        .send(SystemCommand::ShutdownActor { reply: tx })
        .await
        .is_ok();
    drop(handle);
    if sent {
        let _ = rx.await;
    }
}
