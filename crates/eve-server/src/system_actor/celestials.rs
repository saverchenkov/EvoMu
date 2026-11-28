//! Celestial & asteroid belt population and `SystemHandle` client methods for `system_actor`.

#![forbid(unsafe_code)]

use super::{
    ActiveModuleCycle, ActiveNaniteRepair, AsteroidState, CharacterId,
    MiningExtractionResult, ProtectionBreakEvents, SystemCommand, TickBroadcast,
};
use eve_db::lock_rank::{
    assert_no_locks_held, RankedMutex, RankedRwLock, RANK_ACTOR_STATE, RANK_BALLPARK, RANK_LEAF,
};
use eve_destiny::{
    Ball, BallId, Ballpark, CombatState, DamageVector, DestinyEvent, MovementMode, SlimItemMeta,
    TargetLock,
};
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, oneshot};

#[cfg(test)]
pub(crate) type SpawnBallWithMetaTestHook = Box<
    dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + 'static,
>;

#[cfg(test)]
type SpawnBallWithMetaHookMap = std::sync::OnceLock<
    std::sync::Mutex<HashMap<(SolarSystemId, BallId), SpawnBallWithMetaTestHook>>,
>;

#[cfg(test)]
static SPAWN_BALL_WITH_META_AFTER_SEND_HOOKS: SpawnBallWithMetaHookMap =
    std::sync::OnceLock::new();

#[cfg(test)]
static SPAWN_BALL_WITH_META_PRE_REPLY_HOOKS: SpawnBallWithMetaHookMap =
    std::sync::OnceLock::new();

#[cfg(test)]
fn set_spawn_ball_with_meta_hook_in(
    slot: &SpawnBallWithMetaHookMap,
    solar_system_id: SolarSystemId,
    ball_id: BallId,
    hook: Option<SpawnBallWithMetaTestHook>,
) {
    let map = slot.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    match hook {
        Some(h) => {
            guard.insert((solar_system_id, ball_id), h);
        }
        None => {
            guard.remove(&(solar_system_id, ball_id));
        }
    }
}

#[cfg(test)]
fn take_spawn_ball_with_meta_hook_for_test(
    slot: &SpawnBallWithMetaHookMap,
    solar_system_id: SolarSystemId,
    ball_id: BallId,
) -> Option<SpawnBallWithMetaTestHook> {
    let map = slot.get()?;
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard.remove(&(solar_system_id, ball_id))
}

#[cfg(test)]
pub(crate) fn set_spawn_ball_with_meta_after_send_hook_for_test(
    solar_system_id: SolarSystemId,
    ball_id: BallId,
    hook: Option<SpawnBallWithMetaTestHook>,
) {
    set_spawn_ball_with_meta_hook_in(
        &SPAWN_BALL_WITH_META_AFTER_SEND_HOOKS,
        solar_system_id,
        ball_id,
        hook,
    );
}

#[cfg(test)]
pub(crate) fn set_spawn_ball_with_meta_pre_reply_hook_for_test(
    solar_system_id: SolarSystemId,
    ball_id: BallId,
    hook: Option<SpawnBallWithMetaTestHook>,
) {
    set_spawn_ball_with_meta_hook_in(
        &SPAWN_BALL_WITH_META_PRE_REPLY_HOOKS,
        solar_system_id,
        ball_id,
        hook,
    );
}

enum InnerBallparkWriteGuard<'a> {
    Ranked(eve_db::lock_rank::RankedRwLockWriteGuard<'a, Ballpark, RANK_BALLPARK>),
    Unranked(std::sync::RwLockWriteGuard<'a, Ballpark>),
}

impl std::ops::Deref for InnerBallparkWriteGuard<'_> {
    type Target = Ballpark;

    fn deref(&self) -> &Self::Target {
        match self {
            Self::Ranked(g) => g,
            Self::Unranked(g) => g,
        }
    }
}

impl std::ops::DerefMut for InnerBallparkWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Ranked(g) => g,
            Self::Unranked(g) => g,
        }
    }
}

/// Exclusive write guard for [`BallparkHandle`].
///
/// Captures a fresh [`super::SystemView`] snapshot while releasing `RANK_BALLPARK` before
/// notifying watch subscribers on non-panicking drops, keeping [`SystemHandle::view`] coherent
/// across unmigrated `.ballpark.write()` / `.try_write()` callers.
pub struct BallparkWriteGuard<'a> {
    guard: Option<InnerBallparkWriteGuard<'a>>,
    view_tx: &'a tokio::sync::watch::Sender<Arc<super::SystemView>>,
    tidi_factor_bits: &'a std::sync::atomic::AtomicU64,
    actor_state: Option<&'a std::sync::OnceLock<std::sync::Weak<RankedRwLock<super::SystemActorState, RANK_ACTOR_STATE>>>>,
}

impl std::ops::Deref for BallparkWriteGuard<'_> {
    type Target = Ballpark;

    fn deref(&self) -> &Self::Target {
        self.guard
            .as_deref()
            .expect("BallparkWriteGuard deref after drop")
    }
}

impl std::ops::DerefMut for BallparkWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard
            .as_deref_mut()
            .expect("BallparkWriteGuard deref_mut after drop")
    }
}

impl std::fmt::Debug for BallparkWriteGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BallparkWriteGuard").finish_non_exhaustive()
    }
}

pub(crate) fn clamp_tidi_factor_bits(bits: &std::sync::atomic::AtomicU64) -> f64 {
    let raw = f64::from_bits(bits.load(std::sync::atomic::Ordering::Relaxed));
    if raw.is_finite() {
        raw.clamp(0.10, 1.0)
    } else {
        1.0
    }
}

/// Attempts non-blocking shared read access, recovering the inner guard if the lock is
/// poisoned and returning `None` only on `WouldBlock` (`#1173`).
pub(in crate::system_actor) fn try_read_recover_poison<T, const RANK: u16>(
    lock: &RankedRwLock<T, RANK>,
) -> Option<eve_db::lock_rank::RankedRwLockReadGuard<'_, T, RANK>> {
    match lock.try_read() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::Poisoned(poison)) => Some(poison.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

impl Drop for BallparkWriteGuard<'_> {
    fn drop(&mut self) {
        let Some(guard) = self.guard.take() else {
            return;
        };
        if std::thread::panicking() {
            drop(guard);
            return;
        }
        let tidi = clamp_tidi_factor_bits(self.tidi_factor_bits);
        let prev = { self.view_tx.borrow().clone() };
        let actor_st_arc = self
            .actor_state
            .and_then(|cell| cell.get())
            .and_then(|weak| weak.upgrade())
            .filter(|_| {
                !eve_db::lock_rank::held_lock_ranks()
                    .iter()
                    .any(|&r| r >= RANK_ACTOR_STATE)
            });
        let actor_st_guard = actor_st_arc
            .as_ref()
            .and_then(|st_lock| try_read_recover_poison(st_lock));
        let next = super::SystemView::from_ballpark(
            &guard,
            actor_st_guard.as_deref(),
            tidi,
            Some(prev.as_ref()),
        );
        drop(actor_st_guard);
        // Release RANK_BALLPARK and pop its thread-local held rank before sending on the watch channel.
        drop(guard);
        send_view_preserving_atomic_tidi(&self.view_tx, &self.tidi_factor_bits, next);
    }
}

/// Publishes a [`super::SystemView`] snapshot while ensuring `tidi_factor` reflects the latest
/// lock-free `tidi_factor_bits` inside the watch lock so concurrent `set_tidi_factor_raw` updates
/// cannot be overwritten by an older snapshot.
pub(in crate::system_actor) fn send_view_preserving_atomic_tidi(
    view_tx: &tokio::sync::watch::Sender<Arc<super::SystemView>>,
    tidi_factor_bits: &std::sync::atomic::AtomicU64,
    mut next: super::SystemView,
) {
    view_tx.send_modify(|v| {
        next.tidi_factor = clamp_tidi_factor_bits(tidi_factor_bits);
        *v = Arc::new(next);
    });
}

