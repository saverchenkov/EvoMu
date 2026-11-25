//! Per-system durable module reactivation cooldowns, `CooldownPersistGate` serialization,
//! SQLite checkpoint migration/boot hydration, and sim-clock reload deadlines (`#1541`).

#![forbid(unsafe_code)]

use super::celestials::{clamp_tidi_factor_bits, BallparkHandle};
use super::{BallOpsCtx, SystemActorState, SystemRegistry, SystemView};
use eve_db::lock_rank::{
    held_lock_ranks, RankedMutex, RankedRwLock, RANK_ACTOR_STATE, RANK_BALLPARK, RANK_WORLD_INNER,
};
use eve_db::sqlite_store::system_checkpoint::{
    current_unix_secs, ensure_system_checkpoint_schema, load_all_checkpoints_for_kind,
    load_system_checkpoints, replace_system_checkpoints_for_kind, CLUSTER_SYSTEM_ID,
    KIND_MODULE_COOLDOWNS,
};
use eve_db::{WorldRepository, WorldStateSnapshot};
use eve_sde::SolarSystemId;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Arc;

/// Cooldown record for a module's reactivation delay (`feat:dogma.reactivation-delay`, `DG-4`).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModuleCooldownEntry {
    pub recorded_unix_f64: f64,
    pub delay_s: f64,
    pub solar_system_id: Option<u32>,
    pub recorded_sys_sim_s: f64,
}

/// Sim-clock bounds recorded when a module begins reloading in space (`#1081`, `#1541`).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModuleReloadSimDeadline {
    pub sim_start_s: f64,
    pub sim_deadline_s: f64,
}

/// Transient in-space EWAR state owned per solar system by [`SystemActor`] (`#1541`).
///
/// Explicitly `CrashLost`: cleared cleanly when the owning system actor stops or evicts.

/// `RANK_SQLITE_CONN` (`41`), and strictly above `RANK_ACTOR_STATE` (`21`).
pub(in crate::system_actor) const RANK_COOLDOWN_PERSIST: u16 = 36;

#[derive(Debug, Default)]
struct CooldownPersistGateState {
    last_persisted: Option<(u64, u64)>,
}

/// Per-system SQLite persistence serializer with monotonic `(epoch, seq)` guard (`#1541`).
#[derive(Debug)]
pub(crate) struct CooldownPersistGate {
    next_epoch: std::sync::atomic::AtomicU64,
    inner: RankedMutex<CooldownPersistGateState, RANK_COOLDOWN_PERSIST>,
}

impl Default for CooldownPersistGate {
    fn default() -> Self {
        Self {
            next_epoch: std::sync::atomic::AtomicU64::new(1),
            inner: RankedMutex::new(CooldownPersistGateState::default()),
        }
    }
}

