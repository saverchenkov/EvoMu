//! Per-system active drones in space, repeating drone mining, and drone combat cooldown state
//! owned by `SystemActor` (`#1541`).

#![forbid(unsafe_code)]

use super::{
    call_actor_result_with_deadline, defer_cmd_reply, dispatch_actor_sync_no_flush_with_deadline,
    dispatch_actor_sync_with_deadline, ActorExecCtx, BallOpsCtx, RankedMutex, SystemActorState,
    SystemCommand, SystemHandle, SystemRegistry, ACTOR_CMD_TIMEOUT, RANK_LEAF,
};
use eve_destiny::{BallId, DamageVector};
use eve_sde::SolarSystemId;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Default)]
pub(crate) struct ActiveDronesSharedCtx {
    pub(in crate::system_actor) flush_mutex: Arc<std::sync::Mutex<u64>>,
    pub(in crate::system_actor) db_opt: Option<Arc<eve_db::WorldRepository>>,
    pub(in crate::system_actor) migrated_opt:
        Option<Arc<RankedMutex<HashSet<SolarSystemId>, RANK_LEAF>>>,
    pub(in crate::system_actor) dirty: Arc<AtomicBool>,
    pub(in crate::system_actor) needs_hydration: Arc<AtomicBool>,
}

#[derive(Default)]
pub(crate) struct RegistryDroneRuntimeState {
    pub(crate) migrated_legacy_drone_systems: Arc<RankedMutex<HashSet<SolarSystemId>, RANK_LEAF>>,
    #[cfg(any(test, feature = "testkit"))]
    pub(crate) headless_test_rt: std::sync::OnceLock<tokio::runtime::Runtime>,
}

#[cfg(any(test, feature = "testkit"))]
impl Drop for RegistryDroneRuntimeState {
    fn drop(&mut self) {
        if let Some(rt) = self.headless_test_rt.take() {
            if tokio::runtime::Handle::try_current().is_ok() {
                rt.shutdown_background();
            } else {
                drop(rt);
            }
        }
    }
}

pub(crate) fn init_actor_active_drones(
    actor_st: &mut SystemActorState,
    registry: &SystemRegistry,
    solar_system_id: SolarSystemId,
    db_read_safe: bool,
) {
    let (drones, dirty, needs_sqlite_hydration) =
        load_initial_active_drones_for_system(registry, solar_system_id, db_read_safe);
    actor_st.active_drones_in_space = drones;
    actor_st.active_drones_checkpoint_dirty = dirty;
    actor_st.active_drones_checkpoint_seq = u64::from(dirty);
    actor_st.active_drones_needs_sqlite_hydration = needs_sqlite_hydration;
    actor_st.drone_hydration = ActiveDronesSharedCtx {
        flush_mutex: Arc::default(),
        db_opt: registry.db.clone(),
        migrated_opt: Some(Arc::clone(
            &registry.drone_rt_state.migrated_legacy_drone_systems,
        )),
        dirty: Arc::new(AtomicBool::new(dirty)),
        needs_hydration: Arc::new(AtomicBool::new(needs_sqlite_hydration)),
    };
}

/// Runtime state for a launched or abandoned drone in solar-system space (`#1541` Track D2b).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DroneSpaceState {
    pub ball_id: i64,
    pub drone_item_id: i64,
    pub type_id: u32,
    pub group_id: u32,
    pub owner_char_id: i64,
    pub original_owner_char_id: i64,
    pub parent_ship_id: i64,
    pub solar_system_id: u32,
    pub target_id: Option<i64>,
    pub activity_state: i64,
    pub assist_char_id: Option<i64>,
    pub guard_char_id: Option<i64>,
    pub is_abandoned: bool,
    pub is_sentry: bool,
    pub bandwidth_used: f64,
    pub max_velocity: f64,
    pub optimal_range: f64,
    pub damage_vector: DamageVector,
    pub shield_repair_amount: f64,
    pub armor_repair_amount: f64,
    pub hull_repair_amount: f64,
    pub web_speed_factor: f64,
    pub neut_amount: f64,
    pub ecm_strength: f64,
    pub tp_sig_bonus_pct: f64,
}

impl DroneSpaceState {
    #[inline]
    pub(crate) fn in_solar_system(self, solar_system_id: SolarSystemId) -> Self {
        Self {
            solar_system_id,
            ..self
        }
    }
}

/// Request payload for atomic per-ship drone launch validation and reservation inside [`SystemActor`].
#[derive(Debug, Clone)]
pub struct ReserveDroneLaunchesReq {
    pub ship_id: BallId,
    pub max_drones_allowed: usize,
    pub max_bandwidth: f64,
    pub partial_ok: bool,
    pub drones: Vec<(i64, DroneSpaceState)>,
}

/// Outcome of [`ReserveDroneLaunchesReq`] inside [`SystemActor`].
#[derive(Debug, Clone)]
pub struct ReserveDroneLaunchesOutcome {
    pub kept_indices: Vec<usize>,
    pub reserved_ids: Vec<BallId>,
    pub per_item_errors: Vec<(i64, &'static str)>,
    pub rejected_error: Option<&'static str>,
}

/// Request payload for atomic per-ship drone reconnect validation and ownership transition.
#[derive(Debug, Clone)]
pub struct ReconnectDronesReq {
    pub char_id: i64,
    pub ship_id: BallId,
    pub max_drones_allowed: usize,
    pub max_bandwidth: f64,
    pub target_ids: Vec<i64>,
}

/// Outcome of [`ReconnectDronesReq`] executed atomically inside [`SystemActor`].
#[derive(Debug, Clone, Default)]
pub struct ReconnectDronesOutcome {
    pub reconnected: Vec<(i64, i64, u32)>,
    pub out_of_range: Vec<i64>,
    pub max_drones_exceeded: Vec<i64>,
    pub insufficient_bandwidth: Vec<i64>,
}

/// Closure executed atomically inside [`SystemActor`] to inspect or mutate active drones.
/// Returning `true` marks `active_drones_in_space` dirty for SQLite checkpointing.
pub(crate) type ActiveDroneMutOp = Box<
    dyn FnOnce(
            &mut HashMap<BallId, DroneSpaceState>,
            &mut HashMap<BallId, DroneMiningState>,
            &mut HashMap<BallId, u64>,
        ) -> bool
        + Send,
>;

/// Deferred disconnect/abandon intent stamped onto `scoop_claimed_drones` when
/// warp, disconnect, or `CmdAbandonDrone` runs while a scoop holds a transient claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingScoopAbandonKind {
    Disconnect,
    Abandon,
}

/// Metadata required to apply and notify a deferred disconnect/abandon on `ReleaseClaim`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingScoopAbandon {
    pub kind: PendingScoopAbandonKind,
    pub notify_char_id: i64,
    pub drone_item_id: i64,
    pub type_id: u32,
    pub solar_system_id: u32,
}

impl PendingScoopAbandon {
    /// Merges `next` into `slot`, ensuring `Abandon` overrides `Disconnect` and never downgrades.
    pub fn merge_into(slot: &mut Option<Self>, next: Self) {
        match slot {
            Some(existing) if existing.kind == PendingScoopAbandonKind::Abandon => {}
            _ => *slot = Some(next),
        }
    }
}

/// Closure executed atomically inside [`SystemActor`] with mutable access to active
/// transient `scoop_claimed_drones` so reconnect skips claimed drones and abandon/disconnect
/// stamps deferred intent onto claimed drones instead of dropping updates silently.
pub(crate) type ActiveDroneMutWithClaimsOp = Box<
    dyn FnOnce(
            &mut HashMap<BallId, DroneSpaceState>,
            &mut HashMap<BallId, DroneMiningState>,
            &mut HashMap<BallId, u64>,
            &mut HashMap<BallId, Option<PendingScoopAbandon>>,
        ) -> bool
        + Send,
>;

/// Transient in-actor scoop coordination command operating on `scoop_claimed_drones`.
#[derive(Debug, Clone)]
pub enum DroneScoopClaimAction {
    /// Non-destructive claim for `ship.ScoopToDroneBay`; rejects when already claimed or controlled by another pilot.
    ClaimPlayerScoop { char_id: i64 },
    /// Non-destructive claim for automatic return scoop (`try_atomic_scoop_drone`).
    ClaimReturnScoop {
        expected_owner: i64,
        target_ship_id: i64,
    },
    /// Releases a transient scoop claim without touching `active_drones_in_space` or checkpoint dirty flags.
    ReleaseClaim,
    /// Removes `ball_id` from both `scoop_claimed_drones` and `active_drones_in_space` (marking checkpoint dirty if `ball_id > 0`).
    ConsumeScoop,
    /// Releases `scoop_claimed_drones` and removes/returns live `DroneSpaceState` ONLY if it still needs abandonment.
    ReleaseOrTakeForAbandon {
        char_id_opt: Option<i64>,
        ship_id: i64,
    },
    /// Takes an unclaimed returning controlled drone belonging to `target_ship_id` for abandonment when ship became unavailable.
    TakeUnclaimedForAbandon {
        expected_owner: i64,
        target_ship_id: i64,
        require_returning: bool,
    },
}

/// Outcome of [`DroneScoopClaimAction`] executed inside [`SystemActor`].
#[derive(Debug, Clone)]
pub enum DroneScoopClaimOutcome {
    Claimed(Option<DroneSpaceState>),
    AlreadyClaimed,
    OwnedByOther,
    NotEligible,
    Released,
    ReleasedWithPending(PendingScoopAbandon),
    Consumed(Option<DroneSpaceState>),
    TakeForAbandon(Option<DroneSpaceState>),
}

#[derive(Default, serde::Deserialize)]
struct LegacyActiveDronesPayload {
    #[serde(default)]
    active_drones_in_space: HashMap<i64, DroneSpaceState>,
}

/// Active repeating drone ore-mining loop state owned per-system by [`SystemActor`] (`#1541`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DroneMiningState {
    pub drone_id: i64,
    pub asteroid_id: i64,
    pub ship_id: i64,
    pub owner_char_id: i64,
    pub solar_system_id: u32,
    pub yield_units: u64,
    pub duration_secs: u64,
    pub next_cycle_unix: u64,
    pub waste_prob: f64,
    pub ore_type_id: u32,
    pub destination_flag: eve_db::InventoryFlag,
}

#[derive(Debug, Clone)]
struct ClearDroneRuntimeReq {
    drone_ids: Option<Vec<BallId>>,
    clear_miners: bool,
    clear_timers: bool,
}

