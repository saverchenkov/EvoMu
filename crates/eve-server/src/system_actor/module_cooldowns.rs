//! Per-system durable module reactivation cooldowns, `CooldownPersistGate` serialization,
//! SQLite checkpoint migration/boot hydration, and sim-clock reload deadlines (`#1541`).

#![forbid(unsafe_code)]

use super::{BallOpsCtx, SystemActorState, SystemRegistry};
use eve_db::lock_rank::{
    held_lock_ranks, RankedMutex, RankedRwLock, RANK_SERVICE_STATE, RANK_WORLD_INNER,
};
use eve_db::sqlite_store::system_checkpoint::{
    current_unix_secs, ensure_system_checkpoint_schema, load_all_checkpoints_for_kind,
    load_system_checkpoints, replace_system_checkpoints_for_kind, CLUSTER_SYSTEM_ID,
    KIND_MODULE_COOLDOWNS,
};
use eve_db::{WorldRepository, WorldStateSnapshot};
use eve_sde::SolarSystemId;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// `RANK_SQLITE_CONN` (`41`), and strictly above `RANK_ACTOR_STATE` (`21`).
pub(in crate::system_actor) const RANK_COOLDOWN_PERSIST: u16 = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(in crate::system_actor) enum CooldownGatePhase {
    #[default]
    Cold,
    Loading,
    Live,
    Stopping,
}

#[derive(Debug, Clone)]
pub enum PendingCooldownTransition {
    Record {
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        now_unix_f64: f64,
        delay_s: f64,
        tidi_factor: f64,
        has_tidi_history: bool,
    },
    Seed {
        key: (i64, i64, u32),
        entry: ModuleCooldownEntry,
    },
    Remove {
        key: (i64, i64, u32),
    },
    ClearShip {
        ship_id: i64,
    },
    PruneExpired {
        now_unix_f64: f64,
        tidi_factor: f64,
        has_tidi_history: bool,
    },
    ClearAll,
}

#[derive(Debug, Default)]
pub(in crate::system_actor) struct CooldownPersistGateState {
    pub last_persisted: Option<(u64, u64)>,
    pub phase: CooldownGatePhase,
    pub in_flight_loading_transitions: Vec<PendingCooldownTransition>,
    pub pending_transitions: Vec<PendingCooldownTransition>,
    pub boot_raw_cooldowns: HashMap<(i64, i64, u32), (f64, f64)>,
    pub shutdown_tidi_mode: Option<(f64, bool)>,
}

/// Per-system SQLite persistence serializer with monotonic `(epoch, seq)` guard and
/// lifecycle transition journal (`#1541`).
#[derive(Debug)]
pub(crate) struct CooldownPersistGate {
    next_epoch: AtomicU64,
    has_tidi_history: AtomicBool,
    has_pending_transitions: AtomicBool,
    pub(in crate::system_actor) inner:
        RankedMutex<CooldownPersistGateState, RANK_COOLDOWN_PERSIST>,
}

impl Default for CooldownPersistGate {
    fn default() -> Self {
        Self {
            next_epoch: AtomicU64::new(1),
            has_tidi_history: AtomicBool::new(false),
            has_pending_transitions: AtomicBool::new(false),
            inner: RankedMutex::new(CooldownPersistGateState::default()),
        }
    }
}

impl CooldownPersistGate {
    pub fn current_epoch(&self) -> u64 {
        self.next_epoch.load(Ordering::SeqCst)
    }

    pub fn bump_epoch(&self) -> u64 {
        self.next_epoch.fetch_add(1, Ordering::SeqCst) + 1
    }