impl CooldownPersistGate {
    pub fn current_epoch(&self) -> u64 {
        self.next_epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn bump_epoch(&self) -> u64 {
        self.next_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    pub fn try_advance_cold_epoch(&self, expected: u64) -> bool {
        self.next_epoch
            .compare_exchange(
                expected,
                expected + 1,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
    }
}

/// Per-system durable module reactivation cooldowns and sim-clock reload deadlines owned
/// by [`SystemActor`] (`#1541`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActorModuleCooldownState {
    pub reactivation_cooldowns: HashMap<(i64, i64, u32), ModuleCooldownEntry>,
    pub reload_sim_deadlines: HashMap<i64, ModuleReloadSimDeadline>,
    /// True when the system booted under `!db_read_safe` so pre-existing cold SQLite cooldowns
    /// must be merged rather than wiped on first persist (`#1541`).
    pub boot_db_pending_merge: bool,
    pub pending_db_clear_all: bool,
    pub pending_db_cleared_ships: HashSet<i64>,
    pub pending_db_removed_keys: HashSet<(i64, i64, u32)>,
}

/// Projected target metadata captured inside `SystemActor` right before a module's EWAR
/// projection is removed so notifications can be emitted outside actor locks.

pub type ModuleCooldownCheckpointRow = (String, ((i64, i64, u32), ModuleCooldownEntry));

#[derive(Clone)]
pub struct ModuleCooldownPersistPayload {
    pub db_opt: Option<Arc<WorldRepository>>,
    pub epoch: u64,
    pub seq: u64,
    pub entries: HashMap<(i64, i64, u32), ModuleCooldownEntry>,
    pub boot_db_pending_merge: bool,
    pub pending_db_clear_all: bool,
    pub pending_db_cleared_ships: HashSet<i64>,
    pub pending_db_removed_keys: HashSet<(i64, i64, u32)>,
    pub cur_sys_sim_s: f64,
    pub tidi_factor: f64,
    pub has_tidi_history: bool,
}

impl std::fmt::Debug for ModuleCooldownPersistPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleCooldownPersistPayload")
            .field("has_db", &self.db_opt.is_some())
            .field("epoch", &self.epoch)
            .field("seq", &self.seq)
            .field("entries_len", &self.entries.len())
            .field("boot_db_pending_merge", &self.boot_db_pending_merge)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub(in crate::system_actor) struct RecordModuleCooldownReq {
    pub ship_id: i64,
    pub module_id: i64,
    pub module_type_id: u32,
    pub now_unix_f64: f64,
    pub delay_s: f64,
    pub tidi_factor: f64,
    pub has_tidi_history: bool,
}

/// Pure remaining-cooldown calculation accounting for per-system TiDi dilation (`#1036`, `#1541`).
pub(crate) fn remaining_cooldown_secs_with_tidi(
    entry: &ModuleCooldownEntry,
    now_unix_f64: f64,
    sys_opt: Option<u32>,
    cur_sys_sim_s: f64,
    tidi_factor: f64,
    has_tidi_history: bool,
) -> f64 {
    // If `recorded_unix_f64` sits in the future relative to `now_unix_f64` (e.g. persisted from
    // an earlier `/advancetime` session before server restart reset the virtual clock), treat
    // the stale entry as expired rather than freezing activation until wall clock catches up (#1036).
    if !entry.delay_s.is_finite()
        || !entry.recorded_unix_f64.is_finite()
        || !now_unix_f64.is_finite()
        || entry.delay_s <= 0.0
        || now_unix_f64 + 1e-6 < entry.recorded_unix_f64
    {
        return 0.0;
    }
    let wall_elapsed = (now_unix_f64 - entry.recorded_unix_f64).max(0.0);
    let same_sys = entry.solar_system_id.is_some()
        && entry.solar_system_id == sys_opt
        && cur_sys_sim_s.is_finite()
        && entry.recorded_sys_sim_s.is_finite();
    let has_sim = same_sys && cur_sys_sim_s >= entry.recorded_sys_sim_s;
    let sim_elapsed = if has_sim {
        (cur_sys_sim_s - entry.recorded_sys_sim_s).max(0.0)
    } else {
        0.0
    };
    let tidi_clamped = tidi_factor.clamp(0.10, 1.0);
    let elapsed = if has_sim && (has_tidi_history || tidi_clamped < 0.999_999) {
        if cur_sys_sim_s > 0.0 || entry.recorded_sys_sim_s > 0.0 {
            sim_elapsed
        } else {
            (wall_elapsed * tidi_clamped).max(sim_elapsed)
        }
    } else if has_tidi_history || tidi_clamped < 0.999_999 {
        // When a TiDi system reboots/rehydrates (`cur_sys_sim_s == 0.0` or sim clock reset below
        // pre-crash `recorded_sys_sim_s`), wall time since `recorded_unix_f64` still scales by TiDi.
        (wall_elapsed * tidi_clamped).max(sim_elapsed)
    } else {
        wall_elapsed.max(sim_elapsed)
    };
    (entry.delay_s - elapsed).max(0.0)
}

#[inline]
pub(crate) fn module_cooldown_checkpoint_key(
    ship_id: i64,
    module_id: i64,
    module_type_id: u32,
) -> String {
    format!("{ship_id}|{module_id}|{module_type_id}")
}

pub(crate) fn serialize_module_cooldown_rows(
    cooldowns: &HashMap<(i64, i64, u32), ModuleCooldownEntry>,
) -> Vec<ModuleCooldownCheckpointRow> {
    let mut sorted: Vec<_> = cooldowns.iter().map(|(&k, &v)| (k, v)).collect();
    sorted.sort_by_key(|(k, _)| *k);
    sorted
        .into_iter()
        .map(|(k @ (ship_id, mod_id, mod_tid), entry)| {
            (
                module_cooldown_checkpoint_key(ship_id, mod_id, mod_tid),
                (k, entry),
            )
        })
        .collect()
}

fn can_write_sqlite_safely() -> bool {
    !held_lock_ranks().iter().any(|&r| r >= RANK_WORLD_INNER)
}

/// Post-unlock DB writer replacing `KIND_MODULE_COOLDOWNS` rows for a single `solar_system_id`.
pub(crate) fn persist_system_module_cooldowns_in_db(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
    rows: &[ModuleCooldownCheckpointRow],
) -> bool {
    if solar_system_id == 0 || !can_write_sqlite_safely() {
        return false;
    }
    let now_unix = current_unix_secs();
    db.with_sqlite_conn(|conn| {
        ensure_system_checkpoint_schema(conn)?;
        replace_system_checkpoints_for_kind(
            conn,
            solar_system_id,
            KIND_MODULE_COOLDOWNS,
            rows,
            now_unix,
        )
    })
    .is_ok()
}

pub(crate) fn load_cold_system_module_cooldowns_from_db(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
) -> HashMap<(i64, i64, u32), ModuleCooldownEntry> {
    let Ok(rows) = db.with_sqlite_conn(|conn| {
        ensure_system_checkpoint_schema(conn)?;
        load_system_checkpoints::<((i64, i64, u32), ModuleCooldownEntry)>(
            conn,
            solar_system_id,
            KIND_MODULE_COOLDOWNS,
        )
    }) else {
        return HashMap::new();
    };
    rows.into_iter().map(|(_, (k, v))| (k, v)).collect()
}

pub(crate) fn load_active_system_module_cooldowns_for_boot(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
    tidi_factor: f64,
    has_tidi_history: bool,
    cur_sys_sim_s: f64,
) -> HashMap<(i64, i64, u32), ModuleCooldownEntry> {
    if solar_system_id == 0 {
        return HashMap::new();
    }
    let now_unix_f64 = db.clock().now_unix_secs() as f64;
    let raw = load_cold_system_module_cooldowns_from_db(db, solar_system_id);
    let mut active = HashMap::new();
    for (key, entry) in raw {
        let rem = remaining_cooldown_secs_with_tidi(
            &entry,
            now_unix_f64,
            Some(solar_system_id),
            cur_sys_sim_s,
            tidi_factor,
            has_tidi_history,
        );
        if rem > 1e-6 {
            active.insert(
                key,
                ModuleCooldownEntry {
                    recorded_unix_f64: now_unix_f64,
                    delay_s: rem,
                    solar_system_id: Some(solar_system_id),
                    recorded_sys_sim_s: cur_sys_sim_s,
                },
            );
        }
    }
    active
}

/// Removes expired (and future-clock-stale `#1036`) module reactivation cooldowns in memory.
pub(crate) fn prune_expired_module_cooldowns_in_state(
    cooldowns: &mut ActorModuleCooldownState,
    solar_system_id: SolarSystemId,
    now_unix_f64: f64,
    cur_sys_sim_s: f64,
    tidi_factor: f64,
    has_tidi_history: bool,
) -> bool {
    let before = cooldowns.reactivation_cooldowns.len();
    cooldowns.reactivation_cooldowns.retain(|_, entry| {
        remaining_cooldown_secs_with_tidi(
            entry,
            now_unix_f64,
            Some(solar_system_id),
            cur_sys_sim_s,
            tidi_factor,
            has_tidi_history,
        ) > 1e-6
    });
    cooldowns.reactivation_cooldowns.len() != before
}

/// Rebases all active module reactivation cooldowns to `(now_unix_f64, remaining_s, 0.0)`
/// when a system actor stops or evicts so cold/rebooted lookups never freeze on stale `sim_s`.
pub(crate) fn rebase_module_cooldowns_on_stop_in_state(
    cooldowns: &mut ActorModuleCooldownState,
    solar_system_id: SolarSystemId,
    now_unix_f64: f64,
    cur_sys_sim_s: f64,
    tidi_factor: f64,
    has_tidi_history: bool,
) -> bool {
    let mut rebased = HashMap::new();
    let mut changed = false;
    for (&key, entry) in &cooldowns.reactivation_cooldowns {
        let rem = remaining_cooldown_secs_with_tidi(
            entry,
            now_unix_f64,
            Some(solar_system_id),
            cur_sys_sim_s,
            tidi_factor,
            has_tidi_history,
        );
        if rem > 1e-6 {
            let next = ModuleCooldownEntry {
                recorded_unix_f64: now_unix_f64,
                delay_s: rem,
                solar_system_id: Some(solar_system_id),
                recorded_sys_sim_s: 0.0,
            };
            if next != *entry {
                changed = true;
            }
            rebased.insert(key, next);
        } else {
            changed = true;
        }
    }
    cooldowns.reactivation_cooldowns = rebased;
    changed
}

pub(crate) fn build_persist_payload_from_actor_state(
    st: &SystemActorState,
    tidi_factor: f64,
    has_tidi_history: bool,
) -> ModuleCooldownPersistPayload {
    ModuleCooldownPersistPayload {
        db_opt: st.db_opt.clone(),
        epoch: st.module_cooldown_persist_epoch,
        seq: st.module_cooldown_checkpoint_seq,
        entries: st.module_cooldowns.reactivation_cooldowns.clone(),
        boot_db_pending_merge: st.module_cooldowns.boot_db_pending_merge,
        pending_db_clear_all: st.module_cooldowns.pending_db_clear_all,
        pending_db_cleared_ships: st.module_cooldowns.pending_db_cleared_ships.clone(),
        pending_db_removed_keys: st.module_cooldowns.pending_db_removed_keys.clone(),
        cur_sys_sim_s: st.sim_elapsed_secs,
        tidi_factor,
        has_tidi_history: has_tidi_history || st.has_tidi_history || tidi_factor < 0.999_999,
    }
}

/// Serializes, sequence-guards, merges pending pre-boot cold rows (if any), and commits a
/// per-system module cooldown checkpoint cleanly outside actor locks (`#1541`).
pub(crate) fn persist_cooldown_payload_with_gate(
    solar_system_id: SolarSystemId,
    gate: &CooldownPersistGate,
    ballpark_opt: Option<&BallparkHandle>,
    actor_state_opt: Option<&RankedRwLock<SystemActorState, RANK_ACTOR_STATE>>,
    payload: ModuleCooldownPersistPayload,
) -> bool {
    if solar_system_id == 0 || !can_write_sqlite_safely() {
        return false;
    }
    let Some(ref db) = payload.db_opt else {
        return false;
    };
    let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
    if gate_guard
        .last_persisted
        .is_some_and(|prev| (payload.epoch, payload.seq) <= prev)
    {
        return false;
    }
    let mut final_entries = payload.entries;
    let mut merged_from_db = HashMap::new();
    let now_unix_f64 = db.clock().now_unix_secs() as f64;
    if payload.boot_db_pending_merge && !payload.pending_db_clear_all {
        let db_map = load_cold_system_module_cooldowns_from_db(db, solar_system_id);
        for (k @ (sid, _, _), entry) in db_map {
            if payload.pending_db_cleared_ships.contains(&sid)
                || payload.pending_db_removed_keys.contains(&k)
            {
                continue;
            }
            let db_rem = remaining_cooldown_secs_with_tidi(
                &entry,
                now_unix_f64,
                Some(solar_system_id),
                0.0,
                payload.tidi_factor,
                payload.has_tidi_history,
            );
            if db_rem <= 1e-6 {
                continue;
            }
            let normalized = ModuleCooldownEntry {
                recorded_unix_f64: now_unix_f64,
                delay_s: db_rem,
                solar_system_id: Some(solar_system_id),
                recorded_sys_sim_s: 0.0,
            };
            let take_db = match final_entries.get(&k) {
                Some(live_entry) => {
                    let live_rem = remaining_cooldown_secs_with_tidi(
                        live_entry,
                        now_unix_f64,
                        Some(solar_system_id),
                        payload.cur_sys_sim_s,
                        payload.tidi_factor,
                        payload.has_tidi_history,
                    );
                    db_rem > live_rem + 1e-6
                }
                None => true,
            };
            if take_db {
                merged_from_db.insert(k, normalized);
                final_entries.insert(k, normalized);
            }
        }
    }

    let rows = serialize_module_cooldown_rows(&final_entries);
    let committed = persist_system_module_cooldowns_in_db(db, solar_system_id, &rows);
    if committed {
        gate_guard.last_persisted = Some((payload.epoch, payload.seq));
    }
    drop(gate_guard);

    if !committed {
        return false;
    }
    let mut view_changed = false;
    if let Some(actor_state) = actor_state_opt {
        if !held_lock_ranks().iter().any(|&r| r >= RANK_ACTOR_STATE) {
            let mut st = actor_state.write().unwrap_or_else(|e| e.into_inner());
            if st.module_cooldown_persist_epoch == payload.epoch {
                let is_latest_seq = st.module_cooldown_checkpoint_seq == payload.seq;
                if payload.boot_db_pending_merge && !st.module_cooldowns.pending_db_clear_all {
                    let cur_sim_s = st.sim_elapsed_secs;
                    for (k, merged_entry) in merged_from_db {
                        if st.module_cooldowns.pending_db_cleared_ships.contains(&k.0)
                            || st.module_cooldowns.pending_db_removed_keys.contains(&k)
                        {
                            continue;
                        }
                        if !is_latest_seq && st.module_cooldowns.reactivation_cooldowns.contains_key(&k)
                        {
                            continue;
                        }
                        st.module_cooldowns.reactivation_cooldowns.insert(
                            k,
                            ModuleCooldownEntry {
                                recorded_sys_sim_s: cur_sim_s,
                                ..merged_entry
                            },
                        );
                        view_changed = true;
                    }
                    if is_latest_seq {
                        if st.module_cooldowns.boot_db_pending_merge {
                            st.module_cooldowns.boot_db_pending_merge = false;
                            view_changed = true;
                        }
                        st.module_cooldowns.pending_db_cleared_ships.clear();
                        st.module_cooldowns.pending_db_removed_keys.clear();
                    }
                }
                if is_latest_seq {
                    st.module_cooldowns.pending_db_clear_all = false;
                    st.module_cooldown_checkpoint_dirty = false;
                }
            }
        }
    }
    if view_changed {
        if let Some(bp) = ballpark_opt {
            publish_ballpark_view(bp);
        }
    }
    true
}

pub(in crate::system_actor) fn publish_ballpark_view(bp: &BallparkHandle) {
    if held_lock_ranks().iter().any(|&r| r >= RANK_BALLPARK) {
        return;
    }
    let guard = bp.inner.read().unwrap_or_else(|e| e.into_inner());
    let tidi = clamp_tidi_factor_bits(&bp.tidi_factor_bits);
    let prev = { bp.view_tx.borrow().clone() };
    let actor_st_arc = bp
        .actor_state
        .get()
        .and_then(|weak| weak.upgrade())
        .filter(|_| !held_lock_ranks().iter().any(|&r| r >= RANK_ACTOR_STATE));
    let actor_st_guard = actor_st_arc
        .as_ref()
        .map(|st_lock| st_lock.read().unwrap_or_else(|e| e.into_inner()));
    let next = SystemView::from_ballpark(
        &guard,
        actor_st_guard.as_deref(),
        tidi,
        Some(prev.as_ref()),
    );
    drop(actor_st_guard);
    drop(guard);
    let _ = bp.view_tx.send(Arc::new(next));
}

pub(crate) fn find_all_cold_module_cooldown_entries_in_db(
    db: &WorldRepository,
    key: (i64, i64, u32),
) -> Vec<(u32, ModuleCooldownEntry)> {
    let Ok(all_rows) = db.with_sqlite_conn(|conn| {
        ensure_system_checkpoint_schema(conn)?;
        load_all_checkpoints_for_kind::<((i64, i64, u32), ModuleCooldownEntry)>(
            conn,
            KIND_MODULE_COOLDOWNS,
        )
    }) else {
        return Vec::new();
    };
    all_rows
        .into_iter()
        .filter_map(|(sys_id, _, (k, entry))| (k == key).then_some((sys_id, entry)))
        .collect()
}

pub(crate) fn find_cold_module_cooldown_entry_in_db(
    db: &WorldRepository,
    preferred_sys_opt: Option<u32>,
    key: (i64, i64, u32),
) -> Option<(u32, ModuleCooldownEntry)> {
    if let Some(sys_id) = preferred_sys_opt {
        if sys_id > 0 {
            let map = load_cold_system_module_cooldowns_from_db(db, sys_id);
            if let Some(&entry) = map.get(&key) {
                return Some((sys_id, entry));
            }
        }
    }
    find_all_cold_module_cooldown_entries_in_db(db, key)
        .into_iter()
        .next()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_cold_system_module_cooldown_serialized(
    registry: &SystemRegistry,
    db: &WorldRepository,
    gate: &CooldownPersistGate,
    solar_system_id: SolarSystemId,
    ship_id: i64,
    module_id: i64,
    module_type_id: u32,
    now_unix_f64: f64,
    delay_s: f64,
    tidi_factor: f64,
    has_tidi_history: bool,
) {
    if solar_system_id == 0
        || !delay_s.is_finite()
        || delay_s <= 0.0
        || !can_write_sqlite_safely()
        || registry.is_system_non_cold(solar_system_id)
    {
        return;
    }
    let seen_epoch = gate.current_epoch();
    let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
    if gate.current_epoch() != seen_epoch || registry.is_system_non_cold_try(solar_system_id) {
        return;
    }
    let mut map = load_cold_system_module_cooldowns_from_db(db, solar_system_id);
    let key = (ship_id, module_id, module_type_id);
    let existing_rem = map
        .get(&key)
        .map(|existing| {
            remaining_cooldown_secs_with_tidi(
                existing,
                now_unix_f64,
                Some(solar_system_id),
                0.0,
                tidi_factor,
                has_tidi_history,
            )
        })
        .unwrap_or(0.0);
    map.retain(|_, e| {
        remaining_cooldown_secs_with_tidi(
            e,
            now_unix_f64,
            Some(solar_system_id),
            0.0,
            tidi_factor,
            has_tidi_history,
        ) > 1e-6
    });
    map.insert(
        key,
        ModuleCooldownEntry {
            recorded_unix_f64: now_unix_f64,
            delay_s: existing_rem.max(delay_s),
            solar_system_id: Some(solar_system_id),
            recorded_sys_sim_s: 0.0,
        },
    );
    let rows = serialize_module_cooldown_rows(&map);
    if persist_system_module_cooldowns_in_db(db, solar_system_id, &rows)
        && gate.try_advance_cold_epoch(seen_epoch)
    {
        gate_guard.last_persisted = Some((seen_epoch + 1, 0));
    }
}

pub(crate) fn seed_cold_system_module_cooldown_serialized(
    registry: &SystemRegistry,
    db: &WorldRepository,
    gate: &CooldownPersistGate,
    solar_system_id: SolarSystemId,
    key: (i64, i64, u32),
    entry: ModuleCooldownEntry,
) {
    if solar_system_id == 0
        || !can_write_sqlite_safely()
        || registry.is_system_non_cold(solar_system_id)
    {
        return;
    }
    let seen_epoch = gate.current_epoch();
    let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
    if gate.current_epoch() != seen_epoch || registry.is_system_non_cold_try(solar_system_id) {
        return;
    }
    let mut map = load_cold_system_module_cooldowns_from_db(db, solar_system_id);
    map.insert(key, entry);
    let rows = serialize_module_cooldown_rows(&map);
    if persist_system_module_cooldowns_in_db(db, solar_system_id, &rows)
        && gate.try_advance_cold_epoch(seen_epoch)
    {
        gate_guard.last_persisted = Some((seen_epoch + 1, 0));
    }
}

pub(crate) fn remove_cold_system_module_cooldown_serialized(
    registry: &SystemRegistry,
    db: &WorldRepository,
    gate: &CooldownPersistGate,
    solar_system_id: SolarSystemId,
    key: (i64, i64, u32),
) -> bool {
    if !can_write_sqlite_safely() || registry.is_system_non_cold(solar_system_id) {
        return false;
    }
    let seen_epoch = gate.current_epoch();
    let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
    if gate.current_epoch() != seen_epoch || registry.is_system_non_cold_try(solar_system_id) {
        return false;
    }
    let mut map = load_cold_system_module_cooldowns_from_db(db, solar_system_id);
    if map.remove(&key).is_none() {
        return false;
    }
    let rows = serialize_module_cooldown_rows(&map);
    let now_unix = current_unix_secs();
    let ok = db
        .with_sqlite_conn(|conn| {
            ensure_system_checkpoint_schema(conn)?;
            replace_system_checkpoints_for_kind(
                conn,
                solar_system_id,
                KIND_MODULE_COOLDOWNS,
                &rows,
                now_unix,
            )
        })
        .is_ok();
    if ok && gate.try_advance_cold_epoch(seen_epoch) {
        gate_guard.last_persisted = Some((seen_epoch + 1, 0));
    }
    ok
}

fn resolve_legacy_cooldown_system_id(
    snap: &WorldStateSnapshot,
    ship_id: i64,
    entry: &ModuleCooldownEntry,
) -> Option<u32> {
    if let Some(sid) = entry.solar_system_id.filter(|&s| s > 0) {
        return Some(sid);
    }
    if let Some(sid) = snap
        .characters
        .values()
        .find(|c| c.active_ship_id == ship_id && c.solar_system_id > 0)
        .map(|c| c.solar_system_id)
    {
        return Some(sid);
    }
    snap.items.get(&ship_id).and_then(|it| {
        if (30_000_000..=33_000_000).contains(&it.location_id) {
            Some(it.location_id as u32)
        } else {
            snap.structures.get(&it.location_id).map(|st| st.solar_system_id)
        }
    })
}

/// Prunes expired/future-stale `KIND_MODULE_COOLDOWNS` rows on startup and performs a clean
/// one-way migration of any legacy `CLUSTER_SYSTEM_ID` rows into their owning solar systems.
pub(crate) fn migrate_and_prune_module_cooldowns_in_db<F>(
    db: &WorldRepository,
    now_unix_f64: f64,
    tidi_resolver: F,
) where
    F: Fn(u32) -> (f64, bool),
{
    if !can_write_sqlite_safely() {
        return;
    }
    let Ok(all_rows) = db.with_sqlite_conn(|conn| {
        ensure_system_checkpoint_schema(conn)?;
        load_all_checkpoints_for_kind::<((i64, i64, u32), ModuleCooldownEntry)>(
            conn,
            KIND_MODULE_COOLDOWNS,
        )
    }) else {
        return;
    };
    if all_rows.is_empty() {
        return;
    }

    let mut per_system: BTreeMap<u32, HashMap<(i64, i64, u32), ModuleCooldownEntry>> =
        BTreeMap::new();
    let mut dirty_systems: Vec<u32> = Vec::new();
    let mut unresolved_legacy: Vec<((i64, i64, u32), ModuleCooldownEntry)> = Vec::new();
    let mut had_cluster_rows = false;

    db.with_snapshot(|snap| {
        for (row_sys_id, _, (key @ (ship_id, _, _), entry)) in all_rows {
            if row_sys_id == CLUSTER_SYSTEM_ID {
                had_cluster_rows = true;
                let resolved_opt = resolve_legacy_cooldown_system_id(snap, ship_id, &entry);
                let check_sys = resolved_opt;
                let (tidi_factor, has_tidi_history) = check_sys
                    .map(&tidi_resolver)
                    .unwrap_or((1.0, false));
                let rem = remaining_cooldown_secs_with_tidi(
                    &entry,
                    now_unix_f64,
                    check_sys,
                    0.0,
                    tidi_factor,
                    has_tidi_history,
                );
                if rem <= 1e-6 {
                    continue;
                }
                if let Some(target_sys) = resolved_opt {
                    let normalized = ModuleCooldownEntry {
                        solar_system_id: Some(target_sys),
                        ..entry
                    };
                    per_system
                        .entry(target_sys)
                        .or_default()
                        .insert(key, normalized);
                    dirty_systems.push(target_sys);
                } else {
                    unresolved_legacy.push((key, entry));
                }
                continue;
            }

            let normalized = if entry.solar_system_id.is_none() {
                dirty_systems.push(row_sys_id);
                ModuleCooldownEntry {
                    solar_system_id: Some(row_sys_id),
                    ..entry
                }
            } else {
                entry
            };
            let (tidi_factor, has_tidi_history) = tidi_resolver(row_sys_id);
            let rem = remaining_cooldown_secs_with_tidi(
                &normalized,
                now_unix_f64,
                Some(row_sys_id),
                0.0,
                tidi_factor,
                has_tidi_history,
            );
            if rem <= 1e-6 {
                dirty_systems.push(row_sys_id);
                continue;
            }
            per_system
                .entry(row_sys_id)
                .or_default()
                .insert(key, normalized);
        }
    });

    if had_cluster_rows {
        dirty_systems.push(CLUSTER_SYSTEM_ID);
    }
    if dirty_systems.is_empty() {
        return;
    }
    dirty_systems.sort_unstable();
    dirty_systems.dedup();
    let now_unix = current_unix_secs();
    let _ = db.with_sqlite_conn(|conn| {
        for sys_id in dirty_systems {
            if sys_id == CLUSTER_SYSTEM_ID {
                let mut rows: Vec<ModuleCooldownCheckpointRow> = Vec::new();
                for (k @ (ship_id, mod_id, mod_tid), entry) in &unresolved_legacy {
                    rows.push((
                        module_cooldown_checkpoint_key(*ship_id, *mod_id, *mod_tid),
                        (*k, *entry),
                    ));
                }
                replace_system_checkpoints_for_kind(
                    conn,
                    CLUSTER_SYSTEM_ID,
                    KIND_MODULE_COOLDOWNS,
                    &rows,
                    now_unix,
                )?;
            } else {
                let rows = per_system
                    .get(&sys_id)
                    .map(serialize_module_cooldown_rows)
                    .unwrap_or_default();
                replace_system_checkpoints_for_kind(
                    conn,
                    sys_id,
                    KIND_MODULE_COOLDOWNS,
                    &rows,
                    now_unix,
                )?;
            }
        }
        Ok::<(), String>(())
    });
}

/// One-way importer for legacy monolith JSON cooldowns on boot when no SQLite rows existed yet.
pub(crate) fn import_legacy_monolith_module_cooldowns_in_db<F>(
    db: &WorldRepository,
    now_unix_f64: f64,
    tidi_resolver: F,
    legacy_entries: Vec<((i64, i64, u32), ModuleCooldownEntry)>,
) where
    F: Fn(u32) -> (f64, bool),
{
    if legacy_entries.is_empty() || !can_write_sqlite_safely() {
        return;
    }
    let mut per_system: BTreeMap<u32, HashMap<(i64, i64, u32), ModuleCooldownEntry>> =
        BTreeMap::new();
    db.with_snapshot(|snap| {
        for (key @ (ship_id, _, _), entry) in legacy_entries {
            let sys_opt = resolve_legacy_cooldown_system_id(snap, ship_id, &entry);
            let (tidi_factor, has_tidi_history) =
                sys_opt.map(&tidi_resolver).unwrap_or((1.0, false));
            if remaining_cooldown_secs_with_tidi(
                &entry,
                now_unix_f64,
                sys_opt,
                0.0,
                tidi_factor,
                has_tidi_history,
            ) <= 1e-6
            {
                continue;
            }
            let target_sys = sys_opt.unwrap_or(CLUSTER_SYSTEM_ID);
            let normalized = if entry.solar_system_id.is_none() && target_sys > 0 {
                ModuleCooldownEntry {
                    solar_system_id: Some(target_sys),
                    ..entry
                }
            } else {
                entry
            };
            per_system
                .entry(target_sys)
                .or_default()
                .insert(key, normalized);
        }
    });
    if per_system.is_empty() {
        return;
    }
    let now_unix = current_unix_secs();
    let _ = db.with_sqlite_conn(|conn| {
        ensure_system_checkpoint_schema(conn)?;
        for (sys_id, map) in per_system {
            let rows = serialize_module_cooldown_rows(&map);
            replace_system_checkpoints_for_kind(
                conn,
                sys_id,
                KIND_MODULE_COOLDOWNS,
                &rows,
                now_unix,
            )?;
        }
        Ok::<(), String>(())
    });
}


#[allow(clippy::too_many_arguments)]
pub(in crate::system_actor) fn record_module_cooldown_in_actor_state(
    solar_system_id: SolarSystemId,
    actor_st: &mut SystemActorState,
    ship_id: i64,
    module_id: i64,
    module_type_id: u32,
    now_unix_f64: f64,
    delay_s: f64,
    tidi_factor: f64,
    has_tidi_history: bool,
) -> Option<ModuleCooldownPersistPayload> {
    if !delay_s.is_finite() || delay_s <= 0.0 || solar_system_id == 0 {
        return None;
    }
    let cur_sys_sim_s = actor_st.sim_elapsed_secs;
    let _ = prune_expired_module_cooldowns_in_state(
        &mut actor_st.module_cooldowns,
        solar_system_id,
        now_unix_f64,
        cur_sys_sim_s,
        tidi_factor,
        has_tidi_history,
    );
    let key = (ship_id, module_id, module_type_id);
    let effective_delay = actor_st
        .module_cooldowns
        .reactivation_cooldowns
        .get(&key)
        .map(|e| {
            remaining_cooldown_secs_with_tidi(
                e,
                now_unix_f64,
                Some(solar_system_id),
                cur_sys_sim_s,
                tidi_factor,
                has_tidi_history,
            )
            .max(delay_s)
        })
        .unwrap_or(delay_s);
    actor_st.module_cooldowns.reactivation_cooldowns.insert(
        key,
        ModuleCooldownEntry {
            recorded_unix_f64: now_unix_f64,
            delay_s: effective_delay,
            solar_system_id: Some(solar_system_id),
            recorded_sys_sim_s: cur_sys_sim_s,
        },
    );
    actor_st.module_cooldown_checkpoint_dirty = true;
    actor_st.module_cooldown_checkpoint_seq =
        actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    Some(build_persist_payload_from_actor_state(
        actor_st,
        tidi_factor,
        has_tidi_history,
    ))
}

pub(in crate::system_actor) fn record_module_cooldown_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    req: RecordModuleCooldownReq,
) -> Result<ModuleCooldownPersistPayload, String> {
    if let Some(payload) = record_module_cooldown_in_actor_state(
        ctx.solar_system_id,
        ctx.actor_st,
        req.ship_id,
        req.module_id,
        req.module_type_id,
        req.now_unix_f64,
        req.delay_s,
        req.tidi_factor,
        req.has_tidi_history,
    ) {
        return Ok(payload);
    }
    Ok(build_persist_payload_from_actor_state(
        ctx.actor_st,
        req.tidi_factor,
        req.has_tidi_history,
    ))
}

pub(in crate::system_actor) fn seed_module_cooldown_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    key: (i64, i64, u32),
    entry: ModuleCooldownEntry,
) -> Result<ModuleCooldownPersistPayload, String> {
    if ctx.actor_st.module_cooldowns.boot_db_pending_merge {
        ctx.actor_st.module_cooldowns.pending_db_removed_keys.insert(key);
    } else {
        ctx.actor_st.module_cooldowns.pending_db_removed_keys.remove(&key);
    }
    ctx.actor_st
        .module_cooldowns
        .reactivation_cooldowns
        .insert(key, entry);
    ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    ctx.actor_st.module_cooldown_checkpoint_seq =
        ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    Ok(build_persist_payload_from_actor_state(
        ctx.actor_st,
        1.0,
        ctx.actor_st.has_tidi_history,
    ))
}

pub(in crate::system_actor) fn remove_module_cooldown_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    key: (i64, i64, u32),
) -> Result<Option<ModuleCooldownPersistPayload>, String> {
    let had_entry = ctx
        .actor_st
        .module_cooldowns
        .reactivation_cooldowns
        .remove(&key)
        .is_some();
    if ctx.actor_st.module_cooldowns.boot_db_pending_merge {
        ctx.actor_st.module_cooldowns.pending_db_removed_keys.insert(key);
    }
    if !had_entry && !ctx.actor_st.module_cooldowns.boot_db_pending_merge {
        return Ok(None);
    }
    ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    ctx.actor_st.module_cooldown_checkpoint_seq =
        ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    Ok(Some(build_persist_payload_from_actor_state(
        ctx.actor_st,
        1.0,
        ctx.actor_st.has_tidi_history,
    )))
}

