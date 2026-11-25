//! Per-system active drones in space, repeating drone mining, and drone combat cooldown state
//! owned by `SystemActor` (`#1541`).

#![forbid(unsafe_code)]

use super::{
    call_actor_result_with_deadline, defer_reply, dispatch_actor_sync_with_deadline,
    ActorExecCtx, BallOpsCtx, SystemActorState, SystemCommand, SystemHandle, SystemRegistry,
    ACTOR_CMD_TIMEOUT,
};
use eve_destiny::{BallId, DamageVector};
use eve_sde::SolarSystemId;
use std::collections::HashMap;
use std::sync::Arc;

/// Runtime state for a launched or abandoned drone in solar-system space (`#1541` Track D2b).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
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
}

/// Clears repeating mining state, combat cooldown, and any active-space drone entry on explicit ball removal.
pub(in crate::system_actor) fn clear_removed_ball_drones_in_state(
    actor_st: &mut SystemActorState,
    ball_id: BallId,
) {
    clear_drone_runtime_in_state(actor_st, ball_id);
    actor_st.scoop_claimed_drones.remove(&ball_id);
    if actor_st.active_drones_in_space.remove(&ball_id).is_some() && ball_id > 0 {
        mark_active_drones_dirty_in_state(actor_st);
    }
}

fn capture_dirty_active_drones_snapshot(
    actor_st: &SystemActorState,
) -> Option<(u64, Arc<eve_db::WorldRepository>, Vec<(BallId, DroneSpaceState)>)> {
    if !actor_st.active_drones_checkpoint_dirty {
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

fn flush_actor_active_drones_checkpoint_post_unlock_inner(
    solar_system_id: SolarSystemId,
    actor_state_lock: &super::RankedRwLock<SystemActorState, { super::RANK_ACTOR_STATE }>,
    blocking: bool,
) {
    if !eve_db::lock_rank::held_lock_ranks().is_empty() {
        return;
    }
    let (dirty_fast, flush_mutex) = match actor_state_lock.read() {
        Ok(st) => (
            st.active_drones_checkpoint_dirty,
            Arc::clone(&st.active_drones_flush_mutex),
        ),
        Err(e) => {
            let st = e.into_inner();
            (
                st.active_drones_checkpoint_dirty,
                Arc::clone(&st.active_drones_flush_mutex),
            )
        }
    };
    if !dirty_fast {
        return;
    }
    let _flush_guard = if blocking {
        flush_mutex.lock().unwrap_or_else(|e| e.into_inner())
    } else {
        match flush_mutex.try_lock() {
            Ok(g) => g,
            Err(_) => return,
        }
    };
    let batch = {
        let st = actor_state_lock.read().unwrap_or_else(|e| e.into_inner());
        capture_dirty_active_drones_snapshot(&st)
    };
    let Some((seq, db, entries)) = batch else {
        return;
    };
    let now_unix = db.clock().now_unix_secs();
    let keyed: Vec<(String, &DroneSpaceState)> = entries
        .iter()
        .map(|(bid, d)| (bid.to_string(), d))
        .collect();
    let write_res = if blocking {
        Some(db.with_sqlite_conn(|conn| {
            eve_db::sqlite_store::system_checkpoint::replace_system_checkpoints_for_kind(
                conn,
                solar_system_id,
                eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
                &keyed,
                now_unix,
            )
        }))
    } else {
        db.try_with_sqlite_conn_mut(|conn| {
            eve_db::sqlite_store::system_checkpoint::replace_system_checkpoints_for_kind(
                conn,
                solar_system_id,
                eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
                &keyed,
                now_unix,
            )
        })
    };
    if matches!(write_res, Some(Ok(()))) {
        let mut st = actor_state_lock.write().unwrap_or_else(|e| e.into_inner());
        if st.active_drones_checkpoint_seq == seq {
            st.active_drones_checkpoint_dirty = false;
        }
    }
}

/// Opportunistically flushes dirty `active_drones_in_space` to SQLite without blocking on `db.writer`
/// or `active_drones_flush_mutex` (`#1541` Rule 3; prevents deadlocking the actor loop while another
/// thread holds `db.with_mut_snapshot`). Leaves dirty flag set if contended.
pub(crate) fn try_flush_actor_active_drones_checkpoint_post_unlock(
    solar_system_id: SolarSystemId,
    actor_state_lock: &super::RankedRwLock<SystemActorState, { super::RANK_ACTOR_STATE }>,
) {
    flush_actor_active_drones_checkpoint_post_unlock_inner(
        solar_system_id,
        actor_state_lock,
        false,
    );
}

/// Acquires `actor_state` briefly only to take a dirty active-drones snapshot, drops the lock,
/// and commits to SQLite post-unlock (used outside live actor turns, e.g. `stop_system` / shutdown).
pub(crate) fn flush_actor_active_drones_checkpoint_post_unlock(
    solar_system_id: SolarSystemId,
    actor_state_lock: &super::RankedRwLock<SystemActorState, { super::RANK_ACTOR_STATE }>,
) {
    flush_actor_active_drones_checkpoint_post_unlock_inner(
        solar_system_id,
        actor_state_lock,
        true,
    );
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
                .migrated_legacy_drone_systems
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            for sys_id in by_sys.keys() {
                guard.insert(*sys_id);
            }
        }
    }
}

