//! Per-system in-space EWAR projection/active-start state, module reactivation cooldowns,
//! and sim-clock module reload deadlines owned by [`SystemActor`] (`#1541`).

#![forbid(unsafe_code)]

use super::{
    call_actor_result_with_deadline, defer_reply, dispatch_actor_sync_with_deadline,
    ActiveModuleCycle, ActorExecCtx, BallOpsCtx, CharacterId, ModuleDeactivationHook,
    ModuleDeactivationPostUnlockHook, PostUnlockHookAction, SystemActorState, SystemCommand,
    SystemHandle, SystemRegistry, SystemView, ACTOR_CMD_TIMEOUT,
};
use eve_db::lock_rank::{
    held_lock_ranks, RankedMutex, RankedRwLock, RANK_ACTOR_STATE, RANK_LEAF, RANK_WORLD_INNER,
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
    pub fn bump_epoch(&self) -> u64 {
        self.next_epoch
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
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
        tidi_factor,
        has_tidi_history,
    }
}

/// Serializes, sequence-guards, merges pending pre-boot cold rows (if any), and commits a
/// per-system module cooldown checkpoint cleanly outside actor locks (`#1541`).
pub(crate) fn persist_cooldown_payload_with_gate(
    solar_system_id: SolarSystemId,
    gate: &CooldownPersistGate,
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
                        live_entry.recorded_sys_sim_s,
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
    if let Some(actor_state) = actor_state_opt {
        if !held_lock_ranks().iter().any(|&r| r >= RANK_ACTOR_STATE) {
            let mut st = actor_state.write().unwrap_or_else(|e| e.into_inner());
            if st.module_cooldown_persist_epoch == payload.epoch {
                if payload.boot_db_pending_merge && !st.module_cooldowns.pending_db_clear_all {
                    let cur_sim_s = st.sim_elapsed_secs;
                    for (k, merged_entry) in merged_from_db {
                        if st.module_cooldowns.pending_db_cleared_ships.contains(&k.0)
                            || st.module_cooldowns.pending_db_removed_keys.contains(&k)
                            || st.module_cooldowns.reactivation_cooldowns.contains_key(&k)
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
                    }
                    if st.module_cooldown_checkpoint_seq == payload.seq {
                        st.module_cooldowns.boot_db_pending_merge = false;
                        st.module_cooldowns.pending_db_clear_all = false;
                        st.module_cooldowns.pending_db_cleared_ships.clear();
                        st.module_cooldowns.pending_db_removed_keys.clear();
                    }
                }
                if st.module_cooldown_checkpoint_seq == payload.seq {
                    st.module_cooldown_checkpoint_dirty = false;
                }
            }
        }
    }
    true
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
    if solar_system_id == 0 || !delay_s.is_finite() || delay_s <= 0.0 || !can_write_sqlite_safely() {
        return;
    }
    let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
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
    if persist_system_module_cooldowns_in_db(db, solar_system_id, &rows) {
        let next_epoch = gate.bump_epoch();
        gate_guard.last_persisted = Some((next_epoch, 0));
    }
}

pub(crate) fn seed_cold_system_module_cooldown_serialized(
    db: &WorldRepository,
    gate: &CooldownPersistGate,
    solar_system_id: SolarSystemId,
    key: (i64, i64, u32),
    entry: ModuleCooldownEntry,
) {
    if solar_system_id == 0 || !can_write_sqlite_safely() {
        return;
    }
    let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
    let mut map = load_cold_system_module_cooldowns_from_db(db, solar_system_id);
    map.insert(key, entry);
    let rows = serialize_module_cooldown_rows(&map);
    if persist_system_module_cooldowns_in_db(db, solar_system_id, &rows) {
        let next_epoch = gate.bump_epoch();
        gate_guard.last_persisted = Some((next_epoch, 0));
    }
}