pub(in crate::system_actor) fn clear_ship_module_cooldowns_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ship_id: i64,
) -> Result<Option<ModuleCooldownPersistPayload>, String> {
    let before = ctx.actor_st.module_cooldowns.reactivation_cooldowns.len();
    ctx.actor_st
        .module_cooldowns
        .reactivation_cooldowns
        .retain(|&(sid, _, _), _| sid != ship_id);
    if ctx.actor_st.module_cooldowns.boot_db_pending_merge {
        ctx.actor_st
            .module_cooldowns
            .pending_db_cleared_ships
            .insert(ship_id);
        ctx.actor_st
            .module_cooldowns
            .pending_db_removed_keys
            .retain(|&(sid, _, _)| sid != ship_id);
    }
    let changed = ctx.actor_st.module_cooldowns.reactivation_cooldowns.len() != before;
    if !changed && !ctx.actor_st.module_cooldowns.boot_db_pending_merge {
        return Ok(None);
    }
    ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    ctx.actor_st.module_cooldown_checkpoint_seq =
        ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    Ok(Some(build_persist_payload_from_actor_state(
        ctx.actor_st,
        1.0,
        ctx.actor_st.has_tidi_history,
    )))
}

pub(in crate::system_actor) fn clear_all_module_cooldowns_in_actor(
    ctx: &mut BallOpsCtx<'_>,
) -> Result<ModuleCooldownPersistPayload, String> {
    if !ctx
        .actor_st
        .module_cooldowns
        .reactivation_cooldowns
        .is_empty()
        || ctx.actor_st.module_cooldowns.boot_db_pending_merge
        || ctx.actor_st.module_cooldown_checkpoint_dirty
    {
        ctx.actor_st.module_cooldowns.reactivation_cooldowns.clear();
        ctx.actor_st.module_cooldowns.boot_db_pending_merge = false;
        ctx.actor_st.module_cooldowns.pending_db_clear_all = true;
        ctx.actor_st.module_cooldowns.pending_db_cleared_ships.clear();
        ctx.actor_st.module_cooldowns.pending_db_removed_keys.clear();
        ctx.actor_st.module_cooldown_checkpoint_dirty = true;
        ctx.actor_st.module_cooldown_checkpoint_seq =
            ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    }
    Ok(build_persist_payload_from_actor_state(
        ctx.actor_st,
        1.0,
        ctx.actor_st.has_tidi_history,
    ))
}