/// Rank-checked `Ballpark` lock wrapper that automatically refreshes the published
/// [`super::SystemView`] snapshot whenever a write guard drops cleanly.
pub struct BallparkHandle {
    pub(in crate::system_actor) inner: RankedRwLock<Ballpark, RANK_BALLPARK>,
    pub(in crate::system_actor) view_tx: Arc<tokio::sync::watch::Sender<Arc<super::SystemView>>>,
    pub(in crate::system_actor) tidi_factor_bits: Arc<std::sync::atomic::AtomicU64>,
    pub(in crate::system_actor) actor_state:
        std::sync::OnceLock<std::sync::Weak<RankedRwLock<super::SystemActorState, RANK_ACTOR_STATE>>>,
    drone_hydration: std::sync::OnceLock<super::drones::ActiveDronesSharedCtx>,
}

impl std::ops::Deref for BallparkHandle {
    type Target = RankedRwLock<Ballpark, RANK_BALLPARK>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl BallparkHandle {
    /// Wraps `ballpark` alongside the system's view watch sender and TiDi factor atomic.
    pub(crate) fn new(
        ballpark: Ballpark,
        view_tx: Arc<tokio::sync::watch::Sender<Arc<super::SystemView>>>,
        tidi_factor_bits: Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        Self {
            inner: RankedRwLock::new(ballpark),
            view_tx,
            tidi_factor_bits,
            actor_state: std::sync::OnceLock::new(),
            drone_hydration: std::sync::OnceLock::new(),
        }
    }

    #[inline]
    pub(in crate::system_actor) fn attach_drone_hydration(&self, s: super::drones::ActiveDronesSharedCtx) { let _ = self.drone_hydration.set(s); }
    #[inline]
    pub(in crate::system_actor) fn drone_hydration_shared(&self) -> Option<&super::drones::ActiveDronesSharedCtx> { self.drone_hydration.get() }

    /// Attaches the owning system's `actor_state` lock so drop-time `SystemView` refreshes
    /// capture `sim_elapsed_secs` and active-module flags when `actor_state` is rank-safe.
    pub(crate) fn attach_actor_state(
        &self,
        actor_state: &Arc<RankedRwLock<super::SystemActorState, RANK_ACTOR_STATE>>,
    ) {
        let _ = self.actor_state.set(Arc::downgrade(actor_state));
    }

    fn wrap_guard<'a>(&'a self, inner: InnerBallparkWriteGuard<'a>) -> BallparkWriteGuard<'a> {
        BallparkWriteGuard {
            guard: Some(inner),
            view_tx: &self.view_tx,
            tidi_factor_bits: &self.tidi_factor_bits,
            actor_state: Some(&self.actor_state),
        }
    }

    fn map_try_guard<'a, G>(
        res: std::sync::TryLockResult<G>,
        wrap: impl FnOnce(G) -> BallparkWriteGuard<'a>,
    ) -> std::sync::TryLockResult<BallparkWriteGuard<'a>> {
        match res {
            Ok(g) => Ok(wrap(g)),
            Err(std::sync::TryLockError::Poisoned(poison)) => Err(
                std::sync::TryLockError::Poisoned(std::sync::PoisonError::new(wrap(
                    poison.into_inner(),
                ))),
            ),
            Err(std::sync::TryLockError::WouldBlock) => Err(std::sync::TryLockError::WouldBlock),
        }
    }

    /// Acquires exclusive `Ballpark` write access and refreshes [`super::SystemView`] on drop.
    #[track_caller]
    pub fn write(&self) -> std::sync::LockResult<BallparkWriteGuard<'_>> {
        match self.inner.write() {
            Ok(g) => Ok(self.wrap_guard(InnerBallparkWriteGuard::Ranked(g))),
            Err(poison) => Err(std::sync::PoisonError::new(
                self.wrap_guard(InnerBallparkWriteGuard::Ranked(poison.into_inner())),
            )),
        }
    }

    /// Attempts non-blocking exclusive `Ballpark` write access and refreshes [`super::SystemView`] on drop.
    #[track_caller]
    pub fn try_write(&self) -> std::sync::TryLockResult<BallparkWriteGuard<'_>> {
        Self::map_try_guard(self.inner.try_write(), |g| {
            self.wrap_guard(InnerBallparkWriteGuard::Ranked(g))
        })
    }

    /// Attempts unranked exclusive `Ballpark` write access and refreshes [`super::SystemView`] on drop.
    pub fn try_write_unranked(&self) -> std::sync::TryLockResult<BallparkWriteGuard<'_>> {
        Self::map_try_guard(self.inner.try_write_unranked(), |g| {
            self.wrap_guard(InnerBallparkWriteGuard::Unranked(g))
        })
    }

    /// Internal write lock without drop-time `SystemView` publish for callers that already
    /// publish alongside `SystemActorState`.
    #[track_caller]
    pub(crate) fn write_untracked(
        &self,
    ) -> std::sync::LockResult<eve_db::lock_rank::RankedRwLockWriteGuard<'_, Ballpark, RANK_BALLPARK>>
    {
        self.inner.write()
    }
}

impl SystemHandle {
    /// Returns the latest lock-free published [`super::SystemView`] snapshot.
    pub fn view(&self) -> Arc<super::SystemView> {
        self.view_rx.borrow().clone()
    }

    pub(in crate::system_actor) fn has_in_flight_post_unlock(&self) -> bool {
        self.post_unlock_in_flight
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
    }

    /// Publishes a fresh [`super::SystemView`] from an already-locked `ballpark` and `actor_state`.
    pub(crate) fn publish_view_from(
        &self,
        ballpark: &Ballpark,
        actor_state: &super::SystemActorState,
    ) {
        let tidi = self.tidi_factor();
        let next = super::SystemView::from_ballpark(ballpark, Some(actor_state), tidi, None);
        send_view_preserving_atomic_tidi(&self.view_tx, &self.tidi_factor_bits, next);
    }

    #[track_caller]
    pub(crate) fn cmd_sender(&self) -> &mpsc::Sender<SystemCommand> {
        assert_no_locks_held();
        &self.cmd_tx
    }
    pub fn subscribe(&self) -> broadcast::Receiver<TickBroadcast> {
        self.hibernated
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.hibernated_since_unix
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.idle_ticks
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.event_tx.subscribe()
    }

    /// Returns `true` when this solar system has no active session subscribers, no dynamic balls,
    /// and no active module/repair/anchoring timers (`#919`, `#866`).
    pub fn is_hibernated(&self) -> bool {
        if self.event_tx.receiver_count() > 0 {
            return false;
        }
        if self
            .dynamic_ball_count
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
        {
            return false;
        }
        if !self.hibernated.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let view = self.view();
        if view.balls.keys().any(|&bid| !view.is_static_ball(bid)) {
            return false;
        }
        true
    }

    pub fn hibernated_since_unix(&self) -> u64 {
        self.hibernated_since_unix
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn idle_ticks(&self) -> u64 {
        self.idle_ticks.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn record_idle_tick(&self) -> u64 {
        self.idle_ticks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1
    }

    pub fn get_ball_position_sync(&self, ball_id: BallId) -> Option<DVec3> {
        if eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_LEAF)
        {
            self.ball_positions
                .read_unranked()
                .unwrap_or_else(|e| e.into_inner())
                .get(&ball_id)
                .copied()
        } else {
            self.ball_positions
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&ball_id)
                .copied()
        }
    }