fn upsert_drone_miner_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    state: DroneMiningState,
) -> Result<(), String> {
    ctx.actor_st.active_drone_miners.insert(state.drone_id, state);
    Ok(())
}

fn reschedule_drone_miner_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    drone_id: BallId,
    now_unix: u64,
) -> Result<bool, String> {
    let Some(miner) = ctx.actor_st.active_drone_miners.get_mut(&drone_id) else {
        return Ok(false);
    };
    miner.next_cycle_unix = now_unix.saturating_add(miner.duration_secs);
    Ok(true)
}

fn set_drone_next_ready_unix_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    drone_id: BallId,
    next_ready_unix: u64,
    require_ball_in_space: bool,
) -> Result<bool, String> {
    if require_ball_in_space && !ctx.ballpark.balls.contains_key(&drone_id) {
        return Ok(false);
    }
    ctx.actor_st
        .drone_next_ready_unix
        .insert(drone_id, next_ready_unix);
    Ok(true)
}

fn clear_drone_runtime_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    req: ClearDroneRuntimeReq,
) -> Result<usize, String> {
    let mut removed = 0usize;
    match req.drone_ids {
        Some(ids) => {
            for id in ids {
                if req.clear_miners && ctx.actor_st.active_drone_miners.remove(&id).is_some() {
                    removed += 1;
                }
                if req.clear_timers && ctx.actor_st.drone_next_ready_unix.remove(&id).is_some() {
                    removed += 1;
                }
            }
        }
        None => {
            if req.clear_miners {
                removed += ctx.actor_st.active_drone_miners.len();
                ctx.actor_st.active_drone_miners.clear();
            }
            if req.clear_timers {
                removed += ctx.actor_st.drone_next_ready_unix.len();
                ctx.actor_st.drone_next_ready_unix.clear();
            }
        }
    }
    Ok(removed)
}

/// Clears both repeating drone mining state and combat cooldown timer for `ball_id` in actor state.
pub(in crate::system_actor) fn clear_drone_runtime_in_state(
    actor_st: &mut SystemActorState,
    ball_id: BallId,
) {
    actor_st.active_drone_miners.remove(&ball_id);
    actor_st.drone_next_ready_unix.remove(&ball_id);
}

pub(in crate::system_actor) fn mark_active_drones_dirty_in_state(
    actor_st: &mut SystemActorState,
) {
    actor_st.active_drones_checkpoint_dirty = true;
    actor_st.active_drones_checkpoint_seq = actor_st.active_drones_checkpoint_seq.saturating_add(1);
    actor_st.drone_hydration.dirty.store(true, Ordering::Release);
}

#[inline]
fn record_removed_drone_pre_hydration(actor_st: &mut SystemActorState, ball_id: BallId) {
    if actor_st.active_drones_needs_sqlite_hydration && ball_id > 0 {
        actor_st.active_drones_removed_pre_hydration.insert(ball_id);
    }
}

/// Clears repeating mining state, combat cooldown, and any active-space drone entry on explicit ball removal.
pub(in crate::system_actor) fn clear_removed_ball_drones_in_state(
    actor_st: &mut SystemActorState,
    ball_id: BallId,
) {
    clear_drone_runtime_in_state(actor_st, ball_id);
    actor_st.scoop_claimed_drones.remove(&ball_id);
    record_removed_drone_pre_hydration(actor_st, ball_id);
    if actor_st.active_drones_in_space.remove(&ball_id).is_some() && ball_id > 0 {
        mark_active_drones_dirty_in_state(actor_st);
    }
}

pub(in crate::system_actor) fn capture_dirty_active_drones_snapshot(
    actor_st: &SystemActorState,
) -> Option<(u64, Arc<eve_db::WorldRepository>, Vec<(BallId, DroneSpaceState)>)> {
    if !actor_st.active_drones_checkpoint_dirty || actor_st.active_drones_needs_sqlite_hydration {
        return None;
    }
    let db = actor_st.db_opt.as_ref().map(Arc::clone)?;
    let mut entries: Vec<(BallId, DroneSpaceState)> = actor_st
        .active_drones_in_space
        .iter()
        .filter(|(&bid, _)| bid > 0)
        .map(|(&bid, d)| (bid, d.clone()))
        .collect();
    entries.sort_by_key(|(bid, _)| *bid);
    Some((actor_st.active_drones_checkpoint_seq, db, entries))
}

fn load_legacy_drones_into_map_if_unmigrated(
    db: &eve_db::WorldRepository,
    migrated: &RankedMutex<HashSet<SolarSystemId>, RANK_LEAF>,
    solar_system_id: SolarSystemId,
    drones: &mut HashMap<BallId, DroneSpaceState>,
    mark_migrated: bool,
) -> bool {
    let already_migrated = migrated
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&solar_system_id);
    if already_migrated {
        return false;
    }
    let mut dirty = false;
    let ext_json = db.with_snapshot(|snap| snap.combat_crimewatch_state.extended_runtime_json.clone());
    if !ext_json.is_empty() && ext_json.contains("active_drones_in_space") {
        if let Ok(legacy) = serde_json::from_str::<LegacyActiveDronesPayload>(&ext_json) {
            for (bid, mut d) in legacy.active_drones_in_space {
                if bid > 0 && d.solar_system_id == solar_system_id {
                    d.ball_id = bid;
                    drones.insert(bid, d);
                    dirty = true;
                }
            }
        }
    }
    if mark_migrated {
        migrated
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(solar_system_id);
    }
    dirty
}

fn read_sqlite_drones_for_hydration(
    db: &eve_db::WorldRepository,
    solar_system_id: SolarSystemId,
    migrated_opt: Option<&Arc<RankedMutex<HashSet<SolarSystemId>, RANK_LEAF>>>,
    blocking: bool,
) -> Option<(Vec<DroneSpaceState>, bool)> {
    let load_res = if blocking {
        Some(db.with_sqlite_conn(|conn| {
            eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(
                conn,
                solar_system_id,
                eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
            )
        }))
    } else {
        db.try_with_sqlite_conn(|conn| {
            eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(
                conn,
                solar_system_id,
                eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
            )
        })
    };
    let Some(Ok(rows)) = load_res else {
        return None;
    };
    let mut hydrated: HashMap<BallId, DroneSpaceState> = HashMap::new();
    for (_, d) in rows {
        if d.ball_id > 0 {
            hydrated.insert(d.ball_id, d.in_solar_system(solar_system_id));
        }
    }
    let mut legacy_dirty = false;
    if hydrated.is_empty() {
        if let Some(migrated) = migrated_opt {
            legacy_dirty = load_legacy_drones_into_map_if_unmigrated(
                db,
                migrated,
                solar_system_id,
                &mut hydrated,
                false,
            );
        }
    }
    Some((hydrated.into_values().collect(), legacy_dirty))
}

pub(in crate::system_actor) fn hydrate_active_drones_in_state(
    actor_st: &mut SystemActorState,
    solar_system_id: SolarSystemId,
    drones: Vec<DroneSpaceState>,
    legacy_dirty: bool,
) -> bool {
    if !actor_st.active_drones_needs_sqlite_hydration {
        return false;
    }
    let mut dropped_pre_hydration = false;
    for d in drones {
        if d.ball_id > 0 {
            if actor_st.active_drones_removed_pre_hydration.contains(&d.ball_id) {
                dropped_pre_hydration = true;
            } else {
                actor_st
                    .active_drones_in_space
                    .entry(d.ball_id)
                    .or_insert_with(|| d.in_solar_system(solar_system_id));
            }
        }
    }
    if legacy_dirty || dropped_pre_hydration {
        mark_active_drones_dirty_in_state(actor_st);
    }
    if let Some(migrated) = &actor_st.drone_hydration.migrated_opt {
        migrated.lock().unwrap_or_else(|e| e.into_inner()).insert(solar_system_id);
    }
    actor_st.active_drones_needs_sqlite_hydration = false;
    actor_st.drone_hydration.needs_hydration.store(false, Ordering::Release);
    actor_st.active_drones_removed_pre_hydration.clear();
    true
}

fn refresh_view_and_hibernation(
    bp: &eve_destiny::Ballpark,
    st: &SystemActorState,
    view_tx: &tokio::sync::watch::Sender<Arc<super::SystemView>>,
    tidi_factor_bits: &std::sync::atomic::AtomicU64,
    active_subscribers: usize,
) {
    super::celestials::update_system_hibernation_flag(st, bp, active_subscribers);
    let tidi = super::celestials::clamp_tidi_factor_bits(tidi_factor_bits);
    let prev = view_tx.borrow().clone();
    let next = super::SystemView::from_ballpark(bp, Some(st), tidi, Some(prev.as_ref()));
    let _ = view_tx.send(Arc::new(next));
}

#[inline]
pub(in crate::system_actor) fn active_drone_flush_flags(
    shared: &ActiveDronesSharedCtx,
) -> (bool, bool) {
    (
        shared.dirty.load(Ordering::Acquire),
        shared.needs_hydration.load(Ordering::Acquire),
    )
}

pub(in crate::system_actor) fn read_sqlite_for_shared(
    shared: &ActiveDronesSharedCtx,
    solar_system_id: SolarSystemId,
    blocking: bool,
) -> Option<(Vec<DroneSpaceState>, bool)> {
    let Some(ref db) = shared.db_opt else {
        return Some((Vec::new(), false));
    };
    let _flush_guard = if blocking {
        shared.flush_mutex.lock().unwrap_or_else(|e| e.into_inner())
    } else {
        shared.flush_mutex.try_lock().ok()?
    };
    read_sqlite_drones_for_hydration(db, solar_system_id, shared.migrated_opt.as_ref(), blocking)
}

pub(in crate::system_actor) fn write_sqlite_for_shared(
    shared: &ActiveDronesSharedCtx,
    solar_system_id: SolarSystemId,
    seq: u64,
    entries: &[(BallId, DroneSpaceState)],
    blocking: bool,
) -> bool {
    let Some(ref db) = shared.db_opt else { return false; };
    let mut last_seq = if blocking {
        shared.flush_mutex.lock().unwrap_or_else(|e| e.into_inner())
    } else if let Ok(g) = shared.flush_mutex.try_lock() {
        g
    } else {
        return false;
    };
    if seq == 0 || seq <= *last_seq {
        return false;
    }
    let now_unix = db.clock().now_unix_secs();
    let keyed: Vec<(String, &DroneSpaceState)> = entries.iter().map(|(bid, d)| (bid.to_string(), d)).collect();
    let kind = eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES;
    let write_op = |conn: &mut rusqlite::Connection| {
        eve_db::sqlite_store::system_checkpoint::replace_system_checkpoints_for_kind(
            conn, solar_system_id, kind, &keyed, now_unix,
        )
    };
    let res = if blocking { Some(db.with_sqlite_conn(write_op)) } else { db.try_with_sqlite_conn_mut(write_op) };
    if matches!(res, Some(Ok(()))) {
        *last_seq = seq;
        true
    } else {
        false
    }
}