pub(in crate::system_actor) fn replace_module_cooldowns_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    cooldowns: HashMap<(i64, i64, u32), ModuleCooldownEntry>,
) -> Result<(), String> {
    ctx.actor_st.module_cooldowns.reactivation_cooldowns = cooldowns;
    ctx.actor_st.module_cooldowns.boot_db_pending_merge = false;
    ctx.actor_st.module_cooldowns.pending_db_clear_all = false;
    ctx.actor_st.module_cooldowns.pending_db_cleared_ships.clear();
    ctx.actor_st.module_cooldowns.pending_db_removed_keys.clear();
    ctx.actor_st.module_cooldown_checkpoint_dirty = false;
    ctx.actor_st.module_cooldown_checkpoint_seq =
        ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    Ok(())
}

pub(in crate::system_actor) fn snapshot_module_cooldown_checkpoint_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    only_if_dirty: bool,
) -> Result<Option<ModuleCooldownPersistPayload>, String> {
    if only_if_dirty && !ctx.actor_st.module_cooldown_checkpoint_dirty {
        return Ok(None);
    }
    if !only_if_dirty {
        ctx.actor_st.module_cooldown_checkpoint_seq =
            ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    }
    // Keep `module_cooldown_checkpoint_dirty` set until SQLite persistence actually commits (`#1541`).
    Ok(Some(build_persist_payload_from_actor_state(
        ctx.actor_st,
        1.0,
        ctx.actor_st.has_tidi_history,
    )))
}

