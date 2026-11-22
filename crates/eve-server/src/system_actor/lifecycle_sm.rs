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
    clear_db_system_ballpark_if_matching, register_db_system_ballpark,
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
    pub(in crate::system_actor) after_read_probe_hook:
        RankedMutex<Option<Arc<dyn Fn(SolarSystemId) + Send + Sync>>, RANK_LEAF>,
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
            after_read_probe_hook: RankedMutex::new(None),
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
    Loading { owner_thread: ThreadId },
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
    state: Mutex<GateResolution>,
    cvar: Condvar,
    async_notify: tokio::sync::Notify,
}

impl LifecycleGate {
    fn new_loading() -> Self {
        Self {
            kind: GateKind::Loading {
                owner_thread: std::thread::current().id(),
            },
            state: Mutex::new(GateResolution::Pending),
            cvar: Condvar::new(),
            async_notify: tokio::sync::Notify::new(),
        }
    }

    fn new_stopping() -> Self {
        Self {
            kind: GateKind::Stopping,
            state: Mutex::new(GateResolution::Pending),
            cvar: Condvar::new(),
            async_notify: tokio::sync::Notify::new(),
        }
    }

    fn current_resolution(&self) -> GateResolution {
        *self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn check_wait_preconditions(
        &self,
        solar_system_id: SolarSystemId,
    ) -> Result<(), SystemActivationError> {
        if let GateKind::Loading { owner_thread } = self.kind {
            if std::thread::current().id() == owner_thread {
                return Err(SystemActivationError::ReentrantLoading { solar_system_id });
            }
        }
        if !held_lock_ranks().is_empty() {
            return Err(SystemActivationError::WaitRejectedWhileHoldingLocks {
                solar_system_id,
            });
        }
        Ok(())
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

    fn wait_cvar_with_timeout(
        &self,
        solar_system_id: SolarSystemId,
        timeout: Duration,
    ) -> Result<GateResolution, SystemActivationError> {
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

    /// Blocks a synchronous wake waiter until this gate settles or `timeout` expires.
    /// Must only be invoked AFTER releasing `SystemRegistry::systems` and never while holding
    /// ranked locks or DB write locks (`R1-03`). Uses `block_in_place` on Tokio multi-thread
    /// workers and fails closed on `current_thread` runtimes (`BF-02`).
    fn wait_for_slot_with_timeout(
        &self,
        solar_system_id: SolarSystemId,
        timeout: Duration,
    ) -> Result<GateResolution, SystemActivationError> {
        self.check_wait_preconditions(solar_system_id)?;
        let current = self.current_resolution();
        if !matches!(current, GateResolution::Pending) {
            return Ok(current);
        }

        match tokio::runtime::Handle::try_current() {
            Err(_) => self.wait_cvar_with_timeout(solar_system_id, timeout),
            Ok(rt_handle) => match rt_handle.runtime_flavor() {
                tokio::runtime::RuntimeFlavor::MultiThread => {
                    tokio::task::block_in_place(|| {
                        self.wait_cvar_with_timeout(solar_system_id, timeout)
                    })
                }
                tokio::runtime::RuntimeFlavor::CurrentThread => Err(
                    SystemActivationError::ConcurrentActivationAborted { solar_system_id },
                ),
                _ => Err(SystemActivationError::ConcurrentActivationAborted {
                    solar_system_id,
                }),
            },
        }
    }

    async fn wait_async_with_timeout(
        &self,
        solar_system_id: SolarSystemId,
        timeout: Duration,
    ) -> Result<GateResolution, SystemActivationError> {
        self.check_wait_preconditions(solar_system_id)?;
        tokio::time::timeout(timeout, self.wait_async_until_settled())
            .await
            .map_err(|_| SystemActivationError::ConcurrentActivationAborted { solar_system_id })
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

fn try_write_systems_in_drop(
    systems: &RankedRwLock<HashMap<SolarSystemId, SystemLifecycleSlot>, { RANK_SYSTEM_REGISTRY }>,
) -> Option<SystemsWriteGuard<'_>> {
    if held_lock_ranks()
        .iter()
        .any(|&r| r >= RANK_SYSTEM_REGISTRY)
    {
        match systems.try_write_unranked() {
            Ok(g) => Some(SystemsWriteGuard::Unranked(g)),
            Err(std::sync::TryLockError::Poisoned(e)) => {
                Some(SystemsWriteGuard::Unranked(e.into_inner()))
            }
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    } else {
        Some(SystemsWriteGuard::Ranked(
            systems.write().unwrap_or_else(|e| e.into_inner()),
        ))
    }
}

fn with_telemetry_write_safe<R>(
    telemetry: &RankedRwLock<LifecycleTelemetryStore, RANK_LEAF>,
    f: impl FnOnce(&mut LifecycleTelemetryStore) -> R,
) -> Option<R> {
    if held_lock_ranks().iter().any(|&r| r >= RANK_LEAF) {
        match telemetry.try_write_unranked() {
            Ok(mut g) => Some(f(&mut g)),
            Err(std::sync::TryLockError::Poisoned(e)) => Some(f(&mut e.into_inner())),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    } else {
        let mut g = telemetry.write().unwrap_or_else(|e| e.into_inner());
        Some(f(&mut g))
    }
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
        let mut cleared = false;
        if let Some(mut guard) = try_write_systems_in_drop(&self.registry.systems) {
            cleared = remove_matching_loading_slot(
                &mut guard,
                &self.registry.lifecycle_telemetry,
                self.solar_system_id,
                self.generation,
            );
        }
        if !cleared {
            let _ = with_telemetry_write_safe(&self.registry.lifecycle_telemetry, |telem| {
                telem.mark_retired_floor(self.solar_system_id, self.generation);
            });
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
    let _ = with_telemetry_write_safe(telemetry, |telem| {
        telem.record_aborted_loading(solar_system_id, generation, "loading_aborted_rollback");
    });
    true
}

/// Panic/runtime-drop safety guard owned exclusively by the spawned one-way stop task
/// (`#1542`). Normal caller future drops never cancel or roll back `Stopping`.
struct StopTaskCleanupGuard {
    registry: Arc<SystemRegistry>,
    solar_system_id: SolarSystemId,
    generation: u64,
    gate: Arc<LifecycleGate>,
    handle: Option<SystemHandle>,
    armed: bool,
}

impl StopTaskCleanupGuard {
    fn new(
        registry: Arc<SystemRegistry>,
        solar_system_id: SolarSystemId,
        generation: u64,
        gate: Arc<LifecycleGate>,
        handle: SystemHandle,
    ) -> Self {
        Self {
            registry,
            solar_system_id,
            generation,
            gate,
            handle: Some(handle),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StopTaskCleanupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(handle) = self.handle.take() {
            self.registry
                .clear_db_ballpark_for_handle(self.solar_system_id, &handle);
            self.registry.shutdown_unregistered_handle(handle);
        }
        self.registry.finish_stopping_slot(
            self.solar_system_id,
            self.generation,
            &self.gate,
            "stop_task_aborted_to_cold",
        );
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
    let _ = with_telemetry_write_safe(telemetry, |telem| {
        telem.record_settled_cold(
            solar_system_id,
            generation,
            SystemLifecycleState::Stopping,
            detail,
        );
    });
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
}

#[derive(Clone, Copy)]
enum StopOrigin {
    StopSystem { from_state: SystemLifecycleState },
    IdleEvict,
}

impl StopOrigin {
    fn from_state(self) -> SystemLifecycleState {
        match self {
            Self::StopSystem { from_state } => from_state,
            Self::IdleEvict => SystemLifecycleState::Hibernating,
        }
    }

    fn begin_detail(self) -> &'static str {
        match self {
            Self::StopSystem { .. } => "stop_system_begin",
            Self::IdleEvict => "evict_idle_begin",
        }
    }

    fn cold_detail(self) -> &'static str {
        match self {
            Self::StopSystem { .. } => "stop_system_to_cold",
            Self::IdleEvict => "evict_idle_to_cold",
        }
    }

    fn reclaim_transient(self) -> bool {
        matches!(self, Self::IdleEvict)
    }
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

    /// Asynchronously wakes `solar_system_id` on behalf of a client session arrival without blocking
    /// the calling Tokio worker thread on concurrent `Loading`/`Stopping` gates (`#1542`, `BF-02`).
    pub async fn wake_for_session_arrival_async(
        &self,
        solar_system_id: SolarSystemId,
        reason: SessionArrivalWakeReason,
    ) -> Result<SystemHandle, SystemActivationError> {
        self.wake_with_reason_async(solar_system_id, SystemWakeReason::SessionArrival(reason))
            .await
    }

    /// Wakes `solar_system_id` on behalf of a background cluster tick work item (`#1542`).
    pub fn wake_for_cluster_tick(
        &self,
        solar_system_id: SolarSystemId,
        reason: ClusterTickWakeReason,
    ) -> Result<SystemHandle, SystemActivationError> {
        self.wake_with_reason(solar_system_id, SystemWakeReason::ClusterTick(reason))
    }

    /// Asynchronously wakes `solar_system_id` for a background cluster tick without blocking Tokio
    /// workers on concurrent transition gates (`#1542`, `BF-02`).
    pub async fn wake_for_cluster_tick_async(
        &self,
        solar_system_id: SolarSystemId,
        reason: ClusterTickWakeReason,
    ) -> Result<SystemHandle, SystemActivationError> {
        self.wake_with_reason_async(solar_system_id, SystemWakeReason::ClusterTick(reason))
            .await
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

    /// Asynchronously wakes `solar_system_id` for an explicit GM command (`#1542`, `BF-02`).
    pub async fn wake_for_gm_command_async(
        &self,
        solar_system_id: SolarSystemId,
        command: impl Into<String>,
    ) -> Result<SystemHandle, SystemActivationError> {
        let cmd = command.into();
        if cmd.trim().is_empty() {
            return Err(SystemActivationError::InvalidGmCommand { solar_system_id });
        }
        self.wake_with_reason_async(solar_system_id, SystemWakeReason::GmCommand(cmd))
            .await
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

    /// Async counterpart to [`Self::wake_with_reason`] that awaits `LifecycleGate`
    /// via Tokio notifications without blocking worker threads (`#1542`, `BF-02`).
    pub async fn wake_with_reason_async(
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
                    let resolution = gate
                        .wait_async_with_timeout(solar_system_id, self.gate_wait_timeout())
                        .await?;
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
    /// returns `Ok(None)` cleanly when not already `Live` and no Tokio runtime is active (`#1542`).
    pub fn try_wake_if_runtime(
        &self,
        solar_system_id: SolarSystemId,
        wake_reason: SystemWakeReason,
    ) -> Result<Option<SystemHandle>, SystemActivationError> {
        if let Some(handle) = self.get_handle(solar_system_id) {
            return Ok(Some(handle));
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return Ok(None);
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
            .and_then(|slot| match slot {
                SystemLifecycleSlot::Live { generation, handle } if handle.cmd_tx.is_closed() => {
                    None
                }
                other => Some(other.generation()),
            })
    }

    fn allocate_lifecycle_generation(&self) -> u64 {
        match self.db {
            Some(ref db) => db.allocate_system_lifecycle_generation(),
            None => self
                .next_lifecycle_generation
                .fetch_add(1, Ordering::SeqCst),
        }
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
    ) -> Result<
        RankedRwLockWriteGuard<'_, HashMap<SolarSystemId, SystemLifecycleSlot>, { RANK_SYSTEM_REGISTRY }>,
        SystemActivationError,
    > {
        if held_lock_ranks()
            .iter()
            .any(|&r| r >= RANK_SYSTEM_REGISTRY)
        {
            return Err(SystemActivationError::WaitRejectedWhileHoldingLocks { solar_system_id });
        }
        self.systems
            .write()
            .map_err(|_| SystemActivationError::PoisonedLock {
                solar_system_id: Some(solar_system_id),
                lock: "systems",
            })
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
    /// to `Cold` via `Stopping` (`#1541`, `#1542`). Once `Live -> Stopping` begins, a spawned
    /// registry task drives shutdown to `Cold`; dropping this caller future does not cancel it.
    pub async fn stop_system(self: &Arc<Self>, solar_system_id: SolarSystemId) -> bool {
        match self.begin_stop_slot(solar_system_id) {
            StopBeginAction::AlreadyCold => false,
            StopBeginAction::PreemptedLoading => true,
            StopBeginAction::WaitOnStopping { gate } => {
                let _ = gate.wait_async_until_settled().await;
                true
            }
        }
    }

    async fn run_stop_system_task(
        self: Arc<Self>,
        solar_system_id: SolarSystemId,
        generation: u64,
        gate: Arc<LifecycleGate>,
        handle: SystemHandle,
        origin: StopOrigin,
    ) {
        let mut cleanup = StopTaskCleanupGuard::new(
            Arc::clone(&self),
            solar_system_id,
            generation,
            Arc::clone(&gate),
            handle,
        );
        let cmd_tx = cleanup
            .handle
            .as_ref()
            .expect("stop task handle initialized")
            .cmd_tx
            .clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sent = cmd_tx
            .send(SystemCommand::ShutdownActor { reply: tx })
            .await
            .is_ok();
        if let Some(handle) = cleanup.handle.take() {
            self.clear_db_ballpark_for_handle(solar_system_id, &handle);
            drop(handle);
        }
        if sent {
            let _ = rx.await;
        }
        cmd_tx.closed().await;
        if origin.reclaim_transient() {
            self.reclaim_transient_items_in_db(solar_system_id);
        }
        #[cfg(test)]
        self.run_before_stop_finish_hook_for_test(solar_system_id, generation)
            .await;
        self.finish_stopping_slot(solar_system_id, generation, &gate, origin.cold_detail());
        cleanup.disarm();
    }

    /// Evicts hibernated systems that have been idle for at least `min_idle_secs` after flushing
    /// a final condition checkpoint (`RFC-1 E-4`, `#919`, `#866`, `#1542`).
    pub async fn evict_idle_systems(self: &Arc<Self>, min_idle_secs: u64) -> Vec<SolarSystemId> {
        self.evict_idle_systems_with_min_ticks(min_idle_secs, 0).await
    }

    /// Evicts hibernated systems that have reached both `min_idle_secs` and `min_idle_ticks`,
    /// provided no external [`SystemHandle`] clones are still held outside the registry/actor.
    pub async fn evict_idle_systems_with_min_ticks(
        self: &Arc<Self>,
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

            let Some(gate) = self.try_begin_evict_stopping(
                sys_id,
                candidate_gen,
                handle,
                now_unix,
                min_idle_secs,
                min_idle_ticks,
            ) else {
                continue;
            };

            let _ = gate.wait_async_until_settled().await;
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
        let held = held_lock_ranks();
        let any_lock_held = !held.is_empty();
        let held_above_registry = held.iter().any(|&r| r >= RANK_SYSTEM_REGISTRY);

        // Fast read probe: already-`Live` callers only take a shared lock and never fail with
        // `NoTokioRuntime` or lock-rank checks (`BF-01`).
        {
            let guard = self.read_systems_slot_map();
            if let Some(slot) = guard.get(&solar_system_id) {
                match slot {
                    SystemLifecycleSlot::Live { handle, .. } if !handle.cmd_tx.is_closed() => {
                        return Ok(SlotStep::Live(handle.clone()));
                    }
                    SystemLifecycleSlot::Loading { gate, .. } => {
                        if any_lock_held {
                            return Err(SystemActivationError::WaitRejectedWhileHoldingLocks {
                                solar_system_id,
                            });
                        }
                        if matches!(gate.current_resolution(), GateResolution::Pending) {
                            return Ok(SlotStep::Wait(Arc::clone(gate)));
                        }
                    }
                    SystemLifecycleSlot::Stopping { gate, .. } => {
                        if any_lock_held {
                            return Err(SystemActivationError::WaitRejectedWhileHoldingLocks {
                                solar_system_id,
                            });
                        }
                        if matches!(gate.current_resolution(), GateResolution::Pending) {
                            return Ok(SlotStep::Wait(Arc::clone(gate)));
                        }
                    }
                    SystemLifecycleSlot::Live { .. } => {}
                }
            }
        }

        #[cfg(test)]
        self.run_after_read_probe_hook_for_test(solar_system_id);

        // Reject `Cold`/stale transitions before claiming when holding `>= RANK_SYSTEM_REGISTRY`
        // (including `RANK_WORLD_INNER`) so `Loading` slots never depend on fallible
        // `try_write_unranked` (`BF-01`, `BF-05`).
        if held_above_registry {
            return Err(SystemActivationError::WaitRejectedWhileHoldingLocks { solar_system_id });
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
        self.claim_loading_under_write_lock(
            &mut guard,
            solar_system_id,
            wake_reason,
            rt_handle,
            any_lock_held,
        )
    }

    fn claim_loading_under_write_lock(
        &self,
        guard: &mut HashMap<SolarSystemId, SystemLifecycleSlot>,
        solar_system_id: SolarSystemId,
        wake_reason: &SystemWakeReason,
        rt_handle: tokio::runtime::Handle,
        any_lock_held: bool,
    ) -> Result<SlotStep, SystemActivationError> {
        let from_state = match guard.get(&solar_system_id) {
            None => SystemLifecycleState::Cold,
            Some(SystemLifecycleSlot::Live { handle, .. }) if !handle.cmd_tx.is_closed() => {
                return Ok(SlotStep::Live(handle.clone()));
            }
            Some(SystemLifecycleSlot::Loading { gate, .. })
                if matches!(gate.current_resolution(), GateResolution::Pending) =>
            {
                if any_lock_held {
                    return Err(SystemActivationError::WaitRejectedWhileHoldingLocks {
                        solar_system_id,
                    });
                }
                return Ok(SlotStep::Wait(Arc::clone(gate)));
            }
            Some(SystemLifecycleSlot::Loading {
                generation: stale_gen,
                ..
            }) => {
                let stale_gen = *stale_gen;
                let mut telem = self
                    .lifecycle_telemetry
                    .write()
                    .map_err(|_| SystemActivationError::PoisonedLock {
                        solar_system_id: Some(solar_system_id),
                        lock: "lifecycle_telemetry",
                    })?;
                telem.record_aborted_loading(
                    solar_system_id,
                    stale_gen,
                    "reclaimed_aborted_loading_on_wake",
                );
                SystemLifecycleState::Loading
            }
            Some(SystemLifecycleSlot::Stopping { gate, .. })
                if matches!(gate.current_resolution(), GateResolution::Pending) =>
            {
                if any_lock_held {
                    return Err(SystemActivationError::WaitRejectedWhileHoldingLocks {
                        solar_system_id,
                    });
                }
                return Ok(SlotStep::Wait(Arc::clone(gate)));
            }
            Some(SystemLifecycleSlot::Stopping {
                generation: stale_gen,
                ..
            }) => {
                let stale_gen = *stale_gen;
                let mut telem = self
                    .lifecycle_telemetry
                    .write()
                    .map_err(|_| SystemActivationError::PoisonedLock {
                        solar_system_id: Some(solar_system_id),
                        lock: "lifecycle_telemetry",
                    })?;
                telem.record_settled_cold(
                    solar_system_id,
                    stale_gen,
                    SystemLifecycleState::Stopping,
                    "reclaimed_abandoned_stopping_on_wake",
                );
                SystemLifecycleState::Stopping
            }
            Some(SystemLifecycleSlot::Live {
                generation: dead_gen,
                handle,
            }) => {
                let dead_gen = *dead_gen;
                self.clear_db_ballpark_for_handle(solar_system_id, handle);
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

        let generation = self.allocate_lifecycle_generation();
        let gate = Arc::new(LifecycleGate::new_loading());
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
            if let Ok(mut telem) = self.lifecycle_telemetry.write() {
                telem.record_discard_stale(solar_system_id, generation, observed_state, wake_reason);
            }
            drop(guard);
            loading_guard.disarm();
            gate.resolve(GateResolution::Aborted);
            self.shutdown_unregistered_handle(handle);
            return Err(SystemActivationError::ConcurrentActivationAborted { solar_system_id });
        }

        let telem_ok = self
            .lifecycle_telemetry
            .write()
            .map(|mut telem| telem.record_publish_live(solar_system_id, generation, wake_reason))
            .unwrap_or(false);
        if !telem_ok {
            let _ = remove_matching_loading_slot(
                &mut guard,
                &self.lifecycle_telemetry,
                solar_system_id,
                generation,
            );
            loading_guard.disarm();
            drop(guard);
            gate.resolve(GateResolution::Aborted);
            self.shutdown_unregistered_handle(handle);
            return Err(SystemActivationError::ConcurrentActivationAborted { solar_system_id });
        }

        if let Some(ref db) = self.db {
            register_db_system_ballpark(db, solar_system_id, &handle);
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

    fn move_live_slot_to_stopping_and_spawn(
        self: &Arc<Self>,
        mut guard: RankedRwLockWriteGuard<
            '_,
            HashMap<SolarSystemId, SystemLifecycleSlot>,
            { RANK_SYSTEM_REGISTRY },
        >,
        solar_system_id: SolarSystemId,
        generation: u64,
        handle: SystemHandle,
        origin: StopOrigin,
    ) -> Arc<LifecycleGate> {
        let gate = Arc::new(LifecycleGate::new_stopping());
        if let Ok(mut telem) = self.lifecycle_telemetry.write() {
            telem.record_begin_stopping(
                solar_system_id,
                generation,
                origin.from_state(),
                origin.begin_detail(),
            );
        }
        guard.insert(
            solar_system_id,
            SystemLifecycleSlot::Stopping {
                generation,
                gate: Arc::clone(&gate),
            },
        );
        drop(guard);
        tokio::spawn(Arc::clone(self).run_stop_system_task(
            solar_system_id,
            generation,
            Arc::clone(&gate),
            handle,
            origin,
        ));
        gate
    }

    fn begin_stop_slot(self: &Arc<Self>, solar_system_id: SolarSystemId) -> StopBeginAction {
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
                if handle.cmd_tx.is_closed() {
                    self.clear_db_ballpark_for_handle(solar_system_id, &handle);
                    if let Ok(mut telem) = self.lifecycle_telemetry.write() {
                        telem.record_settled_cold(
                            solar_system_id,
                            generation,
                            SystemLifecycleState::Live,
                            "stop_dead_live_to_cold",
                        );
                    }
                    return StopBeginAction::AlreadyCold;
                }
                let from_state = if self.is_handle_hibernated_safe(&handle) {
                    SystemLifecycleState::Hibernating
                } else {
                    SystemLifecycleState::Live
                };
                let gate = self.move_live_slot_to_stopping_and_spawn(
                    guard,
                    solar_system_id,
                    generation,
                    handle,
                    StopOrigin::StopSystem { from_state },
                );
                StopBeginAction::WaitOnStopping { gate }
            }
        }
    }

    fn try_begin_evict_stopping(
        self: &Arc<Self>,
        solar_system_id: SolarSystemId,
        candidate_gen: u64,
        candidate_handle: SystemHandle,
        now_unix: u64,
        min_idle_secs: u64,
        min_idle_ticks: u64,
    ) -> Option<Arc<LifecycleGate>> {
        let guard = self.systems.write().unwrap_or_else(|e| e.into_inner());
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
        Some(self.move_live_slot_to_stopping_and_spawn(
            guard,
            solar_system_id,
            candidate_gen,
            candidate_handle,
            StopOrigin::IdleEvict,
        ))
    }

    fn finish_stopping_slot(
        &self,
        solar_system_id: SolarSystemId,
        generation: u64,
        gate: &Arc<LifecycleGate>,
        detail: &'static str,
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
        }
        gate.resolve(GateResolution::SettledToCold);
    }

    pub(crate) fn current_wallclock_ms(&self) -> u64 {
        self.db
            .as_ref()
            .map(|db| machonet_wallclock_ms(db.clock().as_ref(), &self.unrebased_clock_offset_secs))
            .unwrap_or(0)
    }

    fn clear_db_ballpark_for_handle(&self, solar_system_id: SolarSystemId, handle: &SystemHandle) {
        if let Some(ref db) = self.db {
            clear_db_system_ballpark_if_matching(db, solar_system_id, handle);
        }
    }

    pub(crate) fn shutdown_unregistered_handle(&self, handle: SystemHandle) {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        match handle
            .cmd_tx
            .try_send(SystemCommand::ShutdownActor { reply: tx })
        {
            Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(cmd)) => {
                let cmd_tx = handle.cmd_tx.clone();
                if let Ok(rt) = tokio::runtime::Handle::try_current() {
                    rt.spawn(async move {
                        tokio::select! {
                            _ = cmd_tx.closed() => {}
                            _ = cmd_tx.send(cmd) => {}
                        }
                    });
                }
            }
        }
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
    fn run_after_read_probe_hook_for_test(&self, solar_system_id: SolarSystemId) {
        let hook = self
            .lifecycle_test_hooks
            .after_read_probe_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = hook {
            hook(solar_system_id);
        }
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
    async fn run_before_stop_finish_hook_for_test(
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
            let _ = tokio::task::spawn_blocking(move || hook(solar_system_id, generation)).await;
        }
    }

    #[cfg(test)]
    pub(crate) fn set_after_read_probe_hook_for_test(
        &self,
        hook: Option<Arc<dyn Fn(SolarSystemId) + Send + Sync>>,
    ) {
        *self
            .lifecycle_test_hooks
            .after_read_probe_hook
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = hook;
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
            .unwrap_or_else(|| self.allocate_lifecycle_generation());
        guard.insert(
            solar_system_id,
            SystemLifecycleSlot::Live { generation, handle },
        );
    }
}