pub(in crate::system_actor) fn hydrate_in_actor_and_refresh(
    solar_system_id: SolarSystemId,
    bp: &eve_destiny::Ballpark,
    st: &mut SystemActorState,
    view_tx: &tokio::sync::watch::Sender<Arc<super::SystemView>>,
    tidi_factor_bits: &std::sync::atomic::AtomicU64,
    active_subscribers: usize,
    hydrated_drones: Vec<DroneSpaceState>,
    legacy_dirty: bool,
) -> bool {
    if hydrate_active_drones_in_state(st, solar_system_id, hydrated_drones, legacy_dirty) {
        refresh_view_and_hibernation(bp, st, view_tx, tidi_factor_bits, active_subscribers);
    }
    st.active_drones_checkpoint_dirty
}

pub(in crate::system_actor) fn finish_flush_in_actor(
    bp: &eve_destiny::Ballpark,
    st: &mut SystemActorState,
    active_subscribers: usize,
    seq: u64,
) {
    if st.active_drones_checkpoint_seq == seq {
        st.active_drones_checkpoint_dirty = false;
        st.drone_hydration.dirty.store(false, Ordering::Release);
        super::celestials::update_system_hibernation_flag(st, bp, active_subscribers);
    }
}

fn read_deferred_hydration_for_handle(handle: &SystemHandle) -> Option<(Vec<DroneSpaceState>, bool)> {
    handle.view().active_drones_needs_sqlite_hydration
        .then(|| handle.ballpark.drone_hydration_shared().and_then(|s| read_sqlite_for_shared(s, handle.solar_system_id, true)))
        .flatten()
}

/// Ensures deferred SQLite active-drone hydration has completed synchronously before running actor commands.
pub(crate) fn ensure_active_drones_hydrated_sync(handle: &SystemHandle) {
    if !handle.view().active_drones_needs_sqlite_hydration || !eve_db::lock_rank::held_lock_ranks().is_empty() {
        return;
    }
    let Some((drones, legacy_dirty)) = read_deferred_hydration_for_handle(handle) else { return; };
    let _ = dispatch_actor_sync_with_deadline(
        handle, "HydrateActiveDronesFromSqlite", ACTOR_CMD_TIMEOUT, (drones, legacy_dirty),
        |(drones, legacy_dirty), reply| SystemCommand::HydrateActiveDronesFromSqlite { drones, legacy_dirty, reply },
        |ctx, (drones, legacy_dirty)| {
            hydrate_active_drones_in_state(ctx.actor_st, ctx.solar_system_id, drones, legacy_dirty);
            Ok(())
        },
    );
}

#[derive(Clone, Copy)]
enum FlushTarget<'a> {
    Handle(&'a SystemHandle),
    Sender(&'a tokio::sync::mpsc::Sender<SystemCommand>),
}

impl<'a> FlushTarget<'a> {
    fn send_sync<P: Send, T: Send + 'static>(
        self,
        payload: P,
        make_cmd: impl FnOnce(P, tokio::sync::oneshot::Sender<Result<T, String>>) -> SystemCommand + Send,
        direct_fn: impl FnOnce(&mut BallOpsCtx<'_>, P) -> Result<T, String>,
    ) -> Option<T> {
        match self {
            Self::Handle(h) => dispatch_actor_sync_no_flush_with_deadline(
                h, "ActiveDroneFlushStep", ACTOR_CMD_TIMEOUT, payload, make_cmd, direct_fn,
            ).ok(),
            Self::Sender(tx) if !tx.is_closed() => {
                let fut = async {
                    let (rtx, rrx) = tokio::sync::oneshot::channel();
                    tokio::time::timeout(ACTOR_CMD_TIMEOUT, tx.send(make_cmd(payload, rtx))).await.ok()?.ok()?;
                    tokio::time::timeout(ACTOR_CMD_TIMEOUT, rrx).await.ok()?.ok()?.ok()
                };
                match tokio::runtime::Handle::try_current() {
                    Ok(h) if h.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread => None,
                    Ok(h) => tokio::task::block_in_place(|| h.block_on(fut)),
                    Err(_) => tokio::runtime::Builder::new_current_thread().enable_time().build().ok().and_then(|rt| rt.block_on(fut)),
                }
            }
            Self::Sender(_) => None,
        }
    }
}

fn flush_sync_with_target(sys_id: SolarSystemId, shared: &ActiveDronesSharedCtx, target: FlushTarget<'_>, blocking: bool) {
    if !eve_db::lock_rank::held_lock_ranks().is_empty() { return; }
    let (dirty, needs) = active_drone_flush_flags(shared);
    if !dirty && !needs { return; }
    if needs {
        let Some((drones, legacy_dirty)) = read_sqlite_for_shared(shared, sys_id, blocking) else { return; };
        let _ = target.send_sync(
            (drones, legacy_dirty),
            |(drones, legacy_dirty), reply| SystemCommand::HydrateActiveDronesFromSqlite { drones, legacy_dirty, reply },
            |ctx, (drones, legacy_dirty)| {
                hydrate_active_drones_in_state(ctx.actor_st, sys_id, drones, legacy_dirty);
                Ok(())
            },
        );
    }
    if let Some(Some((seq, entries))) = target.send_sync(
        (),
        |_, reply| SystemCommand::SnapshotDirtyActiveDrones { reply },
        |ctx, _| Ok(capture_dirty_active_drones_snapshot(ctx.actor_st).map(|(s, _, e)| (s, e))),
    ) {
        if write_sqlite_for_shared(shared, sys_id, seq, &entries, blocking) {
            let _ = target.send_sync(
                seq,
                |seq, reply| SystemCommand::CommitActiveDronesFlushed { seq, reply },
                |ctx, seq| { finish_flush_in_actor(ctx.ballpark, ctx.actor_st, 0, seq); Ok(()) },
            );
        }
    }
}

pub(in crate::system_actor) async fn flush_from_handle_async(handle: &SystemHandle) {
    let Some(shared_ref) = handle.ballpark.drone_hydration_shared() else { return; };
    if !eve_db::lock_rank::held_lock_ranks().is_empty() { return; }
    let (dirty, needs) = active_drone_flush_flags(shared_ref);
    if !dirty && !needs { return; }
    if needs {
        let (shared, sys_id) = (shared_ref.clone(), handle.solar_system_id);
        if let Some((drones, legacy_dirty)) = tokio::task::spawn_blocking(move || read_sqlite_for_shared(&shared, sys_id, true)).await.ok().flatten() {
            let _ = call_actor_result_with_deadline(handle, "HydrateActiveDronesFromSqlite", ACTOR_CMD_TIMEOUT, |reply| {
                SystemCommand::HydrateActiveDronesFromSqlite { drones, legacy_dirty, reply }
            }).await;
        }
    }
    let Ok(Some((seq, entries))) = call_actor_result_with_deadline(
        handle, "SnapshotDirtyActiveDrones", ACTOR_CMD_TIMEOUT, |reply| SystemCommand::SnapshotDirtyActiveDrones { reply },
    ).await else { return; };
    let (shared, sys_id) = (shared_ref.clone(), handle.solar_system_id);
    if tokio::task::spawn_blocking(move || write_sqlite_for_shared(&shared, sys_id, seq, &entries, true)).await.unwrap_or(false) {
        let _ = call_actor_result_with_deadline(
            handle, "CommitActiveDronesFlushed", ACTOR_CMD_TIMEOUT, |reply| SystemCommand::CommitActiveDronesFlushed { seq, reply },
        ).await;
    }
}

pub(in crate::system_actor) fn try_flush_from_sender(
    solar_system_id: SolarSystemId,
    cmd_tx: &tokio::sync::mpsc::Sender<SystemCommand>,
    shared: &ActiveDronesSharedCtx,
) {
    flush_sync_with_target(solar_system_id, shared, FlushTarget::Sender(cmd_tx), false);
}

pub(in crate::system_actor) fn try_flush_from_handle(handle: &SystemHandle) {
    if let Some(shared) = handle.ballpark.drone_hydration_shared() {
        flush_sync_with_target(handle.solar_system_id, shared, FlushTarget::Handle(handle), false);
    }
}

pub(in crate::system_actor) fn flush_from_handle(handle: &SystemHandle) {
    if let Some(shared) = handle.ballpark.drone_hydration_shared() {
        flush_sync_with_target(handle.solar_system_id, shared, FlushTarget::Handle(handle), true);
    }
}

/// Migrates all `active_drones_in_space` entries from legacy `extended_runtime_json` directly into
/// SQLite `system_checkpoint` (`KIND_ACTIVE_DRONES`) across all solar systems at startup and removes
/// the migrated key from persisted `extended_runtime_json` on disk without waking cold systems.
pub(crate) fn migrate_legacy_active_drones_at_startup(ctx: &crate::router::ServerContext) {
    let ext_json = ctx.db.with_snapshot(|snap| {
        snap.combat_crimewatch_state.extended_runtime_json.clone()
    });
    if ext_json.is_empty() || !ext_json.contains("active_drones_in_space") {
        return;
    }
    let Ok(legacy) = serde_json::from_str::<LegacyActiveDronesPayload>(&ext_json) else {
        return;
    };
    let mut by_sys: std::collections::BTreeMap<SolarSystemId, Vec<(BallId, DroneSpaceState)>> =
        std::collections::BTreeMap::new();
    for (bid, mut d) in legacy.active_drones_in_space {
        if d.solar_system_id == 0 || bid <= 0 {
            continue;
        }
        d.ball_id = bid;
        by_sys.entry(d.solar_system_id).or_default().push((bid, d));
    }
    let now_unix = ctx.db.clock().now_unix_secs();
    let migrated_sqlite = ctx
        .db
        .with_sqlite_conn(|conn| {
            for (&sys_id, entries) in &by_sys {
                let existing =
                    eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<
                        DroneSpaceState,
                    >(
                        conn,
                        sys_id,
                        eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
                    )?;
                if !existing.is_empty() {
                    continue;
                }
                let mut sorted = entries.clone();
                sorted.sort_by_key(|(bid, _)| *bid);
                let rows: Vec<(String, &DroneSpaceState)> =
                    sorted.iter().map(|(bid, d)| (bid.to_string(), d)).collect();
                eve_db::sqlite_store::system_checkpoint::replace_system_checkpoints_for_kind(
                    conn,
                    sys_id,
                    eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
                    &rows,
                    now_unix,
                )?;
            }
            Ok::<(), String>(())
        })
        .is_ok();
    if migrated_sqlite {
        // Persist removal of `active_drones_in_space` immediately so restarts when `KIND_ACTIVE_DRONES`
        // becomes legitimately empty cannot re-import already-scooped or destroyed legacy drones.
        ctx.db.with_mut_snapshot(|snap| {
            let cur = &snap.combat_crimewatch_state.extended_runtime_json;
            if !cur.is_empty() && cur.contains("active_drones_in_space") {
                if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(cur) {
                    if let Some(obj) = val.as_object_mut() {
                        if obj.remove("active_drones_in_space").is_some() {
                            snap.combat_crimewatch_state.extended_runtime_json = if obj.is_empty() {
                                String::new()
                            } else {
                                serde_json::to_string(&val).unwrap_or_default()
                            };
                        }
                    }
                }
            }
        });
        if !by_sys.is_empty() {
            let mut guard = ctx
                .systems
                .drone_rt_state
                .migrated_legacy_drone_systems
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for sys_id in by_sys.keys() {
                guard.insert(*sys_id);
            }
        }
    }
}

/// Loads `active_drones_in_space` for `solar_system_id` from SQLite `system_checkpoint`
/// without blocking on a contended `db.writer`, returning `(drones, dirty, needs_sqlite_hydration)`.
pub(crate) fn load_initial_active_drones_for_system(
    registry: &SystemRegistry,
    solar_system_id: SolarSystemId,
    db_read_safe: bool,
) -> (HashMap<BallId, DroneSpaceState>, bool, bool) {
    let mut drones = HashMap::new();
    let Some(ref db) = registry.db else {
        return (drones, false, false);
    };
    if !db_read_safe {
        return (drones, false, true);
    }
    let mut dirty = false;
    let mut needs_sqlite_hydration = false;
    match db.try_with_sqlite_conn(|conn| {
        eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(
            conn,
            solar_system_id,
            eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
        )
    }) {
        Some(Ok(rows)) => {
            for (_, d) in rows {
                if d.ball_id > 0 {
                    drones.insert(d.ball_id, d.in_solar_system(solar_system_id));
                }
            }
        }
        Some(Err(_)) | None => {
            needs_sqlite_hydration = true;
        }
    }
    if !needs_sqlite_hydration && drones.is_empty() {
        dirty = load_legacy_drones_into_map_if_unmigrated(
            db,
            &registry.drone_rt_state.migrated_legacy_drone_systems,
            solar_system_id,
            &mut drones,
            true,
        );
    }
    (drones, dirty, needs_sqlite_hydration)
}

fn upsert_active_drones_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    drones: Vec<DroneSpaceState>,
    only_if_absent: bool,
) -> Result<usize, String> {
    let mut inserted = 0usize;
    let mut any_persistent_change = false;
    for d in drones {
        let d = d.in_solar_system(ctx.solar_system_id);
        let bid = d.ball_id;
        if bid <= 0 {
            continue;
        }
        if only_if_absent && ctx.actor_st.active_drones_in_space.contains_key(&bid) {
            continue;
        }
        ctx.actor_st.active_drones_in_space.insert(bid, d);
        inserted += 1;
        any_persistent_change = true;
    }
    if any_persistent_change {
        mark_active_drones_dirty_in_state(ctx.actor_st);
    }
    Ok(inserted)
}