pub(in crate::system_actor) fn record_module_reload_sim_deadlines_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    module_ids: Vec<i64>,
    sim_start_s: f64,
    sim_deadline_s: f64,
) -> Result<(), String> {
    for mid in module_ids {
        ctx.actor_st.module_cooldowns.reload_sim_deadlines.insert(
            mid,
            ModuleReloadSimDeadline {
                sim_start_s,
                sim_deadline_s,
            },
        );
    }
    Ok(())
}

pub(in crate::system_actor) fn clear_module_reload_sim_deadline_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    module_id: i64,
) -> Result<bool, String> {
    Ok(ctx
        .actor_st
        .module_cooldowns
        .reload_sim_deadlines
        .remove(&module_id)
        .is_some())
}

// ---------------------------------------------------------------------------
// SystemRegistry cross-system facades
// ---------------------------------------------------------------------------

impl SystemRegistry {
    /// Returns the shared per-system [`CooldownPersistGate`] for `solar_system_id` without
    /// acquiring the gate mutex (`RANK_SERVICE_STATE < RANK_COOLDOWN_PERSIST`) (`#1541`).
    pub(crate) fn cooldown_persist_gate(
        &self,
        solar_system_id: SolarSystemId,
    ) -> Arc<CooldownPersistGate> {
        let mut guard = self
            .cooldown_persist_gates
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Arc::clone(
            guard
                .entry(solar_system_id)
                .or_insert_with(|| Arc::new(CooldownPersistGate::default())),
        )
    }