pub(crate) fn remove_cold_system_module_cooldown_serialized(
    db: &WorldRepository,
    gate: &CooldownPersistGate,
    solar_system_id: SolarSystemId,
    key: (i64, i64, u32),
) -> bool {
    if !can_write_sqlite_safely() {
        return false;
    }
    let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
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
    if ok {
        let next_epoch = gate.bump_epoch();
        gate_guard.last_persisted = Some((next_epoch, 0));
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
    actor_st.module_cooldowns.pending_db_removed_keys.remove(&key);
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

fn record_module_cooldown_in_actor(
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

fn seed_module_cooldown_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    key: (i64, i64, u32),
    entry: ModuleCooldownEntry,
) -> Result<ModuleCooldownPersistPayload, String> {
    ctx.actor_st.module_cooldowns.pending_db_removed_keys.remove(&key);
    ctx.actor_st
        .module_cooldowns
        .reactivation_cooldowns
        .insert(key, entry);
    ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    ctx.actor_st.module_cooldown_checkpoint_seq =
        ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    Ok(build_persist_payload_from_actor_state(ctx.actor_st, 1.0, false))
}

fn remove_module_cooldown_in_actor(
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
        false,
    )))
}

fn clear_ship_module_cooldowns_in_actor(
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
        false,
    )))
}

fn clear_all_module_cooldowns_in_actor(
    ctx: &mut BallOpsCtx<'_>,
) -> Result<ModuleCooldownPersistPayload, String> {
    if !ctx
        .actor_st
        .module_cooldowns
        .reactivation_cooldowns
        .is_empty()
        || ctx.actor_st.module_cooldowns.boot_db_pending_merge
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
        false,
    ))
}