    fn read_leaf_ball_set(
        set: &RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>,
        ball_id: BallId,
    ) -> bool {
        if eve_db::lock_rank::held_lock_ranks()
            .iter()
            .any(|&r| r >= eve_db::lock_rank::RANK_LEAF)
        {
            set.read_unranked()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&ball_id)
        } else {
            set.read()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&ball_id)
        }
    }

    pub fn is_ball_scrambled_sync(&self, ball_id: BallId) -> bool {
        Self::read_leaf_ball_set(&self.scrambled_balls, ball_id)
    }

    /// Lock-free (wrt `Ballpark`) check of whether `ball_id` is currently tethered (`#1167`).
    pub fn is_ball_tethered_sync(&self, ball_id: BallId) -> bool {
        Self::read_leaf_ball_set(&self.tethered_balls, ball_id)
    }

    /// Reads the tracked activation state for stargate `gate_id`, if present (`#1080`).
    pub fn stargate_activation_state(&self, gate_id: BallId) -> Option<eve_destiny::StargateActivationState> {
        self.view().stargate_activation_state(gate_id)
    }

    pub async fn spawn_ball(&self, ball: Ball) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SpawnBall { ball, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn set_ball_max_velocity(
        &self,
        ball_id: BallId,
        max_velocity: f64,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetBallMaxVelocity {
                ball_id,
                max_velocity,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn set_ball_max_velocity_cap(
        &self,
        ball_id: BallId,
        cap: Option<f64>,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetBallMaxVelocityCap {
                ball_id,
                cap,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn spawn_ball_with_combat(
        &self,
        ball: Ball,
        combat_state: CombatState,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SpawnBallWithCombat {
                ball,
                combat_state,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn spawn_ball_with_meta(
        &self,
        ball: Ball,
        combat_state: CombatState,
        slim_meta: SlimItemMeta,
    ) -> Result<u64, String> {
        self.spawn_ball_with_meta_impl(ball, combat_state, slim_meta, None)
            .await
    }

    pub async fn spawn_ball_with_meta_tagged(
        &self,
        ball: Ball,
        combat_state: CombatState,
        slim_meta: SlimItemMeta,
        spawn_token: u64,
    ) -> Result<u64, String> {
        self.spawn_ball_with_meta_impl(ball, combat_state, slim_meta, Some(spawn_token))
            .await
    }

    async fn spawn_ball_with_meta_impl(
        &self,
        ball: Ball,
        combat_state: CombatState,
        slim_meta: SlimItemMeta,
        spawn_token: Option<u64>,
    ) -> Result<u64, String> {
        #[cfg(test)]
        let ball_id = ball.id;
        #[cfg(test)]
        let (after_send_hook, pre_reply_hook) = (
            take_spawn_ball_with_meta_hook_for_test(
                &SPAWN_BALL_WITH_META_AFTER_SEND_HOOKS,
                self.solar_system_id,
                ball_id,
            ),
            take_spawn_ball_with_meta_hook_for_test(
                &SPAWN_BALL_WITH_META_PRE_REPLY_HOOKS,
                self.solar_system_id,
                ball_id,
            ),
        );
        let (tx, rx) = oneshot::channel();
        let cmd = match spawn_token {
            Some(tok) => SystemCommand::SpawnBallWithMetaTagged {
                ball,
                combat_state,
                slim_meta,
                spawn_token: tok,
                reply: tx,
            },
            None => SystemCommand::SpawnBallWithMeta {
                ball,
                combat_state,
                slim_meta,
                reply: tx,
            },
        };
        self.cmd_sender()
            .send(cmd)
            .await
            .map_err(|_| format!("system {} actor closed", self.solar_system_id))?;
        #[cfg(test)]
        if let Some(hook) = after_send_hook {
            hook().await;
        }
        let res = rx
            .await
            .map_err(|_| {
                format!(
                    "system {} actor dropped SpawnBallWithMeta reply",
                    self.solar_system_id
                )
            })?;
        #[cfg(test)]
        if let Some(hook) = pre_reply_hook {
            hook().await;
        }
        res
    }

    /// Updates the slim item metadata of an in-space ball and emits `DestinyEvent::OnSlimItemChange`
    /// to every observer on its grid via `Ballpark::change_slim_item`.
    pub async fn change_slim_item(
        &self,
        ball_id: BallId,
        slim_meta: SlimItemMeta,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ChangeSlimItem {
                ball_id,
                slim_meta,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn set_ball_wormhole_stages(
        &self,
        ball_id: BallId,
        wormhole_age: i64,
        wormhole_size: f64,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetBallWormholeStages {
                ball_id,
                wormhole_age,
                wormhole_size,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn get_ball_slim_item(
        &self,
        ball_id: BallId,
    ) -> Result<Option<eve_proto::PyRep>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::GetBallSlimItem { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn spawn_asteroid(
        &self,
        asteroid: AsteroidState,
        ball: Ball,
    ) -> Result<BallId, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SpawnAsteroid {
                asteroid,
                ball,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn mine_asteroid(
        &self,
        asteroid_id: BallId,
        requested_units: u64,
        waste_probability: f64,
    ) -> Result<MiningExtractionResult, String> {
        self.mine_asteroid_with_db_sync(asteroid_id, requested_units, waste_probability, true)
            .await
    }

    pub async fn mine_asteroid_with_db_sync(
        &self,
        asteroid_id: BallId,
        requested_units: u64,
        waste_probability: f64,
        persist_db: bool,
    ) -> Result<MiningExtractionResult, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::MineAsteroid {
                asteroid_id,
                requested_units,
                waste_probability,
                persist_db,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn get_asteroid_state(
        &self,
        asteroid_id: BallId,
    ) -> Result<Option<AsteroidState>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::GetAsteroidState {
                asteroid_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn break_gate_cloak(&self, ball_id: BallId) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::BreakGateCloak {
                ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn populate_asteroid_belt_entities(&self) -> Result<Vec<AsteroidState>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::PopulateAsteroids { reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn remove_ball(&self, ball_id: BallId) -> Option<Ball> {
        self.ball_positions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ball_id);
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_sender()
            .send(SystemCommand::RemoveBall {
                ball_id,
                reply: tx,
            })
            .await;
        rx.await.ok().flatten()
    }

    /// Removes `ball_id` only if its active actor spawn token still equals `spawn_token`.
    /// Never pre-clears `ball_positions` or nanite repairs if a newer spawn owns `ball_id`.
    pub async fn remove_ball_if_token(
        &self,
        ball_id: BallId,
        spawn_token: u64,
    ) -> Option<Ball> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_sender()
            .send(SystemCommand::RemoveBallIfToken {
                ball_id,
                spawn_token,
                reply: tx,
            })
            .await;
        rx.await.ok().flatten()
    }

    pub async fn accept_docking(
        &self,
        ship_id: BallId,
        station_id: BallId,
        station_pos: DVec3,
    ) -> Option<Ball> {
        self.ball_positions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ship_id);
        let (tx, rx) = oneshot::channel();
        let _ = self
            .cmd_sender()
            .send(SystemCommand::AcceptDocking {
                ship_id,
                station_id,
                station_pos,
                reply: tx,
            })
            .await;
        rx.await.ok().flatten()
    }

    /// Atomically removes `ball_id` from this solar system (`#842`, `#917` E-2),
    /// clearing its active module cycles and target locks and returning its final
    /// `Ball`, `CombatState`, and `SlimItemMeta`. Returns `None` if `ball_id` is
    /// not present in space.
    async fn take_ball_impl(
        &self,
        ball_id: BallId,
        accept_docking_target: Option<(BallId, DVec3)>,
        preserve_db_active_states: bool,
    ) -> Option<super::TakenBall> {
        self.ball_positions
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ball_id);
        super::protection::call_actor_reply(self, "TakeBall", |reply| SystemCommand::TakeBall {
            ball_id,
            accept_docking_target,
            preserve_db_active_states,
            reply,
        })
        .await
        .ok()
        .flatten()
    }

    /// Atomically removes `ball_id` from space and returns its `TakenBall` snapshot, or `None` if
    /// not present in space.
    pub async fn take_ball(&self, ball_id: BallId) -> Option<super::TakenBall> {
        self.take_ball_impl(ball_id, None, false).await
    }

    /// Atomically removes `ball_id` for a stargate transition while keeping DB active module
    /// states intact until `commit_stargate_db_transition` deactivates them (`#1237` `BF-1237C-02`).
    pub async fn take_ball_preserving_db(&self, ball_id: BallId) -> Option<super::TakenBall> {
        self.take_ball_impl(ball_id, None, true).await
    }

    /// Atomically removes `ship_id` for docking at `station_id` (`#842`, `#917` E-2, `#1188`),
    /// preserving any queued observer events and returning its final `TakenBall` snapshot,
    /// or `None` if `ship_id` is not present in space.
    pub async fn take_ball_for_docking(
        &self,
        ship_id: BallId,
        station_id: BallId,
        station_pos: DVec3,
    ) -> Option<super::TakenBall> {
        self.take_ball_impl(ship_id, Some((station_id, station_pos)), false)
            .await
    }

    async fn restore_ball_impl(
        &self,
        taken: super::TakenBall,
        only_if_absent: bool,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        let cmd = if only_if_absent {
            SystemCommand::RestoreBallIfAbsent { taken, reply: tx }
        } else {
            SystemCommand::RestoreBall { taken, reply: tx }
        };
        self.cmd_sender()
            .send(cmd)
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    /// Re-inserts a previously [`Self::take_ball`] snapshot into this solar system
    /// when a handoff's subsequent DB transaction fails (`#842`, `#917` E-2).
    pub async fn restore_ball(&self, taken: super::TakenBall) -> Result<u64, String> {
        self.restore_ball_impl(taken, false).await
    }

    /// Atomically re-inserts `taken` into this solar system only if `taken.ball.id`
    /// is not currently present in `ballpark.balls` (`#1237`).
    pub async fn restore_ball_if_absent(&self, taken: super::TakenBall) -> Result<u64, String> {
        self.restore_ball_impl(taken, true).await
    }

    pub async fn emit_packaged_actions(
        &self,
        ball_id: BallId,
        events: Vec<DestinyEvent>,
    ) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_sender()
            .send(SystemCommand::EmitPackagedActions {
                ball_id,
                events,
                reply: tx,
            })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    pub fn has_active_nanite_repair_sync(&self, ship_id: BallId, module_id: i64) -> bool {
        self.view()
            .active_nanite_repairs
            .iter()
            .any(|r| r.ship_id == ship_id && r.module_id == module_id)
    }

    pub fn has_active_module_cycle_sync(&self, ship_id: BallId, module_id: i64) -> bool {
        self.view()
            .active_module_cycles
            .iter()
            .any(|c| c.ship_id == ship_id && c.module_id == module_id)
    }

    pub fn has_active_mjd_or_mjfg_spool_sync(&self, ship_id: BallId) -> bool {
        self.view()
            .active_module_cycles
            .iter()
            .any(|c| c.ship_id == ship_id && c.mjd_jump_distance_m.is_some())
    }

    pub fn is_module_mjd_or_mjfg_spooling_sync(&self, ship_id: BallId, module_id: i64) -> bool {
        self.view()
            .active_module_cycles
            .iter()
            .any(|c| c.ship_id == ship_id && c.module_id == module_id && c.mjd_jump_distance_m.is_some())
    }

    pub(in crate::system_actor) fn drain_ship_nanite_repairs_collecting(
        active_nanite_repairs: &mut Vec<super::ActiveNaniteRepair>,
        ship_id: BallId,
        refund_paste: bool,
        ops: &mut super::post_unlock::PostUnlockOps,
    ) {
        active_nanite_repairs.retain(|r| {
            if r.ship_id == ship_id {
                let frac = (r.elapsed_s / r.duration_s.max(0.001)).clamp(0.0, 1.0);
                let rem_dmg = (r.initial_damage_hp * (1.0 - frac)).max(0.0);
                let paste_used = ((r.paste_total_needed as f64) * frac).ceil() as i32;
                let refund = (r.paste_consumed_so_far
                    - paste_used.clamp(0, r.paste_consumed_so_far))
                .max(0);
                ops.push(super::post_unlock::PostUnlockIntent::NaniteRepairCancel(
                    super::post_unlock::NaniteRepairCancelDbOp {
                        char_id: r.char_id,
                        ship_id,
                        module_id: r.module_id,
                        rem_dmg,
                        refund_paste,
                        refund,
                    },
                ));
                false
            } else {
                true
            }
        });
    }

    /// Inserts a rigid static asteroid ball and neutral `SlimItemMeta` into `bp` without adding it
    /// to dynamic `grids`/`ball_grid_ids`, and broadcasts `AddBalls2` to all grid observers.
    pub(crate) fn spawn_asteroid_ball_in_ballpark(
        bp: &mut Ballpark,
        sde_opt: &Option<Arc<SdeRegistry>>,
        ast_ball: &mut Ball,
    ) {
        let ball_id = ast_ball.id;
        ast_ball.mode = MovementMode::Rigid;
        let (group_id, category_id, name) = sde_opt
            .as_ref()
            .and_then(|s| s.types.get(&ast_ball.type_id))
            .map(|t| {
                (
                    t.group_id,
                    t.category_id,
                    (!t.name.is_empty()).then(|| t.name.clone()),
                )
            })
            .unwrap_or((25, 25, None));
        let _ = bp.add_static_ball_with_meta(
            ast_ball.clone(),
            SlimItemMeta {
                group_id,
                category_id,
                owner_id: 1,
                char_id: None,
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                name,
                ..Default::default()
            },
        );
        // Keep spawned asteroids static outside dynamic `grids`/`ball_grid_ids` (matching
        // `belts.rs`) so empty dynamic grids contract/delete and `remove_ball` emits `RemoveBall`.
        bp.queue_event_for_all_grid_observers(
            Some(ball_id),
            &DestinyEvent::AddBalls2 {
                balls: vec![ast_ball.clone()],
            },
        );
    }

    pub fn tidi_factor(&self) -> f64 {
        clamp_tidi_factor_bits(&self.tidi_factor_bits)
    }

    pub fn set_tidi_factor_raw(&self, factor: f64) -> f64 {
        let clamped = if factor.is_finite() {
            factor.clamp(0.10, 1.0)
        } else {
            1.0
        };
        let prev_bits = self
            .tidi_factor_bits
            .swap(clamped.to_bits(), std::sync::atomic::Ordering::Relaxed);
        let prev = f64::from_bits(prev_bits);
        let prev_clamped = if prev.is_finite() {
            prev.clamp(0.10, 1.0)
        } else {
            1.0
        };
        if clamped < 0.999_999 {
            self.cooldown_persist_gate.mark_tidi_history();
        }
        if (clamped - prev_clamped).abs() > f64::EPSILON {
            self.view_tx.send_if_modified(|v| {
                let latest = clamp_tidi_factor_bits(&self.tidi_factor_bits);
                if (v.tidi_factor - latest).abs() <= f64::EPSILON {
                    return false;
                }
                let mut next = (**v).clone();
                next.tidi_factor = latest;
                *v = Arc::new(next);
                true
            });
        }
        prev_clamped
    }

    /// Records an owner-visible or prime (`SetState`) Destiny stamp so subsequent `OnSpecialFX`
    /// and activation stamps never backstep behind it while preserving scene stamp (`#1081`).
    pub fn record_visible_or_prime_stamp(&self, stamp: u64) {
        self.last_visible_stamp
            .fetch_max(stamp, std::sync::atomic::Ordering::AcqRel);
    }

    /// Returns the highest owner-visible / prime Destiny stamp recorded on this system (`#1081`).
    pub fn visible_or_prime_stamp(&self) -> u64 {
        self.last_visible_stamp
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Returns a monotonic `OnSpecialFX` Destiny stamp `>= base_stamp`, `>= owner_visible_stamp`,
    /// and `>= prime_update_stamp` without altering `ballpark.current_tick` (`#1081`).
    pub fn safe_fx_stamp(&self, base_stamp: u64) -> u64 {
        let vis = self
            .last_visible_stamp
            .load(std::sync::atomic::Ordering::Acquire);
        let prev_fx = self
            .last_fx_stamp
            .load(std::sync::atomic::Ordering::Acquire);
        let safe = base_stamp.max(vis).max(prev_fx);
        self.last_fx_stamp
            .fetch_max(safe, std::sync::atomic::Ordering::AcqRel);
        safe
    }

    /// Returns a history-safe Destiny stamp for an immediate module activation after a TiDi
    /// transition (e.g. `0.5 -> 1.0`), preserving the current sim stamp without backstepping (`#1081`).
    pub fn next_history_safe_activation_stamp(&self, current_tick: u64) -> u64 {
        let vis = self
            .last_visible_stamp
            .load(std::sync::atomic::Ordering::Acquire);
        let prev_fx = self
            .last_fx_stamp
            .load(std::sync::atomic::Ordering::Acquire);
        current_tick.max(vis).max(prev_fx)
    }

    /// Computes live simulation load in this solar system (dynamic balls + weighted active module/repair/anchoring cycles).
    pub fn active_entity_and_cycle_load(&self) -> usize {
        let view = self.view();
        let dyn_balls = view.balls.keys().filter(|&&bid| !view.is_static_ball(bid)).count();
        let cycle_load = view.active_module_cycles.len().saturating_mul(2)
            + view.pending_anchoring_disruptor_count
            + view.active_nanite_repairs.len();
        dyn_balls.saturating_add(cycle_load)
    }

    pub fn sim_elapsed_secs(&self) -> f64 {
        f64::from_bits(
            self.sim_elapsed_secs_bits
                .load(std::sync::atomic::Ordering::Acquire),
        )
    }

    pub async fn set_movement_mode(
        &self,
        ball_id: BallId,
        mode: MovementMode,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetMovementMode {
                ball_id,
                mode,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    /// Sets `ball_id` movement mode while atomically recording its [`WarpProvenance`] in actor
    /// state so concurrent `UpdateStateRequest` calls never misclassify a new player/fleet warp (`#1175`).
    pub async fn set_warp_movement_mode(
        &self,
        ball_id: BallId,
        mode: MovementMode,
        provenance: super::WarpProvenance,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetWarpMovementMode {
                ball_id,
                mode,
                provenance,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub fn warp_provenance(&self, ball_id: BallId) -> Option<super::WarpProvenance> {
        self.view().warp_provenance(ball_id)
    }

    pub fn active_warp_char(&self, ball_id: BallId) -> Option<CharacterId> {
        self.view().active_warp_char(ball_id)
    }

    pub fn is_active_warp(&self, ball_id: BallId) -> bool {
        self.view().is_active_warp(ball_id)
    }

    pub fn is_return_warp(&self, ball_id: BallId) -> bool {
        self.view().is_return_warp(ball_id)
    }

    pub fn is_pending_warp_finished(&self, ball_id: BallId) -> bool {
        self.view().is_pending_warp_finished(ball_id)
    }

    pub async fn set_warp_scramble_strength(
        &self,
        ball_id: BallId,
        strength: i32,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetWarpScrambleStrength {
                ball_id,
                strength,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn set_warp_core_strength(
        &self,
        ball_id: BallId,
        strength: i32,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetWarpCoreStrength {
                ball_id,
                strength,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn add_warp_bubble(
        &self,
        id: BallId,
        position: DVec3,
        radius: f64,
    ) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::AddWarpBubble {
                id,
                position,
                radius,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn remove_warp_bubble(&self, id: BallId) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::RemoveWarpBubble { id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn set_speed_fraction(&self, ball_id: BallId, fraction: f64) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetSpeedFraction {
                ball_id,
                fraction,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn set_ball_position(&self, ball_id: BallId, position: glam::DVec3) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_sender()
            .send(SystemCommand::SetBallPosition {
                ball_id,
                position,
                reply: tx,
            })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Queues `event` for all grid observers of `ball_id`; returns `false` if the ball is unknown.
    pub async fn emit_destiny_event(&self, ball_id: BallId, event: DestinyEvent) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_sender()
            .send(SystemCommand::EmitDestinyEvent { ball_id, event, reply: tx })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Queues `event` directly into `observer_ball_id`'s observer queue (returns `false` if absent).
    pub async fn emit_observer_destiny_event(
        &self,
        observer_ball_id: BallId,
        event: DestinyEvent,
    ) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_sender()
            .send(SystemCommand::EmitObserverDestinyEvent {
                observer_ball_id,
                event,
                reply: tx,
            })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Steers `ball_id` toward `point` (`beyonce.CmdGotoPoint`) and returns the current tick.
    pub async fn goto_point(&self, ball_id: BallId, point: glam::DVec3) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::GotoPoint { ball_id, point, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    /// Queues a `SetState` destiny update for `observer_ball_id` (`P-7`).
    pub async fn push_set_state(&self, observer_ball_id: BallId) -> Result<u64, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::PushSetState {
                observer_ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    /// Immediately flushes queued `DoDestinyUpdate` payloads over `event_tx` for all active
    /// peer observers (excluding `exclude_ball_id` and any observer whose queue still awaits
    /// initial `SetState` consumption), avoiding up to `1.0s` of next-tick latency.
    pub async fn flush_peer_observer_updates(
        &self,
        exclude_ball_id: Option<BallId>,
    ) -> Result<usize, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::FlushPeerObserverUpdates {
                exclude_ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn get_ball_snapshot(&self, ball_id: BallId) -> Result<Option<(u64, Ball)>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::GetBallSnapshot { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn get_ball_group_id(&self, ball_id: BallId) -> Result<Option<u32>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::GetBallGroupId { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn list_balls(&self) -> Result<Vec<Ball>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ListBalls { reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    /// Returns all balls visible to `observer_ball_id` (all static system celestials/gates/stations
    /// plus dynamic balls within the same shared `GridBubble`).
    pub async fn list_balls_in_bubble(&self, observer_ball_id: BallId) -> Result<Vec<Ball>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ListBallsInBubble {
                observer_ball_id,
                dynamic_only: false,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    /// Returns only the dynamic balls within the shared `GridBubble` of `observer_ball_id`.
    pub async fn list_dynamic_balls_in_bubble(
        &self,
        observer_ball_id: BallId,
    ) -> Result<Vec<Ball>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ListBallsInBubble {
                observer_ball_id,
                dynamic_only: true,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    /// Builds the marshaled `SetState` `KeyVal` and `DoDestinyUpdate` list for `observer_ball_id` (`P-7`, `SP-1`, `SP-2`).
    pub async fn get_set_state(
        &self,
        observer_ball_id: BallId,
    ) -> Result<(u64, PyRep, PyRep), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::GetSetState {
                observer_ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    /// Builds the marshaled `SetState` `KeyVal` and `DoDestinyUpdate` list for `observer_ball_id`
    /// along with the actor's latest `TickBroadcast.broadcast_seq` watermark at snapshot time (`#1461`).
    pub async fn get_set_state_with_seq(
        &self,
        observer_ball_id: BallId,
    ) -> Result<(u64, PyRep, PyRep, u64), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::GetSetStateWithSeq {
                observer_ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn get_bubble_id_for_ball(&self, ball_id: BallId) -> Result<Option<u64>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::GetBubbleIdForBall { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    /// Drains pending bubble-scoped `DestinyEvent`s for `observer_ball_id` and returns `(tick, events, do_destiny_update_pyrep)`.
    pub async fn drain_observer_updates(
        &self,
        observer_ball_id: BallId,
    ) -> Result<(u64, Vec<DestinyEvent>, PyRep), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::DrainObserverUpdates {
                observer_ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    /// Drains pending bubble-scoped `DestinyEvent`s for `observer_ball_id` along with the actor's
    /// latest `TickBroadcast.broadcast_seq` watermark at the instant of the drain (`GH #1172`).
    pub async fn drain_observer_updates_with_seq(
        &self,
        observer_ball_id: BallId,
    ) -> Result<(u64, Vec<DestinyEvent>, PyRep, u64), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::DrainObserverUpdatesWithSeq {
                observer_ball_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn apply_damage(&self, ball_id: BallId, damage: DamageVector) -> Result<Option<(f64, bool, Ball)>, String> {
        self.apply_damage_with_attacker(ball_id, damage, None).await
    }

    pub async fn apply_true_damage(&self, ball_id: BallId, amount: f64) -> Result<Option<(f64, bool, Ball)>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ApplyTrueDamage { ball_id, amount, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn apply_damage_with_attacker(
        &self,
        ball_id: BallId,
        damage: DamageVector,
        attacker_id: Option<CharacterId>,
    ) -> Result<Option<(f64, bool, Ball)>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ApplyDamage { ball_id, damage, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        let res = rx.await.map_err(|_| "Solar system reply dropped".to_string())?;
        if let Some((_, true, _)) = &res {
            let hook = self.npc_death_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(fut) = super::on_ball_destroyed_by_damage(
                self.solar_system_id,
                ball_id,
                attacker_id,
                hook.as_ref(),
            ) {
                let _ = fut.await;
            }
        }
        Ok(res)
    }

    /// Asks the actor to flush all dirty condition checkpoint entries and commits them to
    /// `WorldRepository` via its ordered `post_unlock_worker` (`W1`, `W4`, `W6`, `#1170`, `#919`).
    pub async fn flush_condition_checkpoint(&self) -> Result<usize, String> {
        let count = super::protection::call_actor_reply(self, "FlushConditionCheckpoint", |reply| {
            SystemCommand::FlushConditionCheckpoint { reply }
        })
        .await?;
        super::drones::flush_from_handle_async(self).await;
        Ok(count)
    }

    async fn commit_destroyed_ball_hooks(
        &self,
        dt_sim: f64,
        destroyed_balls: &[(BallId, Option<CharacterId>)],
        destroyed_lease: Option<&std::sync::Arc<super::PendingDestroyedDeliveryGuard>>,
        run_step_hook_when_empty: bool,
    ) {
        if !destroyed_balls.is_empty() {
            let hook = self
                .npc_death_hook
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            for &(target_ball_id, owner_char_id) in destroyed_balls {
                if let Some(fut) = super::on_ball_destroyed_by_damage(
                    self.solar_system_id,
                    target_ball_id,
                    owner_char_id,
                    hook.as_ref(),
                ) {
                    fut.await;
                }
            }
        }
        if run_step_hook_when_empty || !destroyed_balls.is_empty() {
            let step_hook = self
                .system_step_hook
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(hook) = step_hook {
                hook(self.solar_system_id, dt_sim, destroyed_balls.to_vec()).await;
            }
        }
        // Acknowledge delivery lease only after both `npc_death_hook` and `system_step_hook`
        // (`on_ball_destroyed_sync`) finish committing downstream artifacts.
        if let Some(lease) = destroyed_lease {
            lease.ack();
        }
    }

    /// Commits a [`super::StepOutput`] produced by this system's actor (`#919` E-4, `#839` B1):
    /// applies any staggered dirty-only condition checkpoint (`T-2`, `T-5`), invokes `npc_death_hook`
    /// for destroyed balls, and executes `system_step_hook` (`T-4` inventory-visible step outputs
    /// and player ship destruction).
    pub async fn apply_step_output(&self, output: &super::StepOutput) {
        let need_barrier = output.has_post_unlock_work
            || output
                .condition_checkpoint
                .as_ref()
                .is_some_and(|entries| !entries.is_empty())
            || !output.destroyed_balls.is_empty()
            || self.has_in_flight_post_unlock();
        let worker_alive = if need_barrier {
            self.wait_for_post_unlock_barrier().await
        } else {
            true
        };
        if let Some(ref entries) = output.condition_checkpoint {
            if !entries.is_empty() && !worker_alive {
                if let Some(ref db) = self.fighter_ctx.db_opt {
                    let _ = crate::sp6_lifecycle::apply_condition_checkpoint_for_system_in_db(
                        db,
                        self.solar_system_id,
                        entries,
                    );
                }
            }
        }
        if let Some(ref snap) = output.fighter_checkpoint { self.flush_step_fighter_checkpoint(snap); }
        self.commit_destroyed_ball_hooks(output.dt_sim, &output.destroyed_balls, output.destroyed_lease.as_ref(), true).await;
    }

    /// Steps this system by `dt` seconds and returns the full [`super::StepOutput`] after driver commit (`#919` E-4).
    pub async fn step_with_output(&self, dt: f64) -> Result<super::StepOutput, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::StepManual { dt, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        let output = rx
            .await
            .map_err(|_| "Solar system reply dropped".to_string())?;
        self.apply_step_output(&output).await;
        Ok(output)
    }

    pub async fn step_manual(&self, dt: f64) -> Result<TickBroadcast, String> {
        let output = self.step_with_output(dt).await?;
        Ok(output.broadcast.unwrap_or_else(|| TickBroadcast {
            solar_system_id: self.solar_system_id,
            tick: output.tick,
            broadcast_seq: 0,
            events: Vec::new(),
            observer_events: std::collections::BTreeMap::new(),
            observer_updates_pyrep: std::collections::BTreeMap::new(),
        }))
    }

    pub(crate) async fn start_step_manual_tick_only(
        &self,
        dt: f64,
    ) -> Result<oneshot::Receiver<super::StepOutput>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::StepManualTickOnly { dt, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        Ok(rx)
    }

    pub(crate) async fn ask_step_tick_only(&self, dt: f64) -> Result<super::StepOutput, String> {
        let rx = self.start_step_manual_tick_only(dt).await?;
        rx.await
            .map_err(|_| "Solar system reply dropped".to_string())
    }

    #[cfg(any(test, feature = "testkit"))]
    pub(crate) async fn step_manual_tick_only(&self, dt: f64) -> Result<u64, String> {
        let output = self.ask_step_tick_only(dt).await?;
        self.apply_step_output(&output).await;
        Ok(output.tick)
    }

    pub async fn fast_forward_clock(&self, ship_id: BallId, secs: u64) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::FastForwardClock { ship_id, secs, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn get_target_locks(&self, ball_id: BallId) -> Result<Vec<TargetLock>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender().send(SystemCommand::GetTargetLocks { ball_id, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_target_lock(&self, source_id: BallId, lock: TargetLock) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender().send(SystemCommand::AddTargetLock { source_id, lock, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn add_target_lock_breaking_protection(
        &self,
        source_id: BallId,
        lock: TargetLock,
    ) -> Result<ProtectionBreakEvents, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::AddTargetLockBreakingProtection {
                source_id,
                lock,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn remove_target_lock(&self, source_id: BallId, target_id: BallId) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender().send(SystemCommand::RemoveTargetLock { source_id, target_id, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn remove_target_lock_quiet(&self, source_id: BallId, target_id: BallId) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender().send(SystemCommand::RemoveTargetLockQuiet { source_id, target_id, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn clear_target_locks(&self, source_id: BallId) -> Result<usize, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender().send(SystemCommand::ClearTargetLocks { source_id, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn clear_target_locks_quiet(&self, source_id: BallId) -> Result<usize, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender().send(SystemCommand::ClearTargetLocksQuiet { source_id, reply: tx }).await.map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn set_capacitor(&self, ball_id: BallId, capacitor: f64) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetCapacitor { ball_id, capacitor, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn heal_combat_state(&self, ball_id: BallId) -> Result<CombatState, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::HealCombatState { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn set_combat_state(&self, ball_id: BallId, combat_state: CombatState) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetCombatState { ball_id, combat_state, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn get_combat_state(&self, ball_id: BallId) -> Result<Option<CombatState>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::GetCombatState { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    /// Atomically mutates `ball_id`'s [`CombatState`] inside the actor loop without a
    /// stale get-modify-set round-trip (`#846`, `#917` E-2). Returns `Ok(None)` if
    /// `ball_id` is not present in space.
    pub async fn modify_combat_state(
        &self,
        ball_id: BallId,
        f: impl FnOnce(&mut CombatState) + Send + 'static,
    ) -> Result<Option<CombatState>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ModifyCombatState {
                ball_id,
                f: Box::new(f),
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    /// Asynchronously restores up to `(hull_heal, armor_heal, shield_heal)` HP deltas on
    /// `ball_id` via `SystemActor`, emitting `OnDamageStateChange` and persisting the updated
    /// `CombatState` (`#1167`).
    pub async fn repair_combat_state(
        &self,
        ball_id: BallId,
        hull_heal: f64,
        armor_heal: f64,
        shield_heal: f64,
    ) -> Result<Option<CombatState>, String> {
        let hull_delta = hull_heal.max(0.0);
        let armor_delta = armor_heal.max(0.0);
        let shield_delta = shield_heal.max(0.0);
        if hull_delta <= 0.0 && armor_delta <= 0.0 && shield_delta <= 0.0 {
            return Ok(None);
        }
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::RepairCombatState {
                ball_id,
                hull_heal: hull_delta,
                armor_heal: armor_delta,
                shield_heal: shield_delta,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_module_cycle(&self, cycle: ActiveModuleCycle) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::AddModuleCycle { cycle, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn remove_module_cycle(&self, ship_id: BallId, module_id: i64) -> Result<Option<f64>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::RemoveModuleCycle { ship_id, module_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn request_cycle_deactivation(&self, ship_id: BallId, module_id: i64) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::RequestCycleDeactivation { ship_id, module_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub(crate) async fn start_fire_weapon_cycle_volley(
        &self,
        ship_id: BallId,
        module_id: i64,
    ) -> Result<oneshot::Receiver<super::WeaponVolleyOutput>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::FireWeaponCycleVolley {
                ship_id,
                module_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        Ok(rx)
    }

    pub async fn fire_weapon_cycle_volley(
        &self,
        ship_id: BallId,
        module_id: i64,
    ) -> Result<(Vec<(BallId, Option<CharacterId>)>, Vec<super::PendingCycleAction>), String> {
        let rx = self.start_fire_weapon_cycle_volley(ship_id, module_id).await?;
        let volley_out = rx.await.map_err(|_| "Solar system reply dropped".to_string())?;
        if !volley_out.destroyed.is_empty()
            || volley_out.has_post_unlock_work
            || self.has_in_flight_post_unlock()
        {
            let _ = self.wait_for_post_unlock_barrier().await;
        }
        self.commit_destroyed_ball_hooks(
            0.0,
            &volley_out.destroyed,
            volley_out.destroyed_lease.as_ref(),
            false,
        )
        .await;
        Ok((volley_out.destroyed, volley_out.actions))
    }

    pub async fn has_module_cycle(&self, ship_id: BallId, module_id: i64) -> bool {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_sender()
            .send(SystemCommand::HasModuleCycle {
                ship_id,
                module_id,
                reply: tx,
            })
            .await
            .is_err()
        {
            return false;
        }
        rx.await.unwrap_or(false)
    }

    /// Expires stale (`repeat == 0` / smartbomb clock-elapsed or dead-target) cycles before
    /// `Activate` validation, returning `(active_repeat_and_target, cleaned_stale_cycle)`.
    pub async fn reconcile_module_cycle_before_activate(
        &self,
        ship_id: BallId,
        module_id: i64,
        now_unix: u64,
    ) -> (Option<(i32, Option<BallId>)>, bool) {
        let (tx, rx) = oneshot::channel();
        if self
            .cmd_sender()
            .send(SystemCommand::ReconcileModuleCycleBeforeActivate {
                ship_id,
                module_id,
                now_unix,
                reply: tx,
            })
            .await
            .is_err()
        {
            return (None, false);
        }
        rx.await.unwrap_or((None, false))
    }

    /// Parity: [plan:DG-4] Entering warp halts active non-warp module cycles.
    pub async fn stop_non_warp_module_cycles(&self, ship_id: BallId) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::StopNonWarpModuleCycles { ship_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn set_interdiction_nullified(&self, ball_id: BallId, nullified: bool) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::SetInterdictionNullified { ball_id, nullified, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn micro_jump(&self, ball_id: BallId, distance_meters: f64) -> Result<DVec3, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::MicroJump { ball_id, distance_meters, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn micro_jump_field(
        &self,
        ball_id: BallId,
        distance_meters: f64,
        field_radius_meters: f64,
        max_jumped_ships: usize,
    ) -> Result<Vec<BallId>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::MicroJumpField {
                ball_id,
                distance_meters,
                field_radius_meters,
                max_jumped_ships,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())?
    }

    pub async fn tick(&self) -> Result<TickBroadcast, String> {
        self.step_manual(1.0).await
    }

    pub async fn is_ball_scrambled(&self, ball_id: BallId) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::IsBallScrambled { ball_id, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_interdiction_sphere(&self, id: BallId, position: DVec3, radius: f64) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::AddInterdictionSphere { id, position, radius, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn add_mobile_warp_disruptor(&self, id: BallId, position: DVec3, radius: f64) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::AddMobileWarpDisruptor { id, position, radius, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn apply_ecm_jam(&self, target_id: BallId, duration_s: f64) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ApplyEcmJam {
                target_id,
                duration_s,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn apply_ecm_jam_from(
        &self,
        source_id: BallId,
        target_id: BallId,
        duration_s: f64,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ApplyEcmJamFrom {
                source_id,
                target_id,
                duration_s,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn apply_module_ecm_jam(
        &self,
        source_id: BallId,
        module_id: i64,
        target_id: BallId,
        duration_s: f64,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ApplyEcmJamFromModule {
                source_id,
                module_id,
                target_id,
                duration_s,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn apply_ecm_burst(
        &self,
        source_id: BallId,
        burst_radius_m: f64,
        jam_strengths: [f64; 4],
        duration_s: f64,
        roll: f64,
    ) -> Result<Vec<BallId>, String> {
        self.apply_ecm_burst_ships(source_id, burst_radius_m, jam_strengths, duration_s, roll, None)
            .await
    }

    pub async fn apply_ecm_burst_ships(
        &self,
        source_id: BallId,
        burst_radius_m: f64,
        jam_strengths: [f64; 4],
        duration_s: f64,
        roll: f64,
        ship_eval_overrides: Option<HashMap<BallId, (usize, f64, f64)>>,
    ) -> Result<Vec<BallId>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ApplyEcmBurst {
                source_id,
                burst_radius_m,
                jam_strengths,
                duration_s,
                roll,
                ship_eval_overrides,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn schedule_mobile_warp_disruptor(
        &self,
        id: BallId,
        type_id: u32,
        position: DVec3,
        radius: f64,
        anchoring_delay_s: f64,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::ScheduleMobileWarpDisruptor {
                id,
                type_id,
                position,
                radius,
                anchoring_delay_s,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn update_module_overload(
        &self,
        ship_id: BallId,
        module_id: i64,
        is_overloaded: bool,
    ) -> Result<bool, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::UpdateModuleOverload {
                ship_id,
                module_id,
                is_overloaded,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn start_nanite_repair(&self, repair: ActiveNaniteRepair) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::StartNaniteRepair { repair, reply: tx })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub async fn stop_nanite_repair(
        &self,
        ship_id: BallId,
        module_id: i64,
    ) -> Result<Option<(f64, i32)>, String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::StopNaniteRepair {
                ship_id,
                module_id,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }

    pub fn has_any_active_nanite_repair_sync(&self, ship_id: BallId) -> bool {
        self.view().active_nanite_repairs.iter().any(|r| r.ship_id == ship_id)
    }

    pub fn is_module_being_nanite_repaired_sync(&self, ship_id: BallId, module_id: i64) -> bool {
        self.view()
            .active_nanite_repairs
            .iter()
            .any(|r| r.ship_id == ship_id && r.module_id == module_id)
    }

    pub fn has_active_overloaded_cycle_sync(&self, ship_id: BallId) -> bool {
        self.view()
            .active_module_cycles
            .iter()
            .any(|c| c.ship_id == ship_id && c.is_overloaded)
    }

    pub fn active_offensive_aggressors_on_sync(
        &self,
        target_ship_id: BallId,
        sde: Option<&eve_sde::SdeRegistry>,
    ) -> Vec<BallId> {
        self.view()
            .active_module_cycles
            .iter()
            .filter(|c| {
                c.target_id == Some(target_ship_id)
                    && c.ship_id != target_ship_id
                    && crate::services::mining_phased_autotarget::is_offensive_aggressor_cycle(c, sde)
            })
            .map(|c| c.ship_id)
            .collect()
    }

    pub fn has_active_industrial_core_sync(&self, ship_id: BallId) -> bool {
        self.view().active_module_cycles.iter().any(|c| {
            c.ship_id == ship_id
                && matches!(c.module_type_id, 28583 | 42890 | 58945 | 58950 | 62590 | 62591)
        })
    }

    pub async fn record_incoming_armor_damage(
        &self,
        ship_id: BallId,
        damage: [f64; 4],
    ) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.cmd_sender()
            .send(SystemCommand::RecordIncomingArmorDamage {
                ship_id,
                damage,
                reply: tx,
            })
            .await
            .map_err(|_| "Solar system actor stopped".to_string())?;
        rx.await.map_err(|_| "Solar system reply dropped".to_string())
    }
}

#[path = "celestials_helpers.rs"]
mod celestials_helpers;
pub use celestials_helpers::try_remove_ball_for_db;
pub(crate) use celestials_helpers::{
    clear_landed_warp_tracking_from_events,
    collect_all_condition_checkpoint_entries, collect_ball_positions_for_post_unlock,
    collect_dirty_condition_checkpoint_entries,
    collect_immediate_armor_hull_checkpoint_entries,
    collect_landed_warp_positions,
    collect_landed_warps_for_post_unlock_with_in_flight,
    fast_forward_clock_in_actor_collecting,
    record_spawned_ball_checkpoint_baseline, record_warp_stops_from_events_in_map,
    BallPositionSyncCache, register_db_system_ballpark_arc,
    remove_ball_checkpoint_tracking, send_tick_broadcast_sequenced,
    send_tick_broadcast_sequenced_owned, sync_ball_positions, clear_db_system_ballpark_if_matching,
    try_enqueue_remove_ball_for_db, update_system_hibernation_flag,
};
#[cfg(test)]
pub(crate) use celestials_helpers::{db_ballpark_entry_for_test, register_db_system_ballpark};

#[cfg(test)] #[path = "celestials_unit_tests.rs"] mod celestials_unit_tests;

// Kept at EOF so non-depth-aware `strip_cfg_test_items` does not mask `impl SystemHandle` after
// comma-terminated `#[cfg(test)]` struct fields (`#1342`).
#[derive(Clone)]
pub struct SystemHandle {
    pub solar_system_id: SolarSystemId,
    pub(crate) cmd_tx: mpsc::Sender<SystemCommand>,
    pub(in crate::system_actor) post_unlock_tx: tokio::sync::mpsc::UnboundedSender<super::post_unlock::PostUnlockMsg>,
    pub(in crate::system_actor) worker_cb_tx:
        tokio::sync::mpsc::UnboundedSender<super::post_unlock::PostUnlockActorCallback>,
    #[allow(dead_code)] // Retains CurrentThread actor driver runtime while handle lives.
    pub(in crate::system_actor) ct_driver: Option<Arc<super::CtActorDriver>>,
    pub(in crate::system_actor) actor_rt_handle: Option<tokio::runtime::Handle>,
    pub(in crate::system_actor) post_unlock_worker_thread_id: Arc<std::sync::OnceLock<std::thread::ThreadId>>,
    pub(in crate::system_actor) post_unlock_in_flight: Arc<std::sync::atomic::AtomicUsize>,
    pub(in crate::system_actor) completed_post_unlock_seq: Arc<std::sync::atomic::AtomicU64>,
    pub(in crate::system_actor) fighters_needs_hydration: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) fighter_ctx: Arc<super::fighters_persistence::FighterHandleCtx>,
    pub(crate) event_tx: broadcast::Sender<TickBroadcast>,
    pub(crate) ball_positions: Arc<BallPositionSyncCache>,
    pub(crate) scrambled_balls: Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    pub(crate) tethered_balls: Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    pub(crate) committed_warp_balls: Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    pub(crate) warp_stopped_at: Arc<RankedRwLock<HashMap<BallId, tokio::time::Instant>, RANK_LEAF>>,
    pub(crate) active_module_ships: Arc<RankedRwLock<std::collections::HashSet<BallId>, RANK_LEAF>>,
    pub ballpark: Arc<BallparkHandle>,
    pub(crate) actor_state: Arc<RankedRwLock<super::SystemActorState, RANK_ACTOR_STATE>>,
    pub(crate) view_tx: Arc<tokio::sync::watch::Sender<Arc<super::SystemView>>>,
    pub(crate) view_rx: tokio::sync::watch::Receiver<Arc<super::SystemView>>,
    pub(crate) last_auto_stepped_unix: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) tidi_factor_bits: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) sim_elapsed_secs_bits: Arc<std::sync::atomic::AtomicU64>,
    /// Highest visible / prime (`SetState`) Destiny stamp delivered in this system (`#1081`).
    pub(crate) last_visible_stamp: Arc<std::sync::atomic::AtomicU64>,
    /// Highest `OnSpecialFX` Destiny stamp emitted in this system so FX stamps never backstep (`#1081`).
    pub(crate) last_fx_stamp: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) hibernated: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) hibernated_since_unix: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) idle_ticks: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) dynamic_ball_count: Arc<std::sync::atomic::AtomicUsize>,
    pub command_panics: Arc<std::sync::atomic::AtomicU64>,
    pub(crate) pending_destroyed_balls: super::PendingDestroyedOutbox,
    #[cfg(test)]
    pub(crate) panic_next_command_for_test: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    pub(crate) panic_on_command_countdown_for_test: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    pub(crate) panic_after_destruction_for_test: Arc<std::sync::atomic::AtomicBool>,
    /// Test-only switch that panics inside the actor command loop after the command handler runs,
    /// while both `ballpark` and `actor_state` write guards are still held (`#1173`).
    #[cfg(test)]
    pub(crate) panic_after_command_guards_for_test: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    pub(crate) before_step_reply_hook_for_test:
        Arc<RankedMutex<Option<Box<dyn FnOnce() + Send>>, RANK_LEAF>>,
    pub(crate) npc_death_hook: Arc<RankedRwLock<Option<super::NpcDeathHook>, RANK_LEAF>>,
    pub(crate) module_deactivation_hook: Arc<RankedRwLock<Option<super::ModuleDeactivationHook>, RANK_LEAF>>,
    pub(crate) module_deactivation_post_unlock_hook:
        Arc<RankedRwLock<Option<super::PairedModuleDeactivationPostUnlockHook>, RANK_LEAF>>,
    pub(crate) target_lock_completed_hook: Arc<RankedRwLock<Option<super::TargetLockCompletedHook>, RANK_LEAF>>,
    pub(crate) system_step_hook: Arc<RankedRwLock<Option<super::SystemStepHook>, RANK_LEAF>>,
    pub(crate) post_unlock_actions: Arc<RankedMutex<Vec<super::PostUnlockHookAction>, RANK_LEAF>>,
    pub(in crate::system_actor) post_unlock_actions_in_flight: Arc<std::sync::atomic::AtomicUsize>,
    pub(in crate::system_actor) post_unlock_fallback_drain_lock: Arc<std::sync::Mutex<()>>,
    pub(in crate::system_actor) post_unlock_fallback_owner_tid:
        Arc<std::sync::Mutex<Option<std::thread::ThreadId>>>,
    pub(in crate::system_actor) worker_awaiting_actor_cmd: Arc<std::sync::atomic::AtomicUsize>,
    pub(in crate::system_actor) published_live: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) cooldown_persist_gate: Arc<super::CooldownPersistGate>,
}