/// Loads `active_drones_in_space` for `solar_system_id` from SQLite `system_checkpoint`,
/// with one-way legacy load fallback from `PersistedCombatCrimewatchState::extended_runtime_json`.
pub(crate) fn load_initial_active_drones_for_system(
    registry: &SystemRegistry,
    solar_system_id: SolarSystemId,
    db_read_safe: bool,
) -> (HashMap<BallId, DroneSpaceState>, bool) {
    let mut drones = HashMap::new();
    let mut dirty = false;
    if !db_read_safe {
        return (drones, dirty);
    }
    let Some(ref db) = registry.db else {
        return (drones, dirty);
    };
    if let Ok(rows) = db.with_sqlite_conn(|conn| {
        eve_db::sqlite_store::system_checkpoint::load_system_checkpoints::<DroneSpaceState>(
            conn,
            solar_system_id,
            eve_db::sqlite_store::system_checkpoint::KIND_ACTIVE_DRONES,
        )
    }) {
        for (_, d) in rows {
            let d = d.in_solar_system(solar_system_id);
            drones.insert(d.ball_id, d);
        }
    }
    if drones.is_empty() {
        let already_migrated = registry
            .migrated_legacy_drone_systems
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&solar_system_id);
        if !already_migrated {
            let ext_json = db.with_snapshot(|snap| {
                snap.combat_crimewatch_state.extended_runtime_json.clone()
            });
            if !ext_json.is_empty() && ext_json.contains("active_drones_in_space") {
                if let Ok(legacy) = serde_json::from_str::<LegacyActiveDronesPayload>(&ext_json) {
                    for (bid, mut d) in legacy.active_drones_in_space {
                        if d.solar_system_id == solar_system_id {
                            d.ball_id = bid;
                            drones.insert(bid, d);
                            dirty = true;
                        }
                    }
                }
            }
            registry
                .migrated_legacy_drone_systems
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(solar_system_id);
        }
    }
    (drones, dirty)
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

fn reserve_drone_launches_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    req: ReserveDroneLaunchesReq,
) -> Result<ReserveDroneLaunchesOutcome, String> {
    let (cur_count, cur_bw) = ctx
        .actor_st
        .active_drones_in_space
        .values()
        .filter(|d| d.ball_id > 0)
        .chain(ctx.actor_st.reserved_drone_launches.values())
        .filter(|d| d.parent_ship_id == req.ship_id && !d.is_abandoned)
        .fold((0usize, 0.0f64), |(c, bw), d| (c + 1, bw + d.bandwidth_used));
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
                let removed = ctx.actor_st.active_drones_in_space.remove(&ball_id);
                if removed.is_some() && ball_id > 0 {
                    mark_active_drones_dirty_in_state(ctx.actor_st);
                }
                Ok(DroneScoopClaimOutcome::TakeForAbandon(removed))
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
                let removed = ctx.actor_st.active_drones_in_space.remove(&ball_id);
                if removed.is_some() && ball_id > 0 {
                    mark_active_drones_dirty_in_state(ctx.actor_st);
                }
                Ok(DroneScoopClaimOutcome::TakeForAbandon(removed))
            } else {
                Ok(DroneScoopClaimOutcome::TakeForAbandon(None))
            }
        }
    }
}

fn mutate_active_drones_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    op: ActiveDroneMutOp,
) -> Result<(), String> {
    let dirty = op(
        &mut ctx.actor_st.active_drones_in_space,
        &mut ctx.actor_st.active_drone_miners,
        &mut ctx.actor_st.drone_next_ready_unix,
    );
    if dirty {
        mark_active_drones_dirty_in_state(ctx.actor_st);
    }
    Ok(())
}