fn count_ship_controlled_and_reserved_drones(
    actor_st: &SystemActorState,
    ship_id: BallId,
) -> (usize, f64) {
    actor_st
        .active_drones_in_space
        .values()
        .filter(|d| d.ball_id > 0)
        .chain(actor_st.reserved_drone_launches.values())
        .filter(|d| d.parent_ship_id == ship_id && !d.is_abandoned)
        .fold((0usize, 0.0f64), |(c, bw), d| (c + 1, bw + d.bandwidth_used))
}

fn reserve_drone_launches_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    req: ReserveDroneLaunchesReq,
) -> Result<ReserveDroneLaunchesOutcome, String> {
    let (cur_count, cur_bw) = count_ship_controlled_and_reserved_drones(ctx.actor_st, req.ship_id);
    let mut kept_indices = Vec::new();
    let mut reserved_ids = Vec::new();
    let mut per_item_errors = Vec::new();

    if req.partial_ok {
        if !req.drones.is_empty()
            && (req.max_drones_allowed == 0 || cur_count >= req.max_drones_allowed)
        {
            return Ok(ReserveDroneLaunchesOutcome {
                kept_indices,
                reserved_ids,
                per_item_errors,
                rejected_error: Some("NoDroneManagementAbilitiesLeft"),
            });
        }
        let mut sim_count = cur_count;
        let mut sim_bw = cur_bw;
        let mut first_error: Option<&'static str> = None;
        let mut staged = Vec::with_capacity(req.drones.len());
        for (idx, (source_item_id, st)) in req.drones.into_iter().enumerate() {
            let st = st.in_solar_system(ctx.solar_system_id);
            if sim_count + 1 > req.max_drones_allowed {
                let code = "NoDroneManagementAbilitiesLeft";
                per_item_errors.push((source_item_id, code));
                first_error.get_or_insert(code);
                continue;
            }
            if sim_bw + st.bandwidth_used > req.max_bandwidth + 1e-4 {
                let code = "MaxBandwidthExceeded";
                per_item_errors.push((source_item_id, code));
                first_error.get_or_insert(code);
                continue;
            }
            sim_count += 1;
            sim_bw += st.bandwidth_used;
            kept_indices.push(idx);
            reserved_ids.push(st.ball_id);
            staged.push(st);
        }
        if staged.is_empty() && !per_item_errors.is_empty() {
            return Ok(ReserveDroneLaunchesOutcome {
                kept_indices,
                reserved_ids,
                per_item_errors,
                rejected_error: Some(first_error.unwrap_or("NoDroneManagementAbilitiesLeft")),
            });
        }
        for st in staged {
            ctx.actor_st.reserved_drone_launches.insert(st.ball_id, st);
        }
    } else {
        if cur_count + req.drones.len() > req.max_drones_allowed {
            return Ok(ReserveDroneLaunchesOutcome {
                kept_indices,
                reserved_ids,
                per_item_errors,
                rejected_error: Some("NoDroneManagementAbilitiesLeft"),
            });
        }
        let req_bw: f64 = req.drones.iter().map(|(_, d)| d.bandwidth_used).sum();
        if cur_bw + req_bw > req.max_bandwidth + 1e-4 {
            return Ok(ReserveDroneLaunchesOutcome {
                kept_indices,
                reserved_ids,
                per_item_errors,
                rejected_error: Some("MaxBandwidthExceeded"),
            });
        }
        for (idx, (_, st)) in req.drones.into_iter().enumerate() {
            let st = st.in_solar_system(ctx.solar_system_id);
            kept_indices.push(idx);
            reserved_ids.push(st.ball_id);
            ctx.actor_st.reserved_drone_launches.insert(st.ball_id, st);
        }
    }

    Ok(ReserveDroneLaunchesOutcome {
        kept_indices,
        reserved_ids,
        per_item_errors,
        rejected_error: None,
    })
}

fn finalize_drone_launches_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    mappings: Vec<(BallId, BallId, i64)>,
    rollback_res_ids: Vec<BallId>,
) -> Result<usize, String> {
    let mut updated = 0usize;
    for rid in rollback_res_ids {
        if ctx.actor_st.reserved_drone_launches.remove(&rid).is_some() {
            updated += 1;
        }
    }
    for (rid, bid, drone_item_id) in mappings {
        if let Some(st) = ctx.actor_st.reserved_drone_launches.remove(&rid) {
            if bid <= 0 {
                continue;
            }
            let mut st = st.in_solar_system(ctx.solar_system_id);
            st.ball_id = bid;
            st.drone_item_id = drone_item_id;
            ctx.actor_st.drone_next_ready_unix.remove(&bid);
            ctx.actor_st.active_drones_in_space.insert(bid, st);
            updated += 1;
            mark_active_drones_dirty_in_state(ctx.actor_st);
        }
    }
    Ok(updated)
}

fn reconnect_drones_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    req: ReconnectDronesReq,
) -> Result<ReconnectDronesOutcome, String> {
    let (mut cur_count, mut cur_bw) = count_ship_controlled_and_reserved_drones(ctx.actor_st, req.ship_id);
    let ship_pos = ctx.ballpark.balls.get(&req.ship_id).map(|b| b.position).unwrap_or(glam::DVec3::ZERO);
    let mut candidate_bids: Vec<BallId> = ctx
        .actor_st
        .active_drones_in_space
        .values()
        .filter(|d| {
            d.ball_id > 0
                && d.solar_system_id == ctx.solar_system_id
                && ((d.is_abandoned && d.original_owner_char_id == req.char_id)
                    || (d.owner_char_id == req.char_id && d.parent_ship_id == 0))
                && (req.target_ids.is_empty()
                    || req.target_ids.contains(&d.ball_id)
                    || req.target_ids.contains(&d.drone_item_id))
        })
        .map(|d| d.ball_id)
        .collect();
    candidate_bids.sort_unstable();

    let mut out = ReconnectDronesOutcome::default();
    for bid in candidate_bids {
        if ctx.actor_st.scoop_claimed_drones.contains_key(&bid) {
            continue;
        }
        if ctx.ballpark.balls.get(&bid).is_some_and(|b| b.position.distance(ship_pos) > 250_000.0) {
            out.out_of_range.push(bid);
            continue;
        }
        let Some(d) = ctx.actor_st.active_drones_in_space.get_mut(&bid) else {
            continue;
        };
        if cur_count >= req.max_drones_allowed {
            out.max_drones_exceeded.push(bid);
        } else if cur_bw + d.bandwidth_used > req.max_bandwidth + 1e-4 {
            out.insufficient_bandwidth.push(bid);
        } else {
            (d.is_abandoned, d.owner_char_id, d.parent_ship_id, d.activity_state, d.target_id) =
                (false, req.char_id, req.ship_id, 0, None);
            cur_count += 1;
            cur_bw += d.bandwidth_used;
            out.reconnected.push((bid, d.drone_item_id, d.type_id));
        }
    }
    if !out.reconnected.is_empty() {
        mark_active_drones_dirty_in_state(ctx.actor_st);
    }
    Ok(out)
}

