//! SQLite `KIND_FIGHTERS` persistence, one-way legacy migration fallback, and `Cold` system
//! query/mutation helpers for [`super::fighters`] (`#1541` Track F2, `#1170`).
//!
//! All functions in this module enforce the `#1170` post-unlock writer invariant: never hold
//! `SystemRegistry`, `Ballpark`, or `SystemActorState` locks while invoking SQLite checkpoint
//! operations on `WorldRepository`.

use super::lifecycle_sm::{FighterColdProbe, LifecycleGate};
use super::types::FighterCheckpointSnapshot;
use super::{
    exec_tube_op_in_state, ActiveFighterSquadron, FighterActorError, FighterTubeOp,
    ShipFighterTubeTransfer, SystemActivationError, SystemFighterState, SystemHandle,
    SystemRegistry,
};
use eve_db::lock_rank::{
    held_lock_ranks, RankedMutex, RankedMutexGuard, RANK_LEAF, RANK_SERVICE_STATE,
};
use eve_db::sqlite_store::system_checkpoint::{
    current_unix_secs, load_all_checkpoints_for_kind, load_system_checkpoints,
    replace_system_checkpoints_and_delete_legacy, replace_system_checkpoints_for_kind,
    CLUSTER_SYSTEM_ID, KIND_FIGHTERS,
};
use eve_db::{InventoryFlag, WorldRepository, WorldStateSnapshot};
use eve_destiny::Ballpark;
use eve_sde::{SdeRegistry, SolarSystemId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Immutable persistence/registry handles and lock-free dirty stamp shared with [`SystemHandle`].
#[derive(Default)]
pub(crate) struct FighterHandleCtx {
    pub(crate) dirty_stamp: Arc<AtomicU64>,
    pub(crate) persist: Arc<FighterPersistCoord>,
    pub(crate) db_opt: Option<Arc<WorldRepository>>,
    pub(crate) sde_opt: Option<Arc<SdeRegistry>>,
}

const DEFAULT_FALLBACK_SYSTEM_ID: SolarSystemId = 30_000_142;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) enum FighterCheckpointEntry {
    Squadron(ActiveFighterSquadron),
    Cooldown((i64, u32), u64),
    Charge((i64, u32), u32),
    TubeLoad((i64, usize), i64),
    TubeLoadTyped((i64, usize), i64, u32),
    UnitDamageAccum(i64, f64),
    TubeRearmUntil((i64, usize), u64),
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct LegacyCombatFighterExtended {
    fighter_unit_damage_accum: HashMap<i64, f64>,
    fighter_tube_rearm_until: Vec<((i64, usize), u64)>,
}

/// Resolves an entity, ship, or station-contained item's solar system ID without holding
/// ballpark/actor locks.
pub(crate) fn resolve_entity_solar_system_id_with_sde(snap: &WorldStateSnapshot, sde: Option<&SdeRegistry>, entity_id: i64) -> u32 {
    if let Some(st) = snap.structures.get(&entity_id) { return st.solar_system_id; }
    if let Some(st) = sde.and_then(|reg| reg.stations.get(&entity_id)) { return st.solar_system_id; }
    if let Some(ch) = snap.characters.values().find(|c| c.active_ship_id == entity_id) {
        if (30_000_000..40_000_000).contains(&ch.solar_system_id) { return ch.solar_system_id; }
    }
    let mut cur = entity_id;
    for _ in 0..8 {
        if let Ok(loc_u32) = u32::try_from(cur) {
            if (30_000_000..40_000_000).contains(&loc_u32) { return loc_u32; }
        }
        if let Some(st) = snap.structures.get(&cur) { return st.solar_system_id; }
        if let Some(st) = sde.and_then(|reg| reg.stations.get(&cur)) { return st.solar_system_id; }
        let item_opt = snap.items.get(&cur).or_else(|| {
            snap.sp6_lifecycle.space_object_meta.iter().find(|(_, m)| m.drone_ball_id == Some(cur)).and_then(|(iid, _)| snap.items.get(iid))
        });
        let Some(item) = item_opt else { break; };
        if item.location_id == cur || item.location_id <= 0 { break; }
        cur = item.location_id;
    }
    CLUSTER_SYSTEM_ID
}

fn entry_target_solar_system_id(snap: &WorldStateSnapshot, sde: Option<&SdeRegistry>, entry: &FighterCheckpointEntry) -> u32 {
    let pick = |ids: &[i64]| {
        ids.iter()
            .map(|&id| resolve_entity_solar_system_id_with_sde(snap, sde, id))
            .find(|&sys| sys != CLUSTER_SYSTEM_ID)
            .unwrap_or(CLUSTER_SYSTEM_ID)
    };
    match entry {
        FighterCheckpointEntry::Squadron(sq) => pick(&[sq.item_id, sq.ball_id, sq.parent_ship_id]),
        FighterCheckpointEntry::Cooldown((id, _), _) | FighterCheckpointEntry::Charge((id, _), _) | FighterCheckpointEntry::UnitDamageAccum(id, _) => pick(&[*id]),
        FighterCheckpointEntry::TubeLoad((ship_id, _), fid) | FighterCheckpointEntry::TubeLoadTyped((ship_id, _), fid, _) => pick(&[*ship_id, *fid]),
        FighterCheckpointEntry::TubeRearmUntil((ship_id, _), _) => pick(&[*ship_id]),
    }
}

fn insert_tube_load_entry(state: &mut SystemFighterState, k: (i64, usize), fid: i64, ftype: u32) {
    match state.tube_loads.entry(k) {
        std::collections::hash_map::Entry::Vacant(e) => { e.insert((fid, ftype)); }
        std::collections::hash_map::Entry::Occupied(mut e) if e.get().0 == fid && e.get().1 == 0 && ftype != 0 => {
            e.get_mut().1 = ftype;
        }
        _ => {}
    }
}

pub(crate) fn apply_checkpoint_entry_if_absent(state: &mut SystemFighterState, entry: FighterCheckpointEntry, item_types: &HashMap<i64, u32>) {
    match entry {
        FighterCheckpointEntry::Squadron(sq) => { state.active_fighters.entry(sq.ball_id).or_insert(sq); }
        FighterCheckpointEntry::Cooldown(k, v) => { state.cooldown_state.entry(k).or_insert(v); }
        FighterCheckpointEntry::Charge(k, v) => { state.charge_state.entry(k).or_insert(v); }
        FighterCheckpointEntry::TubeLoad(k, fid) => insert_tube_load_entry(state, k, fid, item_types.get(&fid).copied().unwrap_or(0)),
        FighterCheckpointEntry::TubeLoadTyped(k, fid, raw_ftype) => {
            insert_tube_load_entry(state, k, fid, if raw_ftype != 0 { raw_ftype } else { item_types.get(&fid).copied().unwrap_or(0) });
        }
        FighterCheckpointEntry::UnitDamageAccum(k, v) => { state.unit_damage_accum.entry(k).or_insert(v); }
        FighterCheckpointEntry::TubeRearmUntil(k, v) => { state.tube_rearm_until.entry(k).or_insert(v); }
    }
}

pub(crate) fn collect_fighter_checkpoint_rows(state: &SystemFighterState, skip_reservation_ids: &HashSet<i64>) -> Vec<(String, FighterCheckpointEntry)> {
    let mut rows = Vec::with_capacity(
        state.active_fighters.len() + state.cooldown_state.len() + state.charge_state.len()
            + state.tube_loads.len() + state.unit_damage_accum.len() + state.tube_rearm_until.len(),
    );
    for sq in state.active_fighters.values().filter(|sq| !skip_reservation_ids.contains(&sq.ball_id)) {
        rows.push((format!("sq:{}", sq.ball_id), FighterCheckpointEntry::Squadron(sq.clone())));
    }
    for (&(id, slot), &cd) in &state.cooldown_state { rows.push((format!("cd:{id}:{slot}"), FighterCheckpointEntry::Cooldown((id, slot), cd))); }
    for (&(id, slot), &ch) in &state.charge_state { rows.push((format!("ch:{id}:{slot}"), FighterCheckpointEntry::Charge((id, slot), ch))); }
    for (&(sid, tidx), &(fid, ftype)) in &state.tube_loads { rows.push((format!("tl:{sid}:{tidx}"), FighterCheckpointEntry::TubeLoadTyped((sid, tidx), fid, ftype))); }
    for (&fid, &dmg) in &state.unit_damage_accum { if dmg > 0.0 { rows.push((format!("da:{fid}"), FighterCheckpointEntry::UnitDamageAccum(fid, dmg))); } }
    for (&(sid, tidx), &until) in &state.tube_rearm_until { rows.push((format!("tr:{sid}:{tidx}"), FighterCheckpointEntry::TubeRearmUntil((sid, tidx), until))); }
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum FighterSlotMode {
    #[default]
    Cold,
    Loading { generation: u64 },
    Live { generation: u64 },
    Stopping { generation: u64 },
}

#[derive(Debug, Default)]
struct FighterSlotPhase {
    mode: FighterSlotMode,
    retired_external_up_to: u64,
    retired_all_up_to: u64,
}

#[derive(Debug, Default)]
pub(crate) struct FighterIoWatermark {
    last_persisted: Option<(u64, u64)>,
}

impl FighterIoWatermark {
    pub(crate) fn on_system_loaded(&mut self, generation: u64) {
        self.last_persisted = Some((generation, 0));
    }

    pub(crate) fn on_system_loaded_if_unloaded(&mut self, generation: u64) {
        if !matches!(self.last_persisted, Some((g, _)) if g >= generation) {
            self.last_persisted = Some((generation, 0));
        }
    }

    pub(crate) fn advance_at_least(&mut self, gen: u64, rev: u64) {
        if !matches!(self.last_persisted, Some((pg, pr)) if pg > gen || (pg == gen && pr >= rev)) {
            self.last_persisted = Some((gen, rev));
        }
    }

    pub(crate) fn on_cold_mutated(&mut self) {
        self.last_persisted = None;
    }
}

struct HydrateFlight {
    done: std::sync::atomic::AtomicBool,
    mu: std::sync::Mutex<bool>,
    cvar: std::sync::Condvar,
    watch_tx: tokio::sync::watch::Sender<bool>,
    watch_rx: tokio::sync::watch::Receiver<bool>,
}

impl HydrateFlight {
    fn new() -> Self {
        let (watch_tx, watch_rx) = tokio::sync::watch::channel(false);
        Self {
            done: std::sync::atomic::AtomicBool::new(false),
            mu: std::sync::Mutex::new(false),
            cvar: std::sync::Condvar::new(),
            watch_tx,
            watch_rx,
        }
    }

    fn finish(&self) {
        let mut done_mu = self.mu.lock().unwrap_or_else(|e| e.into_inner());
        *done_mu = true;
        self.done.store(true, std::sync::atomic::Ordering::Release);
        self.cvar.notify_all();
        drop(done_mu);
        let _ = self.watch_tx.send(true);
    }

    fn wait_sync(&self) {
        let mut done_mu = self.mu.lock().unwrap_or_else(|e| e.into_inner());
        if !*done_mu && !self.done.load(std::sync::atomic::Ordering::Acquire) {
            let (g, _) = self
                .cvar
                .wait_timeout_while(done_mu, std::time::Duration::from_secs(30), |d| !*d)
                .unwrap_or_else(|e| e.into_inner());
            done_mu = g;
        }
        drop(done_mu);
    }

    async fn wait_async(&self) {
        if self.done.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let mut rx = self.watch_rx.clone();
        let _ = rx.wait_for(|&done| done).await;
    }
}

struct HydrateFlightGuard<'a> {
    slot: &'a PerSystemFighterSlot,
    flight: Arc<HydrateFlight>,
}

impl Drop for HydrateFlightGuard<'_> {
    fn drop(&mut self) {
        let mut active = self.slot.hydrate_flight.lock().unwrap_or_else(|e| e.into_inner());
        if active.as_ref().is_some_and(|f| Arc::ptr_eq(f, &self.flight)) {
            *active = None;
        }
        drop(active);
        self.flight.finish();
    }
}