    pub fn try_advance_cold_epoch(&self, expected: u64) -> bool {
        self.next_epoch
            .compare_exchange(expected, expected + 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub(in crate::system_actor) fn commit_cold_epoch(
        &self,
        gate_guard: &mut CooldownPersistGateState,
        expected_epoch: u64,
    ) {
        let next_epoch = if self.try_advance_cold_epoch(expected_epoch) {
            expected_epoch + 1
        } else {
            self.bump_epoch()
        };
        gate_guard.last_persisted = Some((next_epoch, 0));
    }

    pub fn mark_tidi_history(&self) {
        self.has_tidi_history.store(true, Ordering::Release);
    }

    pub fn has_tidi_history(&self) -> bool {
        self.has_tidi_history.load(Ordering::Acquire)
    }

    pub fn mark_pending_transitions(&self) {
        self.has_pending_transitions.store(true, Ordering::Release);
    }

    pub fn clear_pending_transitions_flag(&self) {
        self.has_pending_transitions.store(false, Ordering::Release);
    }

    pub fn has_pending_transitions(&self) -> bool {
        self.has_pending_transitions.load(Ordering::Acquire)
    }

    pub(in crate::system_actor) fn record_shutdown_tidi_mode(
        &self,
        tidi: f64,
        has_tidi_history: bool,
    ) {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        guard.shutdown_tidi_mode = Some((tidi, has_tidi_history));
    }

    pub(in crate::system_actor) fn take_all_pending_transitions(
        &self,
        guard: &mut CooldownPersistGateState,
    ) -> Vec<PendingCooldownTransition> {
        let mut all = std::mem::take(&mut guard.in_flight_loading_transitions);
        all.append(&mut guard.pending_transitions);
        self.clear_pending_transitions_flag();
        all
    }

    pub(in crate::system_actor) fn overlay_key_in_state(
        guard: &CooldownPersistGateState,
        key: (i64, i64, u32),
        base_entry: Option<ModuleCooldownEntry>,
        solar_system_id: SolarSystemId,
        tidi_factor: f64,
        has_tidi_history: bool,
    ) -> Option<ModuleCooldownEntry> {
        let mut cur = base_entry;
        for tr in guard.in_flight_loading_transitions.iter().chain(&guard.pending_transitions) {
            match *tr {
                PendingCooldownTransition::ClearAll => cur = None,
                PendingCooldownTransition::ClearShip { ship_id } if ship_id == key.0 => cur = None,
                PendingCooldownTransition::Remove { key: rk } if rk == key => cur = None,
                PendingCooldownTransition::Seed { key: sk, entry } if sk == key => cur = Some(entry),
                PendingCooldownTransition::Record {
                    ship_id, module_id, module_type_id, now_unix_f64, delay_s,
                    tidi_factor: rec_tidi, has_tidi_history: rec_hist,
                } if (ship_id, module_id, module_type_id) == key => {
                    cur = Some(compute_recorded_cooldown_entry(
                        solar_system_id, cur.as_ref(), now_unix_f64, delay_s, 0.0,
                        tidi_factor, rec_tidi, has_tidi_history, rec_hist,
                    ).0);
                }
                PendingCooldownTransition::PruneExpired {
                    now_unix_f64,
                    tidi_factor: prune_tidi,
                    has_tidi_history: prune_hist,
                } => {
                    if let Some(entry) = cur {
                        let eff_tidi = if tidi_factor < 0.999_999 { tidi_factor } else { prune_tidi };
                        let eff_hist = has_tidi_history || prune_hist || eff_tidi < 0.999_999;
                        if remaining_cooldown_secs_with_tidi(
                            &entry,
                            now_unix_f64,
                            Some(solar_system_id),
                            0.0,
                            eff_tidi,
                            eff_hist,
                        ) <= 1e-6
                        {
                            cur = None;
                        }
                    }
                }
                _ => {}
            }
        }
        cur
    }

    pub(in crate::system_actor) fn overlay_key(&self, key: (i64, i64, u32), base_entry: Option<ModuleCooldownEntry>, solar_system_id: SolarSystemId, tidi_factor: f64, has_tidi_history: bool) -> Option<ModuleCooldownEntry> {
        if !self.has_pending_transitions() { return base_entry; }
        let guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        Self::overlay_key_in_state(&guard, key, base_entry, solar_system_id, tidi_factor, has_tidi_history)
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
            .field("has_db", &self.db_opt.is_some()).field("epoch", &self.epoch)
            .field("seq", &self.seq).field("entries_len", &self.entries.len())
            .field("boot_db_pending_merge", &self.boot_db_pending_merge).finish()
    }
}

#[derive(Debug, Clone)]
pub(in crate::system_actor) struct RecordModuleCooldownReq {
    pub ship_id: i64, pub module_id: i64, pub module_type_id: u32,
    pub now_unix_f64: f64, pub delay_s: f64, pub tidi_factor: f64, pub has_tidi_history: bool,
}

fn compute_recorded_cooldown_entry(solar_system_id: SolarSystemId, prev: Option<&ModuleCooldownEntry>, now_unix_f64: f64, delay_s: f64, cur_sim_s: f64, sys_tidi: f64, rec_tidi: f64, sys_hist: bool, rec_hist: bool) -> (ModuleCooldownEntry, f64, bool) {
    let eff_tidi = if sys_tidi < 0.999_999 { sys_tidi } else { rec_tidi };
    let eff_hist = sys_hist || rec_hist || eff_tidi < 0.999_999;
    let prev_rem = prev.map(|e| remaining_cooldown_secs_with_tidi(e, now_unix_f64, Some(solar_system_id), cur_sim_s, eff_tidi, eff_hist)).unwrap_or(0.0);
    (ModuleCooldownEntry { recorded_unix_f64: now_unix_f64, delay_s: prev_rem.max(delay_s), solar_system_id: Some(solar_system_id), recorded_sys_sim_s: cur_sim_s }, eff_tidi, eff_hist)
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

pub(crate) fn serialize_module_cooldown_rows(cooldowns: &HashMap<(i64, i64, u32), ModuleCooldownEntry>) -> Vec<ModuleCooldownCheckpointRow> {
    let mut sorted: Vec<_> = cooldowns.iter().map(|(&k, &v)| (k, v)).collect();
    sorted.sort_by_key(|(k, _)| *k);
    sorted.into_iter().map(|(k @ (ship_id, mod_id, mod_tid), entry)| (module_cooldown_checkpoint_key(ship_id, mod_id, mod_tid), (k, entry))).collect()
}

pub(in crate::system_actor) fn can_write_sqlite_safely() -> bool {
    !held_lock_ranks().iter().any(|&r| r >= RANK_WORLD_INNER)
}

fn persist_module_cooldowns_rows_in_db(db: &WorldRepository, solar_system_id: SolarSystemId, rows: &[ModuleCooldownCheckpointRow]) -> bool {
    if !can_write_sqlite_safely() { return false; }
    let now_unix = current_unix_secs();
    db.with_sqlite_conn(|conn| {
        ensure_system_checkpoint_schema(conn)?;
        replace_system_checkpoints_for_kind(conn, solar_system_id, KIND_MODULE_COOLDOWNS, rows, now_unix)
    }).is_ok()
}

/// Post-unlock DB writer replacing `KIND_MODULE_COOLDOWNS` rows for a single `solar_system_id`.
pub(crate) fn persist_system_module_cooldowns_in_db(db: &WorldRepository, solar_system_id: SolarSystemId, rows: &[ModuleCooldownCheckpointRow]) -> bool {
    solar_system_id != 0 && persist_module_cooldowns_rows_in_db(db, solar_system_id, rows)
}

fn collect_cold_cooldown_rows(conn: &rusqlite::Connection, solar_system_id: SolarSystemId) -> HashMap<(i64, i64, u32), ModuleCooldownEntry> {
    let Ok(rows) = load_system_checkpoints::<((i64, i64, u32), ModuleCooldownEntry)>(conn, solar_system_id, KIND_MODULE_COOLDOWNS) else {
        return HashMap::new();
    };
    rows.into_iter().map(|(_, (k, v))| (k, v)).collect()
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

/// Nonblocking counterpart to [`load_cold_system_module_cooldowns_from_db`] for actor
/// activation paths (`prepare_system_activation`), returning `None` immediately when
/// `db.writer` is held by another thread (`bf_1170` deadlock avoidance).
pub(crate) fn load_cold_system_module_cooldowns_from_db_nonblocking(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
) -> Option<HashMap<(i64, i64, u32), ModuleCooldownEntry>> {
    db.try_with_sqlite_conn(|conn| collect_cold_cooldown_rows(conn, solar_system_id))
}

fn filter_active_cooldowns_with_raw_for_boot(
    db: &WorldRepository,
    raw: HashMap<(i64, i64, u32), ModuleCooldownEntry>,
    solar_system_id: SolarSystemId,
    tidi_factor: f64,
    has_tidi_history: bool,
    eval_sys_sim_s: f64,
    anchor_sys_sim_s: f64,
) -> (
    HashMap<(i64, i64, u32), ModuleCooldownEntry>,
    HashMap<(i64, i64, u32), (f64, f64)>,
) {
    let raw_boot_map = raw.iter().map(|(&k, e)| (k, (e.recorded_unix_f64, e.delay_s))).collect();
    let now_unix_f64 = db.clock().now_unix_secs() as f64;
    let mut active = HashMap::new();
    for (key, entry) in raw {
        let rem = remaining_cooldown_secs_with_tidi(
            &entry, now_unix_f64, Some(solar_system_id), eval_sys_sim_s, tidi_factor, has_tidi_history,
        );
        if rem > 1e-6 {
            active.insert(
                key,
                ModuleCooldownEntry {
                    recorded_unix_f64: now_unix_f64, delay_s: rem,
                    solar_system_id: Some(solar_system_id), recorded_sys_sim_s: anchor_sys_sim_s,
                },
            );
        }
    }
    (active, raw_boot_map)
}

pub(crate) fn load_active_system_module_cooldowns_for_boot_nonblocking(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
    tidi_factor: f64,
    has_tidi_history: bool,
    eval_sys_sim_s: f64,
    anchor_sys_sim_s: f64,
) -> Option<(HashMap<(i64, i64, u32), ModuleCooldownEntry>, HashMap<(i64, i64, u32), (f64, f64)>)> {
    if solar_system_id == 0 {
        return Some((HashMap::new(), HashMap::new()));
    }
    let raw = load_cold_system_module_cooldowns_from_db_nonblocking(db, solar_system_id)?;
    Some(filter_active_cooldowns_with_raw_for_boot(db, raw, solar_system_id, tidi_factor, has_tidi_history, eval_sys_sim_s, anchor_sys_sim_s))
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
        remaining_cooldown_secs_with_tidi(entry, now_unix_f64, Some(solar_system_id), cur_sys_sim_s, tidi_factor, has_tidi_history) > 1e-6
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
        let rem = remaining_cooldown_secs_with_tidi(entry, now_unix_f64, Some(solar_system_id), cur_sys_sim_s, tidi_factor, has_tidi_history);
        if rem > 1e-6 {
            let next = ModuleCooldownEntry { recorded_unix_f64: now_unix_f64, delay_s: rem, solar_system_id: Some(solar_system_id), recorded_sys_sim_s: 0.0 };
            changed |= next != *entry;
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

#[derive(Debug, Clone)]
pub struct PersistedCooldownCommit {
    pub epoch: u64,
    pub seq: u64,
    pub boot_db_pending_merge: bool,
    pub merged_from_db: HashMap<(i64, i64, u32), ModuleCooldownEntry>,
}

/// Applies post-commit DB merge/clean flags onto `SystemActorState` on the actor loop (`#1541`).
pub(in crate::system_actor) fn apply_persisted_cooldown_commit_to_state(
    st: &mut SystemActorState,
    commit: &PersistedCooldownCommit,
) -> bool {
    if st.module_cooldown_persist_epoch != commit.epoch {
        return false;
    }
    let mut view_changed = false;
    let is_latest_seq = st.module_cooldown_checkpoint_seq == commit.seq;
    if commit.boot_db_pending_merge && !st.module_cooldowns.pending_db_clear_all {
        let cur_sim_s = st.sim_elapsed_secs;
        for (&k, &merged_entry) in &commit.merged_from_db {
            if st.module_cooldowns.pending_db_cleared_ships.contains(&k.0)
                || st.module_cooldowns.pending_db_removed_keys.contains(&k)
                || (!is_latest_seq && st.module_cooldowns.reactivation_cooldowns.contains_key(&k))
            {
                continue;
            }
            st.module_cooldowns.reactivation_cooldowns.insert(k, ModuleCooldownEntry { recorded_sys_sim_s: cur_sim_s, ..merged_entry });
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
    view_changed
}

/// Serializes, sequence-guards, merges pending pre-boot cold rows (if any), and commits a
/// per-system module cooldown checkpoint cleanly outside actor locks (`#1541`).
pub(crate) fn persist_cooldown_payload_with_gate_in_db(
    solar_system_id: SolarSystemId,
    gate: &CooldownPersistGate,
    payload: ModuleCooldownPersistPayload,
) -> Option<PersistedCooldownCommit> {
    if solar_system_id == 0 || !can_write_sqlite_safely() {
        return None;
    }
    let Some(ref db) = payload.db_opt else { return None; };
    let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
    if gate_guard.last_persisted.is_some_and(|prev| (payload.epoch, payload.seq) <= prev) {
        return None;
    }
    let mut final_entries = payload.entries;
    let mut merged_from_db = HashMap::new();
    let now_unix_f64 = db.clock().now_unix_secs() as f64;
    if payload.boot_db_pending_merge && !payload.pending_db_clear_all {
        for (k @ (sid, _, _), entry) in load_cold_system_module_cooldowns_from_db(db, solar_system_id) {
            if payload.pending_db_cleared_ships.contains(&sid) || payload.pending_db_removed_keys.contains(&k) {
                continue;
            }
            let db_rem = remaining_cooldown_secs_with_tidi(&entry, now_unix_f64, Some(solar_system_id), 0.0, payload.tidi_factor, payload.has_tidi_history);
            if db_rem <= 1e-6 { continue; }
            let normalized = ModuleCooldownEntry { recorded_unix_f64: now_unix_f64, delay_s: db_rem, solar_system_id: Some(solar_system_id), recorded_sys_sim_s: 0.0 };
            let take_db = final_entries.get(&k).map_or(true, |live| {
                db_rem > remaining_cooldown_secs_with_tidi(live, now_unix_f64, Some(solar_system_id), payload.cur_sys_sim_s, payload.tidi_factor, payload.has_tidi_history) + 1e-6
            });
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
    committed.then_some(PersistedCooldownCommit {
        epoch: payload.epoch,
        seq: payload.seq,
        boot_db_pending_merge: payload.boot_db_pending_merge,
        merged_from_db,
    })
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

#[cfg(any(test, feature = "testkit"))]
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

pub(in crate::system_actor) fn apply_pending_transitions_to_actor_state(
    solar_system_id: SolarSystemId,
    st: &mut SystemActorState,
    tidi_factor: f64,
    has_tidi_history: bool,
    transitions: Vec<PendingCooldownTransition>,
) -> bool {
    if transitions.is_empty() {
        return false;
    }
    for tr in transitions {
        match tr {
            PendingCooldownTransition::Record {
                ship_id, module_id, module_type_id, now_unix_f64, delay_s,
                tidi_factor: tr_tidi, has_tidi_history: tr_hist,
            } => {
                if tr_tidi < 0.999_999 || tr_hist {
                    st.has_tidi_history = true;
                }
                let eff_tidi = if tidi_factor < 0.999_999 { tidi_factor } else { tr_tidi };
                let eff_hist = st.has_tidi_history || has_tidi_history || tr_hist || eff_tidi < 0.999_999;
                let _ = record_module_cooldown_in_actor_state(
                    solar_system_id, st, ship_id, module_id, module_type_id,
                    now_unix_f64, delay_s, eff_tidi, eff_hist,
                );
                continue;
            }
            PendingCooldownTransition::Seed { key, entry } => {
                if st.module_cooldowns.boot_db_pending_merge {
                    st.module_cooldowns.pending_db_removed_keys.insert(key);
                } else {
                    st.module_cooldowns.pending_db_removed_keys.remove(&key);
                }
                st.module_cooldowns.reactivation_cooldowns.insert(key, entry);
            }
            PendingCooldownTransition::Remove { key } => {
                st.module_cooldowns.reactivation_cooldowns.remove(&key);
                if st.module_cooldowns.boot_db_pending_merge {
                    st.module_cooldowns.pending_db_removed_keys.insert(key);
                }
            }
            PendingCooldownTransition::ClearShip { ship_id } => {
                st.module_cooldowns.reactivation_cooldowns.retain(|&(sid, _, _), _| sid != ship_id);
                if st.module_cooldowns.boot_db_pending_merge {
                    st.module_cooldowns.pending_db_cleared_ships.insert(ship_id);
                    st.module_cooldowns.pending_db_removed_keys.retain(|&(sid, _, _)| sid != ship_id);
                }
            }
            PendingCooldownTransition::PruneExpired { now_unix_f64, tidi_factor: prune_tidi, has_tidi_history: prune_hist } => {
                if prune_tidi < 0.999_999 || prune_hist { st.has_tidi_history = true; }
                let eff_tidi = if tidi_factor < 0.999_999 { tidi_factor } else { prune_tidi };
                let eff_hist = st.has_tidi_history || has_tidi_history || prune_hist || eff_tidi < 0.999_999;
                let cur_sim_s = st.sim_elapsed_secs;
                let _ = prune_expired_module_cooldowns_in_state(&mut st.module_cooldowns, solar_system_id, now_unix_f64, cur_sim_s, eff_tidi, eff_hist);
            }
            PendingCooldownTransition::ClearAll => {
                st.module_cooldowns.reactivation_cooldowns.clear();
                st.module_cooldowns.boot_db_pending_merge = false;
                st.module_cooldowns.pending_db_clear_all = true;
                st.module_cooldowns.pending_db_cleared_ships.clear();
                st.module_cooldowns.pending_db_removed_keys.clear();
            }
        }
        st.module_cooldown_checkpoint_dirty = true;
        st.module_cooldown_checkpoint_seq = st.module_cooldown_checkpoint_seq.saturating_add(1);
    }
    true
}

pub(in crate::system_actor) fn apply_pending_transitions_to_cold_map(
    solar_system_id: SolarSystemId,
    map: &mut HashMap<(i64, i64, u32), ModuleCooldownEntry>,
    tidi_factor: f64,
    has_tidi_history: bool,
    transitions: &[PendingCooldownTransition],
) -> bool {
    let mut changed = false;
    for tr in transitions {
        match *tr {
            PendingCooldownTransition::Record { ship_id, module_id, module_type_id, now_unix_f64, delay_s, tidi_factor: tr_tidi, has_tidi_history: tr_hist } => {
                if !delay_s.is_finite() || delay_s <= 0.0 || solar_system_id == 0 { continue; }
                let key = (ship_id, module_id, module_type_id);
                let (entry, eff_tidi, eff_hist) = compute_recorded_cooldown_entry(solar_system_id, map.get(&key), now_unix_f64, delay_s, 0.0, tidi_factor, tr_tidi, has_tidi_history, tr_hist);
                map.retain(|_, e| remaining_cooldown_secs_with_tidi(e, now_unix_f64, Some(solar_system_id), 0.0, eff_tidi, eff_hist) > 1e-6);
                map.insert(key, entry);
                changed = true;
            }
            PendingCooldownTransition::Seed { key, entry } => { map.insert(key, entry); changed = true; }
            PendingCooldownTransition::Remove { key } => { changed |= map.remove(&key).is_some(); }
            PendingCooldownTransition::ClearShip { ship_id } => {
                let before = map.len();
                map.retain(|&(sid, _, _), _| sid != ship_id);
                changed |= map.len() != before;
            }
            PendingCooldownTransition::PruneExpired { now_unix_f64, tidi_factor: prune_tidi, has_tidi_history: prune_hist } => {
                let eff_tidi = if tidi_factor < 0.999_999 { tidi_factor } else { prune_tidi };
                let eff_hist = has_tidi_history || prune_hist || eff_tidi < 0.999_999;
                let before = map.len();
                map.retain(|_, e| remaining_cooldown_secs_with_tidi(e, now_unix_f64, Some(solar_system_id), 0.0, eff_tidi, eff_hist) > 1e-6);
                changed |= map.len() != before;
            }
            PendingCooldownTransition::ClearAll => if !map.is_empty() { map.clear(); changed = true; },
        }
    }
    changed
}

pub(in crate::system_actor) fn flush_cold_transitions_under_gate(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
    gate: &CooldownPersistGate,
    gate_guard: &mut CooldownPersistGateState,
    tidi_factor: f64,
    has_tidi_history: bool,
    transitions: &[PendingCooldownTransition],
) -> bool {
    let allow_cluster = solar_system_id == CLUSTER_SYSTEM_ID
        && transitions.iter().all(|tr| !matches!(tr, PendingCooldownTransition::Record { .. } | PendingCooldownTransition::Seed { .. }));
    if transitions.is_empty() || (solar_system_id == 0 && !allow_cluster) || !can_write_sqlite_safely() {
        return false;
    }
    let mut map = load_cold_system_module_cooldowns_from_db(db, solar_system_id);
    if !apply_pending_transitions_to_cold_map(solar_system_id, &mut map, tidi_factor, has_tidi_history, transitions) {
        return false;
    }
    let cur_epoch = gate.current_epoch();
    let rows = serialize_module_cooldown_rows(&map);
    let ok = persist_module_cooldowns_rows_in_db(db, solar_system_id, &rows);
    if ok {
        gate.commit_cold_epoch(gate_guard, cur_epoch);
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

fn commit_migrated_cooldown_map_with_gate(
    systems: &SystemRegistry,
    db: &WorldRepository,
    sys_id: u32,
    migrated: HashMap<(i64, i64, u32), ModuleCooldownEntry>,
    prune_cfg: Option<(f64, f64, bool)>,
    unresolved_cluster_rows: Option<&[ModuleCooldownCheckpointRow]>,
) {
    if sys_id == CLUSTER_SYSTEM_ID {
        let Some(rows) = unresolved_cluster_rows else { return };
        let gate = systems.cooldown_persist_gate(CLUSTER_SYSTEM_ID);
        let mut cg = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
        let cur_epoch = gate.current_epoch();
        if persist_module_cooldowns_rows_in_db(db, CLUSTER_SYSTEM_ID, rows) {
            gate.commit_cold_epoch(&mut cg, cur_epoch);
        }
        return;
    }
    let (eval_now, eval_tidi, eval_hist) = prune_cfg.unwrap_or_else(|| {
        let (tf, hist) = systems.resolve_effective_tidi_mode(sys_id, true);
        (db.clock().now_unix_secs() as f64, tf, hist)
    });
    for _ in 0..4 {
        let gate = systems.cooldown_persist_gate(sys_id);
        let mut cg = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
        match cg.phase {
            CooldownGatePhase::Cold => {
                let mut cur_map = load_cold_system_module_cooldowns_from_db(db, sys_id);
                let mut changed = false;
                if let Some((p_now, p_tidi, p_hist)) = prune_cfg {
                    let before = cur_map.len();
                    cur_map.retain(|_, e| remaining_cooldown_secs_with_tidi(e, p_now, Some(sys_id), 0.0, p_tidi, p_hist) > 1e-6);
                    changed |= cur_map.len() != before;
                    for entry in cur_map.values_mut() {
                        if entry.solar_system_id.is_none() {
                            entry.solar_system_id.get_or_insert(sys_id);
                            changed = true;
                        }
                    }
                }
                for (&k, &entry) in &migrated {
                    let norm = ModuleCooldownEntry { solar_system_id: entry.solar_system_id.or(Some(sys_id)), ..entry };
                    let inc_rem = remaining_cooldown_secs_with_tidi(&norm, eval_now, Some(sys_id), 0.0, eval_tidi, eval_hist);
                    if inc_rem <= 1e-6 { continue; }
                    let cur_rem = cur_map.get(&k).map(|cur| remaining_cooldown_secs_with_tidi(cur, eval_now, Some(sys_id), 0.0, eval_tidi, eval_hist));
                    if cur_rem.is_none_or(|rem| inc_rem > rem + 1e-6) {
                        cur_map.insert(k, norm);
                        changed = true;
                    } else if let Some(cur) = cur_map.get_mut(&k).filter(|c| c.solar_system_id.is_none()) {
                        cur.solar_system_id.get_or_insert(sys_id);
                        changed = true;
                    }
                }
                if changed {
                    let cur_epoch = gate.current_epoch();
                    let rows = serialize_module_cooldown_rows(&cur_map);
                    if persist_system_module_cooldowns_in_db(db, sys_id, &rows) {
                        gate.commit_cold_epoch(&mut cg, cur_epoch);
                    }
                }
                break;
            }
            CooldownGatePhase::Loading | CooldownGatePhase::Stopping => {
                if let Some((now_unix_f64, tidi_factor, has_tidi_history)) = prune_cfg {
                    if tidi_factor < 0.999_999 || has_tidi_history { gate.mark_tidi_history(); }
                    cg.pending_transitions.push(PendingCooldownTransition::PruneExpired { now_unix_f64, tidi_factor, has_tidi_history });
                    gate.mark_pending_transitions();
                }
                for (&(ship_id, module_id, module_type_id), &entry) in &migrated {
                    let rem = remaining_cooldown_secs_with_tidi(&entry, eval_now, Some(sys_id), 0.0, eval_tidi, eval_hist);
                    if rem <= 1e-6 { continue; }
                    if eval_tidi < 0.999_999 || eval_hist { gate.mark_tidi_history(); }
                    cg.pending_transitions.push(PendingCooldownTransition::Record {
                        ship_id, module_id, module_type_id, now_unix_f64: eval_now, delay_s: rem, tidi_factor: eval_tidi, has_tidi_history: eval_hist,
                    });
                    gate.mark_pending_transitions();
                }
                break;
            }
            CooldownGatePhase::Live => {
                drop(cg);
                let Some(handle) = systems.get_handle(sys_id) else {
                    if !systems.try_reclaim_dead_live_slot_to_cold(sys_id) {
                        std::thread::yield_now();
                    }
                    continue;
                };
                if let Some((p_now, p_tidi, p_hist)) = prune_cfg {
                    if handle.prune_expired_module_cooldowns_sync(p_now, p_tidi, p_hist, migrated.is_empty()).is_err() { std::thread::yield_now(); continue; }
                }
                for (&(ship_id, module_id, module_type_id), &entry) in &migrated {
                    let rem = remaining_cooldown_secs_with_tidi(&entry, eval_now, Some(sys_id), 0.0, eval_tidi, eval_hist);
                    if rem > 1e-6 && handle.record_module_cooldown_sync(ship_id, module_id, module_type_id, eval_now, rem, eval_tidi, eval_hist).is_err() {
                        systems.record_cold_system_module_cooldown_in_db(db, sys_id, ship_id, module_id, module_type_id, eval_now, rem, eval_tidi, eval_hist);
                    }
                }
                break;
            }
        }
    }
}

/// Prunes expired/future-stale `KIND_MODULE_COOLDOWNS` rows on startup and performs a clean
/// one-way migration of any legacy `CLUSTER_SYSTEM_ID` rows into their owning solar systems,
/// serialized per system through [`CooldownPersistGate`] (`#1541`).
pub(crate) fn migrate_and_prune_module_cooldowns_in_db(systems: &SystemRegistry, db: &WorldRepository, now_unix_f64: f64) {
    if !can_write_sqlite_safely() { return; }
    let Ok(all_rows) = db.with_sqlite_conn(|conn| {
        ensure_system_checkpoint_schema(conn)?;
        load_all_checkpoints_for_kind::<((i64, i64, u32), ModuleCooldownEntry)>(conn, KIND_MODULE_COOLDOWNS)
    }) else { return; };
    if all_rows.is_empty() { return; }

    // Resolve ship -> system strictly inside snapshot BEFORE calling `resolve_effective_tidi_mode`
    // (`RANK_WORLD_INNER` cannot re-enter `db.with_snapshot`).
    let mut staged = Vec::with_capacity(all_rows.len());
    db.with_snapshot(|snap| {
        for (row_sys_id, _, (key @ (ship_id, _, _), entry)) in all_rows {
            let resolved_opt = (row_sys_id == CLUSTER_SYSTEM_ID).then(|| resolve_legacy_cooldown_system_id(snap, ship_id, &entry)).flatten();
            staged.push((row_sys_id, key, entry, resolved_opt));
        }
    });

    let mut per_system_migrated: BTreeMap<u32, HashMap<(i64, i64, u32), ModuleCooldownEntry>> = BTreeMap::new();
    let mut tidi_cache: HashMap<u32, (f64, bool)> = HashMap::new();
    let mut dirty_systems: BTreeSet<u32> = BTreeSet::new();
    let mut unresolved_legacy: Vec<ModuleCooldownCheckpointRow> = Vec::new();
    let mut had_cluster_rows = false;
    let mut insert_migrated = |target_sys: u32, key: (i64, i64, u32), norm: ModuleCooldownEntry, tf: f64, hist: bool| {
        let map = per_system_migrated.entry(target_sys).or_default();
        let inc_rem = remaining_cooldown_secs_with_tidi(&norm, now_unix_f64, Some(target_sys), 0.0, tf, hist);
        if map.get(&key).is_none_or(|cur| inc_rem > remaining_cooldown_secs_with_tidi(cur, now_unix_f64, Some(target_sys), 0.0, tf, hist) + 1e-6) {
            map.insert(key, norm);
        }
    };

    for (row_sys_id, key @ (ship_id, mod_id, mod_tid), entry, resolved_opt) in staged {
        let check_sys = if row_sys_id == CLUSTER_SYSTEM_ID { had_cluster_rows = true; resolved_opt } else { Some(row_sys_id) };
        let (tidi_factor, has_tidi_history) = check_sys
            .map(|sid| *tidi_cache.entry(sid).or_insert_with(|| systems.resolve_effective_tidi_mode(sid, true)))
            .unwrap_or((1.0, false));
        let normalized = if row_sys_id > 0 && entry.solar_system_id.is_none() {
            dirty_systems.insert(row_sys_id);
            ModuleCooldownEntry { solar_system_id: Some(row_sys_id), ..entry }
        } else {
            entry
        };
        if remaining_cooldown_secs_with_tidi(&normalized, now_unix_f64, check_sys, 0.0, tidi_factor, has_tidi_history) <= 1e-6 {
            if row_sys_id > 0 { dirty_systems.insert(row_sys_id); }
            continue;
        }
        if row_sys_id == CLUSTER_SYSTEM_ID {
            if let Some(target_sys) = resolved_opt {
                let norm = ModuleCooldownEntry { solar_system_id: Some(target_sys), ..entry };
                insert_migrated(target_sys, key, norm, tidi_factor, has_tidi_history);
                dirty_systems.insert(target_sys);
                tidi_cache.entry(target_sys).or_insert_with(|| systems.resolve_effective_tidi_mode(target_sys, true));
            } else {
                unresolved_legacy.push((module_cooldown_checkpoint_key(ship_id, mod_id, mod_tid), (key, entry)));
            }
        }
    }

    if had_cluster_rows { dirty_systems.insert(CLUSTER_SYSTEM_ID); }
    for sys_id in dirty_systems {
        let migrated = per_system_migrated.remove(&sys_id).unwrap_or_default();
        let prune_cfg = (sys_id != CLUSTER_SYSTEM_ID).then(|| {
            let (tf, hist) = tidi_cache.get(&sys_id).copied().unwrap_or_else(|| systems.resolve_effective_tidi_mode(sys_id, true));
            (now_unix_f64, tf, hist)
        });
        commit_migrated_cooldown_map_with_gate(systems, db, sys_id, migrated, prune_cfg, Some(&unresolved_legacy));
    }
}

/// One-way importer for legacy monolith JSON cooldowns on boot, serialized per system via
/// [`CooldownPersistGate`] (`#1541`).
pub(crate) fn import_legacy_monolith_module_cooldowns_in_db(
    systems: &SystemRegistry, db: &WorldRepository, now_unix_f64: f64,
    legacy_entries: Vec<((i64, i64, u32), ModuleCooldownEntry)>,
) {
    if legacy_entries.is_empty() || !can_write_sqlite_safely() { return; }
    let mut staged = Vec::with_capacity(legacy_entries.len());
    db.with_snapshot(|snap| {
        for (key @ (ship_id, _, _), entry) in legacy_entries {
            staged.push((key, entry, resolve_legacy_cooldown_system_id(snap, ship_id, &entry)));
        }
    });

    let mut tidi_cache: HashMap<u32, (f64, bool)> = HashMap::new();
    let mut per_system: BTreeMap<u32, HashMap<(i64, i64, u32), ModuleCooldownEntry>> = BTreeMap::new();
    let mut cluster_rows: Vec<ModuleCooldownCheckpointRow> = Vec::new();
    for (key @ (ship_id, mod_id, mod_tid), entry, sys_opt) in staged {
        let (tidi_factor, has_tidi_history) = sys_opt
            .map(|sid| *tidi_cache.entry(sid).or_insert_with(|| systems.resolve_effective_tidi_mode(sid, true)))
            .unwrap_or((1.0, false));
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
        if target_sys == CLUSTER_SYSTEM_ID {
            cluster_rows.push((
                module_cooldown_checkpoint_key(ship_id, mod_id, mod_tid),
                (key, normalized),
            ));
        } else {
            per_system.entry(target_sys).or_default().insert(key, normalized);
        }
    }
    if !cluster_rows.is_empty() {
        commit_migrated_cooldown_map_with_gate(
            systems,
            db,
            CLUSTER_SYSTEM_ID,
            HashMap::new(),
            None,
            Some(&cluster_rows),
        );
    }
    for (sys_id, map) in per_system {
        commit_migrated_cooldown_map_with_gate(systems, db, sys_id, map, None, None);
    }
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
    let _ = prune_expired_module_cooldowns_in_state(&mut actor_st.module_cooldowns, solar_system_id, now_unix_f64, cur_sys_sim_s, tidi_factor, has_tidi_history);
    let key = (ship_id, module_id, module_type_id);
    let effective_delay = actor_st.module_cooldowns.reactivation_cooldowns.get(&key)
        .map(|e| remaining_cooldown_secs_with_tidi(e, now_unix_f64, Some(solar_system_id), cur_sys_sim_s, tidi_factor, has_tidi_history).max(delay_s))
        .unwrap_or(delay_s);
    actor_st.module_cooldowns.reactivation_cooldowns.insert(key, ModuleCooldownEntry {
        recorded_unix_f64: now_unix_f64, delay_s: effective_delay, solar_system_id: Some(solar_system_id), recorded_sys_sim_s: cur_sys_sim_s,
    });
    actor_st.module_cooldown_checkpoint_dirty = true;
    actor_st.module_cooldown_checkpoint_seq = actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    Some(build_persist_payload_from_actor_state(actor_st, tidi_factor, has_tidi_history))
}

pub(in crate::system_actor) fn record_module_cooldown_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    req: RecordModuleCooldownReq,
) -> Result<ModuleCooldownPersistPayload, String> {
    if let Some(payload) = record_module_cooldown_in_actor_state(
        ctx.solar_system_id, ctx.actor_st, req.ship_id, req.module_id, req.module_type_id,
        req.now_unix_f64, req.delay_s, req.tidi_factor, req.has_tidi_history,
    ) {
        return Ok(payload);
    }
    Ok(build_persist_payload_from_actor_state(ctx.actor_st, req.tidi_factor, req.has_tidi_history))
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
    ctx.actor_st.module_cooldowns.reactivation_cooldowns.insert(key, entry);
    ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    ctx.actor_st.module_cooldown_checkpoint_seq = ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    Ok(build_persist_payload_from_actor_state(ctx.actor_st, ctx.tidi_factor, ctx.actor_st.has_tidi_history))
}

pub(in crate::system_actor) fn remove_module_cooldown_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    key: (i64, i64, u32),
) -> Result<Option<ModuleCooldownPersistPayload>, String> {
    let had_entry = ctx.actor_st.module_cooldowns.reactivation_cooldowns.remove(&key).is_some();
    if ctx.actor_st.module_cooldowns.boot_db_pending_merge {
        ctx.actor_st.module_cooldowns.pending_db_removed_keys.insert(key);
    } else if !had_entry {
        return Ok(None);
    }
    ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    ctx.actor_st.module_cooldown_checkpoint_seq = ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    Ok(Some(build_persist_payload_from_actor_state(ctx.actor_st, ctx.tidi_factor, ctx.actor_st.has_tidi_history)))
}

pub(in crate::system_actor) fn clear_ship_module_cooldowns_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ship_id: i64,
) -> Result<Option<ModuleCooldownPersistPayload>, String> {
    let before = ctx.actor_st.module_cooldowns.reactivation_cooldowns.len();
    ctx.actor_st.module_cooldowns.reactivation_cooldowns.retain(|&(sid, _, _), _| sid != ship_id);
    if ctx.actor_st.module_cooldowns.boot_db_pending_merge {
        ctx.actor_st.module_cooldowns.pending_db_cleared_ships.insert(ship_id);
        ctx.actor_st.module_cooldowns.pending_db_removed_keys.retain(|&(sid, _, _)| sid != ship_id);
    } else if ctx.actor_st.module_cooldowns.reactivation_cooldowns.len() == before {
        return Ok(None);
    }
    ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    ctx.actor_st.module_cooldown_checkpoint_seq = ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    Ok(Some(build_persist_payload_from_actor_state(ctx.actor_st, ctx.tidi_factor, ctx.actor_st.has_tidi_history)))
}

pub(in crate::system_actor) fn clear_all_module_cooldowns_in_actor(
    ctx: &mut BallOpsCtx<'_>,
) -> Result<ModuleCooldownPersistPayload, String> {
    let mc = &mut ctx.actor_st.module_cooldowns;
    if !mc.reactivation_cooldowns.is_empty() || mc.boot_db_pending_merge || ctx.actor_st.module_cooldown_checkpoint_dirty {
        mc.reactivation_cooldowns.clear();
        mc.boot_db_pending_merge = false;
        mc.pending_db_clear_all = true;
        mc.pending_db_cleared_ships.clear();
        mc.pending_db_removed_keys.clear();
        ctx.actor_st.module_cooldown_checkpoint_dirty = true;
        ctx.actor_st.module_cooldown_checkpoint_seq = ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    }
    Ok(build_persist_payload_from_actor_state(ctx.actor_st, ctx.tidi_factor, ctx.actor_st.has_tidi_history))
}

pub(in crate::system_actor) fn replace_module_cooldowns_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    mut cooldowns: HashMap<(i64, i64, u32), ModuleCooldownEntry>,
    expected_gate_token: Option<(u64, Option<(u64, u64)>)>,
) -> Result<(), String> {
    if let Some((exp_epoch, exp_last)) = expected_gate_token {
        let gate = ctx.cooldown_persist_gate.ok_or_else(|| "missing cooldown persist gate".to_string())?;
        let mut cg = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
        let actor_epoch = ctx.actor_st.module_cooldown_persist_epoch;
        let uncommitted_seq = ctx.actor_st.module_cooldown_checkpoint_seq > 0
            && match cg.last_persisted {
                Some((ep, seq)) => ep != actor_epoch || ctx.actor_st.module_cooldown_checkpoint_seq > seq,
                None => true,
            };
        if gate.current_epoch() != exp_epoch
            || actor_epoch != exp_epoch
            || cg.last_persisted != exp_last
            || ctx.actor_st.module_cooldown_checkpoint_dirty
            || ctx.actor_st.module_cooldowns.boot_db_pending_merge
            || uncommitted_seq
        {
            return Err("STALE_REHYDRATE_TOKEN".to_string());
        }
        if ctx.actor_st.module_cooldown_checkpoint_seq > 0 {
            return Ok(());
        }
        let next_epoch = gate.bump_epoch();
        let next_seq = ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
        cg.last_persisted = Some((next_epoch, next_seq));
        ctx.actor_st.module_cooldown_persist_epoch = next_epoch;
        if gate.has_tidi_history() || ctx.tidi_factor < 0.999_999 {
            ctx.actor_st.has_tidi_history = true;
        }
        let cur_sim_s = ctx.actor_st.sim_elapsed_secs;
        let now_unix_f64 = ctx.actor_st.db_opt.as_ref().map(|db| db.clock().now_unix_secs() as f64).unwrap_or_else(|| current_unix_secs() as f64);
        let existing_map = &ctx.actor_st.module_cooldowns.reactivation_cooldowns;
        let mut merged = HashMap::new();
        for (k, raw_entry) in cooldowns {
            let raw_rem = remaining_cooldown_secs_with_tidi(&raw_entry, now_unix_f64, Some(ctx.solar_system_id), 0.0, ctx.tidi_factor, ctx.actor_st.has_tidi_history);
            let boot_unchanged_or_older = cg.boot_raw_cooldowns.get(&k).is_some_and(|&(br_rec, br_del)| {
                raw_entry.recorded_unix_f64 <= br_rec + 1e-6 && raw_entry.delay_s <= br_del + 1e-6
            });
            if let Some(p) = existing_map.get(&k) {
                let live_rem = remaining_cooldown_secs_with_tidi(p, now_unix_f64, Some(ctx.solar_system_id), cur_sim_s, ctx.tidi_factor, ctx.actor_st.has_tidi_history);
                if boot_unchanged_or_older {
                    if live_rem > 1e-6 {
                        merged.insert(k, ModuleCooldownEntry { solar_system_id: p.solar_system_id.or(Some(ctx.solar_system_id)), ..*p });
                    }
                } else if live_rem > 1e-6 && (raw_rem <= live_rem + 1e-6 || (raw_entry.recorded_unix_f64 <= p.recorded_unix_f64 + 1e-6 && raw_entry.delay_s <= p.delay_s + 1e-6)) {
                    merged.insert(k, ModuleCooldownEntry { solar_system_id: p.solar_system_id.or(Some(ctx.solar_system_id)), ..*p });
                } else if raw_rem > 1e-6 {
                    merged.insert(k, ModuleCooldownEntry { recorded_unix_f64: now_unix_f64, delay_s: raw_rem, solar_system_id: Some(ctx.solar_system_id), recorded_sys_sim_s: cur_sim_s });
                    cg.boot_raw_cooldowns.insert(k, (raw_entry.recorded_unix_f64, raw_entry.delay_s));
                }
            } else if !boot_unchanged_or_older && raw_rem > 1e-6 {
                merged.insert(k, ModuleCooldownEntry { recorded_unix_f64: now_unix_f64, delay_s: raw_rem, solar_system_id: Some(ctx.solar_system_id), recorded_sys_sim_s: cur_sim_s });
                cg.boot_raw_cooldowns.insert(k, (raw_entry.recorded_unix_f64, raw_entry.delay_s));
            }
        }
        for (k, p) in existing_map {
            if merged.contains_key(k) { continue; }
            let live_rem = remaining_cooldown_secs_with_tidi(p, now_unix_f64, Some(ctx.solar_system_id), cur_sim_s, ctx.tidi_factor, ctx.actor_st.has_tidi_history);
            if live_rem > 1e-6 {
                merged.insert(*k, ModuleCooldownEntry { solar_system_id: p.solar_system_id.or(Some(ctx.solar_system_id)), ..*p });
            }
        }
        cooldowns = merged;
        ctx.actor_st.module_cooldown_checkpoint_seq = next_seq;
    } else {
        ctx.actor_st.module_cooldown_checkpoint_seq =
            ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    }
    let mc = &mut ctx.actor_st.module_cooldowns;
    mc.reactivation_cooldowns = cooldowns;
    mc.boot_db_pending_merge = false;
    mc.pending_db_clear_all = false;
    mc.pending_db_cleared_ships.clear();
    mc.pending_db_removed_keys.clear();
    ctx.actor_st.module_cooldown_checkpoint_dirty = false;
    Ok(())
}

pub(in crate::system_actor) fn prune_expired_module_cooldowns_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    now_unix_f64: f64,
    tidi_factor: f64,
    has_tidi_history: bool,
    force_persist: bool,
) -> Result<Option<ModuleCooldownPersistPayload>, String> {
    let eff_tidi = if ctx.tidi_factor < 0.999_999 { ctx.tidi_factor } else { tidi_factor };
    let eff_hist = ctx.actor_st.has_tidi_history
        || has_tidi_history
        || ctx.cooldown_persist_gate.is_some_and(|g| g.has_tidi_history())
        || eff_tidi < 0.999_999;
    if eff_hist {
        ctx.actor_st.has_tidi_history = true;
    }
    let mut normalized = false;
    for entry in ctx.actor_st.module_cooldowns.reactivation_cooldowns.values_mut() {
        if entry.solar_system_id.is_none() {
            entry.solar_system_id.get_or_insert(ctx.solar_system_id);
            normalized = true;
        }
    }
    let pruned = prune_expired_module_cooldowns_in_state(
        &mut ctx.actor_st.module_cooldowns, ctx.solar_system_id, now_unix_f64,
        ctx.actor_st.sim_elapsed_secs, eff_tidi, eff_hist,
    );
    if !pruned && !force_persist && !normalized {
        return Ok(None);
    }
    ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    ctx.actor_st.module_cooldown_checkpoint_seq = ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    Ok(Some(build_persist_payload_from_actor_state(ctx.actor_st, eff_tidi, eff_hist)))
}

pub(in crate::system_actor) fn snapshot_module_cooldown_checkpoint_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    only_if_dirty: bool,
) -> Result<Option<ModuleCooldownPersistPayload>, String> {
    let has_uncommitted_seq = ctx.actor_st.module_cooldown_checkpoint_seq > 0
        && ctx.cooldown_persist_gate.is_some_and(|gate| {
            let cg = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
            let epoch = ctx.actor_st.module_cooldown_persist_epoch;
            match cg.last_persisted {
                Some((ep, seq)) => ep != epoch || ctx.actor_st.module_cooldown_checkpoint_seq > seq,
                None => true,
            }
        });
    if only_if_dirty
        && !ctx.actor_st.module_cooldown_checkpoint_dirty
        && !ctx.actor_st.module_cooldowns.boot_db_pending_merge
        && !has_uncommitted_seq
    {
        return Ok(None);
    }
    ctx.actor_st.module_cooldown_checkpoint_seq =
        ctx.actor_st.module_cooldown_checkpoint_seq.saturating_add(1);
    // Keep `module_cooldown_checkpoint_dirty` set until SQLite persistence actually commits (`#1541`).
    Ok(Some(build_persist_payload_from_actor_state(
        ctx.actor_st,
        ctx.tidi_factor,
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
    /// Returns the shared [`CooldownPersistGate`] if already registered without acquiring a
    /// ranked lock above `RANK_SERVICE_STATE` (`G1A-R4-P1-02`).
    pub(in crate::system_actor) fn get_existing_cooldown_persist_gate(
        &self,
        solar_system_id: SolarSystemId,
    ) -> Option<Arc<CooldownPersistGate>> {
        if held_lock_ranks().iter().any(|&r| r >= eve_db::lock_rank::RANK_SERVICE_STATE) {
            self.cooldown_persist_gates.read_unranked().unwrap_or_else(|e| e.into_inner()).get(&solar_system_id).cloned()
        } else {
            self.cooldown_persist_gates.read().unwrap_or_else(|e| e.into_inner()).get(&solar_system_id).cloned()
        }
    }

    /// Returns the shared per-system [`CooldownPersistGate`] for `solar_system_id` without
    /// acquiring the gate mutex (`RANK_SERVICE_STATE < RANK_COOLDOWN_PERSIST`) (`#1541`).
    pub(crate) fn cooldown_persist_gate(&self, solar_system_id: SolarSystemId) -> Arc<CooldownPersistGate> {
        if let Some(gate) = self.cooldown_persist_gates.read().unwrap_or_else(|e| e.into_inner()).get(&solar_system_id) {
            return Arc::clone(gate);
        }
        let mut guard = self.cooldown_persist_gates.write().unwrap_or_else(|e| e.into_inner());
        Arc::clone(guard.entry(solar_system_id).or_insert_with(|| Arc::new(CooldownPersistGate::default())))
    }
}

fn resolve_actor_fallback_tidi_mode(
    cluster_nodes: &RankedRwLock<super::ClusterNodeRegistry, RANK_SERVICE_STATE>,
    db_opt: Option<&WorldRepository>,
    gate: &CooldownPersistGate,
    solar_system_id: SolarSystemId,
) -> (f64, bool) {
    let held_svc = held_lock_ranks()
        .iter()
        .any(|&r| r >= RANK_SERVICE_STATE);
    let (manual_override, cn_tidi_hist) = if held_svc {
        let cn = cluster_nodes
            .read_unranked()
            .unwrap_or_else(|e| e.into_inner());
        (
            cn.manual_tidi_overrides.get(&solar_system_id).copied(),
            cn.tidi_history_systems.contains(&solar_system_id),
        )
    } else {
        let cn = cluster_nodes.read().unwrap_or_else(|e| e.into_inner());
        (
            cn.manual_tidi_overrides.get(&solar_system_id).copied(),
            cn.tidi_history_systems.contains(&solar_system_id),
        )
    };
    let db_tidi = manual_override
        .or_else(|| {
            if !held_lock_ranks().iter().any(|&r| r >= RANK_WORLD_INNER) {
                db_opt.and_then(|db| {
                    db.with_snapshot(|snap| {
                        let nid = -(7_500_000_000_000_i64.saturating_add(solar_system_id as i64));
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
    let has_tidi_history = db_tidi < 0.999_999 || cn_tidi_hist || gate.has_tidi_history();
    (db_tidi, has_tidi_history)
}

pub(in crate::system_actor) fn take_stopped_cooldown_persist_payload_in_state(
    st: &mut SystemActorState,
    solar_system_id: SolarSystemId,
    gate: &CooldownPersistGate,
    cluster_nodes: &RankedRwLock<super::ClusterNodeRegistry, RANK_SERVICE_STATE>,
    captured_tidi: f64,
) -> Option<ModuleCooldownPersistPayload> {
    let (shutdown_tidi_opt, gate_has_tidi, drained) = {
        let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(
            gate_guard.phase,
            CooldownGatePhase::Live | CooldownGatePhase::Stopping
        ) || st.module_cooldown_persist_epoch != gate.current_epoch()
        {
            return None;
        }
        let shutdown_tidi_opt = gate_guard.shutdown_tidi_mode.take();
        let gate_has_tidi = gate.has_tidi_history();
        let drained = gate.take_all_pending_transitions(&mut gate_guard);
        (shutdown_tidi_opt, gate_has_tidi, drained)
    };
    let (reg_tidi, reg_has_tidi_history) = shutdown_tidi_opt.unwrap_or_else(|| {
        resolve_actor_fallback_tidi_mode(
            cluster_nodes,
            st.db_opt.as_deref(),
            gate,
            solar_system_id,
        )
    });
    let eff_tidi = if captured_tidi.is_finite() && captured_tidi < 0.999_999 {
        captured_tidi.clamp(0.10, 1.0)
    } else {
        reg_tidi
    };
    let has_tidi_history =
        st.has_tidi_history || gate_has_tidi || eff_tidi < 0.999_999 || reg_has_tidi_history;
    let applied_transitions = apply_pending_transitions_to_actor_state(
        solar_system_id,
        st,
        eff_tidi,
        has_tidi_history,
        drained,
    );
    let has_entries = !st.module_cooldowns.reactivation_cooldowns.is_empty();
    if !st.module_cooldown_checkpoint_dirty && !has_entries && !applied_transitions {
        return None;
    }
    let final_tidi_hist = has_tidi_history || st.has_tidi_history;
    let now_unix_f64 = st
        .db_opt
        .as_ref()
        .map(|db| db.clock().now_unix_secs() as f64)
        .unwrap_or(0.0);
    if has_entries {
        rebase_module_cooldowns_on_stop_in_state(
            &mut st.module_cooldowns,
            solar_system_id,
            now_unix_f64,
            st.sim_elapsed_secs,
            eff_tidi,
            final_tidi_hist,
        );
        st.module_cooldown_checkpoint_seq = st.module_cooldown_checkpoint_seq.saturating_add(1);
    }
    Some(build_persist_payload_from_actor_state(
        st,
        eff_tidi,
        final_tidi_hist,
    ))
}

impl SystemRegistry {
    /// Dispatches a cooldown mutation to `solar_system_id`, whether `Live`, `Loading`,
    /// `Stopping`, or `Cold` (`G1A-R3-P1-02`).
    pub(in crate::system_actor) fn apply_cooldown_op_to_system(
        &self,
        db: &WorldRepository,
        solar_system_id: SolarSystemId,
        op: PendingCooldownTransition,
    ) -> bool {
        if solar_system_id == CLUSTER_SYSTEM_ID {
            if !can_write_sqlite_safely() || matches!(op, PendingCooldownTransition::Record { .. } | PendingCooldownTransition::Seed { .. }) {
                return false;
            }
            let gate = self.cooldown_persist_gate(CLUSTER_SYSTEM_ID);
            let mut cg = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
            return flush_cold_transitions_under_gate(db, CLUSTER_SYSTEM_ID, &gate, &mut cg, 1.0, false, &[op]);
        }
        let (eff_tidi, eff_hist) = if let PendingCooldownTransition::Record {
            tidi_factor,
            has_tidi_history,
            ..
        } = op
        {
            (tidi_factor, has_tidi_history)
        } else {
            self.resolve_effective_tidi_mode(solar_system_id, true)
        };
        for _ in 0..8 {
            if let Some(handle) = self.get_handle(solar_system_id) {
                let res = match &op {
                    PendingCooldownTransition::Record { ship_id, module_id, module_type_id, now_unix_f64, delay_s, tidi_factor, has_tidi_history } => {
                        handle.record_module_cooldown_sync(*ship_id, *module_id, *module_type_id, *now_unix_f64, *delay_s, *tidi_factor, *has_tidi_history).map(|()| true)
                    }
                    PendingCooldownTransition::Seed { key, entry } => handle.seed_module_cooldown_entry_sync(*key, *entry).map(|()| true),
                    PendingCooldownTransition::Remove { key } => handle.remove_module_cooldown_sync(*key),
                    PendingCooldownTransition::ClearShip { ship_id } => handle.clear_ship_module_cooldowns_sync(*ship_id),
                    PendingCooldownTransition::PruneExpired { now_unix_f64, tidi_factor, has_tidi_history } => {
                        handle.prune_expired_module_cooldowns_sync(*now_unix_f64, *tidi_factor, *has_tidi_history, false)
                    }
                    PendingCooldownTransition::ClearAll => handle.clear_all_module_cooldowns_sync().map(|()| true),
                };
                if let Ok(ok) = res { return ok; }
            }
            let gate = self.cooldown_persist_gate(solar_system_id);
            let mut gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
            match gate_guard.phase {
                CooldownGatePhase::Live => {
                    drop(gate_guard);
                    if !self.try_reclaim_dead_live_slot_to_cold(solar_system_id) {
                        std::thread::yield_now();
                    }
                }
                CooldownGatePhase::Loading | CooldownGatePhase::Stopping => {
                    if let PendingCooldownTransition::Record { tidi_factor, has_tidi_history, .. } = &op {
                        if *tidi_factor < 0.999_999 || *has_tidi_history { gate.mark_tidi_history(); }
                    }
                    let matched = match &op {
                        PendingCooldownTransition::Remove { key } => {
                            let db_entry = load_cold_system_module_cooldowns_from_db(db, solar_system_id).get(key).copied();
                            CooldownPersistGate::overlay_key_in_state(&gate_guard, *key, db_entry, solar_system_id, eff_tidi, eff_hist).is_some()
                        }
                        _ => true,
                    };
                    gate_guard.pending_transitions.push(op);
                    gate.mark_pending_transitions();
                    return matched;
                }
                CooldownGatePhase::Cold => return flush_cold_transitions_under_gate(db, solar_system_id, &gate, &mut gate_guard, eff_tidi, eff_hist, &[op]),
            }
        }
        false
    }

    pub(crate) fn overlay_transition_cooldown_for_system(&self, solar_system_id: SolarSystemId, key: (i64, i64, u32), base_entry: Option<ModuleCooldownEntry>) -> Option<ModuleCooldownEntry> {
        let Some(gate) = self.get_existing_cooldown_persist_gate(solar_system_id) else { return base_entry; };
        if !gate.has_pending_transitions() { return base_entry; }
        let (tidi, hist) = self.resolve_effective_tidi_mode(solar_system_id, true);
        gate.overlay_key(key, base_entry, solar_system_id, tidi, hist)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_cold_system_module_cooldown_in_db(
        &self, db: &WorldRepository, solar_system_id: SolarSystemId, ship_id: i64, module_id: i64,
        module_type_id: u32, now_unix_f64: f64, delay_s: f64, tidi_factor: f64, has_tidi_history: bool,
    ) {
        if !delay_s.is_finite() || delay_s <= 0.0 { return; }
        let _ = self.apply_cooldown_op_to_system(db, solar_system_id, PendingCooldownTransition::Record { ship_id, module_id, module_type_id, now_unix_f64, delay_s, tidi_factor, has_tidi_history });
    }

    #[cfg(any(test, feature = "testkit"))]
    pub(crate) fn seed_cold_system_module_cooldown_in_db(&self, db: &WorldRepository, solar_system_id: SolarSystemId, key: (i64, i64, u32), entry: ModuleCooldownEntry) {
        let _ = self.apply_cooldown_op_to_system(db, solar_system_id, PendingCooldownTransition::Seed { key, entry });
    }

    fn apply_cooldown_op_non_live_with_retry(&self, db: &WorldRepository, solar_system_id: SolarSystemId, retry_live: &HashSet<u32>, op: PendingCooldownTransition) -> bool {
        if self.get_handle(solar_system_id).is_some() && !retry_live.contains(&solar_system_id) { return false; }
        self.apply_cooldown_op_to_system(db, solar_system_id, op)
    }

    #[cfg(test)]
    pub(crate) fn remove_cold_module_cooldown_key_across_systems(&self, db: &WorldRepository, exclude_sys_opt: Option<u32>, key: (i64, i64, u32)) -> bool {
        self.remove_cooldown_key_across_non_live_systems(db, exclude_sys_opt, &[], key)
    }

    /// Removes `key` from all non-live (`Cold`, `Loading`, `Stopping`, or raced `retry_sids`)
    /// systems (`exclude_sys_opt` skipped) via [`CooldownPersistGate`] (`G1A-R3-P1-02`).
    pub(crate) fn remove_cooldown_key_across_non_live_systems(&self, db: &WorldRepository, exclude_sys_opt: Option<u32>, retry_sids: &[u32], key: (i64, i64, u32)) -> bool {
        if !can_write_sqlite_safely() { return false; }
        let retry_set: HashSet<u32> = retry_sids.iter().copied().collect();
        let mut sys_ids: BTreeSet<u32> = find_all_cold_module_cooldown_entries_in_db(db, key).into_iter().map(|(sid, _)| sid).collect();
        sys_ids.extend(self.transitioning_system_ids(exclude_sys_opt));
        sys_ids.extend(retry_sids.iter().copied());
        if let Some(ex) = exclude_sys_opt { sys_ids.remove(&ex); }
        let mut removed = false;
        for sys_id in sys_ids {
            removed |= self.apply_cooldown_op_non_live_with_retry(db, sys_id, &retry_set, PendingCooldownTransition::Remove { key });
        }
        removed
    }

    fn collect_non_live_cooldown_systems(&self, db: &WorldRepository, ship_filter: Option<i64>, retry_sids: &[u32]) -> (HashSet<u32>, BTreeSet<u32>) {
        let retry_set: HashSet<u32> = retry_sids.iter().copied().collect();
        let mut sys_ids: BTreeSet<u32> = self.transitioning_system_ids(None).into_iter().collect();
        sys_ids.extend(retry_sids.iter().copied());
        if let Ok(all_rows) = db.with_sqlite_conn(|conn| {
            ensure_system_checkpoint_schema(conn)?;
            load_all_checkpoints_for_kind::<((i64, i64, u32), ModuleCooldownEntry)>(conn, KIND_MODULE_COOLDOWNS)
        }) {
            for (sys_id, _, ((sid, _, _), _)) in all_rows {
                if ship_filter.is_none_or(|f| sid == f) {
                    sys_ids.insert(sys_id);
                }
            }
        }
        (retry_set, sys_ids)
    }

    #[cfg(test)]
    pub(crate) fn clear_cold_ship_module_cooldowns_in_db(&self, db: &WorldRepository, ship_id: i64) {
        self.clear_non_live_ship_module_cooldowns(db, &[], ship_id);
    }

    pub(crate) fn clear_non_live_ship_module_cooldowns(&self, db: &WorldRepository, retry_sids: &[u32], ship_id: i64) {
        if !can_write_sqlite_safely() { return; }
        let (retry_set, sys_ids) = self.collect_non_live_cooldown_systems(db, Some(ship_id), retry_sids);
        for sys_id in sys_ids {
            let _ = self.apply_cooldown_op_non_live_with_retry(db, sys_id, &retry_set, PendingCooldownTransition::ClearShip { ship_id });
        }
    }

    #[cfg(test)]
    pub(crate) fn clear_all_cold_module_cooldowns_in_db(&self, db: &WorldRepository) { self.clear_all_non_live_module_cooldowns(db, &[]); }

    #[cfg(any(test, feature = "testkit"))]
    pub(crate) fn clear_all_non_live_module_cooldowns(&self, db: &WorldRepository, retry_sids: &[u32]) {
        if !can_write_sqlite_safely() { return; }
        let (retry_set, sys_ids) = self.collect_non_live_cooldown_systems(db, None, retry_sids);
        for sys_id in sys_ids {
            let _ = self.apply_cooldown_op_non_live_with_retry(db, sys_id, &retry_set, PendingCooldownTransition::ClearAll);
        }
    }

    /// Flushes durable module reactivation cooldown checkpoints across all currently loaded systems.
    pub fn flush_all_module_cooldown_checkpoints_to_db(&self) {
        for (_, handle) in self.active_system_handles() { let _ = handle.flush_module_cooldown_checkpoint_sync(); }
    }

    /// Reloads live actor cooldown maps from SQLite `system_checkpoint` after DB rehydration.
    pub fn rehydrate_active_system_cooldowns_from_db(&self) {
        let Some(ref db) = self.db else { return; };
        for (sys_id, handle) in self.active_system_handles() {
            for _ in 0..8 {
                for spin in 0..512 {
                    if !handle.has_in_flight_post_unlock() { break; }
                    if spin < 64 { std::thread::yield_now(); } else { std::thread::sleep(std::time::Duration::from_millis(1)); }
                }
                let _ = handle.flush_dirty_module_cooldown_checkpoint_sync();
                let gate = self.cooldown_persist_gate(sys_id);
                let (token, loaded) = {
                    let gate_guard = gate.inner.lock().unwrap_or_else(|e| e.into_inner());
                    if gate_guard.phase != CooldownGatePhase::Live { break; }
                    ((gate.current_epoch(), gate_guard.last_persisted), load_cold_system_module_cooldowns_from_db(db, sys_id))
                };
                match handle.replace_module_cooldowns_checked_sync(loaded, Some(token)) {
                    Ok(()) => break,
                    Err(err) if err == "STALE_REHYDRATE_TOKEN" => std::thread::yield_now(),
                    Err(_) => break,
                }
            }
        }
    }

    /// Records sim-clock reload deadlines on `solar_system_id`'s actor when live (`#1081`, `#1541`).
    pub fn record_module_reload_sim_deadlines(&self, module_ids: &[i64], solar_system_id: SolarSystemId, sim_start_s: f64, sim_deadline_s: f64) {
        if module_ids.is_empty() { return; }
        if let Some(handle) = self.get_handle(solar_system_id) {
            let _ = handle.record_module_reload_sim_deadlines_sync(module_ids.to_vec(), sim_start_s, sim_deadline_s);
        }
    }

    /// Clears a module's sim-clock reload deadline on whichever live actor owns it (`#1081`, `#1541`).
    pub fn clear_module_reload_sim_deadline(&self, module_id: i64) {
        for (_, handle) in self.active_system_handles() {
            if handle.view().module_cooldowns.reload_sim_deadlines.contains_key(&module_id) {
                let _ = handle.clear_module_reload_sim_deadline_sync(module_id);
                return;
            }
        }
    }

    /// Checks a module's sim-clock reload deadline lock-free from [`super::SystemView`] (`#1081`, `#1541`).
    pub fn check_module_reload_sim_deadline(&self, module_id: i64) -> Option<(SolarSystemId, f64, f64)> {
        for (sys_id, handle) in self.active_system_handles() {
            if let Some(d) = handle.view().module_reload_sim_deadline(module_id) { return Some((sys_id, d.sim_start_s, d.sim_deadline_s)); }
        }
        None
    }
}