fn apply_pending_scoop_abandon_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    pending: PendingScoopAbandon,
) -> Option<PendingScoopAbandon> {
    if ball_id <= 0 {
        return None;
    }
    let st = ctx.actor_st.active_drones_in_space.get_mut(&ball_id)?;
    if st.is_abandoned {
        return None;
    }
    let notify_char_id = if pending.notify_char_id > 0 {
        pending.notify_char_id
    } else {
        st.owner_char_id
    };
    let out = PendingScoopAbandon {
        kind: pending.kind,
        notify_char_id,
        drone_item_id: st.drone_item_id,
        type_id: st.type_id,
        solar_system_id: if st.solar_system_id > 0 {
            st.solar_system_id
        } else {
            ctx.solar_system_id
        },
    };
    st.parent_ship_id = 0;
    st.target_id = None;
    st.activity_state = 0;
    ctx.actor_st.active_drone_miners.remove(&ball_id);
    if pending.kind == PendingScoopAbandonKind::Abandon {
        st.is_abandoned = true;
        st.owner_char_id = 0;
        st.assist_char_id = None;
        st.guard_char_id = None;
        ctx.actor_st.drone_next_ready_unix.remove(&ball_id);
    }
    mark_active_drones_dirty_in_state(ctx.actor_st);
    Some(out)
}

fn abandon_drone_in_place_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
) -> Option<DroneSpaceState> {
    if ball_id <= 0 {
        return None;
    }
    let st = ctx.actor_st.active_drones_in_space.get_mut(&ball_id)?;
    if st.solar_system_id == 0 {
        *st = std::mem::take(st).in_solar_system(ctx.solar_system_id);
    }
    let snapshot = st.clone();
    st.is_abandoned = true;
    st.owner_char_id = 0;
    st.parent_ship_id = 0;
    st.target_id = None;
    st.activity_state = 0;
    st.assist_char_id = None;
    st.guard_char_id = None;
    clear_drone_runtime_in_state(ctx.actor_st, ball_id);
    mark_active_drones_dirty_in_state(ctx.actor_st);
    Some(snapshot)
}

fn execute_drone_scoop_claim_op_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    action: DroneScoopClaimAction,
) -> Result<DroneScoopClaimOutcome, String> {
    let is_returning_for_ship = |st: &DroneSpaceState, expected_owner: i64, target_ship_id: i64| {
        !st.is_abandoned
            && st.solar_system_id == ctx.solar_system_id
            && st.owner_char_id == expected_owner
            && (st.parent_ship_id == target_ship_id
                || (st.parent_ship_id == 0 && expected_owner > 0))
    };
    match action {
        DroneScoopClaimAction::ClaimPlayerScoop { char_id } => {
            if ctx.actor_st.scoop_claimed_drones.contains_key(&ball_id) {
                return Ok(DroneScoopClaimOutcome::AlreadyClaimed);
            }
            if let Some(st) = ctx.actor_st.active_drones_in_space.get(&ball_id) {
                if !st.is_abandoned && st.owner_char_id != char_id {
                    return Ok(DroneScoopClaimOutcome::OwnedByOther);
                }
            }
            ctx.actor_st.scoop_claimed_drones.insert(ball_id, None);
            Ok(DroneScoopClaimOutcome::Claimed(
                ctx.actor_st.active_drones_in_space.get(&ball_id).cloned(),
            ))
        }
        DroneScoopClaimAction::ClaimReturnScoop {
            expected_owner,
            target_ship_id,
        } => {
            if ctx.actor_st.scoop_claimed_drones.contains_key(&ball_id) {
                return Ok(DroneScoopClaimOutcome::AlreadyClaimed);
            }
            let Some(existing) = ctx.actor_st.active_drones_in_space.get(&ball_id).cloned() else {
                return Ok(DroneScoopClaimOutcome::NotEligible);
            };
            if !is_returning_for_ship(&existing, expected_owner, target_ship_id) {
                return Ok(DroneScoopClaimOutcome::NotEligible);
            }
            ctx.actor_st.scoop_claimed_drones.insert(ball_id, None);
            Ok(DroneScoopClaimOutcome::Claimed(Some(existing)))
        }
        DroneScoopClaimAction::ReleaseClaim => {
            let pending = ctx.actor_st.scoop_claimed_drones.remove(&ball_id).flatten();
            if let Some(applied) =
                pending.and_then(|p| apply_pending_scoop_abandon_in_actor(ctx, ball_id, p))
            {
                Ok(DroneScoopClaimOutcome::ReleasedWithPending(applied))
            } else {
                Ok(DroneScoopClaimOutcome::Released)
            }
        }
        DroneScoopClaimAction::ConsumeScoop => {
            ctx.actor_st.scoop_claimed_drones.remove(&ball_id);
            clear_drone_runtime_in_state(ctx.actor_st, ball_id);
            record_removed_drone_pre_hydration(ctx.actor_st, ball_id);
            let removed = ctx.actor_st.active_drones_in_space.remove(&ball_id);
            if removed.is_some() && ball_id > 0 {
                mark_active_drones_dirty_in_state(ctx.actor_st);
            }
            Ok(DroneScoopClaimOutcome::Consumed(removed))
        }
        DroneScoopClaimAction::ReleaseOrTakeForAbandon {
            char_id_opt,
            ship_id,
        } => {
            let pending = ctx.actor_st.scoop_claimed_drones.remove(&ball_id).flatten();
            if let Some(p) = pending
                .as_ref()
                .filter(|p| p.kind == PendingScoopAbandonKind::Disconnect)
                .cloned()
            {
                return Ok(match apply_pending_scoop_abandon_in_actor(ctx, ball_id, p) {
                    Some(applied) => DroneScoopClaimOutcome::ReleasedWithPending(applied),
                    None => DroneScoopClaimOutcome::TakeForAbandon(None),
                });
            }
            let can_abandon = ctx
                .actor_st
                .active_drones_in_space
                .get(&ball_id)
                .is_some_and(|st| {
                    !st.is_abandoned
                        && st.solar_system_id == ctx.solar_system_id
                        && match char_id_opt {
                            Some(cid) => st.owner_char_id == cid || st.parent_ship_id == ship_id,
                            None => st.parent_ship_id == ship_id || st.owner_char_id > 0,
                        }
                });
            if can_abandon {
                Ok(DroneScoopClaimOutcome::TakeForAbandon(
                    abandon_drone_in_place_in_actor(ctx, ball_id),
                ))
            } else if let Some(applied) =
                pending.and_then(|p| apply_pending_scoop_abandon_in_actor(ctx, ball_id, p))
            {
                Ok(DroneScoopClaimOutcome::ReleasedWithPending(applied))
            } else {
                Ok(DroneScoopClaimOutcome::TakeForAbandon(None))
            }
        }
        DroneScoopClaimAction::TakeUnclaimedForAbandon {
            expected_owner,
            target_ship_id,
            require_returning,
        } => {
            if let Some(pending_slot) = ctx.actor_st.scoop_claimed_drones.get_mut(&ball_id) {
                if let Some(st) = ctx.actor_st.active_drones_in_space.get(&ball_id) {
                    if st.ball_id > 0
                        && (!require_returning || st.activity_state == 4)
                        && is_returning_for_ship(st, expected_owner, target_ship_id)
                    {
                        let notify_char_id = if st.owner_char_id > 0 {
                            st.owner_char_id
                        } else {
                            expected_owner
                        };
                        PendingScoopAbandon::merge_into(
                            pending_slot,
                            PendingScoopAbandon {
                                kind: PendingScoopAbandonKind::Abandon,
                                notify_char_id,
                                drone_item_id: st.drone_item_id,
                                type_id: st.type_id,
                                solar_system_id: st.solar_system_id,
                            },
                        );
                    }
                }
                return Ok(DroneScoopClaimOutcome::TakeForAbandon(None));
            }
            let can_take = ctx
                .actor_st
                .active_drones_in_space
                .get(&ball_id)
                .is_some_and(|st| {
                    (!require_returning || st.activity_state == 4)
                        && is_returning_for_ship(st, expected_owner, target_ship_id)
                });
            if can_take {
                Ok(DroneScoopClaimOutcome::TakeForAbandon(
                    abandon_drone_in_place_in_actor(ctx, ball_id),
                ))
            } else {
                Ok(DroneScoopClaimOutcome::TakeForAbandon(None))
            }
        }
    }
}

fn run_drone_mutation_pre_hydration_aware(
    actor_st: &mut SystemActorState,
    mutate: impl FnOnce(&mut SystemActorState) -> bool,
) {
    let prev_keys: Vec<BallId> = if actor_st.active_drones_needs_sqlite_hydration {
        actor_st
            .active_drones_in_space
            .keys()
            .copied()
            .filter(|&k| k > 0)
            .collect()
    } else {
        Vec::new()
    };
    let dirty = mutate(actor_st);
    if actor_st.active_drones_needs_sqlite_hydration {
        for k in prev_keys {
            if !actor_st.active_drones_in_space.contains_key(&k) {
                actor_st.active_drones_removed_pre_hydration.insert(k);
            }
        }
    }
    if dirty {
        mark_active_drones_dirty_in_state(actor_st);
    }
}

fn mutate_active_drones_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    op: ActiveDroneMutOp,
) -> Result<(), String> {
    run_drone_mutation_pre_hydration_aware(ctx.actor_st, |st| {
        op(
            &mut st.active_drones_in_space,
            &mut st.active_drone_miners,
            &mut st.drone_next_ready_unix,
        )
    });
    Ok(())
}

fn mutate_active_drones_with_claims_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    op: ActiveDroneMutWithClaimsOp,
) -> Result<(), String> {
    run_drone_mutation_pre_hydration_aware(ctx.actor_st, |st| {
        op(
            &mut st.active_drones_in_space,
            &mut st.active_drone_miners,
            &mut st.drone_next_ready_unix,
            &mut st.scoop_claimed_drones,
        )
    });
    Ok(())
}