pub(crate) struct PerSystemFighterSlot {
    phase: RankedMutex<FighterSlotPhase, RANK_LEAF>,
    io_lock: RankedMutex<FighterIoWatermark, RANK_SERVICE_STATE>,
    hydrate_flight: RankedMutex<Option<Arc<HydrateFlight>>, RANK_LEAF>,
}

impl Default for PerSystemFighterSlot {
    fn default() -> Self {
        Self {
            phase: RankedMutex::new(FighterSlotPhase::default()),
            io_lock: RankedMutex::new(FighterIoWatermark::default()),
            hydrate_flight: RankedMutex::new(None),
        }
    }
}

impl PerSystemFighterSlot {
    fn try_begin_hydrate(&self) -> Result<HydrateFlightGuard<'_>, Arc<HydrateFlight>> {
        let mut active = self.hydrate_flight.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref existing) = *active {
            if !existing.done.load(std::sync::atomic::Ordering::Acquire) {
                return Err(Arc::clone(existing));
            }
        }
        let flight = Arc::new(HydrateFlight::new());
        *active = Some(Arc::clone(&flight));
        Ok(HydrateFlightGuard { slot: self, flight })
    }
    pub(crate) fn begin_loading(&self, generation: u64) {
        let mut ph = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        ph.mode = FighterSlotMode::Loading { generation };
    }

    pub(crate) fn mark_live(&self, generation: u64) {
        let mut ph = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(ph.mode, FighterSlotMode::Loading { generation: g } if g == generation) {
            ph.mode = FighterSlotMode::Live { generation };
        }
    }

    pub(crate) fn begin_stopping(&self, generation: u64) {
        let mut ph = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        ph.retired_external_up_to = ph.retired_external_up_to.max(generation);
        if matches!(
            ph.mode,
            FighterSlotMode::Live { generation: g } | FighterSlotMode::Loading { generation: g }
                if g == generation
        ) {
            ph.mode = FighterSlotMode::Stopping { generation };
        }
    }

    pub(crate) fn retire_to_cold(&self, generation: u64) {
        let mut ph = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        ph.retired_external_up_to = ph.retired_external_up_to.max(generation);
        ph.retired_all_up_to = ph.retired_all_up_to.max(generation);
        if matches!(
            ph.mode,
            FighterSlotMode::Loading { generation: g }
                | FighterSlotMode::Live { generation: g }
                | FighterSlotMode::Stopping { generation: g }
                if g == generation
        ) {
            ph.mode = FighterSlotMode::Cold;
        }
    }

    pub(crate) fn retire_if_loading_gen(&self, generation: u64) {
        let mut ph = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        ph.retired_external_up_to = ph.retired_external_up_to.max(generation);
        ph.retired_all_up_to = ph.retired_all_up_to.max(generation);
        if matches!(ph.mode, FighterSlotMode::Loading { generation: g } if g == generation) {
            ph.mode = FighterSlotMode::Cold;
        }
    }

    pub(crate) fn is_cold_mode(&self) -> bool {
        matches!(
            self.phase.lock().unwrap_or_else(|e| e.into_inner()).mode,
            FighterSlotMode::Cold
        )
    }

    pub(crate) fn is_loading_gen_active(&self, generation: u64) -> bool {
        let ph = self.phase.lock().unwrap_or_else(|e| e.into_inner());
        matches!(ph.mode, FighterSlotMode::Loading { generation: g } if g == generation)
            && generation > ph.retired_all_up_to
    }

    pub(crate) fn lock_io(&self) -> RankedMutexGuard<'_, FighterIoWatermark, RANK_SERVICE_STATE> {
        self.io_lock.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Per-system coordinator serializing `KIND_FIGHTERS` SQLite writes (`io_lock`) and guarding
/// against stale revisions or retired lifecycle generations (`phase`, `#1541` Track F2).
#[derive(Default)]
pub(crate) struct FighterPersistCoord {
    slots: RankedMutex<HashMap<SolarSystemId, Arc<PerSystemFighterSlot>>, RANK_LEAF>,
}

impl FighterPersistCoord {
    pub(crate) fn slot_for(&self, solar_system_id: SolarSystemId) -> Arc<PerSystemFighterSlot> {
        let mut map = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        Arc::clone(map.entry(solar_system_id).or_default())
    }
}

/// Persists `state` directly under `(solar_system_id, KIND_FIGHTERS)`. Must only be called when
/// NO `SystemRegistry`, `Ballpark`, `SystemActorState`, or `WorldRepository` write locks are held.
pub(crate) fn persist_system_fighters_in_db(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
    state: &SystemFighterState,
) -> Result<usize, String> {
    if solar_system_id == CLUSTER_SYSTEM_ID {
        return Ok(0);
    }
    let rows = collect_fighter_checkpoint_rows(state, &HashSet::new());
    let count = rows.len();
    let now_unix = current_unix_secs();
    db.with_sqlite_conn(|conn| {
        replace_system_checkpoints_for_kind(conn, solar_system_id, KIND_FIGHTERS, &rows, now_unix)
    })?;
    Ok(count)
}

/// Serializes live checkpoint flushes per system against concurrent writers, stale revisions,
/// retired generations (`Stopping`/`Cold`), and clears `dirty_stamp` via CAS if unchanged.
pub(crate) fn persist_live_fighter_snapshot_sync(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
    coord: &FighterPersistCoord,
    snap: &FighterCheckpointSnapshot,
    dirty_stamp: &AtomicU64,
    sde_opt: Option<&SdeRegistry>,
    allow_during_stopping: bool,
) -> Result<usize, String> {
    if solar_system_id == CLUSTER_SYSTEM_ID {
        return Ok(0);
    }
    if held_lock_ranks().iter().any(|&r| r >= RANK_SERVICE_STATE) {
        return Err(format!(
            "Refusing fighter checkpoint flush for {solar_system_id} while holding >= RANK_SERVICE_STATE"
        ));
    }
    let slot = coord.slot_for(solar_system_id);
    let mut wm = slot.lock_io();
    if snap.lifecycle_gen > 0 {
        let ph = slot.phase.lock().unwrap_or_else(|e| e.into_inner());
        let retired = if allow_during_stopping {
            ph.retired_all_up_to
        } else {
            ph.retired_external_up_to
        };
        if snap.lifecycle_gen <= retired {
            return Ok(0);
        }
    }
    if let Some((p_gen, p_rev)) = wm.last_persisted {
        if snap.lifecycle_gen < p_gen || (snap.lifecycle_gen == p_gen && snap.rev <= p_rev) {
            let same_gen = snap.lifecycle_gen == p_gen;
            drop(wm);
            if same_gen {
                let _ = dirty_stamp.compare_exchange(snap.dirty_stamp, 0, Ordering::AcqRel, Ordering::Acquire);
            }
            return Ok(0);
        }
    }
    let written = persist_system_fighters_in_db(db, solar_system_id, &snap.state)?;
    wm.last_persisted = Some((snap.lifecycle_gen, snap.rev));
    drop(wm);
    let _ = dirty_stamp.compare_exchange(snap.dirty_stamp, 0, Ordering::AcqRel, Ordering::Acquire);
    if let Some((df, tr)) =
        collect_legacy_ext_prune_targets(db, sde_opt, solar_system_id, &snap.state)
    {
        prune_migrated_ext_json(db, &df, &tr);
    }
    Ok(written)
}