fn mutate_active_drones_with_claims_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    op: ActiveDroneMutWithClaimsOp,
) -> Result<(), String> {
    let dirty = op(
        &mut ctx.actor_st.active_drones_in_space,
        &mut ctx.actor_st.active_drone_miners,
        &mut ctx.actor_st.drone_next_ready_unix,
        &mut ctx.actor_st.scoop_claimed_drones,
    );
    if dirty {
        mark_active_drones_dirty_in_state(ctx.actor_st);
    }
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
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::RescheduleDroneMiner {
            drone_id,
            now_unix,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = reschedule_drone_miner_in_actor(&mut ctx, drone_id, now_unix);
            defer_reply(exec_ctx.deferred_replies, reply, res);
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
            defer_reply(exec_ctx.deferred_replies, reply, res);
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
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::UpsertActiveDrones {
            drones,
            only_if_absent,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = upsert_active_drones_in_actor(&mut ctx, drones, only_if_absent);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ReserveDroneLaunches { req, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = reserve_drone_launches_in_actor(&mut ctx, req);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::FinalizeDroneLaunches {
            mappings,
            rollback_res_ids,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = finalize_drone_launches_in_actor(&mut ctx, mappings, rollback_res_ids);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::MutateActiveDrones { op, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = mutate_active_drones_in_actor(&mut ctx, op);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::MutateActiveDronesWithClaims { op, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = mutate_active_drones_with_claims_in_actor(&mut ctx, op);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ScoopDroneClaimOp {
            ball_id,
            action,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = execute_drone_scoop_claim_op_in_actor(&mut ctx, ball_id, action);
            defer_reply(exec_ctx.deferred_replies, reply, res);
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

    /// Returns the published [`DroneSpaceState`] for `ball_id` in this system, if active.
    pub fn active_drone(&self, ball_id: BallId) -> Option<DroneSpaceState> {
        (ball_id > 0)
            .then(|| self.view().active_drones_in_space.get(&ball_id).cloned())
            .flatten()
    }

    /// Returns `true` when `ball_id` is present in this system's published `active_drones_in_space`.
    pub fn has_active_drone(&self, ball_id: BallId) -> bool {
        ball_id > 0 && self.view().active_drones_in_space.contains_key(&ball_id)
    }

    /// Returns a snapshot of all published active drones in this solar system.
    pub fn active_drones_snapshot(&self) -> HashMap<BallId, DroneSpaceState> {
        self.view().active_drones_in_space.clone()
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

    /// Executes an atomic closure over this system's active drones, mining states, and cooldown timers.
    pub async fn mutate_active_drones<R: Send + 'static>(
        &self,
        op: impl FnOnce(
                &mut HashMap<BallId, DroneSpaceState>,
                &mut HashMap<BallId, DroneMiningState>,
                &mut HashMap<BallId, u64>,
            ) -> (bool, R)
            + Send
            + 'static,
    ) -> Result<R, String> {
        let (res_tx, mut res_rx) = tokio::sync::oneshot::channel::<R>();
        let boxed: ActiveDroneMutOp = Box::new(move |drones, miners, timers| {
            let (dirty, out) = op(drones, miners, timers);
            let _ = res_tx.send(out);
            dirty
        });
        call_actor_result_with_deadline(self, "MutateActiveDrones", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::MutateActiveDrones { op: boxed, reply }
        })
        .await?;
        res_rx
            .try_recv()
            .map_err(|_| format!("system {} active-drone mutation dropped result", self.solar_system_id))
    }

    /// Synchronous bridge for [`Self::mutate_active_drones`].
    pub fn mutate_active_drones_sync<R: Send + 'static>(
        &self,
        op: impl FnOnce(
                &mut HashMap<BallId, DroneSpaceState>,
                &mut HashMap<BallId, DroneMiningState>,
                &mut HashMap<BallId, u64>,
            ) -> (bool, R)
            + Send
            + 'static,
    ) -> Result<R, String> {
        let (res_tx, mut res_rx) = tokio::sync::oneshot::channel::<R>();
        let boxed: ActiveDroneMutOp = Box::new(move |drones, miners, timers| {
            let (dirty, out) = op(drones, miners, timers);
            let _ = res_tx.send(out);
            dirty
        });
        dispatch_actor_sync_with_deadline(
            self,
            "MutateActiveDrones",
            ACTOR_CMD_TIMEOUT,
            boxed,
            |op, reply| SystemCommand::MutateActiveDrones { op, reply },
            mutate_active_drones_in_actor,
        )?;
        res_rx
            .try_recv()
            .map_err(|_| format!("system {} active-drone mutation dropped result", self.solar_system_id))
    }

    /// Executes an atomic closure over this system's active drones, mining states, cooldown timers,
    /// and transient `scoop_claimed_drones` map inside [`SystemActor`].
    pub async fn mutate_active_drones_with_claims<R: Send + 'static>(
        &self,
        op: impl FnOnce(
                &mut HashMap<BallId, DroneSpaceState>,
                &mut HashMap<BallId, DroneMiningState>,
                &mut HashMap<BallId, u64>,
                &mut HashMap<BallId, Option<PendingScoopAbandon>>,
            ) -> (bool, R)
            + Send
            + 'static,
    ) -> Result<R, String> {
        let (res_tx, mut res_rx) = tokio::sync::oneshot::channel::<R>();
        let boxed: ActiveDroneMutWithClaimsOp = Box::new(move |drones, miners, timers, claimed| {
            let (dirty, out) = op(drones, miners, timers, claimed);
            let _ = res_tx.send(out);
            dirty
        });
        call_actor_result_with_deadline(
            self,
            "MutateActiveDronesWithClaims",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::MutateActiveDronesWithClaims { op: boxed, reply },
        )
        .await?;
        res_rx
            .try_recv()
            .map_err(|_| format!("system {} active-drone mutation dropped result", self.solar_system_id))
    }

    /// Synchronous bridge for [`Self::mutate_active_drones_with_claims`].
    pub fn mutate_active_drones_with_claims_sync<R: Send + 'static>(
        &self,
        op: impl FnOnce(
                &mut HashMap<BallId, DroneSpaceState>,
                &mut HashMap<BallId, DroneMiningState>,
                &mut HashMap<BallId, u64>,
                &mut HashMap<BallId, Option<PendingScoopAbandon>>,
            ) -> (bool, R)
            + Send
            + 'static,
    ) -> Result<R, String> {
        let (res_tx, mut res_rx) = tokio::sync::oneshot::channel::<R>();
        let boxed: ActiveDroneMutWithClaimsOp = Box::new(move |drones, miners, timers, claimed| {
            let (dirty, out) = op(drones, miners, timers, claimed);
            let _ = res_tx.send(out);
            dirty
        });
        dispatch_actor_sync_with_deadline(
            self,
            "MutateActiveDronesWithClaims",
            ACTOR_CMD_TIMEOUT,
            boxed,
            |op, reply| SystemCommand::MutateActiveDronesWithClaims { op, reply },
            mutate_active_drones_with_claims_in_actor,
        )?;
        res_rx
            .try_recv()
            .map_err(|_| format!("system {} active-drone mutation dropped result", self.solar_system_id))
    }

    /// Removes and returns `ball_id` from `active_drones_in_space` inside this system actor,
    /// also clearing any transient `scoop_claimed_drones` claim on `ball_id`.
    pub async fn remove_active_drone(&self, ball_id: BallId) -> Result<Option<DroneSpaceState>, String> {
        match self
            .drone_scoop_claim_op(ball_id, DroneScoopClaimAction::ConsumeScoop)
            .await?
        {
            DroneScoopClaimOutcome::Consumed(removed) => Ok(removed),
            _ => Ok(None),
        }
    }

    /// Synchronous bridge for [`Self::remove_active_drone`].
    pub fn remove_active_drone_sync(&self, ball_id: BallId) -> Result<Option<DroneSpaceState>, String> {
        match dispatch_actor_sync_with_deadline(
            self,
            "ScoopDroneClaimOp",
            ACTOR_CMD_TIMEOUT,
            (ball_id, DroneScoopClaimAction::ConsumeScoop),
            |(ball_id, action), reply| SystemCommand::ScoopDroneClaimOp {
                ball_id,
                action,
                reply,
            },
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
            let view = handle.view();
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
            let view = handle.view();
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
                h.view()
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
            if handle.has_active_drone(ball_id) {
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
            if !handle.view().active_drones_in_space.is_empty() {
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
            let rt = self.headless_test_rt.get_or_init(|| {
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