pub(in crate::system_actor) fn handle_drone_runtime_command(
    exec_ctx: &mut ActorExecCtx<'_>,
    cmd: SystemCommand,
) -> Option<SystemCommand> {
    match cmd {
        SystemCommand::UpsertDroneMiner { state, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = upsert_drone_miner_in_actor(&mut ctx, state);
            defer_cmd_reply(exec_ctx, reply, res);
            None
        }
        SystemCommand::RescheduleDroneMiner {
            drone_id,
            now_unix,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = reschedule_drone_miner_in_actor(&mut ctx, drone_id, now_unix);
            defer_cmd_reply(exec_ctx, reply, res);
            None
        }
        SystemCommand::SetDroneNextReadyUnix {
            drone_id,
            next_ready_unix,
            require_ball_in_space,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = set_drone_next_ready_unix_in_actor(
                &mut ctx,
                drone_id,
                next_ready_unix,
                require_ball_in_space,
            );
            defer_cmd_reply(exec_ctx, reply, res);
            None
        }
        SystemCommand::ClearDroneRuntime {
            drone_ids,
            clear_miners,
            clear_timers,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = clear_drone_runtime_in_actor(
                &mut ctx,
                ClearDroneRuntimeReq {
                    drone_ids,
                    clear_miners,
                    clear_timers,
                },
            );
            defer_cmd_reply(exec_ctx, reply, res);
            None
        }
        SystemCommand::UpsertActiveDrones {
            drones,
            only_if_absent,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = upsert_active_drones_in_actor(&mut ctx, drones, only_if_absent);
            defer_cmd_reply(exec_ctx, reply, res);
            None
        }
        SystemCommand::ReserveDroneLaunches { req, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = reserve_drone_launches_in_actor(&mut ctx, req);
            defer_cmd_reply(exec_ctx, reply, res);
            None
        }
        SystemCommand::FinalizeDroneLaunches {
            mappings,
            rollback_res_ids,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = finalize_drone_launches_in_actor(&mut ctx, mappings, rollback_res_ids);
            defer_cmd_reply(exec_ctx, reply, res);
            None
        }
        SystemCommand::ReconnectDrones { req, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = reconnect_drones_in_actor(&mut ctx, req);
            defer_cmd_reply(exec_ctx, reply, res);
            None
        }
        SystemCommand::MutateActiveDrones { op, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = mutate_active_drones_in_actor(&mut ctx, op);
            defer_cmd_reply(exec_ctx, reply, res);
            None
        }
        SystemCommand::MutateActiveDronesWithClaims { op, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = mutate_active_drones_with_claims_in_actor(&mut ctx, op);
            defer_cmd_reply(exec_ctx, reply, res);
            None
        }
        SystemCommand::ScoopDroneClaimOp {
            ball_id,
            action,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = execute_drone_scoop_claim_op_in_actor(&mut ctx, ball_id, action);
            defer_cmd_reply(exec_ctx, reply, res);
            None
        }
        SystemCommand::HydrateActiveDronesFromSqlite {
            drones,
            legacy_dirty,
            reply,
        } => {
            hydrate_active_drones_in_state(
                exec_ctx.actor_st,
                exec_ctx.solar_system_id,
                drones,
                legacy_dirty,
            );
            defer_cmd_reply(exec_ctx, reply, Ok(()));
            None
        }
        SystemCommand::SnapshotDirtyActiveDrones { reply } => {
            let snap = capture_dirty_active_drones_snapshot(exec_ctx.actor_st)
                .map(|(seq, _, entries)| (seq, entries));
            defer_cmd_reply(exec_ctx, reply, Ok(snap));
            None
        }
        SystemCommand::CommitActiveDronesFlushed { seq, reply } => {
            finish_flush_in_actor(
                exec_ctx.ballpark,
                exec_ctx.actor_st,
                exec_ctx.event_tx_actor.receiver_count(),
                seq,
            );
            defer_cmd_reply(exec_ctx, reply, Ok(()));
            None
        }
        other => Some(other),
    }
}

impl SystemHandle {
    /// Returns the published repeating mining state for `drone_id` in this solar system, if active.
    pub fn active_drone_miner(&self, drone_id: BallId) -> Option<DroneMiningState> {
        self.view().active_drone_miners.get(&drone_id).cloned()
    }

    /// Returns a snapshot of all published repeating drone mining states in this solar system.
    pub fn active_drone_miners(&self) -> HashMap<BallId, DroneMiningState> {
        self.view().active_drone_miners.clone()
    }

    /// Returns the published next weapon/combat ready UNIX timestamp for `drone_id`, if set.
    pub fn drone_next_ready_unix(&self, drone_id: BallId) -> Option<u64> {
        self.view().drone_next_ready_unix.get(&drone_id).copied()
    }

    /// Returns a snapshot of all published drone combat cooldown timestamps in this solar system.
    pub fn drone_next_ready_unix_map(&self) -> HashMap<BallId, u64> {
        self.view().drone_next_ready_unix.clone()
    }