struct PreloadedLegacyHints {
    ext_damage: Vec<(i64, f64, u32)>,
    ext_tube_rearms: Vec<((i64, usize), u64, u32)>,
    abandoned_cat87: Vec<ActiveFighterSquadron>,
    fighter_item_types: HashMap<i64, u32>,
    cluster_rows_resolved: Option<Vec<(String, FighterCheckpointEntry, u32)>>,
}

struct MergeFighterOutcome {
    state: SystemFighterState,
    saw_legacy_fallback: bool,
    migrated_cluster_keys: Vec<String>,
    migrated_ext_damage_fids: Vec<i64>,
    migrated_ext_tube_rearms: Vec<(i64, usize)>,
}

fn collect_preloaded_hints(
    db: &WorldRepository, sde: Option<&SdeRegistry>, solar_system_id: SolarSystemId,
    cluster_rows: Option<&[(String, FighterCheckpointEntry)]>,
) -> PreloadedLegacyHints {
    db.with_snapshot(|snap| {
        let (mut ext_damage, mut ext_tube_rearms) = (Vec::new(), Vec::new());
        let ext_json = &snap.combat_crimewatch_state.extended_runtime_json;
        if !ext_json.is_empty() {
            if let Ok(legacy_ext) = serde_json::from_str::<LegacyCombatFighterExtended>(ext_json) {
                for (fid, dmg) in legacy_ext.fighter_unit_damage_accum {
                    if dmg > 0.0 { ext_damage.push((fid, dmg, resolve_entity_solar_system_id_with_sde(snap, sde, fid))); }
                }
                for ((ship_id, tube_idx), until) in legacy_ext.fighter_tube_rearm_until {
                    if until > 0 { ext_tube_rearms.push(((ship_id, tube_idx), until, resolve_entity_solar_system_id_with_sde(snap, sde, ship_id))); }
                }
            }
        }
        let (mut abandoned_cat87, mut fighter_item_types) = (Vec::new(), HashMap::new());
        if let Some(sde_ref) = sde {
            for item in snap.items.values() {
                let Some(ty) = sde_ref.types.get(&item.type_id).filter(|t| t.category_id == 87) else { continue; };
                fighter_item_types.insert(item.item_id, item.type_id);
                if item.location_id != solar_system_id as i64 || item.flag != InventoryFlag::None || !item.singleton { continue; }
                let ball_id = snap.sp6_lifecycle.space_object_meta.get(&item.item_id).and_then(|m| m.drone_ball_id).unwrap_or(item.item_id);
                if snap.combat_crimewatch_state.is_recently_destroyed_item_id(item.item_id) || snap.combat_crimewatch_state.is_recently_destroyed_item_id(ball_id) { continue; }
                let max_sq = ty.default_attributes.get(&2215).copied().map(|v| v as u32).unwrap_or(match ty.group_id { 1537 | 4778 => 3, 1653 | 4779 => 6, _ => 9 });
                let squadron_size = item.quantity.max(1).min(max_sq as i32) as u32;
                abandoned_cat87.push(ActiveFighterSquadron { ball_id, item_id: item.item_id, type_id: item.type_id, owner_char_id: item.owner_id, parent_ship_id: 0, tube_index: 0, squadron_size, target_id: None, is_abandoned: true, mwd_active: false });
            }
        }
        let cluster_rows_resolved = cluster_rows.map(|rows| {
            rows.iter().map(|(k, e)| (k.clone(), e.clone(), entry_target_solar_system_id(snap, sde, e))).collect()
        });
        PreloadedLegacyHints { ext_damage, ext_tube_rearms, abandoned_cat87, fighter_item_types, cluster_rows_resolved }
    })
}

fn merge_loaded_fighter_state(
    solar_system_id: SolarSystemId, primary_rows: Vec<(String, FighterCheckpointEntry)>,
    hints: PreloadedLegacyHints, extra_entries: &[FighterCheckpointEntry],
) -> MergeFighterOutcome {
    let mut state = SystemFighterState::default();
    let has_f2_primary_rows = primary_rows.iter().any(|(k, _)| k.contains(':'));
    for (_, entry) in primary_rows {
        apply_checkpoint_entry_if_absent(&mut state, entry, &hints.fighter_item_types);
    }
    let (mut migrated_cluster_keys, mut migrated_cluster_fighters, mut migrated_cluster_ships) = (Vec::new(), HashSet::new(), HashSet::new());
    let (mut migrated_ext_damage_fids, mut migrated_ext_tube_rearms, mut saw_legacy_fallback) = (Vec::new(), Vec::new(), false);

    // Two-pass merge from legacy `CLUSTER_SYSTEM_ID` rows so `Squadron` and `TubeLoad`
    // populate before auxiliary entries (`cd`/`ch`/`da`/`tr`) whose keys sort before `sq`.
    if let Some(cluster_resolved) = hints.cluster_rows_resolved {
        let mut record_migrated = |entry: &FighterCheckpointEntry| match entry {
            FighterCheckpointEntry::Squadron(sq) => {
                migrated_cluster_fighters.extend([sq.ball_id, sq.item_id]);
                if sq.parent_ship_id > 0 { migrated_cluster_ships.insert(sq.parent_ship_id); }
            }
            FighterCheckpointEntry::TubeLoad((sid, _), fid) | FighterCheckpointEntry::TubeLoadTyped((sid, _), fid, _) => {
                migrated_cluster_fighters.insert(*fid);
                migrated_cluster_ships.insert(*sid);
            }
            FighterCheckpointEntry::TubeRearmUntil((sid, _), _) => { migrated_cluster_ships.insert(*sid); }
            FighterCheckpointEntry::Cooldown((id, _), _) | FighterCheckpointEntry::Charge((id, _), _) | FighterCheckpointEntry::UnitDamageAccum(id, _) => {
                migrated_cluster_fighters.insert(*id);
            }
        };
        let mut deferred = Vec::new();
        for (key, entry, resolved_sys) in cluster_resolved {
            match &entry {
                FighterCheckpointEntry::Squadron(_) | FighterCheckpointEntry::TubeLoad(_, _) | FighterCheckpointEntry::TubeLoadTyped(_, _, _) => {
                    if resolved_sys == solar_system_id {
                        record_migrated(&entry);
                        apply_checkpoint_entry_if_absent(&mut state, entry, &hints.fighter_item_types);
                        migrated_cluster_keys.push(key);
                        saw_legacy_fallback = true;
                    } else if resolved_sys == CLUSTER_SYSTEM_ID { deferred.push((key, entry, resolved_sys)); }
                }
                _ if resolved_sys == solar_system_id || resolved_sys == CLUSTER_SYSTEM_ID => deferred.push((key, entry, resolved_sys)),
                _ => {}
            }
        }
        for (key, entry, resolved_sys) in deferred {
            let matches_loaded = resolved_sys == CLUSTER_SYSTEM_ID && match &entry {
                FighterCheckpointEntry::Cooldown((id, _), _) | FighterCheckpointEntry::Charge((id, _), _) | FighterCheckpointEntry::UnitDamageAccum(id, _) => {
                    state.active_fighters.values().any(|f| f.ball_id == *id || f.item_id == *id) || state.tube_loads.values().any(|&(fid, _)| fid == *id)
                }
                FighterCheckpointEntry::TubeRearmUntil((sid, _), _) => {
                    state.active_fighters.values().any(|f| f.parent_ship_id == *sid) || state.tube_loads.keys().any(|&(s, _)| s == *sid)
                }
                FighterCheckpointEntry::Squadron(sq) => state.active_fighters.values().any(|f| f.ball_id == sq.ball_id || f.item_id == sq.item_id),
                FighterCheckpointEntry::TubeLoad((sid, _), fid) | FighterCheckpointEntry::TubeLoadTyped((sid, _), fid, _) => {
                    state.active_fighters.values().any(|f| f.parent_ship_id == *sid || f.item_id == *fid)
                }
            };
            if resolved_sys == solar_system_id || matches_loaded {
                record_migrated(&entry);
                apply_checkpoint_entry_if_absent(&mut state, entry, &hints.fighter_item_types);
                migrated_cluster_keys.push(key);
                saw_legacy_fallback = true;
            }
        }
    }

    // Apply legacy monolith entries & space-abandoned cat-87 hints BEFORE matching
    // `extended_runtime_json` damage/rearm timers so `ball_id != item_id` resolves (`F2-R4-03`).
    for entry in extra_entries {
        apply_checkpoint_entry_if_absent(&mut state, entry.clone(), &hints.fighter_item_types);
        saw_legacy_fallback = true;
    }
    for sq in hints.abandoned_cat87 {
        if !state.active_fighters.contains_key(&sq.ball_id) && !state.active_fighters.values().any(|f| f.item_id == sq.item_id) {
            state.active_fighters.insert(sq.ball_id, sq);
        }
    }
    for (fid, dmg, sys) in hints.ext_damage {
        let belongs = dmg > 0.0 && (sys == solar_system_id || (sys == CLUSTER_SYSTEM_ID && state.active_fighters.values().any(|f| f.ball_id == fid || f.item_id == fid)));
        if belongs {
            migrated_ext_damage_fids.push(fid);
            if (!has_f2_primary_rows || !extra_entries.is_empty() || migrated_cluster_fighters.contains(&fid)) && !state.unit_damage_accum.contains_key(&fid) {
                state.unit_damage_accum.insert(fid, dmg);
                saw_legacy_fallback = true;
            }
        }
    }
    for ((ship_id, tube_idx), until, sys) in hints.ext_tube_rearms {
        let belongs = until != 0 && (sys == solar_system_id || (sys == CLUSTER_SYSTEM_ID && (state.tube_loads.keys().any(|&(sid, _)| sid == ship_id) || state.active_fighters.values().any(|f| f.parent_ship_id == ship_id))));
        if belongs {
            migrated_ext_tube_rearms.push((ship_id, tube_idx));
            if (!has_f2_primary_rows || !extra_entries.is_empty() || migrated_cluster_ships.contains(&ship_id)) && !state.tube_rearm_until.contains_key(&(ship_id, tube_idx)) {
                state.tube_rearm_until.insert((ship_id, tube_idx), until);
                saw_legacy_fallback = true;
            }
        }
    }
    MergeFighterOutcome { state, saw_legacy_fallback, migrated_cluster_keys, migrated_ext_damage_fids, migrated_ext_tube_rearms }
}