fn replace_module_cooldowns_in_actor(
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

fn snapshot_module_cooldown_checkpoint_in_actor(
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
        false,
    )))
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
        post_unlock_hook: Option<ModuleDeactivationPostUnlockHook>,
    ) -> Self {
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
        let mut actions_to_push: Vec<PostUnlockHookAction> = Vec::with_capacity(items.len());
        for (deact_sys_id, char_id, cycle) in items {
            let allow_db_fallback = actor_st.db_opt.as_ref().is_some_and(|db| {
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
            let actor_state_w = actor_state_shared.map(Arc::downgrade);
            actions_to_push.push(Box::new(move || {
                if let Some(payload) = payload_opt {
                    let upgraded = actor_state_w.and_then(|w| w.upgrade());
                    let _ = persist_cooldown_payload_with_gate(
                        deact_sys_id,
                        &gate_c,
                        upgraded.as_deref(),
                        payload,
                    );
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

fn record_module_reload_sim_deadlines_in_actor(
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

fn clear_module_reload_sim_deadline_in_actor(
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
            Some(&self.actor_state),
            payload,
        )
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
        self.persist_cooldown_payload(payload);
        Ok(())
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
        self.persist_cooldown_payload(payload);
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
        let has_tidi_history = tidi_factor < 0.999_999
            || self
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
            let _ = persist_cooldown_payload_with_gate(solar_system_id, &gate, None, payload);
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
        let gate = self.cooldown_persist_gate(solar_system_id);
        record_cold_system_module_cooldown_serialized(
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
        let gate = self.cooldown_persist_gate(solar_system_id);
        seed_cold_system_module_cooldown_serialized(db, &gate, solar_system_id, key, entry);
    }

    /// Removes `key` from cold SQLite checkpoints across systems (`exclude_sys_opt` skipped),
    /// serialized per system through [`CooldownPersistGate`].
    pub(crate) fn remove_cold_module_cooldown_key_across_systems(
        &self,
        db: &WorldRepository,
        exclude_sys_opt: Option<u32>,
        skip_live_merged_systems: bool,
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
            if skip_live_merged_systems {
                if let Some(h) = self.get_handle(sys_id) {
                    if !h.view().module_cooldowns.boot_db_pending_merge {
                        continue;
                    }
                }
            }
            let gate = self.cooldown_persist_gate(sys_id);
            if remove_cold_system_module_cooldown_serialized(db, &gate, sys_id, key) {
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
            let gate = self.cooldown_persist_gate(sys_id);
            let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
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
            if ok {
                let next_epoch = gate.bump_epoch();
                gate_guard.last_persisted = Some((next_epoch, 0));
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
            let gate = self.cooldown_persist_gate(sys_id);
            let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
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
            if ok {
                let next_epoch = gate.bump_epoch();
                gate_guard.last_persisted = Some((next_epoch, 0));
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use eve_db::{InventoryFlag, InventoryItemRecord, ServerClock};
    use eve_dogma::{DogmaItemState, ProjectedEwarEffect};
    use eve_sde::SdeRegistry;

    fn test_sde() -> Arc<SdeRegistry> {
        SdeRegistry::open_shared(&SdeRegistry::default_db_path())
            .expect("default SDE must open for ewar_cooldowns tests")
    }

    fn test_registry_with_db() -> (Arc<SystemRegistry>, Arc<WorldRepository>, Arc<SdeRegistry>) {
        let db = Arc::new(WorldRepository::new_empty());
        let sde = test_sde();
        let reg = SystemRegistry::with_sde_and_db(Arc::clone(&sde), Arc::clone(&db));
        (Arc::new(reg), db, sde)
    }

    #[tokio::test]
    async fn ewar_state_is_actor_scoped_and_cleared_on_stop_restart() {
        let (reg, _db, sde) = test_registry_with_db();
        let jita = 30_000_142_u32;
        let amarr = 30_002_187_u32;
        let h_jita = reg.get_or_spawn(jita);
        let h_amarr = reg.get_or_spawn(amarr);

        let mod_state = DogmaItemState::from_sde(700_001, 526, &sde);
        h_jita
            .upsert_projected_ewar(
                800_001,
                900_001,
                ProjectedEwarEffect {
                    source_module_id: 700_001,
                    source_module_type_id: 526,
                    charge_type_id: None,
                    distance: 5_000.0,
                    overloaded: false,
                    effective_module: mod_state,
                },
            )
            .await
            .expect("upsert projected EWAR");
        h_jita
            .record_active_ewar_start(800_001, 700_001, 900_001, "webify")
            .await
            .expect("record active EWAR start");

        assert_eq!(h_jita.projected_ewar_for(900_001).len(), 1);
        assert!(h_amarr.projected_ewar_for(900_001).is_empty());
        assert_eq!(reg.ship_ewar_projections(900_001).len(), 1);

        assert!(reg.stop_system(jita).await);
        assert!(reg.ship_ewar_projections(900_001).is_empty());

        let h_jita_restarted = reg.get_or_spawn(jita);
        assert!(h_jita_restarted.projected_ewar_for(900_001).is_empty());
        assert!(h_jita_restarted.ewar_state_snapshot().is_empty());
    }

    #[tokio::test]
    async fn cooldowns_persist_per_system_and_reload_sim_deadlines_live_in_actor() {
        let (reg, db, _sde) = test_registry_with_db();
        let jita = 30_000_142_u32;
        let amarr = 30_002_187_u32;
        let now_unix = db.clock().now_unix_secs() as f64;

        let h_jita = reg.get_or_spawn(jita);
        h_jita
            .record_module_cooldown(100_001, 200_001, 11_370, now_unix, 15.0, 1.0, false)
            .await
            .expect("record cooldown");
        h_jita
            .record_module_reload_sim_deadlines_sync(vec![200_002], 2.0, 12.0)
            .expect("record reload deadline");

        assert!(h_jita
            .module_reactivation_cooldown(&(100_001, 200_001, 11_370))
            .is_some());
        assert_eq!(
            reg.check_module_reload_sim_deadline(200_002),
            Some((jita, 2.0, 12.0))
        );

        let jita_rows = load_cold_system_module_cooldowns_from_db(&db, jita);
        let amarr_rows = load_cold_system_module_cooldowns_from_db(&db, amarr);
        let cluster_rows = load_cold_system_module_cooldowns_from_db(&db, CLUSTER_SYSTEM_ID);
        assert!(jita_rows.contains_key(&(100_001, 200_001, 11_370)));
        assert!(amarr_rows.is_empty());
        assert!(cluster_rows.is_empty());

        assert!(reg.stop_system(jita).await);
        assert!(reg.check_module_reload_sim_deadline(200_002).is_none());

        let h_jita_restarted = reg.get_or_spawn(jita);
        assert!(h_jita_restarted
            .module_reactivation_cooldown(&(100_001, 200_001, 11_370))
            .is_some());
    }

    #[test]
    fn legacy_cluster_cooldown_checkpoint_migrates_one_way_to_system() {
        let (_reg, db, _sde) = test_registry_with_db();
        let ship_id = 300_001_i64;
        let mod_id = 300_002_i64;
        let mod_tid = 11_370_u32;
        let sys_id = 30_000_142_u32;
        let now_unix = db.clock().now_unix_secs() as f64;

        db.with_mut_snapshot(|snap| {
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: 90_000_001,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        });

        let entry = ModuleCooldownEntry {
            recorded_unix_f64: now_unix,
            delay_s: 20.0,
            solar_system_id: None,
            recorded_sys_sim_s: 0.0,
        };
        let row_key = module_cooldown_checkpoint_key(ship_id, mod_id, mod_tid);
        db.with_sqlite_conn(|conn| {
            ensure_system_checkpoint_schema(conn)?;
            replace_system_checkpoints_for_kind(
                conn,
                CLUSTER_SYSTEM_ID,
                KIND_MODULE_COOLDOWNS,
                &[(row_key, ((ship_id, mod_id, mod_tid), entry))],
                now_unix as u64,
            )
        })
        .expect("seed legacy cluster checkpoint");

        migrate_and_prune_module_cooldowns_in_db(&db, now_unix, |_| (1.0, false));

        assert!(load_cold_system_module_cooldowns_from_db(&db, CLUSTER_SYSTEM_ID).is_empty());
        let migrated = load_cold_system_module_cooldowns_from_db(&db, sys_id);
        let migrated_entry = migrated
            .get(&(ship_id, mod_id, mod_tid))
            .expect("migrated into ship system");
        assert_eq!(migrated_entry.solar_system_id, Some(sys_id));
    }

    #[tokio::test]
    async fn cooldown_persist_gate_rejects_stale_out_of_order_writes_and_keeps_dirty_until_commit() {
        let (reg, db, _sde) = test_registry_with_db();
        let jita = 30_000_142_u32;
        let now_unix = db.clock().now_unix_secs() as f64;
        let h_jita = reg.get_or_spawn(jita);
        let key = (500_001_i64, 600_001_i64, 11_370_u32);

        let (tx1, rx1) = tokio::sync::oneshot::channel();
        h_jita
            .cmd_sender()
            .send(SystemCommand::RecordModuleCooldown {
                ship_id: key.0,
                module_id: key.1,
                module_type_id: key.2,
                now_unix_f64: now_unix,
                delay_s: 30.0,
                tidi_factor: 1.0,
                has_tidi_history: false,
                reply: tx1,
            })
            .await
            .expect("send record");
        let payload_seq1 = rx1.await.expect("rx1").expect("record ok");

        // Snapshot while dirty MUST NOT clear `module_cooldown_checkpoint_dirty` before DB commit.
        let (snap_tx, snap_rx) = tokio::sync::oneshot::channel();
        h_jita
            .cmd_sender()
            .send(SystemCommand::SnapshotModuleCooldownCheckpoint {
                only_if_dirty: true,
                reply: snap_tx,
            })
            .await
            .expect("send snapshot");
        let snap_payload = snap_rx
            .await
            .expect("snap_rx")
            .expect("snap ok")
            .expect("still dirty before persist");
        assert_eq!(snap_payload.seq, payload_seq1.seq);
        assert!(
            h_jita
                .actor_state
                .read()
                .unwrap()
                .module_cooldown_checkpoint_dirty,
            "dirty flag must stay set until DB write commits"
        );

        // Now remove cooldown (higher seq = 2) and commit seq = 2 FIRST.
        let (tx2, rx2) = tokio::sync::oneshot::channel();
        h_jita
            .cmd_sender()
            .send(SystemCommand::RemoveModuleCooldown { key, reply: tx2 })
            .await
            .expect("send remove");
        let payload_seq2 = rx2.await.expect("rx2").expect("remove ok").expect("had entry");
        assert!(payload_seq2.seq > payload_seq1.seq);
        assert!(persist_cooldown_payload_with_gate(
            jita,
            &h_jita.cooldown_persist_gate,
            Some(&h_jita.actor_state),
            payload_seq2,
        ));
        assert!(
            !h_jita
                .actor_state
                .read()
                .unwrap()
                .module_cooldown_checkpoint_dirty
        );

        // Delayed out-of-order writer carrying seq = 1 MUST be rejected and NOT resurrect `key`.
        assert!(!persist_cooldown_payload_with_gate(
            jita,
            &h_jita.cooldown_persist_gate,
            Some(&h_jita.actor_state),
            payload_seq1,
        ));
        assert!(load_cold_system_module_cooldowns_from_db(&db, jita).is_empty());
    }

    #[test]
    fn boot_with_db_read_unsafe_merges_existing_cold_cooldowns_without_wiping() {
        let (reg, db, _sde) = test_registry_with_db();
        let jita = 30_000_142_u32;
        let now_unix = db.clock().now_unix_secs() as f64;
        let k_pre = (510_001_i64, 610_001_i64, 11_370_u32);
        let k_new = (510_002_i64, 610_002_i64, 11_370_u32);

        // Seed pre-existing cold checkpoint C1 in SQLite.
        reg.seed_cold_system_module_cooldown_in_db(
            &db,
            jita,
            k_pre,
            ModuleCooldownEntry {
                recorded_unix_f64: now_unix,
                delay_s: 25.0,
                solar_system_id: Some(jita),
                recorded_sys_sim_s: 0.0,
            },
        );

        // Simulate `!db_read_safe` boot state: in-memory map starts empty with `boot_db_pending_merge`.
        let gate = reg.cooldown_persist_gate(jita);
        let persist_epoch = gate.bump_epoch();
        let actor_state = Arc::new(RankedRwLock::new(SystemActorState {
            db_opt: Some(Arc::clone(&db)),
            module_cooldown_persist_epoch: persist_epoch,
            module_cooldowns: ActorModuleCooldownState {
                boot_db_pending_merge: true,
                ..Default::default()
            },
            ..Default::default()
        }));

        let payload = {
            let mut st = actor_state.write().unwrap();
            record_module_cooldown_in_actor_state(
                jita, &mut st, k_new.0, k_new.1, k_new.2, now_unix, 15.0, 1.0, false,
            )
            .expect("payload generated")
        };
        assert!(persist_cooldown_payload_with_gate(
            jita,
            &gate,
            Some(&actor_state),
            payload,
        ));

        // Both C1 (pre-existing cold) and C2 (newly recorded) exist in SQLite and live actor state!
        let db_rows = load_cold_system_module_cooldowns_from_db(&db, jita);
        assert!(db_rows.contains_key(&k_pre));
        assert!(db_rows.contains_key(&k_new));
        let st_read = actor_state.read().unwrap();
        assert!(st_read.module_cooldowns.reactivation_cooldowns.contains_key(&k_pre));
        assert!(st_read.module_cooldowns.reactivation_cooldowns.contains_key(&k_new));
        assert!(!st_read.module_cooldowns.boot_db_pending_merge);
        assert!(!st_read.module_cooldown_checkpoint_dirty);
    }

    #[tokio::test]
    async fn tidi_boot_hydration_and_eviction_reactivation_rebase_sim_clock() {
        let base_unix = 1_750_000_000_u64;
        let clock = Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_unix));
        let repo = WorldRepository::new_empty();
        repo.set_clock(clock.clone());
        let db = Arc::new(repo);
        let sde = test_sde();
        let jita = 30_000_142_u32;
        let key = (520_001_i64, 620_001_i64, 11_370_u32);

        let reg = Arc::new(SystemRegistry::with_sde_and_db(
            Arc::clone(&sde),
            Arc::clone(&db),
        ));
        reg.cluster_nodes
            .write()
            .unwrap()
            .manual_tidi_overrides
            .insert(jita, 0.25);

        let h1 = reg.get_or_spawn(jita);
        assert!((h1.tidi_factor() - 0.25).abs() < 1e-6);
        // Advance 200 wall seconds * 0.25 TiDi = 50 sim seconds.
        for _ in 0..200 {
            let _ = h1.step_manual(1.0).await.unwrap();
        }
        assert!((h1.sim_elapsed_secs() - 50.0).abs() < 1e-6);

        // Record 30s cooldown at sim = 50.0, then step 20 wall seconds = 5.0 sim seconds -> 25s rem.
        h1.record_module_cooldown(
            key.0,
            key.1,
            key.2,
            clock.now_unix_secs() as f64,
            30.0,
            0.25,
            true,
        )
        .await
        .expect("record cooldown");
        for _ in 0..20 {
            let _ = h1.step_manual(1.0).await.unwrap();
        }
        assert!((h1.sim_elapsed_secs() - 55.0).abs() < 1e-6);
        drop(h1);

        // Evict / stop actor cleanly -> must rebase persisted entry to (now, 25.0s, 0.0).
        assert!(reg.stop_system(jita).await);
        let cold_rows = load_cold_system_module_cooldowns_from_db(&db, jita);
        let rebased = cold_rows.get(&key).expect("rebased row persisted on stop");
        assert!((rebased.recorded_sys_sim_s - 0.0).abs() < 1e-6);
        assert!((rebased.delay_s - 25.0).abs() < 1e-6);

        // Advance 20 wall seconds while cold under 0.25 TiDi (= 5.0 sim seconds elapsed -> 20.0s left).
        clock.advance_secs(20);

        // Reboot actor with `manual_tidi_overrides = 0.25`: initial_tidi MUST hydrate BEFORE
        // loading cooldowns and rebase again cleanly so sim stepping immediately advances it!
        let h2 = reg.get_or_spawn(jita);
        let booted_entry = h2
            .module_reactivation_cooldown(&key)
            .expect("cooldown hydrated at TiDi boot");
        assert!((booted_entry.recorded_sys_sim_s - 0.0).abs() < 1e-6);
        assert!((booted_entry.delay_s - 20.0).abs() < 1e-6);

        // Step 16 wall seconds * 0.25 = 4.0 sim seconds without advancing wall clock -> 16.0s left.
        for _ in 0..16 {
            let _ = h2.step_manual(1.0).await.unwrap();
        }
        let rem_after_step = remaining_cooldown_secs_with_tidi(
            &h2.module_reactivation_cooldown(&key).expect("still active"),
            clock.now_unix_secs() as f64,
            Some(jita),
            h2.sim_elapsed_secs(),
            0.25,
            true,
        );
        assert!(
            (rem_after_step - 16.0).abs() < 1e-6,
            "rebased TiDi cooldown must continue ticking on rebooted sim clock; got {rem_after_step}"
        );

        // Step 64 more wall seconds (= 16 sim seconds) -> pruned automatically on actor tick.
        for _ in 0..64 {
            let _ = h2.step_manual(1.0).await.unwrap();
        }
        assert!(h2.module_reactivation_cooldown(&key).is_none());
    }

    #[tokio::test]
    async fn tidi_crash_recovery_unrebased_sim_clock_scales_and_advances_after_boot() {
        let base_unix = 1_750_100_000_u64;
        let clock = Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_unix));
        let repo = WorldRepository::new_empty();
        repo.set_clock(clock.clone());
        let db = Arc::new(repo);
        let sde = test_sde();
        let jita = 30_000_142_u32;
        let key = (530_001_i64, 630_001_i64, 11_370_u32);

        let reg = Arc::new(SystemRegistry::with_sde_and_db(
            Arc::clone(&sde),
            Arc::clone(&db),
        ));
        reg.cluster_nodes
            .write()
            .unwrap()
            .manual_tidi_overrides
            .insert(jita, 0.25);

        // Seed an un-rebased DB row simulating a mid-flight crash at sim = 100.0s under TiDi 0.25.
        reg.seed_cold_system_module_cooldown_in_db(
            &db,
            jita,
            key,
            ModuleCooldownEntry {
                recorded_unix_f64: base_unix as f64,
                delay_s: 30.0,
                solar_system_id: Some(jita),
                recorded_sys_sim_s: 100.0,
            },
        );
        clock.advance_secs(40); // 40 * 0.25 = 10s elapsed -> 20s remaining!

        let h = reg.get_or_spawn(jita);
        let entry = h
            .module_reactivation_cooldown(&key)
            .expect("crash-recovered TiDi entry loaded");
        assert!((entry.delay_s - 20.0).abs() < 1e-6);
        assert!((entry.recorded_sys_sim_s - 0.0).abs() < 1e-6);
    }
}