    /// Inserts or replaces a repeating drone mining state inside this solar system actor (`#1541`).
    pub async fn upsert_drone_miner(&self, state: DroneMiningState) -> Result<(), String> {
        call_actor_result_with_deadline(self, "UpsertDroneMiner", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::UpsertDroneMiner { state, reply }
        })
        .await
    }

    /// Synchronous bridge for [`Self::upsert_drone_miner`].
    pub fn upsert_drone_miner_sync(&self, state: DroneMiningState) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "UpsertDroneMiner",
            ACTOR_CMD_TIMEOUT,
            state,
            |state, reply| SystemCommand::UpsertDroneMiner { state, reply },
            upsert_drone_miner_in_actor,
        )
    }

    /// Advances an existing mining drone's `next_cycle_unix` to `now_unix.saturating_add(duration_secs)`.
    pub async fn reschedule_drone_miner(
        &self,
        drone_id: BallId,
        now_unix: u64,
    ) -> Result<bool, String> {
        call_actor_result_with_deadline(self, "RescheduleDroneMiner", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::RescheduleDroneMiner {
                drone_id,
                now_unix,
                reply,
            }
        })
        .await
    }

    async fn clear_runtime_scope(&self, req: ClearDroneRuntimeReq) -> Result<usize, String> {
        if req.drone_ids.as_ref().is_some_and(Vec::is_empty) {
            return Ok(0);
        }
        call_actor_result_with_deadline(self, "ClearDroneRuntime", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::ClearDroneRuntime {
                drone_ids: req.drone_ids,
                clear_miners: req.clear_miners,
                clear_timers: req.clear_timers,
                reply,
            }
        })
        .await
    }

    fn clear_runtime_scope_sync(&self, req: ClearDroneRuntimeReq) -> Result<usize, String> {
        if req.drone_ids.as_ref().is_some_and(Vec::is_empty) {
            return Ok(0);
        }
        dispatch_actor_sync_with_deadline(
            self,
            "ClearDroneRuntime",
            ACTOR_CMD_TIMEOUT,
            req,
            |req, reply| SystemCommand::ClearDroneRuntime {
                drone_ids: req.drone_ids,
                clear_miners: req.clear_miners,
                clear_timers: req.clear_timers,
                reply,
            },
            clear_drone_runtime_in_actor,
        )
    }

    /// Removes active repeating mining state for `drone_id`, leaving combat cooldown intact.
    pub async fn remove_drone_miner(&self, drone_id: BallId) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: Some(vec![drone_id]),
            clear_miners: true,
            clear_timers: false,
        })
        .await
    }

    /// Synchronous bridge for [`Self::remove_drone_miner`].
    pub fn remove_drone_miner_sync(&self, drone_id: BallId) -> Result<usize, String> {
        self.clear_runtime_scope_sync(ClearDroneRuntimeReq {
            drone_ids: Some(vec![drone_id]),
            clear_miners: true,
            clear_timers: false,
        })
    }

    /// Clears all repeating drone mining state in this solar system.
    pub async fn clear_drone_miners(&self) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: None,
            clear_miners: true,
            clear_timers: false,
        })
        .await
    }

    /// Synchronous bridge for [`Self::clear_drone_miners`].
    pub fn clear_drone_miners_sync(&self) -> Result<usize, String> {
        self.clear_runtime_scope_sync(ClearDroneRuntimeReq {
            drone_ids: None,
            clear_miners: true,
            clear_timers: false,
        })
    }

    async fn set_drone_next_ready_unix_inner(
        &self,
        drone_id: BallId,
        next_ready_unix: u64,
        require_ball_in_space: bool,
    ) -> Result<bool, String> {
        call_actor_result_with_deadline(
            self,
            "SetDroneNextReadyUnix",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::SetDroneNextReadyUnix {
                drone_id,
                next_ready_unix,
                require_ball_in_space,
                reply,
            },
        )
        .await
    }

    /// Sets `drone_next_ready_unix` for `drone_id` unconditionally.
    pub async fn set_drone_next_ready_unix(
        &self,
        drone_id: BallId,
        next_ready_unix: u64,
    ) -> Result<bool, String> {
        self.set_drone_next_ready_unix_inner(drone_id, next_ready_unix, false)
            .await
    }

    /// Synchronous bridge for [`Self::set_drone_next_ready_unix`].
    pub fn set_drone_next_ready_unix_sync(
        &self,
        drone_id: BallId,
        next_ready_unix: u64,
    ) -> Result<bool, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "SetDroneNextReadyUnix",
            ACTOR_CMD_TIMEOUT,
            (drone_id, next_ready_unix, false),
            |(drone_id, next_ready_unix, require_ball_in_space), reply| {
                SystemCommand::SetDroneNextReadyUnix {
                    drone_id,
                    next_ready_unix,
                    require_ball_in_space,
                    reply,
                }
            },
            |ctx, (drone_id, next_ready_unix, require_ball_in_space)| {
                set_drone_next_ready_unix_in_actor(
                    ctx,
                    drone_id,
                    next_ready_unix,
                    require_ball_in_space,
                )
            },
        )
    }

    /// Sets `drone_next_ready_unix` only if `drone_id` still has a ball in `Ballpark`, preventing
    /// a racing combat step from resurrecting timer state after ball removal (`#1541`).
    pub async fn set_drone_next_ready_unix_if_present(
        &self,
        drone_id: BallId,
        next_ready_unix: u64,
    ) -> Result<bool, String> {
        self.set_drone_next_ready_unix_inner(drone_id, next_ready_unix, true)
            .await
    }

    /// Clears `drone_next_ready_unix` for `drone_id`.
    pub async fn clear_drone_next_ready_unix(&self, drone_id: BallId) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: Some(vec![drone_id]),
            clear_miners: false,
            clear_timers: true,
        })
        .await
    }

    /// Synchronous bridge for [`Self::clear_drone_next_ready_unix`].
    pub fn clear_drone_next_ready_unix_sync(&self, drone_id: BallId) -> Result<usize, String> {
        self.clear_runtime_scope_sync(ClearDroneRuntimeReq {
            drone_ids: Some(vec![drone_id]),
            clear_miners: false,
            clear_timers: true,
        })
    }

    /// Clears `drone_next_ready_unix` for each drone in `drone_ids`.
    pub async fn clear_drone_next_ready_unix_batch(
        &self,
        drone_ids: Vec<BallId>,
    ) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: Some(drone_ids),
            clear_miners: false,
            clear_timers: true,
        })
        .await
    }

    /// Clears all `drone_next_ready_unix` timers in this solar system.
    pub async fn clear_all_drone_next_ready_unix(&self) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: None,
            clear_miners: false,
            clear_timers: true,
        })
        .await
    }

    /// Clears both active mining state and combat cooldown timer for `drone_id`.
    pub async fn clear_drone_runtime(&self, drone_id: BallId) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: Some(vec![drone_id]),
            clear_miners: true,
            clear_timers: true,
        })
        .await
    }

    /// Synchronous bridge for [`Self::clear_drone_runtime`].
    pub fn clear_drone_runtime_sync(&self, drone_id: BallId) -> Result<usize, String> {
        self.clear_runtime_scope_sync(ClearDroneRuntimeReq {
            drone_ids: Some(vec![drone_id]),
            clear_miners: true,
            clear_timers: true,
        })
    }

    /// Clears both active mining state and combat cooldown timers for all drones in `drone_ids`.
    pub async fn clear_drone_runtime_batch(
        &self,
        drone_ids: Vec<BallId>,
    ) -> Result<usize, String> {
        self.clear_runtime_scope(ClearDroneRuntimeReq {
            drone_ids: Some(drone_ids),
            clear_miners: true,
            clear_timers: true,
        })
        .await
    }

    /// Synchronous bridge for [`Self::clear_drone_runtime_batch`].
    pub fn clear_drone_runtime_batch_sync(&self, drone_ids: Vec<BallId>) -> Result<usize, String> {
        self.clear_runtime_scope_sync(ClearDroneRuntimeReq {
            drone_ids: Some(drone_ids),
            clear_miners: true,
            clear_timers: true,
        })
    }

    /// Returns a published [`super::SystemView`] after hydrating any deferred SQLite active drones.
    pub(crate) fn hydrated_view(&self) -> Arc<super::SystemView> {
        ensure_active_drones_hydrated_sync(self);
        self.view()
    }

    /// Returns the published [`DroneSpaceState`] for `ball_id` in this system, if active.
    pub fn active_drone(&self, ball_id: BallId) -> Option<DroneSpaceState> {
        (ball_id > 0)
            .then(|| self.hydrated_view().active_drones_in_space.get(&ball_id).cloned())
            .flatten()
    }

    /// Returns `true` when `ball_id` is present in this system's published `active_drones_in_space`.
    pub fn has_active_drone(&self, ball_id: BallId) -> bool {
        ball_id > 0 && self.hydrated_view().active_drones_in_space.contains_key(&ball_id)
    }

    /// Returns a snapshot of all published active drones in this solar system.
    pub fn active_drones_snapshot(&self) -> HashMap<BallId, DroneSpaceState> {
        self.hydrated_view().active_drones_in_space.clone()
    }

    pub(crate) async fn ensure_active_drones_hydrated(&self) {
        if !self.view().active_drones_needs_sqlite_hydration
            || !eve_db::lock_rank::held_lock_ranks().is_empty()
        {
            return;
        }
        let h = self.clone();
        if let Some((drones, legacy_dirty)) =
            tokio::task::spawn_blocking(move || read_deferred_hydration_for_handle(&h))
                .await
                .ok()
                .flatten()
        {
            let _ = call_actor_result_with_deadline(
                self,
                "HydrateActiveDronesFromSqlite",
                ACTOR_CMD_TIMEOUT,
                |reply| SystemCommand::HydrateActiveDronesFromSqlite {
                    drones,
                    legacy_dirty,
                    reply,
                },
            )
            .await;
        }
    }

    /// Inserts or replaces an active [`DroneSpaceState`] in this system actor.
    pub async fn upsert_active_drone(&self, drone: DroneSpaceState) -> Result<(), String> {
        self.upsert_active_drones(vec![drone], false).await.map(|_| ())
    }

    /// Synchronous bridge for [`Self::upsert_active_drone`].
    pub fn upsert_active_drone_sync(&self, drone: DroneSpaceState) -> Result<(), String> {
        self.upsert_active_drones_sync(vec![drone], false).map(|_| ())
    }

    /// Inserts or replaces a batch of [`DroneSpaceState`] entries inside this system actor.
    pub async fn upsert_active_drones(
        &self,
        drones: Vec<DroneSpaceState>,
        only_if_absent: bool,
    ) -> Result<usize, String> {
        if drones.is_empty() {
            return Ok(0);
        }
        self.ensure_active_drones_hydrated().await;
        call_actor_result_with_deadline(self, "UpsertActiveDrones", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::UpsertActiveDrones {
                drones,
                only_if_absent,
                reply,
            }
        })
        .await
    }

    /// Synchronous bridge for [`Self::upsert_active_drones`].
    pub fn upsert_active_drones_sync(
        &self,
        drones: Vec<DroneSpaceState>,
        only_if_absent: bool,
    ) -> Result<usize, String> {
        if drones.is_empty() {
            return Ok(0);
        }
        ensure_active_drones_hydrated_sync(self);
        dispatch_actor_sync_with_deadline(
            self,
            "UpsertActiveDrones",
            ACTOR_CMD_TIMEOUT,
            (drones, only_if_absent),
            |(drones, only_if_absent), reply| SystemCommand::UpsertActiveDrones {
                drones,
                only_if_absent,
                reply,
            },
            |ctx, (drones, only_if_absent)| upsert_active_drones_in_actor(ctx, drones, only_if_absent),
        )
    }

    /// Atomically checks active drone capacity/bandwidth and reserves negative ball IDs before DB tx.
    pub async fn reserve_drone_launches(
        &self,
        req: ReserveDroneLaunchesReq,
    ) -> Result<ReserveDroneLaunchesOutcome, String> {
        self.ensure_active_drones_hydrated().await;
        call_actor_result_with_deadline(
            self,
            "ReserveDroneLaunches",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::ReserveDroneLaunches { req, reply },
        )
        .await
    }

    /// Commits `(res_id -> new_ball_id, new_item_id)` launches or rolls back reserved IDs on DB failure.
    pub async fn finalize_drone_launches(
        &self,
        mappings: Vec<(BallId, BallId, i64)>,
        rollback_res_ids: Vec<BallId>,
    ) -> Result<usize, String> {
        if mappings.is_empty() && rollback_res_ids.is_empty() {
            return Ok(0);
        }
        self.ensure_active_drones_hydrated().await;
        call_actor_result_with_deadline(
            self,
            "FinalizeDroneLaunches",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::FinalizeDroneLaunches {
                mappings,
                rollback_res_ids,
                reply,
            },
        )
        .await
    }

    /// Best-effort non-blocking rollback of unpromoted `LaunchDrones` reservations on cancellation/drop.
    pub fn try_rollback_drone_launches(&self, rollback_res_ids: Vec<BallId>) {
        if rollback_res_ids.is_empty() {
            return;
        }
        let (reply, _) = tokio::sync::oneshot::channel();
        if let Err(tokio::sync::mpsc::error::TrySendError::Full(cmd)) =
            self.cmd_tx.try_send(SystemCommand::FinalizeDroneLaunches {
                mappings: Vec::new(),
                rollback_res_ids,
                reply,
            })
        {
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                let tx = self.cmd_tx.clone();
                rt.spawn(async move {
                    let _ = tx.send(cmd).await;
                });
            }
        }
    }

    /// Best-effort non-blocking release of an in-flight scoop claim on task cancellation/drop.
    pub fn try_release_scoop_claim(&self, ball_id: BallId) {
        if ball_id <= 0 {
            return;
        }
        let (reply, _) = tokio::sync::oneshot::channel();
        if let Err(tokio::sync::mpsc::error::TrySendError::Full(cmd)) =
            self.cmd_tx.try_send(SystemCommand::ScoopDroneClaimOp {
                ball_id,
                action: DroneScoopClaimAction::ReleaseClaim,
                reply,
            })
        {
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                let tx = self.cmd_tx.clone();
                rt.spawn(async move {
                    let _ = tx.send(cmd).await;
                });
            }
        }
    }

    /// Atomically validates drone control/bandwidth limits (including `reserved_drone_launches`)
    /// and range inside [`SystemActor`], transitioning unclaimed eligible drones to `ship_id`.
    pub async fn reconnect_drones(
        &self,
        req: ReconnectDronesReq,
    ) -> Result<ReconnectDronesOutcome, String> {
        self.ensure_active_drones_hydrated().await;
        call_actor_result_with_deadline(self, "ReconnectDrones", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::ReconnectDrones { req, reply }
        })
        .await
    }

    /// Executes an atomic closure over this system's active drones, mining states, and cooldown timers.
    pub async fn mutate_active_drones<R: Send + 'static>(
        &self,
        op: impl FnOnce(&mut HashMap<BallId, DroneSpaceState>, &mut HashMap<BallId, DroneMiningState>, &mut HashMap<BallId, u64>) -> (bool, R) + Send + 'static,
    ) -> Result<R, String> {
        self.ensure_active_drones_hydrated().await;
        let (res_tx, mut res_rx) = tokio::sync::oneshot::channel::<R>();
        let boxed: ActiveDroneMutOp = Box::new(move |d, m, t| { let (dirty, out) = op(d, m, t); let _ = res_tx.send(out); dirty });
        call_actor_result_with_deadline(self, "MutateActiveDrones", ACTOR_CMD_TIMEOUT, |reply| SystemCommand::MutateActiveDrones { op: boxed, reply }).await?;
        res_rx.try_recv().map_err(|_| format!("system {} active-drone mutation dropped result", self.solar_system_id))
    }

    /// Synchronous bridge for [`Self::mutate_active_drones`].
    pub fn mutate_active_drones_sync<R: Send + 'static>(
        &self,
        op: impl FnOnce(&mut HashMap<BallId, DroneSpaceState>, &mut HashMap<BallId, DroneMiningState>, &mut HashMap<BallId, u64>) -> (bool, R) + Send + 'static,
    ) -> Result<R, String> {
        ensure_active_drones_hydrated_sync(self);
        let (res_tx, mut res_rx) = tokio::sync::oneshot::channel::<R>();
        let boxed: ActiveDroneMutOp = Box::new(move |d, m, t| { let (dirty, out) = op(d, m, t); let _ = res_tx.send(out); dirty });
        dispatch_actor_sync_with_deadline(
            self, "MutateActiveDrones", ACTOR_CMD_TIMEOUT, boxed,
            |op, reply| SystemCommand::MutateActiveDrones { op, reply }, mutate_active_drones_in_actor,
        )?;
        res_rx.try_recv().map_err(|_| format!("system {} active-drone mutation dropped result", self.solar_system_id))
    }

    /// Executes an atomic closure over active drones, mining states, cooldown timers, and `scoop_claimed_drones`.
    pub async fn mutate_active_drones_with_claims<R: Send + 'static>(
        &self,
        op: impl FnOnce(&mut HashMap<BallId, DroneSpaceState>, &mut HashMap<BallId, DroneMiningState>, &mut HashMap<BallId, u64>, &mut HashMap<BallId, Option<PendingScoopAbandon>>) -> (bool, R) + Send + 'static,
    ) -> Result<R, String> {
        self.ensure_active_drones_hydrated().await;
        let (res_tx, mut res_rx) = tokio::sync::oneshot::channel::<R>();
        let boxed: ActiveDroneMutWithClaimsOp = Box::new(move |d, m, t, c| { let (dirty, out) = op(d, m, t, c); let _ = res_tx.send(out); dirty });
        call_actor_result_with_deadline(self, "MutateActiveDronesWithClaims", ACTOR_CMD_TIMEOUT, |reply| SystemCommand::MutateActiveDronesWithClaims { op: boxed, reply }).await?;
        res_rx.try_recv().map_err(|_| format!("system {} active-drone mutation dropped result", self.solar_system_id))
    }

    /// Synchronous bridge for [`Self::mutate_active_drones_with_claims`].
    pub fn mutate_active_drones_with_claims_sync<R: Send + 'static>(
        &self,
        op: impl FnOnce(&mut HashMap<BallId, DroneSpaceState>, &mut HashMap<BallId, DroneMiningState>, &mut HashMap<BallId, u64>, &mut HashMap<BallId, Option<PendingScoopAbandon>>) -> (bool, R) + Send + 'static,
    ) -> Result<R, String> {
        ensure_active_drones_hydrated_sync(self);
        let (res_tx, mut res_rx) = tokio::sync::oneshot::channel::<R>();
        let boxed: ActiveDroneMutWithClaimsOp = Box::new(move |d, m, t, c| { let (dirty, out) = op(d, m, t, c); let _ = res_tx.send(out); dirty });
        dispatch_actor_sync_with_deadline(
            self, "MutateActiveDronesWithClaims", ACTOR_CMD_TIMEOUT, boxed,
            |op, reply| SystemCommand::MutateActiveDronesWithClaims { op, reply }, mutate_active_drones_with_claims_in_actor,
        )?;
        res_rx.try_recv().map_err(|_| format!("system {} active-drone mutation dropped result", self.solar_system_id))
    }

    /// Removes and returns `ball_id` from `active_drones_in_space`, clearing any transient scoop claim.
    pub async fn remove_active_drone(&self, ball_id: BallId) -> Result<Option<DroneSpaceState>, String> {
        match self.drone_scoop_claim_op(ball_id, DroneScoopClaimAction::ConsumeScoop).await? {
            DroneScoopClaimOutcome::Consumed(removed) => Ok(removed),
            _ => Ok(None),
        }
    }

    /// Synchronous bridge for [`Self::remove_active_drone`].
    pub fn remove_active_drone_sync(&self, ball_id: BallId) -> Result<Option<DroneSpaceState>, String> {
        ensure_active_drones_hydrated_sync(self);
        match dispatch_actor_sync_with_deadline(
            self, "ScoopDroneClaimOp", ACTOR_CMD_TIMEOUT, (ball_id, DroneScoopClaimAction::ConsumeScoop),
            |(ball_id, action), reply| SystemCommand::ScoopDroneClaimOp { ball_id, action, reply },
            |ctx, (ball_id, action)| execute_drone_scoop_claim_op_in_actor(ctx, ball_id, action),
        )? {
            DroneScoopClaimOutcome::Consumed(removed) => Ok(removed),
            _ => Ok(None),
        }
    }

    /// Executes a non-destructive transient scoop claim operation inside this system actor.
    pub async fn drone_scoop_claim_op(
        &self,
        ball_id: BallId,
        action: DroneScoopClaimAction,
    ) -> Result<DroneScoopClaimOutcome, String> {
        if !matches!(action, DroneScoopClaimAction::ReleaseClaim) {
            self.ensure_active_drones_hydrated().await;
        }
        call_actor_result_with_deadline(self, "ScoopDroneClaimOp", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::ScoopDroneClaimOp {
                ball_id,
                action,
                reply,
            }
        })
        .await
    }
}