pub(in crate::system_actor) fn prune_migrated_ext_json_in_snap(
    snap: &mut WorldStateSnapshot, migrated_damage_fids: &[i64], migrated_tube_rearms: &[(i64, usize)],
) {
    let raw = snap.combat_crimewatch_state.extended_runtime_json.as_str();
    if (migrated_damage_fids.is_empty() && migrated_tube_rearms.is_empty()) || raw.is_empty() { return; }
    let Ok(mut val) = serde_json::from_str::<serde_json::Value>(raw) else { return; };
    let Some(root) = val.as_object_mut() else { return; };
    let damage_keys: HashSet<String> = migrated_damage_fids.iter().map(|fid| fid.to_string()).collect();
    let rearm_set: HashSet<(i64, usize)> = migrated_tube_rearms.iter().copied().collect();
    let mut changed = false;
    if !damage_keys.is_empty() {
        if let Some(serde_json::Value::Object(map)) = root.get_mut("fighter_unit_damage_accum") {
            let before = map.len();
            map.retain(|k, _| !damage_keys.contains(k));
            changed |= map.len() != before;
            if map.is_empty() { root.remove("fighter_unit_damage_accum"); }
        }
    }
    if !rearm_set.is_empty() {
        if let Some(serde_json::Value::Array(arr)) = root.get_mut("fighter_tube_rearm_until") {
            let before = arr.len();
            arr.retain(|entry| {
                let key = entry.as_array().and_then(|p| p.first()).and_then(|k| k.as_array()).and_then(|kt| {
                    Some((kt.first()?.as_i64()?, usize::try_from(kt.get(1)?.as_u64()?).ok()?))
                });
                key.is_none_or(|k| !rearm_set.contains(&k))
            });
            changed |= arr.len() != before;
            if arr.is_empty() { root.remove("fighter_tube_rearm_until"); }
        }
    }
    if changed {
        snap.combat_crimewatch_state.extended_runtime_json = if root.is_empty() { String::new() } else { serde_json::to_string(&val).unwrap_or_default() };
    }
}

pub(crate) fn collect_legacy_ext_prune_targets(
    db: &WorldRepository,
    sde: Option<&SdeRegistry>,
    solar_system_id: SolarSystemId,
    state: &SystemFighterState,
) -> Option<(Vec<i64>, Vec<(i64, usize)>)> {
    if solar_system_id == CLUSTER_SYSTEM_ID
        || db.with_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json.is_empty())
    {
        return None;
    }
    let hints = collect_preloaded_hints(db, sde, solar_system_id, None);
    let damage_fids: Vec<i64> = hints.ext_damage.into_iter().filter_map(|(fid, dmg, sys)| {
        let belongs = dmg > 0.0 && (sys == solar_system_id
            || (sys == CLUSTER_SYSTEM_ID && state.active_fighters.values().any(|f| f.ball_id == fid || f.item_id == fid)));
        belongs.then_some(fid)
    }).collect();
    let tube_rearms: Vec<(i64, usize)> = hints.ext_tube_rearms.into_iter().filter_map(|((sid, tidx), until, sys)| {
        let belongs = until != 0 && (sys == solar_system_id
            || (sys == CLUSTER_SYSTEM_ID && (state.tube_loads.keys().any(|&(s, _)| s == sid)
                || state.active_fighters.values().any(|f| f.parent_ship_id == sid))));
        belongs.then_some((sid, tidx))
    }).collect();
    (!damage_fids.is_empty() || !tube_rearms.is_empty()).then_some((damage_fids, tube_rearms))
}

fn prune_migrated_ext_json(db: &WorldRepository, dmg_fids: &[i64], rearms: &[(i64, usize)]) {
    if dmg_fids.is_empty() && rearms.is_empty() { return; }
    if let Err(err) = db.try_with_mut_snapshot(|snap| -> Result<(), String> { prune_migrated_ext_json_in_snap(snap, dmg_fids, rearms); Ok(()) }) {
        tracing::warn!(error = %err, "Failed to prune migrated legacy fighter fields from extended_runtime_json");
    }
}

fn load_fighter_rows(db: &WorldRepository, solar_system_id: SolarSystemId, nonblocking: bool) -> Result<(Vec<(String, FighterCheckpointEntry)>, Vec<(String, FighterCheckpointEntry)>), String> {
    let read_pair = |conn: &rusqlite::Connection| -> Result<_, String> {
        Ok((load_system_checkpoints::<FighterCheckpointEntry>(conn, solar_system_id, KIND_FIGHTERS)?, load_system_checkpoints::<FighterCheckpointEntry>(conn, CLUSTER_SYSTEM_ID, KIND_FIGHTERS)?))
    };
    if !nonblocking { return db.with_sqlite_conn(|conn| read_pair(conn)); }
    if let Some(res) = db.try_with_sqlite_conn(read_pair) { return res; }
    if let Some(path) = db.sqlite_path().filter(|p| p.exists()) {
        let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .map_err(|e| format!("open readonly fighter checkpoint conn: {e}"))?;
        let _ = conn.busy_timeout(std::time::Duration::from_millis(50));
        return read_pair(&conn);
    }
    Err(format!("SQLite writer contended during non-blocking fighter load for system {solar_system_id}"))
}

fn load_system_fighters_internal(
    solar_system_id: SolarSystemId,
    db: &WorldRepository,
    sde: Option<&SdeRegistry>,
    allow_legacy_cleanup: bool,
    nonblocking: bool,
) -> Result<SystemFighterState, String> {
    if solar_system_id == CLUSTER_SYSTEM_ID {
        return Ok(SystemFighterState::default());
    }

    let (primary_rows, cluster_rows) = load_fighter_rows(db, solar_system_id, nonblocking)?;

    let hints = collect_preloaded_hints(
        db, sde, solar_system_id, (!cluster_rows.is_empty()).then_some(cluster_rows.as_slice()),
    );
    let out = merge_loaded_fighter_state(solar_system_id, primary_rows, hints, &[]);

    let mut ext_pruned_ok = true;
    if allow_legacy_cleanup && out.saw_legacy_fallback {
        let rows = collect_fighter_checkpoint_rows(&out.state, &HashSet::new());
        let now_unix = current_unix_secs();
        let cleanup = |conn: &mut rusqlite::Connection| {
            replace_system_checkpoints_and_delete_legacy(
                conn, solar_system_id, KIND_FIGHTERS, &rows, now_unix,
                CLUSTER_SYSTEM_ID, &out.migrated_cluster_keys,
            )
        };
        if nonblocking {
            match db.try_with_sqlite_conn_mut(cleanup) {
                Some(Ok(())) => ext_pruned_ok = true,
                Some(Err(e)) => return Err(e),
                None => {
                    return Err(format!(
                        "SQLite writer contended during legacy fighter cleanup for system {solar_system_id}"
                    ));
                }
            }
        } else {
            db.with_sqlite_conn(cleanup)?;
            ext_pruned_ok = true;
        }
    }
    if allow_legacy_cleanup
        && !nonblocking
        && ext_pruned_ok
        && (!out.migrated_ext_damage_fids.is_empty() || !out.migrated_ext_tube_rearms.is_empty())
    {
        prune_migrated_ext_json(db, &out.migrated_ext_damage_fids, &out.migrated_ext_tube_rearms);
    }

    Ok(out.state)
}

/// Loads `SystemFighterState` for a solar system activating in `prepare_system_activation`
/// without blocking on `db.writer` (`prune_migrated_ext_json` runs via post-unlock worker).
pub(crate) fn load_system_fighters_on_activation(
    solar_system_id: SolarSystemId,
    _ballpark: &Ballpark,
    db: &WorldRepository,
    sde: Option<&SdeRegistry>,
) -> Result<SystemFighterState, String> {
    load_system_fighters_internal(solar_system_id, db, sde, true, true)
}