    /// Flushes any dirty or un-rebased cooldowns for a stopped actor AFTER `cmd_tx.closed()`
    /// so actor shutdown never blocks waiting on actor commands (`#1541`).
    pub(in crate::system_actor) fn flush_stopped_actor_module_cooldowns(
        &self,
        solar_system_id: SolarSystemId,
        actor_state: &RankedRwLock<SystemActorState, RANK_ACTOR_STATE>,
    ) {
        if solar_system_id == 0 {
            return;
        }
        let tidi_factor = self.get_system_tidi_factor(solar_system_id);
        let sim_anchored = self
            .cluster_nodes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .sim_clock_anchoring
            .contains_key(&solar_system_id);
        let payload_opt = {
            let mut st = actor_state.write().unwrap_or_else(|e| e.into_inner());
            let has_entries = !st.module_cooldowns.reactivation_cooldowns.is_empty();
            if !st.module_cooldown_checkpoint_dirty && !has_entries {
                None
            } else {
                let has_tidi_history =
                    st.has_tidi_history || tidi_factor < 0.999_999 || sim_anchored;
                let now_unix_f64 = st
                    .db_opt
                    .as_ref()
                    .map(|db| db.clock().now_unix_secs() as f64)
                    .unwrap_or(0.0);
                let cur_sys_sim_s = st.sim_elapsed_secs;
                if has_entries {
                    rebase_module_cooldowns_on_stop_in_state(
                        &mut st.module_cooldowns,
                        solar_system_id,
                        now_unix_f64,
                        cur_sys_sim_s,
                        tidi_factor,
                        has_tidi_history,
                    );
                    st.module_cooldown_checkpoint_seq =
                        st.module_cooldown_checkpoint_seq.saturating_add(1);
                }
                Some(build_persist_payload_from_actor_state(
                    &st,
                    tidi_factor,
                    has_tidi_history,
                ))
            }
        };
        if let Some(payload) = payload_opt {
            let gate = self.cooldown_persist_gate(solar_system_id);
            let _ = persist_cooldown_payload_with_gate(solar_system_id, &gate, None, None, payload);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_cold_system_module_cooldown_in_db(
        &self,
        db: &WorldRepository,
        solar_system_id: SolarSystemId,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        now_unix_f64: f64,
        delay_s: f64,
        tidi_factor: f64,
        has_tidi_history: bool,
    ) {
        if let Some(handle) = self.get_handle(solar_system_id) {
            let _ = handle.record_module_cooldown_sync(
                ship_id,
                module_id,
                module_type_id,
                now_unix_f64,
                delay_s,
                tidi_factor,
                has_tidi_history,
            );
            return;
        }
        let gate = self.cooldown_persist_gate(solar_system_id);
        record_cold_system_module_cooldown_serialized(
            self,
            db,
            &gate,
            solar_system_id,
            ship_id,
            module_id,
            module_type_id,
            now_unix_f64,
            delay_s,
            tidi_factor,
            has_tidi_history,
        );
    }

    pub(crate) fn seed_cold_system_module_cooldown_in_db(
        &self,
        db: &WorldRepository,
        solar_system_id: SolarSystemId,
        key: (i64, i64, u32),
        entry: ModuleCooldownEntry,
    ) {
        if let Some(handle) = self.get_handle(solar_system_id) {
            let _ = handle.seed_module_cooldown_entry_sync(key, entry);
            return;
        }
        let gate = self.cooldown_persist_gate(solar_system_id);
        seed_cold_system_module_cooldown_serialized(self, db, &gate, solar_system_id, key, entry);
    }

    /// Removes `key` from cold SQLite checkpoints across non-live systems (`exclude_sys_opt`
    /// skipped), serialized per system through [`CooldownPersistGate`]. Live systems handle
    /// pending cold removals through their own actor commands.
    pub(crate) fn remove_cold_module_cooldown_key_across_systems(
        &self,
        db: &WorldRepository,
        exclude_sys_opt: Option<u32>,
        key: (i64, i64, u32),
    ) -> bool {
        if !can_write_sqlite_safely() {
            return false;
        }
        let entries = find_all_cold_module_cooldown_entries_in_db(db, key);
        if entries.is_empty() {
            return false;
        }
        let mut sys_ids: BTreeSet<u32> = entries.into_iter().map(|(sid, _)| sid).collect();
        if let Some(ex) = exclude_sys_opt {
            sys_ids.remove(&ex);
        }
        let mut removed = false;
        for sys_id in sys_ids {
            if self.is_system_non_cold(sys_id) {
                continue;
            }
            let gate = self.cooldown_persist_gate(sys_id);
            if remove_cold_system_module_cooldown_serialized(self, db, &gate, sys_id, key) {
                removed = true;
            }
        }
        removed
    }

    pub(crate) fn clear_cold_ship_module_cooldowns_in_db(
        &self,
        db: &WorldRepository,
        ship_id: i64,
    ) {
        if !can_write_sqlite_safely() {
            return;
        }
        let Ok(all_rows) = db.with_sqlite_conn(|conn| {
            ensure_system_checkpoint_schema(conn)?;
            load_all_checkpoints_for_kind::<((i64, i64, u32), ModuleCooldownEntry)>(
                conn,
                KIND_MODULE_COOLDOWNS,
            )
        }) else {
            return;
        };
        let mut dirty_systems: BTreeSet<u32> = BTreeSet::new();
        for (sys_id, _, ((sid, _, _), _)) in all_rows {
            if sid == ship_id {
                dirty_systems.insert(sys_id);
            }
        }
        for sys_id in dirty_systems {
            if self.is_system_non_cold(sys_id) {
                continue;
            }
            let gate = self.cooldown_persist_gate(sys_id);
            let seen_epoch = gate.current_epoch();
            let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
            if gate.current_epoch() != seen_epoch || self.is_system_non_cold_try(sys_id) {
                continue;
            }
            let mut map = load_cold_system_module_cooldowns_from_db(db, sys_id);
            let before = map.len();
            map.retain(|&(sid, _, _), _| sid != ship_id);
            if map.len() == before {
                continue;
            }
            let rows = serialize_module_cooldown_rows(&map);
            let now_unix = current_unix_secs();
            let ok = db
                .with_sqlite_conn(|conn| {
                    ensure_system_checkpoint_schema(conn)?;
                    replace_system_checkpoints_for_kind(
                        conn,
                        sys_id,
                        KIND_MODULE_COOLDOWNS,
                        &rows,
                        now_unix,
                    )
                })
                .is_ok();
            if ok && gate.try_advance_cold_epoch(seen_epoch) {
                gate_guard.last_persisted = Some((seen_epoch + 1, 0));
            }
        }
    }

    pub(crate) fn clear_all_cold_module_cooldowns_in_db(&self, db: &WorldRepository) {
        if !can_write_sqlite_safely() {
            return;
        }
        let Ok(all_rows) = db.with_sqlite_conn(|conn| {
            ensure_system_checkpoint_schema(conn)?;
            load_all_checkpoints_for_kind::<((i64, i64, u32), ModuleCooldownEntry)>(
                conn,
                KIND_MODULE_COOLDOWNS,
            )
        }) else {
            return;
        };
        let sys_ids: BTreeSet<u32> = all_rows.into_iter().map(|(sys_id, _, _)| sys_id).collect();
        let now_unix = current_unix_secs();
        for sys_id in sys_ids {
            if self.is_system_non_cold(sys_id) {
                continue;
            }
            let gate = self.cooldown_persist_gate(sys_id);
            let seen_epoch = gate.current_epoch();
            let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
            if gate.current_epoch() != seen_epoch || self.is_system_non_cold_try(sys_id) {
                continue;
            }
            let ok = db
                .with_sqlite_conn(|conn| {
                    ensure_system_checkpoint_schema(conn)?;
                    replace_system_checkpoints_for_kind::<((i64, i64, u32), ModuleCooldownEntry)>(
                        conn,
                        sys_id,
                        KIND_MODULE_COOLDOWNS,
                        &[],
                        now_unix,
                    )
                })
                .is_ok();
            if ok && gate.try_advance_cold_epoch(seen_epoch) {
                gate_guard.last_persisted = Some((seen_epoch + 1, 0));
            }
        }
    }

    /// Flushes durable module reactivation cooldown checkpoints across all currently loaded systems.
    pub fn flush_all_module_cooldown_checkpoints_to_db(&self) {
        for (_, handle) in self.active_system_handles() {
            let _ = handle.flush_module_cooldown_checkpoint_sync();
        }
    }

    /// Reloads live actor cooldown maps from SQLite `system_checkpoint` after DB rehydration.
    pub fn rehydrate_active_system_cooldowns_from_db<F>(&self, tidi_resolver: F)
    where
        F: Fn(u32) -> (f64, bool),
    {
        let Some(ref db) = self.db else {
            return;
        };
        for (sys_id, handle) in self.active_system_handles() {
            let (tidi_factor, has_tidi_history) = tidi_resolver(sys_id);
            let loaded = load_active_system_module_cooldowns_for_boot(
                db,
                sys_id,
                tidi_factor,
                has_tidi_history,
                handle.sim_elapsed_secs(),
            );
            let _ = handle.replace_module_cooldowns_sync(loaded);
        }
    }

    /// Records sim-clock reload deadlines on `solar_system_id`'s actor when live (`#1081`, `#1541`).
    pub fn record_module_reload_sim_deadlines(
        &self,
        module_ids: &[i64],
        solar_system_id: SolarSystemId,
        sim_start_s: f64,
        sim_deadline_s: f64,
    ) {
        if module_ids.is_empty() {
            return;
        }
        if let Some(handle) = self.get_handle(solar_system_id) {
            let _ = handle.record_module_reload_sim_deadlines_sync(
                module_ids.to_vec(),
                sim_start_s,
                sim_deadline_s,
            );
        }
    }

    /// Clears a module's sim-clock reload deadline on whichever live actor owns it (`#1081`, `#1541`).
    pub fn clear_module_reload_sim_deadline(&self, module_id: i64) {
        for (_, handle) in self.active_system_handles() {
            if handle
                .view()
                .module_cooldowns
                .reload_sim_deadlines
                .contains_key(&module_id)
            {
                let _ = handle.clear_module_reload_sim_deadline_sync(module_id);
                return;
            }
        }
    }

    /// Checks a module's sim-clock reload deadline lock-free from [`SystemView`] (`#1081`, `#1541`).
    pub fn check_module_reload_sim_deadline(
        &self,
        module_id: i64,
    ) -> Option<(SolarSystemId, f64, f64)> {
        for (sys_id, handle) in self.active_system_handles() {
            if let Some(d) = handle.view().module_reload_sim_deadline(module_id) {
                return Some((sys_id, d.sim_start_s, d.sim_deadline_s));
            }
        }
        None
    }
}