/// RAII drop guard releasing an active transient scoop claim on cancellation or panic drop.
pub struct ScoopClaimGuard<'a> {
    sys_handle: &'a SystemHandle,
    ball_id: Option<BallId>,
}

impl<'a> ScoopClaimGuard<'a> {
    pub fn new(sys_handle: &'a SystemHandle, ball_id: BallId) -> Self {
        Self {
            sys_handle,
            ball_id: (ball_id > 0).then_some(ball_id),
        }
    }

    pub fn disarm(&mut self) {
        self.ball_id = None;
    }
}

impl Drop for ScoopClaimGuard<'_> {
    fn drop(&mut self) {
        if let Some(ball_id) = self.ball_id.take() {
            self.sys_handle.try_release_scoop_claim(ball_id);
        }
    }
}

impl SystemRegistry {
    /// Returns total active repeating drone miners across all currently loaded solar systems.
    pub fn active_drone_miners_count(&self) -> usize {
        self.active_system_handles()
            .into_iter()
            .map(|(_, h)| h.view().active_drone_miners.len())
            .sum()
    }

    /// Collects `(SystemHandle, DroneMiningState)` pairs across all currently loaded systems.
    pub fn all_active_drone_miners(&self) -> Vec<(SystemHandle, DroneMiningState)> {
        let mut out = Vec::new();
        for (_, handle) in self.active_system_handles() {
            let miners = handle.active_drone_miners();
            out.extend(miners.into_values().map(|m| (handle.clone(), m)));
        }
        out
    }

    /// Clears active repeating drone miners across all currently loaded solar systems.
    pub async fn clear_all_drone_miners(&self) {
        for (_, handle) in self.active_system_handles() {
            if !handle.view().active_drone_miners.is_empty() {
                let _ = handle.clear_drone_miners().await;
            }
        }
    }

    /// Finds an active drone by `ball_id` across loaded solar system actors without waking cold systems.
    pub fn find_active_drone(&self, ball_id: BallId) -> Option<(SystemHandle, DroneSpaceState)> {
        if ball_id <= 0 {
            return None;
        }
        for (_, handle) in self.active_system_handles() {
            if let Some(d) = handle.active_drone(ball_id) {
                return Some((handle, d));
            }
        }
        None
    }

    /// Finds an active drone by `drone_item_id` (or `ball_id`) across loaded solar system actors.
    pub fn find_active_drone_by_item(
        &self,
        drone_item_id: i64,
    ) -> Option<(SystemHandle, DroneSpaceState)> {
        for (_, handle) in self.active_system_handles() {
            let view = handle.hydrated_view();
            if let Some(d) = view
                .active_drones_in_space
                .values()
                .find(|d| d.ball_id > 0 && (d.drone_item_id == drone_item_id || d.ball_id == drone_item_id))
            {
                return Some((handle.clone(), d.clone()));
            }
        }
        None
    }

    /// Collects `(SystemHandle, DroneSpaceState)` pairs across all currently loaded solar systems.
    pub fn all_active_drones(&self) -> Vec<(SystemHandle, DroneSpaceState)> {
        let mut out = Vec::new();
        for (_, handle) in self.active_system_handles() {
            let view = handle.hydrated_view();
            for d in view.active_drones_in_space.values() {
                if d.ball_id > 0 {
                    out.push((handle.clone(), d.clone()));
                }
            }
        }
        out
    }

    /// Returns the total active drones across all currently loaded solar system actors.
    pub fn active_drones_count(&self) -> usize {
        self.active_system_handles()
            .into_iter()
            .map(|(_, h)| {
                h.hydrated_view()
                    .active_drones_in_space
                    .values()
                    .filter(|d| d.ball_id > 0)
                    .count()
            })
            .sum()
    }

    /// Removes and returns `ball_id` from whichever active solar system currently owns it.
    pub fn remove_active_drone_across_systems_sync(&self, ball_id: BallId) -> Option<DroneSpaceState> {
        for (_, handle) in self.active_system_handles() {
            if handle.has_active_drone(ball_id) || handle.view().active_drones_needs_sqlite_hydration {
                if let Ok(Some(removed)) = handle.remove_active_drone_sync(ball_id) {
                    return Some(removed);
                }
            }
        }
        None
    }

    /// Clears all active drones across all currently loaded solar system actors.
    pub fn clear_all_active_drones_sync(&self) {
        for (_, handle) in self.active_system_handles() {
            let view = handle.view();
            if view.active_drones_needs_sqlite_hydration || !view.active_drones_in_space.is_empty() {
                let _ = handle.mutate_active_drones_sync(|drones, _, _| {
                    let dirty = drones.keys().any(|&bid| bid > 0);
                    drones.clear();
                    (dirty, ())
                });
            }
        }
    }

    /// Wakes `solar_system_id` for persisted drone seeding or test-helper seeding.
    /// When running inside a synchronous unit `#[test]` with no active Tokio runtime, uses
    /// `self.headless_test_rt` (owned by this registry, never global/static) under `testkit`.
    pub(crate) fn wake_for_drone_seed(&self, solar_system_id: SolarSystemId) -> SystemHandle {
        if let Some(handle) = self.get_handle(solar_system_id) {
            return handle;
        }
        let reason = super::SystemWakeReason::ClusterTick(
            super::ClusterTickWakeReason::ScheduledWork,
        );
        #[cfg(any(test, feature = "testkit"))]
        if tokio::runtime::Handle::try_current().is_err() {
            let rt = self.drone_rt_state.headless_test_rt.get_or_init(|| {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(1)
                    .enable_time()
                    .build()
                    .expect("failed to build per-registry headless test runtime")
            });
            let _guard = rt.enter();
            return self
                .wake_with_reason(solar_system_id, reason)
                .unwrap_or_else(|err| {
                    panic!("failed to activate solar system {solar_system_id} for headless test seed: {err}")
                });
        }
        self.wake_with_reason(solar_system_id, reason)
            .unwrap_or_else(|err| {
                panic!("failed to activate solar system {solar_system_id} for drone seed: {err}")
            })
    }
}