/// Read-only `Cold` system loader for docked reads without waking a `Cold` actor.
pub(crate) fn load_cold_fighters_for_system(
    db: &WorldRepository,
    sde: Option<&SdeRegistry>,
    solar_system_id: SolarSystemId,
) -> Result<SystemFighterState, String> {
    load_system_fighters_internal(solar_system_id, db, sde, false, false)
}

struct ColdFighterTxOut<R> { result: R, persisted: bool, ext_damage_fids: Vec<i64>, ext_tube_rearms: Vec<(i64, usize)> }

fn run_cold_fighter_conn_tx<R>(
    conn: &mut rusqlite::Connection, solar_system_id: SolarSystemId, hints: PreloadedLegacyHints,
    extra_entries: &[FighterCheckpointEntry], f: impl FnOnce(&mut SystemFighterState) -> R, should_persist: impl Fn(&R) -> bool,
) -> Result<ColdFighterTxOut<R>, String> {
    let primary_rows = load_system_checkpoints::<FighterCheckpointEntry>(conn, solar_system_id, KIND_FIGHTERS).unwrap_or_default();
    let mut merged = merge_loaded_fighter_state(solar_system_id, primary_rows, hints, extra_entries);
    let result = f(&mut merged.state);
    let persisted = should_persist(&result);
    if persisted {
        let rows = collect_fighter_checkpoint_rows(&merged.state, &HashSet::new());
        replace_system_checkpoints_and_delete_legacy(
            conn, solar_system_id, KIND_FIGHTERS, &rows, current_unix_secs(), CLUSTER_SYSTEM_ID, &merged.migrated_cluster_keys,
        )?;
    }
    Ok(ColdFighterTxOut {
        result, persisted,
        ext_damage_fids: if persisted { merged.migrated_ext_damage_fids } else { Vec::new() },
        ext_tube_rearms: if persisted { merged.migrated_ext_tube_rearms } else { Vec::new() },
    })
}

/// Atomically loads, merges optional `extra_entries`, mutates, and conditionally persists a `Cold`
/// system's fighter state (plus atomic cluster-key cleanup) within a single SQLite transaction.
pub(crate) fn with_cold_fighters_with_entries_in_db_result<R>(
    db: &WorldRepository, sde: Option<&SdeRegistry>, solar_system_id: SolarSystemId,
    extra_entries: &[FighterCheckpointEntry], f: impl FnOnce(&mut SystemFighterState) -> R, should_persist: impl Fn(&R) -> bool,
) -> Result<R, String> {
    if solar_system_id == CLUSTER_SYSTEM_ID { let mut st = SystemFighterState::default(); return Ok(f(&mut st)); }
    let mut f_opt = Some(f);
    let hints = collect_preloaded_hints(db, sde, solar_system_id, None);
    let first_pass = db.with_sqlite_conn(|conn| -> Result<Result<ColdFighterTxOut<R>, Vec<(String, FighterCheckpointEntry)>>, String> {
        let cluster_rows = load_system_checkpoints::<FighterCheckpointEntry>(conn, CLUSTER_SYSTEM_ID, KIND_FIGHTERS).unwrap_or_default();
        if !cluster_rows.is_empty() { return Ok(Err(cluster_rows)); }
        let cb = f_opt.take().expect("with_cold_fighters_mut_in_db_result closure must run once");
        run_cold_fighter_conn_tx(conn, solar_system_id, hints, extra_entries, cb, &should_persist).map(Ok)
    })?;
    let tx_out = match first_pass {
        Ok(out) => out,
        Err(cluster_rows) => {
            let hints2 = collect_preloaded_hints(db, sde, solar_system_id, Some(&cluster_rows));
            let cb = f_opt.take().expect("with_cold_fighters_mut_in_db_result closure must run once");
            db.with_sqlite_conn(|conn| run_cold_fighter_conn_tx(conn, solar_system_id, hints2, extra_entries, cb, &should_persist))?
        }
    };
    if tx_out.persisted && (!tx_out.ext_damage_fids.is_empty() || !tx_out.ext_tube_rearms.is_empty()) {
        prune_migrated_ext_json(db, &tx_out.ext_damage_fids, &tx_out.ext_tube_rearms);
    }
    Ok(tx_out.result)
}

/// Atomically loads, mutates, and conditionally persists a `Cold` system's fighter state within
/// a single `with_sqlite_conn` connection acquisition.
pub(crate) fn with_cold_fighters_mut_in_db_result<R>(
    db: &WorldRepository, sde: Option<&SdeRegistry>, solar_system_id: SolarSystemId,
    f: impl FnOnce(&mut SystemFighterState) -> R, should_persist: impl Fn(&R) -> bool,
) -> Result<R, String> {
    with_cold_fighters_with_entries_in_db_result(db, sde, solar_system_id, &[], f, should_persist)
}

pub(crate) fn read_all_cold_fighter_tube_items(db: &WorldRepository, live_systems: &HashSet<SolarSystemId>) -> HashSet<i64> {
    let mut out = HashSet::new();
    let rows = db.with_sqlite_conn(|c| load_all_checkpoints_for_kind::<FighterCheckpointEntry>(c, KIND_FIGHTERS).unwrap_or_default());
    for (sys_id, _, entry) in rows {
        if sys_id != CLUSTER_SYSTEM_ID && live_systems.contains(&sys_id) { continue; }
        if let FighterCheckpointEntry::TubeLoad(_, fid) | FighterCheckpointEntry::TubeLoadTyped(_, fid, _) = entry { out.insert(fid); }
    }
    out
}

fn abandon_fighters_in_state(fg: &mut SystemFighterState, char_id: i64, ship_id: i64, db_in_sys_items: &HashSet<i64>) -> Vec<(i64, i64, usize)> {
    let mut abandoned = Vec::new();
    for f in fg.active_fighters.values_mut() {
        if !f.is_abandoned && db_in_sys_items.contains(&f.item_id) && ((char_id > 0 && f.owner_char_id == char_id) || f.parent_ship_id == ship_id) {
            f.is_abandoned = true;
            f.parent_ship_id = 0;
            f.target_id = None;
            abandoned.push((f.ball_id, f.item_id, f.tube_index));
        }
    }
    for &(_, _, tidx) in &abandoned { fg.tube_loads.remove(&(ship_id, tidx)); }
    abandoned
}

enum AsyncColdStep {
    Done(Result<Option<i64>, FighterActorError>),
    CallLive(SystemHandle),
    WaitGate(Arc<LifecycleGate>),
    Retry,
}

pub(crate) fn load_fighters_for_shutdown_sync(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
    sde_opt: Option<&SdeRegistry>,
    coord: &FighterPersistCoord,
    lifecycle_gen: u64,
) -> Result<Option<SystemFighterState>, String> {
    if solar_system_id == CLUSTER_SYSTEM_ID {
        return Ok(None);
    }
    if held_lock_ranks().iter().any(|&r| r >= RANK_SERVICE_STATE) {
        tracing::warn!(
            solar_system_id,
            "SystemActor skipping fighter shutdown hydration while holding >= RANK_SERVICE_STATE; keeping dirty flag"
        );
        return Err("held RANK_SERVICE_STATE".to_string());
    }
    let slot = coord.slot_for(solar_system_id);
    let mut wm = slot.lock_io();
    let loaded = load_system_fighters_internal(solar_system_id, db, sde_opt, true, false)?;
    if lifecycle_gen > 0 {
        wm.on_system_loaded_if_unloaded(lifecycle_gen);
    }
    Ok(Some(loaded))
}

impl SystemRegistry {
    pub(crate) fn prepare_initial_fighters_for_activation(
        &self, solar_system_id: SolarSystemId, generation: u64, db_read_safe: bool,
        db_opt: Option<&Arc<WorldRepository>>, _initial_ballpark: &Ballpark, sde: Option<&SdeRegistry>,
    ) -> Result<(SystemFighterState, bool), SystemActivationError> {
        let Some(db) = db_opt.filter(|_| solar_system_id != CLUSTER_SYSTEM_ID) else {
            return Ok((SystemFighterState::default(), false));
        };
        if !db_read_safe { return Ok((SystemFighterState::default(), true)); }
        if held_lock_ranks().iter().any(|&r| r >= RANK_SERVICE_STATE) {
            return Err(SystemActivationError::WaitRejectedWhileHoldingLocks { solar_system_id });
        }
        let slot = self.fighter_persist.slot_for(solar_system_id);
        let mut io_guard = slot.lock_io();
        if !slot.is_loading_gen_active(generation) {
            return Err(SystemActivationError::ConcurrentActivationAborted { solar_system_id });
        }
        match load_system_fighters_on_activation(solar_system_id, _initial_ballpark, db, sde) {
            Ok(loaded) => { io_guard.on_system_loaded(generation); Ok((loaded, false)) }
            Err(err) => {
                tracing::warn!(solar_system_id, error = %err, "Deferring fighter SQLite hydration post-activation");
                Ok((SystemFighterState::default(), true))
            }
        }
    }

    fn load_hydration_state_under_io(&self, solar_system_id: SolarSystemId, handle: &SystemHandle) -> Result<Option<SystemFighterState>, String> {
        if !handle.fighters_needs_hydration.load(std::sync::atomic::Ordering::Acquire) { return Ok(None); }
        if held_lock_ranks().iter().any(|&r| r >= RANK_SERVICE_STATE) {
            return Err(format!("Refusing fighter hydration on system {solar_system_id} while holding >= RANK_SERVICE_STATE"));
        }
        let Some(ref db) = self.db else {
            handle.fighters_needs_hydration.store(false, std::sync::atomic::Ordering::Release);
            return Ok(None);
        };
        let slot = self.fighter_persist.slot_for(solar_system_id);
        let _io_guard = slot.lock_io();
        if !handle.fighters_needs_hydration.load(std::sync::atomic::Ordering::Acquire) { return Ok(None); }
        Ok(Some(load_system_fighters_internal(solar_system_id, db, self.sde.as_deref(), true, false)?))
    }

    fn finish_hydration_watermark(&self, solar_system_id: SolarSystemId, gen: u64) {
        if gen > 0 { self.fighter_persist.slot_for(solar_system_id).lock_io().on_system_loaded_if_unloaded(gen); }
    }

    pub(crate) fn ensure_live_fighters_hydrated_sync(&self, solar_system_id: SolarSystemId, handle: &SystemHandle) -> Result<(), String> {
        if !handle.fighters_needs_hydration.load(std::sync::atomic::Ordering::Acquire) { return Ok(()); }
        enum SyncRtKind { None, CurrentThread, MultiThread }
        let rt_kind = match tokio::runtime::Handle::try_current() {
            Err(_) => SyncRtKind::None,
            Ok(h) if matches!(h.runtime_flavor(), tokio::runtime::RuntimeFlavor::CurrentThread) => SyncRtKind::CurrentThread,
            Ok(_) => SyncRtKind::MultiThread,
        };
        let slot = self.fighter_persist.slot_for(solar_system_id);
        let mut _flight_guard = None;
        loop {
            if !handle.fighters_needs_hydration.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(());
            }
            match slot.try_begin_hydrate() {
                Ok(guard) => {
                    _flight_guard = Some(guard);
                    break;
                }
                Err(_) if matches!(rt_kind, SyncRtKind::CurrentThread) => break,
                Err(existing) => match rt_kind {
                    SyncRtKind::MultiThread => tokio::task::block_in_place(|| existing.wait_sync()),
                    SyncRtKind::None => existing.wait_sync(),
                    SyncRtKind::CurrentThread => break,
                },
            }
        }
        let Some(loaded) = self.load_hydration_state_under_io(solar_system_id, handle)? else {
            return Ok(());
        };
        let (gen, _) = handle.merge_fighter_state_sync(loaded, false, true)?;
        self.finish_hydration_watermark(solar_system_id, gen);
        Ok(())
    }

    pub(crate) async fn ensure_live_fighters_hydrated_async(
        &self,
        solar_system_id: SolarSystemId,
        handle: &SystemHandle,
    ) -> Result<(), String> {
        if !handle.fighters_needs_hydration.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }
        let slot = self.fighter_persist.slot_for(solar_system_id);
        let _flight_guard = loop {
            if !handle.fighters_needs_hydration.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(());
            }
            match slot.try_begin_hydrate() {
                Ok(guard) => break guard,
                Err(existing) => existing.wait_async().await,
            }
        };
        let Some(loaded) = self.load_hydration_state_under_io(solar_system_id, handle)? else {
            return Ok(());
        };
        let (gen, _) = handle.merge_fighter_state_async(loaded, false, true).await?;
        self.finish_hydration_watermark(solar_system_id, gen);
        Ok(())
    }

    pub(crate) async fn ensure_live_fighters_hydrated_if_unlocked_async(&self, solar_system_id: SolarSystemId, handle: &SystemHandle) {
        if handle.fighters_needs_hydration.load(std::sync::atomic::Ordering::Acquire) && held_lock_ranks().is_empty() {
            let _ = self.ensure_live_fighters_hydrated_async(solar_system_id, handle).await;
        }
    }

    pub(crate) fn ensure_live_fighters_hydrated_if_unlocked_sync(&self, solar_system_id: SolarSystemId, handle: &SystemHandle) {
        if handle.fighters_needs_hydration.load(std::sync::atomic::Ordering::Acquire) && held_lock_ranks().is_empty() {
            let _ = self.ensure_live_fighters_hydrated_sync(solar_system_id, handle);
        }
    }

    fn run_cold_fighter_op_guarded<R>(
        &self, solar_system_id: SolarSystemId, cold_entries: &[FighterCheckpointEntry],
        mut cold_fn: impl FnMut(&mut SystemFighterState) -> R, should_persist: impl Fn(&R) -> bool,
        mut live_fn: impl FnMut(&SystemHandle) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut cold_fn_opt = Some(&mut cold_fn);
        for _ in 0..64 {
            match self.probe_fighter_cold_or_live(solar_system_id) {
                Err(()) => return Err(format!("Refusing cold fighter op on system {solar_system_id} while holding >= RANK_SERVICE_STATE")),
                Ok(FighterColdProbe::Live(handle)) => {
                    match self.ensure_live_fighters_hydrated_sync(solar_system_id, &handle).and_then(|()| live_fn(&handle)) {
                        Ok(out) => return Ok(out),
                        Err(err) => {
                            let same_live = matches!(self.probe_fighter_cold_or_live(solar_system_id), Ok(FighterColdProbe::Live(ref cur)) if Arc::ptr_eq(&cur.ballpark, &handle.ballpark));
                            if !same_live { std::thread::yield_now(); continue; }
                            return Err(err);
                        }
                    }
                }
                Ok(FighterColdProbe::WaitGate(gate)) => {
                    if !self.wait_fighter_gate_sync(solar_system_id, &gate) {
                        return Err(format!("Interrupted or timed out waiting for lifecycle gate on system {solar_system_id}"));
                    }
                }
                Ok(FighterColdProbe::ColdReady) => {
                    let Some(ref db) = self.db else { return Err(format!("No repository configured for cold fighter op on system {solar_system_id}")); };
                    let slot = self.fighter_persist.slot_for(solar_system_id);
                    let mut wm = slot.lock_io();
                    if !slot.is_cold_mode() { drop(wm); std::thread::yield_now(); continue; }
                    let cb = cold_fn_opt.take().ok_or_else(|| "Cold op closure already consumed".to_string())?;
                    let res = with_cold_fighters_with_entries_in_db_result(db, self.sde.as_deref(), solar_system_id, cold_entries, cb, &should_persist);
                    if matches!(&res, Ok(r) if should_persist(r)) { wm.on_cold_mutated(); }
                    return res;
                }
            }
        }
        Err(format!("Exceeded lifecycle transition retries on cold fighter op for system {solar_system_id}"))
    }

    fn step_cold_fighter_tube_op(
        &self,
        solar_system_id: SolarSystemId,
        op: &FighterTubeOp,
        is_read_only: bool,
    ) -> AsyncColdStep {
        match self.probe_fighter_cold_or_live(solar_system_id) {
            Err(()) => AsyncColdStep::Done(Err(FighterActorError::plain(
                "LockOrderViolation",
                format!(
                    "Cannot mutate cold fighter state on system {solar_system_id} while holding service lock"
                ),
            ))),
            Ok(FighterColdProbe::Live(handle)) => AsyncColdStep::CallLive(handle),
            Ok(FighterColdProbe::WaitGate(gate)) => AsyncColdStep::WaitGate(gate),
            Ok(FighterColdProbe::ColdReady) => {
                let Some(ref db) = self.db else {
                    return AsyncColdStep::Done(Err(FighterActorError::plain(
                        "SolarSystemUnavailable",
                        format!("No repository configured for system {solar_system_id}"),
                    )));
                };
                let slot = self.fighter_persist.slot_for(solar_system_id);
                let mut wm = slot.lock_io();
                if !slot.is_cold_mode() {
                    return AsyncColdStep::Retry;
                }
                if is_read_only {
                    let out = match load_cold_fighters_for_system(db, self.sde.as_deref(), solar_system_id) {
                        Ok(mut state) => exec_tube_op_in_state(&mut state, op.clone()),
                        Err(e) => Err(FighterActorError::plain("StorageError", e)),
                    };
                    AsyncColdStep::Done(out)
                } else {
                    let res = with_cold_fighters_mut_in_db_result(
                        db,
                        self.sde.as_deref(),
                        solar_system_id,
                        |state| exec_tube_op_in_state(state, op.clone()),
                        Result::is_ok,
                    )
                    .map_err(|e| FighterActorError::plain("StorageError", e))
                    .and_then(|inner| inner);
                    if res.is_ok() {
                        wm.on_cold_mutated();
                    }
                    AsyncColdStep::Done(res)
                }
            }
        }
    }

    /// Atomically abandons matching deployed fighters in SQLite while `solar_system_id` is `Cold`.
    pub(crate) fn abandon_fighters_in_cold_db(&self, solar_system_id: SolarSystemId, char_id: i64, ship_id: i64, db_in_sys_items: &HashSet<i64>) -> Result<Vec<(i64, i64, usize)>, String> {
        self.run_cold_fighter_op_guarded(
            solar_system_id, &[], |fg| abandon_fighters_in_state(fg, char_id, ship_id, db_in_sys_items),
            |ab| !ab.is_empty(), |h| h.abandon_ship_fighters_sync(char_id, ship_id, db_in_sys_items.clone()).map(|r| r.abandoned),
        )
    }

    pub(crate) fn seed_abandoned_fighters_in_system(&self, solar_system_id: SolarSystemId, list: Vec<ActiveFighterSquadron>) -> Result<(), String> {
        if list.is_empty() || solar_system_id == CLUSTER_SYSTEM_ID { return Ok(()); }
        self.run_cold_fighter_op_guarded(
            solar_system_id, &[],
            |state| { for sq in &list { state.active_fighters.entry(sq.ball_id).or_insert_with(|| sq.clone()); } },
            |_| true, |h| h.seed_abandoned_fighters_sync(list.clone()),
        )
    }

    /// Groups persisted abandoned fighter squadrons by `solar_system_id` and seeds each non-zero system.
    pub(crate) fn seed_abandoned_fighters_by_system(&self, fighters: Vec<(SolarSystemId, ActiveFighterSquadron)>) {
        let mut by_sys: HashMap<SolarSystemId, Vec<ActiveFighterSquadron>> = HashMap::new();
        for (sys_id, sq) in fighters { if sys_id != 0 { by_sys.entry(sys_id).or_default().push(sq); } }
        for (sys_id, list) in by_sys {
            if let Err(err) = self.seed_abandoned_fighters_in_system(sys_id, list) {
                tracing::error!(solar_system_id = sys_id, error = %err, "Failed to seed abandoned fighters into system");
            }
        }
    }

    /// Resolves a deployed fighter's owning player character ID across live systems,
    /// hydrating unhydrated actors first when no lock-rank locks are held (`BF-02`).
    pub(crate) fn resolve_fighter_owner_char_id(&self, killer_id: i64) -> Option<i64> {
        for (sys_id, handle) in self.active_system_handles() {
            self.ensure_live_fighters_hydrated_if_unlocked_sync(sys_id, &handle);
            let af = &handle.view().fighters.active_fighters;
            if let Some(cid) = af
                .get(&killer_id)
                .or_else(|| af.values().find(|f| f.ball_id == killer_id || f.item_id == killer_id))
                .map(|f| f.owner_char_id)
                .filter(|&cid| cid > 0)
            {
                return Some(cid);
            }
        }
        None
    }

    /// Executes a tube load/unload/rollback operation against a live [`SystemActor`] if running
    /// or atomically in SQLite `KIND_FIGHTERS` while `Cold` (without waking docked hubs).
    pub async fn load_or_unload_fighter_tube(&self, solar_system_id: SolarSystemId, op: FighterTubeOp) -> Result<Option<i64>, FighterActorError> {
        let is_read_only = matches!(op, FighterTubeOp::PreCheckLoad { .. });
        for _ in 0..64 {
            match self.step_cold_fighter_tube_op(solar_system_id, &op, is_read_only) {
                AsyncColdStep::Done(res) => return res,
                AsyncColdStep::CallLive(handle) => {
                    self.ensure_live_fighters_hydrated_async(solar_system_id, &handle).await
                        .map_err(|e| FighterActorError::plain("StorageError", e))?;
                    return handle.load_or_unload_fighter_tube(op).await;
                }
                AsyncColdStep::WaitGate(gate) => {
                    if !self.wait_fighter_gate_async(solar_system_id, &gate).await {
                        return Err(FighterActorError::plain("SolarSystemUnavailable", format!("Timed out waiting for lifecycle gate on system {solar_system_id}")));
                    }
                }
                AsyncColdStep::Retry => tokio::task::yield_now().await,
            }
        }
        if let Some(handle) = self.get_handle(solar_system_id) {
            self.ensure_live_fighters_hydrated_async(solar_system_id, &handle).await
                .map_err(|e| FighterActorError::plain("StorageError", e))?;
            return handle.load_or_unload_fighter_tube(op).await;
        }
        Err(FighterActorError::plain("SolarSystemUnavailable", format!("Exceeded lifecycle transition retries on system {solar_system_id}")))
    }

    fn finish_fighter_checkpoint_flush(solar_system_id: SolarSystemId, res: Result<usize, String>) -> usize {
        match res {
            Ok(n) => n,
            Err(err) => {
                tracing::warn!(
                    solar_system_id,
                    error = %err,
                    "Failed to persist system fighter checkpoint; keeping dirty flag"
                );
                0
            }
        }
    }

    /// Flushes `KIND_FIGHTERS` checkpoints to SQLite for `solar_system_id` only when
    /// `fighters_dirty` is set, serialized per system with `(lifecycle_gen, fighters_rev)` guards.
    pub fn flush_system_fighter_checkpoint_sync(&self, solar_system_id: SolarSystemId) -> usize {
        let Some(handle) = self.get_handle(solar_system_id) else {
            return 0;
        };
        if let Err(err) = self.ensure_live_fighters_hydrated_sync(solar_system_id, &handle) {
            tracing::warn!(solar_system_id, error = %err, "Deferring fighter checkpoint flush until hydration succeeds");
            return 0;
        }
        Self::finish_fighter_checkpoint_flush(solar_system_id, handle.flush_fighter_checkpoint_sync())
    }

    /// Async counterpart to [`Self::flush_system_fighter_checkpoint_sync`] (`#1541` Track F2).
    pub async fn flush_system_fighter_checkpoint_async(&self, solar_system_id: SolarSystemId) -> usize {
        let Some(handle) = self.get_handle(solar_system_id) else {
            return 0;
        };
        if let Err(err) = self.ensure_live_fighters_hydrated_async(solar_system_id, &handle).await {
            tracing::warn!(solar_system_id, error = %err, "Deferring fighter checkpoint flush until hydration succeeds");
            return 0;
        }
        if handle.fighter_ctx.dirty_stamp.load(Ordering::Acquire) == 0 {
            return 0;
        }
        Self::finish_fighter_checkpoint_flush(solar_system_id, handle.flush_fighter_checkpoint().await)
    }

    /// Flushes dirty `KIND_FIGHTERS` checkpoints across currently live system actors outside
    /// ballpark/actor/registry locks (`#1541` Track F2).
    pub fn flush_all_fighter_checkpoints_to_db(&self) -> usize {
        let mut total = 0usize;
        for (sys_id, _) in self.active_system_handles() {
            total += self.flush_system_fighter_checkpoint_sync(sys_id);
        }
        total
    }

    /// Async counterpart to [`Self::flush_all_fighter_checkpoints_to_db`] (`#1541` Track F2).
    pub async fn flush_all_fighter_checkpoints_to_db_async(&self) -> usize {
        let mut total = 0usize;
        for (sys_id, _) in self.active_system_handles() {
            total += self.flush_system_fighter_checkpoint_async(sys_id).await;
        }
        total
    }

    fn apply_ship_tube_transfer(
        &self,
        sys_id: SolarSystemId,
        ship_id: i64,
        transfer: ShipFighterTubeTransfer,
    ) -> Result<(), String> {
        self.run_cold_fighter_op_guarded(
            sys_id,
            &[],
            |fg| fg.insert_ship_tube_transfer(ship_id, transfer.clone()),
            |_| true,
            |h| h.insert_ship_fighter_tubes_sync(ship_id, transfer.clone()),
        )
    }

    /// Transfers loaded fighter tubes and per-tube rearm timers when `ship_id` moves across
    /// solar systems (`from_sys != to_sys`). Works cleanly across `Live`, `Loading`/`Stopping`, and `Cold`.
    pub fn transfer_ship_fighter_tubes(
        &self,
        from_sys: SolarSystemId,
        to_sys: SolarSystemId,
        ship_id: i64,
    ) -> Result<(), String> {
        if from_sys == 0 || to_sys == 0 || from_sys == to_sys || ship_id <= 0 {
            return Ok(());
        }
        let transfer = self.run_cold_fighter_op_guarded(
            from_sys,
            &[],
            |fg| fg.extract_ship_tube_transfer(ship_id),
            |tr| !tr.is_empty(),
            |h| h.extract_ship_fighter_tubes_sync(ship_id),
        )?;
        if transfer.is_empty() {
            return Ok(());
        }
        if let Err(err) = self.apply_ship_tube_transfer(to_sys, ship_id, transfer.clone()) {
            tracing::warn!(
                from_sys,
                to_sys,
                ship_id,
                error = %err,
                "Failed to insert transferred fighter tubes into destination system; restoring to origin"
            );
            if let Err(restore_err) = self.apply_ship_tube_transfer(from_sys, ship_id, transfer) {
                tracing::error!(
                    from_sys,
                    to_sys,
                    ship_id,
                    error = %restore_err,
                    "Failed to restore fighter tubes back to origin system after destination failure"
                );
                return Err(format!("{err}; origin restore failed: {restore_err}"));
            }
            return Err(err);
        }
        Ok(())
    }

    /// Reads `SystemFighterState` for `solar_system_id` without waking a `Cold` solar system.
    pub fn snapshot_fighters_for_system(&self, solar_system_id: SolarSystemId) -> SystemFighterState {
        if let Some(handle) = self.get_handle(solar_system_id) {
            let _ = self.ensure_live_fighters_hydrated_sync(solar_system_id, &handle);
            return handle.view().fighters.clone();
        }
        if let Some(ref db) = self.db {
            return load_cold_fighters_for_system(db, self.sde.as_deref(), solar_system_id)
                .unwrap_or_default();
        }
        SystemFighterState::default()
    }

    /// Resolves `solar_system_id` for a carrier ship or fighter entity across live actors,
    /// SQLite checkpoints, and `WorldStateSnapshot` without waking `Cold` systems.
    /// Processes `entity_ids` in priority order so an explicit item/ball location takes
    /// precedence over fallback parent-ship references.
    pub fn resolve_fighter_entity_system_id(&self, entity_ids: &[i64]) -> SolarSystemId {
        let handles = self.active_system_handles();
        for &(sys_id, ref handle) in &handles {
            if handle.fighters_needs_hydration.load(std::sync::atomic::Ordering::Acquire) {
                let _ = self.ensure_live_fighters_hydrated_sync(sys_id, handle);
            }
        }
        let cp_rows_opt: Option<Vec<(u32, String, FighterCheckpointEntry)>> = self.db.as_ref().and_then(|db| {
            db.try_with_sqlite_conn(|c| load_all_checkpoints_for_kind::<FighterCheckpointEntry>(c, KIND_FIGHTERS).unwrap_or_default())
        });
        for &id in entity_ids.iter().filter(|&&id| id > 0) {
            for &(sys_id, ref handle) in &handles {
                let view = handle.view();
                let fg = &view.fighters;
                if fg.active_fighters.values().any(|f| f.ball_id == id || f.item_id == id)
                    || fg.tube_loads.iter().any(|(&(sid, _), &(fid, _))| sid == id || fid == id)
                    || fg.cooldown_state.keys().any(|&(fid, _)| fid == id)
                    || fg.charge_state.keys().any(|&(fid, _)| fid == id)
                    || fg.unit_damage_accum.contains_key(&id)
                    || fg.tube_rearm_until.keys().any(|&(sid, _)| sid == id)
                    || view.balls.contains_key(&id)
                {
                    return sys_id;
                }
            }
            if let Some(ref cp_rows) = cp_rows_opt {
                for (sys_id, _, entry) in cp_rows.iter().filter(|(sid, _, _)| *sid != CLUSTER_SYSTEM_ID) {
                    let matched = match entry {
                        FighterCheckpointEntry::Squadron(sq) => sq.ball_id == id || sq.item_id == id,
                        FighterCheckpointEntry::Cooldown((eid, _), _) | FighterCheckpointEntry::Charge((eid, _), _) | FighterCheckpointEntry::UnitDamageAccum(eid, _) => *eid == id,
                        FighterCheckpointEntry::TubeLoad((sid, _), fid) | FighterCheckpointEntry::TubeLoadTyped((sid, _), fid, _) => *sid == id || *fid == id,
                        FighterCheckpointEntry::TubeRearmUntil((sid, _), _) => *sid == id,
                    };
                    if matched { return *sys_id; }
                }
            }
            if let Some(sys) = self.db.as_ref().and_then(|db| {
                db.with_snapshot(|snap| {
                    let sys = resolve_entity_solar_system_id_with_sde(snap, self.sde.as_deref(), id);
                    (sys != CLUSTER_SYSTEM_ID).then_some(sys)
                })
            }) {
                return sys;
            }
        }
        for &id in entity_ids.iter().filter(|&&id| id > 0) {
            for &(sys_id, ref handle) in &handles {
                if handle.view().fighters.active_fighters.values().any(|f| f.parent_ship_id == id) { return sys_id; }
            }
            if let Some(sys) = cp_rows_opt.as_ref().and_then(|rows| rows.iter().find_map(|(sid, _, e)| match e {
                FighterCheckpointEntry::Squadron(sq) if *sid != CLUSTER_SYSTEM_ID && sq.parent_ship_id == id => Some(*sid),
                _ => None,
            })) {
                return sys;
            }
        }
        if let Some(sys) = self.db.as_ref().and_then(|db| db.with_snapshot(|s| (s.system_states.len() == 1).then(|| s.system_states.keys().copied().next()).flatten())) {
            return sys;
        }
        if handles.len() == 1 { return handles[0].0; }
        DEFAULT_FALLBACK_SYSTEM_ID
    }
}

/// Reloads per-system fighter state from SQLite `KIND_FIGHTERS` for any currently live `SystemActor`s.
pub(crate) fn reload_live_fighters_from_db(
    systems: &SystemRegistry,
    db: &WorldRepository,
    sde: Option<&SdeRegistry>,
) {
    for (sys_id, handle) in systems.active_system_handles() {
        let slot = systems.fighter_persist.slot_for(sys_id);
        let loaded_res = {
            let _wm = slot.lock_io();
            load_system_fighters_internal(sys_id, db, sde, true, false)
        };
        let Ok(loaded) = loaded_res else { continue; };
        if let Some((gen, rev)) = handle.reload_fighter_state_clean_sync(loaded) {
            let mut wm = slot.lock_io();
            wm.advance_at_least(gen, rev);
        }
    }
}

/// One-way upgrade migration from legacy monolith `PersistedServerRuntimeState` fighter fields
/// directly into per-system SQLite `KIND_FIGHTERS` checkpoints (and live actors if spawned),
/// without ever writing back to legacy server runtime blobs.
pub(crate) fn migrate_legacy_monolith_fighters_one_way(
    db: &WorldRepository,
    sde: Option<&SdeRegistry>,
    systems: &SystemRegistry,
    active_fighters: HashMap<i64, ActiveFighterSquadron>,
    fighter_cooldown_state: Vec<((i64, u32), u64)>,
    fighter_charge_state: Vec<((i64, u32), u32)>,
    fighter_tube_loads: Vec<((i64, usize), i64)>,
) {
    if active_fighters.is_empty() && fighter_cooldown_state.is_empty() && fighter_charge_state.is_empty() && fighter_tube_loads.is_empty() {
        return;
    }
    let (mut by_sys, mut item_types): (HashMap<u32, Vec<FighterCheckpointEntry>>, HashMap<i64, u32>) = (HashMap::new(), HashMap::new());
    db.with_snapshot(|snap| {
        let mut sys_by_fid: HashMap<i64, u32> = HashMap::new();
        for sq in active_fighters.into_values() {
            item_types.insert(sq.item_id, sq.type_id);
            let entry = FighterCheckpointEntry::Squadron(sq);
            let sys = entry_target_solar_system_id(snap, sde, &entry);
            if let FighterCheckpointEntry::Squadron(ref s) = entry {
                if sys != CLUSTER_SYSTEM_ID { sys_by_fid.insert(s.ball_id, sys); sys_by_fid.insert(s.item_id, sys); }
            }
            by_sys.entry(sys).or_default().push(entry);
        }
        for ((sid, tube_idx), fid) in fighter_tube_loads {
            let ftype = snap.items.get(&fid).map(|i| i.type_id).unwrap_or(0);
            if ftype != 0 { item_types.insert(fid, ftype); }
            let entry = FighterCheckpointEntry::TubeLoadTyped((sid, tube_idx), fid, ftype);
            let sys = entry_target_solar_system_id(snap, sde, &entry);
            if sys != CLUSTER_SYSTEM_ID { sys_by_fid.entry(fid).or_insert(sys); }
            by_sys.entry(sys).or_default().push(entry);
        }
        let mut push_stat = |fid: i64, entry: FighterCheckpointEntry| {
            let mut sys = resolve_entity_solar_system_id_with_sde(snap, sde, fid);
            if sys == CLUSTER_SYSTEM_ID { if let Some(&mapped) = sys_by_fid.get(&fid) { sys = mapped; } }
            by_sys.entry(sys).or_default().push(entry);
        };
        for ((fid, slot), cd) in fighter_cooldown_state { push_stat(fid, FighterCheckpointEntry::Cooldown((fid, slot), cd)); }
        for ((fid, slot), ch) in fighter_charge_state { push_stat(fid, FighterCheckpointEntry::Charge((fid, slot), ch)); }
    });
    for (sys_id, entries) in by_sys {
        if sys_id == CLUSTER_SYSTEM_ID {
            let existing = db.with_sqlite_conn(|c| load_system_checkpoints::<FighterCheckpointEntry>(c, CLUSTER_SYSTEM_ID, KIND_FIGHTERS)).unwrap_or_default();
            let mut merged = SystemFighterState::default();
            for (_, entry) in existing { apply_checkpoint_entry_if_absent(&mut merged, entry, &item_types); }
            for entry in entries { apply_checkpoint_entry_if_absent(&mut merged, entry, &item_types); }
            let rows = collect_fighter_checkpoint_rows(&merged, &HashSet::new());
            let now_unix = current_unix_secs();
            let _ = db.with_sqlite_conn(|c| replace_system_checkpoints_for_kind(c, CLUSTER_SYSTEM_ID, KIND_FIGHTERS, &rows, now_unix));
        } else {
            if let Err(err) = systems.run_cold_fighter_op_guarded(
                sys_id, &entries, |_| (), |_| true,
                |handle| {
                    let hints = collect_preloaded_hints(db, sde, sys_id, None);
                    let loaded = merge_loaded_fighter_state(sys_id, Vec::new(), hints, &entries).state;
                    handle.merge_fighter_state_sync(loaded, true, false).map(|_| { let _ = systems.flush_system_fighter_checkpoint_sync(sys_id); })
                        .map_err(|_| "merge_fighter_state_sync failed".to_string())
                },
            ) {
                tracing::warn!(solar_system_id = sys_id, error = %err, "Legacy monolith fighter migration skipped or failed");
            }
        }
    }
}
