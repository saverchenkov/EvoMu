//! Per-system entropic disintegrator spools, in-flight missiles, and breacher pod DoTs owned by [`SystemActor`] (`#1541` E1).
//!
//! All state lives in [`SystemActorState::weapon_effects`], publishes lock-free via [`SystemView`],
//! persists per-system through `#1170` `post_unlock_tx` FIFO checkpoints (`KIND_WEAPON_EFFECTS`),
//! and performs one-time read migration from legacy `extended_runtime_json` on first hydration.

#![forbid(unsafe_code)]

use super::{
    call_actor_result_with_deadline, celestials, defer_reply, dispatch_actor_sync_with_deadline,
    post_unlock, ActorExecCtx, BallOpsCtx, ClusterTickWakeReason, SystemActivationError,
    SystemActorState, SystemCommand, SystemHandle, SystemRegistry, SystemView, SystemWakeReason,
    ACTOR_CMD_TIMEOUT,
};
use eve_db::lock_rank::{held_lock_ranks, RankedRwLock, RANK_ACTOR_STATE, RANK_BALLPARK};
use eve_db::sqlite_store::system_checkpoint::{
    load_system_checkpoints, replace_system_checkpoints_for_kind, KIND_WEAPON_EFFECTS,
};
use eve_db::{WorldRepository, WorldStateSnapshot};
use eve_destiny::{BallId, Ballpark, DamageVector};
use eve_sde::SolarSystemId;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const MAX_RETAINED_TERMINAL_MISSILES: usize = 256;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EntropicSpoolState {
    pub target_id: i64,
    pub solar_system_id: u32,
    pub attacker_char_id: i64,
    pub attacker_ship_id: i64,
    pub module_id: i64,
    pub module_type_id: u32,
    pub charge_item_id: Option<i64>,
    pub base_damage: DamageVector,
    pub cap_need: f64,
    pub duration_s: f64,
    pub cycles: u32,
    pub elapsed_in_cycle: f64,
    pub per_cycle_bonus: f64,
    pub max_bonus: f64,
    pub current_multiplier: f64,
}

impl EntropicSpoolState {
    #[inline]
    pub fn with_solar_system(self, solar_system_id: u32) -> Self {
        Self {
            solar_system_id,
            ..self
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InFlightMissileState {
    pub missile_ball_id: i64,
    pub solar_system_id: u32,
    pub attacker_char_id: i64,
    pub attacker_ship_id: i64,
    pub launcher_module_id: i64,
    pub launcher_type_id: u32,
    pub missile_type_id: u32,
    pub target_ball_id: i64,
    pub max_velocity: f64,
    pub flight_time_s: f64,
    pub remaining_flight_s: f64,
    pub hp: f64,
    pub max_hp: f64,
    pub base_damage: DamageVector,
    pub explosion_radius: f64,
    pub explosion_velocity: f64,
    pub drf: f64,
    pub is_fof: bool,
    pub is_breacher_pod: bool,
    pub breacher_dot_duration_s: f64,
    pub breacher_flat_cap: f64,
    pub breacher_pct_cap: f64,
    pub destroyed: bool,
    pub expired: bool,
    pub impacted: bool,
    pub applied_damage: f64,
}

impl InFlightMissileState {
    #[inline]
    pub fn is_terminal(&self) -> bool {
        self.destroyed || self.expired || self.impacted
    }

    #[inline]
    pub fn with_solar_system(self, solar_system_id: u32) -> Self {
        Self {
            solar_system_id,
            ..self
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActiveBreacherDot {
    pub target_ball_id: i64,
    #[serde(default)]
    pub solar_system_id: u32,
    pub source_char_id: i64,
    pub source_ship_id: i64,
    pub weapon_type_id: u32,
    pub charge_type_id: u32,
    pub flat_cap_dps: f64,
    pub pct_cap_fraction: f64,
    pub remaining_s: f64,
    pub tick_damage: f64,
}

impl ActiveBreacherDot {
    #[inline]
    pub fn with_solar_system(self, solar_system_id: u32) -> Self {
        Self {
            solar_system_id,
            ..self
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActorWeaponEffectsState {
    pub entropic_spools: HashMap<(i64, i64), EntropicSpoolState>,
    pub in_flight_missiles: HashMap<i64, InFlightMissileState>,
    pub active_breacher_dots: HashMap<i64, ActiveBreacherDot>,
}

impl ActorWeaponEffectsState {
    pub fn is_empty(&self) -> bool {
        self.entropic_spools.is_empty()
            && self.in_flight_missiles.is_empty()
            && self.active_breacher_dots.is_empty()
    }

    pub fn has_active_work(&self) -> bool {
        !self.entropic_spools.is_empty()
            || !self.active_breacher_dots.is_empty()
            || self.in_flight_missiles.values().any(|m| !m.is_terminal())
    }

    pub fn prune_excess_terminal_missiles(&mut self, protect_id: Option<i64>) {
        let terminal_count = self
            .in_flight_missiles
            .values()
            .filter(|m| m.is_terminal())
            .count();
        if terminal_count <= MAX_RETAINED_TERMINAL_MISSILES {
            return;
        }
        let mut drop_ids: Vec<i64> = self
            .in_flight_missiles
            .iter()
            .filter(|(&id, m)| m.is_terminal() && protect_id != Some(id))
            .map(|(&id, _)| id)
            .collect();
        drop_ids.sort_unstable();
        let to_remove = terminal_count.saturating_sub(MAX_RETAINED_TERMINAL_MISSILES);
        for id in drop_ids.into_iter().take(to_remove) {
            self.in_flight_missiles.remove(&id);
        }
    }

    pub(crate) fn checkpoint_rows(&self) -> Vec<(String, WeaponEffectCheckpointEntry)> {
        let mut rows = Vec::with_capacity(
            self.entropic_spools.len()
                + self.active_breacher_dots.len()
                + self.in_flight_missiles.len(),
        );
        for (&(ship_id, module_id), st) in &self.entropic_spools {
            rows.push((
                format!("spool:{ship_id}:{module_id}"),
                WeaponEffectCheckpointEntry::Spool(st.clone()),
            ));
        }
        for (&mid, m) in &self.in_flight_missiles {
            if !m.is_terminal() {
                rows.push((
                    format!("missile:{mid}"),
                    WeaponEffectCheckpointEntry::Missile(m.clone()),
                ));
            }
        }
        for (&tid, dot) in &self.active_breacher_dots {
            rows.push((
                format!("breacher:{tid}"),
                WeaponEffectCheckpointEntry::Breacher(dot.clone()),
            ));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        rows
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WeaponEffectCheckpointEntry {
    Spool(EntropicSpoolState),
    Missile(InFlightMissileState),
    Breacher(ActiveBreacherDot),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct WeaponEffectsRemovedPreHydration {
    pub spools: HashSet<(i64, i64)>,
    pub spool_ships: HashSet<i64>,
    pub spool_ship_targets: HashSet<(i64, i64)>,
    pub missiles: HashSet<i64>,
    pub breachers: HashSet<i64>,
}

impl WeaponEffectsRemovedPreHydration {
    pub(crate) fn is_empty(&self) -> bool {
        self.spools.is_empty()
            && self.spool_ships.is_empty()
            && self.spool_ship_targets.is_empty()
            && self.missiles.is_empty()
            && self.breachers.is_empty()
    }
}

#[derive(Default)]
pub(crate) struct WeaponEffectsFlushCoord {
    pub dirty: AtomicBool,
    pub needs_hydration: AtomicBool,
    pub retry_needed: AtomicBool,
    pub has_pending_legacy_prune: AtomicBool,
    pub seq: AtomicU64,
    pub flushed_seq: AtomicU64,
    pub sqlite_persist_lock: Mutex<u64>,
    pub pending_legacy_prune: Mutex<Option<LegacyWeaponEffectsPruneKeys>>,
    pub db_opt: Option<Arc<WorldRepository>>,
    pub migrated_legacy_systems: Option<Arc<Mutex<HashSet<SolarSystemId>>>>,
}

impl std::fmt::Debug for WeaponEffectsFlushCoord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WeaponEffectsFlushCoord")
            .field("dirty", &self.dirty.load(Ordering::Relaxed))
            .field(
                "needs_hydration",
                &self.needs_hydration.load(Ordering::Relaxed),
            )
            .field("retry_needed", &self.retry_needed.load(Ordering::Relaxed))
            .field(
                "has_pending_legacy_prune",
                &self.has_pending_legacy_prune.load(Ordering::Relaxed),
            )
            .field("seq", &self.seq.load(Ordering::Relaxed))
            .field("flushed_seq", &self.flushed_seq.load(Ordering::Relaxed))
            .field("has_db", &self.db_opt.is_some())
            .finish()
    }
}

impl WeaponEffectsFlushCoord {
    pub(crate) fn has_pending_work(&self) -> bool {
        self.needs_hydration.load(Ordering::Acquire)
            || self.dirty.load(Ordering::Acquire)
            || self.retry_needed.load(Ordering::Acquire)
            || self.has_pending_legacy_prune.load(Ordering::Acquire)
            || self.flushed_seq.load(Ordering::Acquire) < self.seq.load(Ordering::Acquire)
    }
}

pub(in crate::system_actor) fn mark_flush_coords_retry_needed(
    coords: &[Arc<WeaponEffectsFlushCoord>],
) {
    for coord in coords {
        coord.retry_needed.store(true, Ordering::Release);
    }
}

pub(in crate::system_actor) fn mark_intents_retry_needed(intents: &[post_unlock::PostUnlockIntent]) {
    for intent in intents {
        if let post_unlock::PostUnlockIntent::WeaponEffectsCheckpoint(op) = intent {
            if let Some(ref coord) = op.flush_coord {
                coord.retry_needed.store(true, Ordering::Release);
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RegistryWeaponEffectsState {
    pub migrated_legacy_systems: Arc<Mutex<HashSet<SolarSystemId>>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegacyWeaponEffectsPruneKeys {
    pub spool_keys: Vec<(i64, i64)>,
    pub missile_ids: Vec<i64>,
    pub breacher_ids: Vec<i64>,
}

impl LegacyWeaponEffectsPruneKeys {
    pub fn is_empty(&self) -> bool {
        self.spool_keys.is_empty() && self.missile_ids.is_empty() && self.breacher_ids.is_empty()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WeaponEffectsCheckpointOp {
    pub solar_system_id: SolarSystemId,
    pub seq: u64,
    pub rows: Vec<(String, WeaponEffectCheckpointEntry)>,
    pub flush_coord: Option<Arc<WeaponEffectsFlushCoord>>,
    pub legacy_prune: Option<LegacyWeaponEffectsPruneKeys>,
}

#[derive(Default, serde::Deserialize)]
#[serde(default)]
struct LegacyWeaponEffectsExtJson {
    entropic_spools: Vec<((i64, i64), EntropicSpoolState)>,
    in_flight_missiles: HashMap<i64, InFlightMissileState>,
    active_breacher_dots: HashMap<i64, ActiveBreacherDot>,
}

pub(crate) struct LoadedWeaponEffectsData {
    pub spools: Vec<((i64, i64), EntropicSpoolState)>,
    pub missiles: Vec<(i64, InFlightMissileState)>,
    pub breachers: Vec<(i64, ActiveBreacherDot)>,
    pub legacy_prune: Option<LegacyWeaponEffectsPruneKeys>,
}

fn mark_weapon_effects_dirty_fields(
    weapon_effects_seq: &mut u64,
    weapon_effects_dirty: &mut bool,
    weapon_effects_coord: &Arc<WeaponEffectsFlushCoord>,
) {
    *weapon_effects_seq = weapon_effects_seq.saturating_add(1).max(1);
    *weapon_effects_dirty = true;
    weapon_effects_coord
        .seq
        .store(*weapon_effects_seq, Ordering::Release);
    weapon_effects_coord.dirty.store(true, Ordering::Release);
}

pub(in crate::system_actor) fn mark_weapon_effects_dirty_in_state(state: &mut SystemActorState) {
    mark_weapon_effects_dirty_fields(
        &mut state.weapon_effects_seq,
        &mut state.weapon_effects_dirty,
        &state.weapon_effects_coord,
    );
}

#[inline]
pub(in crate::system_actor) fn has_unflushed_weapon_effects_in_state(
    state: &SystemActorState,
) -> bool {
    state.db_opt.is_some()
        && (state.weapon_effects_needs_sqlite_hydration
            || state.weapon_effects_coord.retry_needed.load(Ordering::Acquire)
            || state
                .weapon_effects_coord
                .has_pending_legacy_prune
                .load(Ordering::Acquire)
            || (state.weapon_effects_dirty
                && state
                    .weapon_effects_coord
                    .flushed_seq
                    .load(Ordering::Acquire)
                    < state.weapon_effects_seq))
}

fn resolve_explicit_breacher_target_system(
    snap: &WorldStateSnapshot,
    target_ball_id: i64,
    solar_system_id: SolarSystemId,
) -> Option<SolarSystemId> {
    if let Some(st) = snap.structures.get(&target_ball_id) {
        return Some(st.solar_system_id);
    }
    if let Some(ch) = snap
        .characters
        .values()
        .find(|c| c.active_ship_id == target_ball_id && c.solar_system_id > 0)
    {
        return Some(ch.solar_system_id);
    }
    if let Some(ch) = snap
        .characters
        .get(&target_ball_id)
        .filter(|c| c.solar_system_id > 0)
    {
        return Some(ch.solar_system_id);
    }
    if let Some(it) = snap.items.get(&target_ball_id) {
        if let Some(st) = snap.structures.get(&it.location_id) {
            return Some(st.solar_system_id);
        }
        if it.location_id == i64::from(solar_system_id) {
            return Some(solar_system_id);
        }
        if (30_000_000..40_000_000).contains(&it.location_id)
            || (snap.item_positions.contains_key(&target_ball_id)
                && u32::try_from(it.location_id).is_ok()
                && it.location_id > 0)
        {
            return Some(it.location_id as SolarSystemId);
        }
    }
    None
}

fn resolve_legacy_breacher_system(
    dot: &ActiveBreacherDot,
    solar_system_id: SolarSystemId,
    live_ball_ids: Option<&HashSet<BallId>>,
    snap: &WorldStateSnapshot,
) -> bool {
    if dot.solar_system_id == solar_system_id {
        return true;
    }
    if dot.solar_system_id != 0 {
        return false;
    }
    if live_ball_ids.is_some_and(|ids| ids.contains(&dot.target_ball_id)) {
        return true;
    }
    if let Some(target_sys) =
        resolve_explicit_breacher_target_system(snap, dot.target_ball_id, solar_system_id)
    {
        return target_sys == solar_system_id;
    }
    dot.source_char_id > 0
        && snap
            .characters
            .get(&dot.source_char_id)
            .is_some_and(|c| c.solar_system_id == solar_system_id)
}

fn load_sqlite_checkpoint_rows(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
    nonblocking: bool,
) -> Result<Vec<(String, WeaponEffectCheckpointEntry)>, String> {
    let read_rows = |conn: &rusqlite::Connection| {
        load_system_checkpoints::<WeaponEffectCheckpointEntry>(
            conn,
            solar_system_id,
            KIND_WEAPON_EFFECTS,
        )
    };
    if !nonblocking {
        return db.with_sqlite_conn(|conn| read_rows(conn));
    }
    if let Some(res) = db.try_with_sqlite_conn(read_rows) {
        return res;
    }
    Err(format!(
        "SQLite writer contended during nonblocking weapon_effects load for system {solar_system_id}"
    ))
}

fn read_sqlite_and_legacy_for_system(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
    live_ball_ids: Option<&HashSet<BallId>>,
    migrated_systems: Option<&Arc<Mutex<HashSet<SolarSystemId>>>>,
    nonblocking: bool,
) -> Result<LoadedWeaponEffectsData, String> {
    let sqlite_rows = load_sqlite_checkpoint_rows(db, solar_system_id, nonblocking)?;
    let mut spools: HashMap<(i64, i64), EntropicSpoolState> = HashMap::new();
    let mut missiles: HashMap<i64, InFlightMissileState> = HashMap::new();
    let mut breachers: HashMap<i64, ActiveBreacherDot> = HashMap::new();

    for (_, entry) in sqlite_rows {
        match entry {
            WeaponEffectCheckpointEntry::Spool(s) => {
                spools.insert(
                    (s.attacker_ship_id, s.module_id),
                    s.with_solar_system(solar_system_id),
                );
            }
            WeaponEffectCheckpointEntry::Missile(m) => {
                if !m.is_terminal() {
                    missiles.insert(m.missile_ball_id, m.with_solar_system(solar_system_id));
                }
            }
            WeaponEffectCheckpointEntry::Breacher(d) => {
                breachers.insert(d.target_ball_id, d.with_solar_system(solar_system_id));
            }
        }
    }

    let already_migrated = migrated_systems.is_some_and(|m| {
        m.lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&solar_system_id)
    });
    let mut legacy_prune = LegacyWeaponEffectsPruneKeys::default();
    if !already_migrated {
        let raw_ext = if nonblocking {
            db.try_with_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json.clone())
                .ok_or_else(|| "world snapshot busy".to_string())?
        } else {
            db.with_snapshot(|s| s.combat_crimewatch_state.extended_runtime_json.clone())
        };
        if !raw_ext.is_empty() {
            if let Ok(legacy) = serde_json::from_str::<LegacyWeaponEffectsExtJson>(&raw_ext) {
                for ((ship_id, mod_id), s) in legacy.entropic_spools {
                    if s.solar_system_id == solar_system_id {
                        legacy_prune.spool_keys.push((ship_id, mod_id));
                        spools
                            .entry((ship_id, mod_id))
                            .or_insert_with(|| s.with_solar_system(solar_system_id));
                    }
                }
                for (mid, m) in legacy.in_flight_missiles {
                    if m.solar_system_id == solar_system_id {
                        legacy_prune.missile_ids.push(mid);
                        if !m.is_terminal() {
                            missiles
                                .entry(mid)
                                .or_insert_with(|| m.with_solar_system(solar_system_id));
                        }
                    }
                }
                if !legacy.active_breacher_dots.is_empty() {
                    let collect_breachers = |snap: &WorldStateSnapshot| {
                        let mut matched = Vec::new();
                        for (tid, d) in &legacy.active_breacher_dots {
                            if resolve_legacy_breacher_system(
                                d,
                                solar_system_id,
                                live_ball_ids,
                                snap,
                            ) {
                                matched.push((*tid, d.clone()));
                            }
                        }
                        matched
                    };
                    let resolved = if nonblocking {
                        db.try_with_snapshot(collect_breachers)
                            .ok_or_else(|| "world snapshot busy".to_string())?
                    } else {
                        db.with_snapshot(collect_breachers)
                    };
                    for (tid, d) in resolved {
                        legacy_prune.breacher_ids.push(tid);
                        breachers
                            .entry(tid)
                            .or_insert_with(|| d.with_solar_system(solar_system_id));
                    }
                }
            }
        }
        if legacy_prune.is_empty() {
            if let Some(m) = migrated_systems {
                m.lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(solar_system_id);
            }
        }
    }

    Ok(LoadedWeaponEffectsData {
        spools: spools.into_iter().collect(),
        missiles: missiles.into_iter().collect(),
        breachers: breachers.into_iter().collect(),
        legacy_prune: (!legacy_prune.is_empty()).then_some(legacy_prune),
    })
}

pub(in crate::system_actor) fn merge_loaded_weapon_effects_in_state(
    state: &mut SystemActorState,
    solar_system_id: SolarSystemId,
    loaded: LoadedWeaponEffectsData,
) -> Option<LegacyWeaponEffectsPruneKeys> {
    let mut tombstone_dropped = false;
    for ((ship_id, mod_id), s) in loaded.spools {
        let blocked = state
            .weapon_effects_removed_pre_hydration
            .spool_ships
            .contains(&ship_id)
            || state
                .weapon_effects_removed_pre_hydration
                .spool_ship_targets
                .contains(&(ship_id, s.target_id))
            || state
                .weapon_effects_removed_pre_hydration
                .spools
                .contains(&(ship_id, mod_id));
        if blocked {
            tombstone_dropped = true;
        } else {
            state
                .weapon_effects
                .entropic_spools
                .entry((ship_id, mod_id))
                .or_insert_with(|| s.with_solar_system(solar_system_id));
        }
    }
    for (mid, m) in loaded.missiles {
        if state
            .weapon_effects_removed_pre_hydration
            .missiles
            .contains(&mid)
        {
            tombstone_dropped = true;
        } else if !m.is_terminal() {
            state
                .weapon_effects
                .in_flight_missiles
                .entry(mid)
                .or_insert_with(|| m.with_solar_system(solar_system_id));
        }
    }
    for (tid, d) in loaded.breachers {
        if state
            .weapon_effects_removed_pre_hydration
            .breachers
            .contains(&tid)
        {
            tombstone_dropped = true;
        } else {
            state
                .weapon_effects
                .active_breacher_dots
                .entry(tid)
                .or_insert_with(|| d.with_solar_system(solar_system_id));
        }
    }
    state.weapon_effects_needs_sqlite_hydration = false;
    state
        .weapon_effects_coord
        .needs_hydration
        .store(false, Ordering::Release);
    state.weapon_effects_removed_pre_hydration = WeaponEffectsRemovedPreHydration::default();
    if let Some(ref prune) = loaded.legacy_prune {
        if !prune.is_empty() {
            *state
                .weapon_effects_coord
                .pending_legacy_prune
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(prune.clone());
            state
                .weapon_effects_coord
                .has_pending_legacy_prune
                .store(true, Ordering::Release);
        }
    }
    if loaded.legacy_prune.is_some() || tombstone_dropped {
        mark_weapon_effects_dirty_in_state(state);
    }
    loaded.legacy_prune
}

pub(in crate::system_actor) fn build_checkpoint_op(
    solar_system_id: SolarSystemId,
    state: &mut SystemActorState,
    legacy_prune: Option<LegacyWeaponEffectsPruneKeys>,
) -> WeaponEffectsCheckpointOp {
    state.weapon_effects_dispatched_seq = state.weapon_effects_seq;
    state
        .weapon_effects_coord
        .retry_needed
        .store(false, Ordering::Release);
    let effective_prune = legacy_prune.or_else(|| {
        state
            .weapon_effects_coord
            .pending_legacy_prune
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    });
    WeaponEffectsCheckpointOp {
        solar_system_id,
        seq: state.weapon_effects_seq,
        rows: state.weapon_effects.checkpoint_rows(),
        flush_coord: Some(Arc::clone(&state.weapon_effects_coord)),
        legacy_prune: effective_prune,
    }
}

pub(in crate::system_actor) fn init_actor_weapon_effects(
    state: &mut SystemActorState,
    registry: &SystemRegistry,
    solar_system_id: SolarSystemId,
    ballpark: &Ballpark,
    db_read_safe: bool,
    initial_ops: &mut post_unlock::PostUnlockOps,
) {
    let coord = Arc::new(WeaponEffectsFlushCoord {
        dirty: AtomicBool::new(false),
        needs_hydration: AtomicBool::new(false),
        retry_needed: AtomicBool::new(false),
        has_pending_legacy_prune: AtomicBool::new(false),
        seq: AtomicU64::new(0),
        flushed_seq: AtomicU64::new(0),
        sqlite_persist_lock: Mutex::new(0),
        pending_legacy_prune: Mutex::new(None),
        db_opt: registry.db.clone(),
        migrated_legacy_systems: Some(Arc::clone(
            &registry.weapon_effects_rt_state.migrated_legacy_systems,
        )),
    });
    state.weapon_effects_coord = Arc::clone(&coord);
    let Some(ref db) = registry.db else {
        return;
    };
    if !db_read_safe {
        state.weapon_effects_needs_sqlite_hydration = true;
        coord.needs_hydration.store(true, Ordering::Release);
        return;
    }
    let live_ball_ids: HashSet<BallId> = ballpark.balls.keys().copied().collect();
    match read_sqlite_and_legacy_for_system(
        db,
        solar_system_id,
        Some(&live_ball_ids),
        coord.migrated_legacy_systems.as_ref(),
        true,
    ) {
        Ok(loaded) => {
            if let Some(prune) = merge_loaded_weapon_effects_in_state(state, solar_system_id, loaded)
            {
                let op = build_checkpoint_op(solar_system_id, state, Some(prune));
                initial_ops.push(post_unlock::PostUnlockIntent::WeaponEffectsCheckpoint(
                    Box::new(op),
                ));
            }
        }
        Err(_) => {
            state.weapon_effects_needs_sqlite_hydration = true;
            coord.needs_hydration.store(true, Ordering::Release);
        }
    }
}

pub(in crate::system_actor) fn collect_dirty_weapon_effects_post_unlock_op_inner(
    solar_system_id: SolarSystemId,
    state: &mut SystemActorState,
    ops: &mut post_unlock::PostUnlockOps,
    force_if_unflushed: bool,
) -> bool {
    let flushed = state.weapon_effects_coord.flushed_seq.load(Ordering::Acquire);
    let retry = state.weapon_effects_coord.retry_needed.load(Ordering::Acquire);
    let pending_prune = state
        .weapon_effects_coord
        .has_pending_legacy_prune
        .load(Ordering::Acquire);
    if state.weapon_effects_dirty
        && !retry
        && !pending_prune
        && flushed >= state.weapon_effects_seq
    {
        state.weapon_effects_dirty = false;
        state
            .weapon_effects_coord
            .dirty
            .store(false, Ordering::Release);
    }
    let still_unflushed =
        state.weapon_effects_dirty || retry || pending_prune || flushed < state.weapon_effects_seq;
    if still_unflushed
        && !state.weapon_effects_needs_sqlite_hydration
        && state.db_opt.is_some()
        && (state.weapon_effects_dispatched_seq < state.weapon_effects_seq
            || force_if_unflushed
            || retry)
    {
        let op = build_checkpoint_op(solar_system_id, state, None);
        ops.push(post_unlock::PostUnlockIntent::WeaponEffectsCheckpoint(
            Box::new(op),
        ));
        return true;
    }
    false
}

pub(in crate::system_actor) fn collect_dirty_weapon_effects_post_unlock_op(
    solar_system_id: SolarSystemId,
    state: &mut SystemActorState,
    ops: &mut post_unlock::PostUnlockOps,
) {
    let _ = collect_dirty_weapon_effects_post_unlock_op_inner(solar_system_id, state, ops, false);
}

fn persist_checkpoint_op_to_sqlite_inner(
    db: &WorldRepository,
    op: &WeaponEffectsCheckpointOp,
    nonblocking: bool,
) -> bool {
    if op.seq == 0 && op.legacy_prune.is_none() {
        return true;
    }
    let coord_opt = op.flush_coord.as_ref();
    let mut seq_guard = if let Some(c) = coord_opt {
        if nonblocking {
            match c.sqlite_persist_lock.try_lock() {
                Ok(g) => Some(g),
                Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => {
                    c.retry_needed.store(true, Ordering::Release);
                    return false;
                }
            }
        } else {
            Some(
                c.sqlite_persist_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()),
            )
        }
    } else {
        None
    };
    if let Some(ref g) = seq_guard {
        if op.seq > 0 && op.seq <= **g {
            return true;
        }
    }
    let keyed: Vec<(String, &WeaponEffectCheckpointEntry)> =
        op.rows.iter().map(|(k, v)| (k.clone(), v)).collect();
    let now_unix = db.clock().now_unix_secs();
    let res = if nonblocking {
        db.try_with_sqlite_conn_mut(|conn| {
            replace_system_checkpoints_for_kind(
                conn,
                op.solar_system_id,
                KIND_WEAPON_EFFECTS,
                &keyed,
                now_unix,
            )
        })
        .unwrap_or_else(|| Err("sqlite writer busy".to_string()))
    } else {
        db.with_sqlite_conn(|conn| {
            replace_system_checkpoints_for_kind(
                conn,
                op.solar_system_id,
                KIND_WEAPON_EFFECTS,
                &keyed,
                now_unix,
            )
        })
    };
    match res {
        Ok(()) => {
            if let Some(ref mut g) = seq_guard {
                **g = (**g).max(op.seq);
            }
            if let Some(coord) = coord_opt {
                coord.flushed_seq.fetch_max(op.seq, Ordering::Release);
                let has_prune = op.legacy_prune.as_ref().is_some_and(|p| !p.is_empty());
                if coord.seq.load(Ordering::Acquire) <= op.seq
                    && !has_prune
                    && !coord.has_pending_legacy_prune.load(Ordering::Acquire)
                {
                    coord.dirty.store(false, Ordering::Release);
                    coord.retry_needed.store(false, Ordering::Release);
                }
            }
            true
        }
        Err(err) => {
            if let Some(coord) = coord_opt {
                coord.retry_needed.store(true, Ordering::Release);
            }
            if !nonblocking {
                tracing::warn!(
                    solar_system_id = op.solar_system_id,
                    seq = op.seq,
                    error = %err,
                    "Failed to persist weapon_effects SQLite checkpoint"
                );
            }
            false
        }
    }
}

pub(in crate::system_actor) fn persist_and_prune_checkpoint_op(
    db: &WorldRepository,
    op: &WeaponEffectsCheckpointOp,
) -> bool {
    if !persist_checkpoint_op_to_sqlite_inner(db, op, false) {
        return false;
    }
    if let Some(ref prune) = op.legacy_prune {
        if !prune.is_empty() {
            if !held_lock_ranks().is_empty()
                || db
                    .try_with_mut_snapshot(|snap| -> Result<(), String> {
                        prune_legacy_weapon_effects_ext_json_in_snap(snap, prune);
                        Ok(())
                    })
                    .is_err()
            {
                if let Some(ref coord) = op.flush_coord {
                    coord.retry_needed.store(true, Ordering::Release);
                }
                return false;
            }
            if let Some(ref coord) = op.flush_coord {
                *coord
                    .pending_legacy_prune
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = None;
                coord
                    .has_pending_legacy_prune
                    .store(false, Ordering::Release);
                if let Some(ref migrated) = coord.migrated_legacy_systems {
                    migrated
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(op.solar_system_id);
                }
                if coord.seq.load(Ordering::Acquire) <= op.seq {
                    coord.dirty.store(false, Ordering::Release);
                    coord.retry_needed.store(false, Ordering::Release);
                }
            }
        }
    }
    true
}

pub(in crate::system_actor) fn prune_legacy_weapon_effects_ext_json_in_snap(
    snap: &mut WorldStateSnapshot,
    keys: &LegacyWeaponEffectsPruneKeys,
) {
    let raw = snap.combat_crimewatch_state.extended_runtime_json.as_str();
    if keys.is_empty() || raw.is_empty() {
        return;
    }
    let Ok(mut val) = serde_json::from_str::<serde_json::Value>(raw) else {
        return;
    };
    let Some(root) = val.as_object_mut() else {
        return;
    };
    let mut changed = false;
    if !keys.spool_keys.is_empty() {
        let spool_set: HashSet<(i64, i64)> = keys.spool_keys.iter().copied().collect();
        if let Some(serde_json::Value::Array(arr)) = root.get_mut("entropic_spools") {
            let before = arr.len();
            arr.retain(|entry| {
                let parsed = entry
                    .as_array()
                    .and_then(|pair| pair.first())
                    .and_then(|k| k.as_array())
                    .and_then(|kt| Some((kt.first()?.as_i64()?, kt.get(1)?.as_i64()?)));
                parsed.is_none_or(|k| !spool_set.contains(&k))
            });
            changed |= arr.len() != before;
            if arr.is_empty() {
                root.remove("entropic_spools");
            }
        }
    }
    if !keys.missile_ids.is_empty() {
        let mset: HashSet<String> = keys.missile_ids.iter().map(|id| id.to_string()).collect();
        if let Some(serde_json::Value::Object(map)) = root.get_mut("in_flight_missiles") {
            let before = map.len();
            map.retain(|k, _| !mset.contains(k));
            changed |= map.len() != before;
            if map.is_empty() {
                root.remove("in_flight_missiles");
            }
        }
    }
    if !keys.breacher_ids.is_empty() {
        let bset: HashSet<String> = keys.breacher_ids.iter().map(|id| id.to_string()).collect();
        if let Some(serde_json::Value::Object(map)) = root.get_mut("active_breacher_dots") {
            let before = map.len();
            map.retain(|k, _| !bset.contains(k));
            changed |= map.len() != before;
            if map.is_empty() {
                root.remove("active_breacher_dots");
            }
        }
    }
    if changed {
        snap.combat_crimewatch_state.extended_runtime_json = if root.is_empty() {
            String::new()
        } else {
            serde_json::to_string(&val).unwrap_or_default()
        };
    }
}

pub(in crate::system_actor) fn clear_removed_ball_weapon_effects_fields(
    weapon_effects: &mut ActorWeaponEffectsState,
    weapon_effects_needs_sqlite_hydration: bool,
    weapon_effects_removed_pre_hydration: &mut WeaponEffectsRemovedPreHydration,
    weapon_effects_seq: &mut u64,
    weapon_effects_dirty: &mut bool,
    weapon_effects_coord: &Arc<WeaponEffectsFlushCoord>,
    ball_id: BallId,
) -> bool {
    let mut dirty = false;
    let spools_before = weapon_effects.entropic_spools.len();
    weapon_effects
        .entropic_spools
        .retain(|&(ship_id, _), _| ship_id != ball_id);
    if weapon_effects.entropic_spools.len() != spools_before {
        dirty = true;
    }
    if let Some(ms) = weapon_effects.in_flight_missiles.get_mut(&ball_id) {
        if !ms.is_terminal() {
            ms.hp = 0.0;
            ms.destroyed = true;
            ms.applied_damage = 0.0;
            dirty = true;
        }
    }
    if dirty {
        weapon_effects.prune_excess_terminal_missiles(Some(ball_id));
    }
    if weapon_effects
        .active_breacher_dots
        .remove(&ball_id)
        .is_some()
    {
        dirty = true;
    }
    if weapon_effects_needs_sqlite_hydration {
        let added_spool_ship = weapon_effects_removed_pre_hydration
            .spool_ships
            .insert(ball_id);
        let added_missile = weapon_effects_removed_pre_hydration
            .missiles
            .insert(ball_id);
        let added_breacher = weapon_effects_removed_pre_hydration
            .breachers
            .insert(ball_id);
        if added_spool_ship || added_missile || added_breacher {
            dirty = true;
        }
    }
    if dirty {
        mark_weapon_effects_dirty_fields(
            weapon_effects_seq,
            weapon_effects_dirty,
            weapon_effects_coord,
        );
    }
    dirty
}

pub(in crate::system_actor) fn clear_removed_ball_weapon_effects_in_state(
    state: &mut SystemActorState,
    ball_id: BallId,
) -> bool {
    clear_removed_ball_weapon_effects_fields(
        &mut state.weapon_effects,
        state.weapon_effects_needs_sqlite_hydration,
        &mut state.weapon_effects_removed_pre_hydration,
        &mut state.weapon_effects_seq,
        &mut state.weapon_effects_dirty,
        &state.weapon_effects_coord,
        ball_id,
    )
}

#[allow(clippy::too_many_arguments)]
pub(in crate::system_actor) fn flush_actor_weapon_effects(
    solar_system_id: SolarSystemId,
    ballpark_actor: &RankedRwLock<Ballpark, RANK_BALLPARK>,
    actor_state_actor: &RankedRwLock<SystemActorState, RANK_ACTOR_STATE>,
    view_tx_actor: &tokio::sync::watch::Sender<Arc<SystemView>>,
    tidi_factor_bits_actor: &AtomicU64,
    active_subscribers: usize,
    db_opt: Option<&WorldRepository>,
    post_unlock_tx: Option<(
        &tokio::sync::mpsc::UnboundedSender<post_unlock::PostUnlockMsg>,
        &Arc<std::sync::atomic::AtomicUsize>,
    )>,
    blocking: bool,
) {
    if !held_lock_ranks().is_empty() {
        return;
    }
    let Some(db) = db_opt else {
        return;
    };
    if !blocking
        && !view_tx_actor
            .borrow()
            .weapon_effects_coord
            .as_ref()
            .is_some_and(|c| c.has_pending_work())
    {
        return;
    }
    let (needs_hyd, dirty_real, migrated_systems) = {
        let st = actor_state_actor.read().unwrap_or_else(|e| e.into_inner());
        let flushed = st.weapon_effects_coord.flushed_seq.load(Ordering::Acquire);
        let retry = st.weapon_effects_coord.retry_needed.load(Ordering::Acquire);
        let pending_prune = st
            .weapon_effects_coord
            .has_pending_legacy_prune
            .load(Ordering::Acquire);
        (
            st.weapon_effects_needs_sqlite_hydration,
            st.weapon_effects_dirty
                || !st.weapon_effects_removed_pre_hydration.is_empty()
                || retry
                || pending_prune
                || flushed < st.weapon_effects_seq,
            st.weapon_effects_coord.migrated_legacy_systems.clone(),
        )
    };
    if !dirty_real && !needs_hyd {
        return;
    }
    if needs_hyd {
        let live_ball_ids: HashSet<BallId> = {
            ballpark_actor
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .balls
                .keys()
                .copied()
                .collect()
        };
        let Ok(loaded) = read_sqlite_and_legacy_for_system(
            db,
            solar_system_id,
            Some(&live_ball_ids),
            migrated_systems.as_ref(),
            !blocking,
        ) else {
            return;
        };
        let (bp, mut st) = (
            ballpark_actor.read().unwrap_or_else(|e| e.into_inner()),
            actor_state_actor.write().unwrap_or_else(|e| e.into_inner()),
        );
        if st.weapon_effects_needs_sqlite_hydration {
            let _ = merge_loaded_weapon_effects_in_state(&mut st, solar_system_id, loaded);
            publish_weapon_effects_view(
                &bp,
                &st,
                view_tx_actor,
                tidi_factor_bits_actor,
                active_subscribers,
            );
        }
    }
    let (op_opt, db_arc_opt, already_flushed) = {
        let mut st = actor_state_actor.write().unwrap_or_else(|e| e.into_inner());
        if st.weapon_effects_needs_sqlite_hydration {
            (None, None, false)
        } else {
            let flushed = st.weapon_effects_coord.flushed_seq.load(Ordering::Acquire);
            let retry = st.weapon_effects_coord.retry_needed.load(Ordering::Acquire);
            let pending_prune = st
                .weapon_effects_coord
                .has_pending_legacy_prune
                .load(Ordering::Acquire);
            if flushed >= st.weapon_effects_seq && !retry && !pending_prune {
                if st.weapon_effects_dirty {
                    st.weapon_effects_dirty = false;
                    st.weapon_effects_coord.dirty.store(false, Ordering::Release);
                }
                (None, None, true)
            } else if blocking || needs_hyd || retry || st.weapon_effects_dispatched_seq < st.weapon_effects_seq {
                let db_arc = st.db_opt.clone();
                let op = build_checkpoint_op(solar_system_id, &mut st, None);
                (Some(op), db_arc, false)
            } else {
                (None, None, false)
            }
        }
    };
    if already_flushed {
        let (bp, st) = (
            ballpark_actor.read().unwrap_or_else(|e| e.into_inner()),
            actor_state_actor.read().unwrap_or_else(|e| e.into_inner()),
        );
        publish_weapon_effects_view(
            &bp,
            &st,
            view_tx_actor,
            tidi_factor_bits_actor,
            active_subscribers,
        );
        return;
    }
    let Some(op) = op_opt else {
        return;
    };
    let persisted = if blocking {
        persist_and_prune_checkpoint_op(db, &op)
    } else if op.legacy_prune.as_ref().is_none_or(|p| p.is_empty())
        && persist_checkpoint_op_to_sqlite_inner(db, &op, true)
    {
        true
    } else if let Some((tx, in_flight)) = post_unlock_tx {
        // Clear retry_needed BEFORE enqueueing so worker failures after enqueueing stay visible,
        // while subsequent actor batches skip blocking sqlite_persist_lock or spamming duplicates.
        let coord_for_restore = op.flush_coord.clone();
        if let Some(ref coord) = coord_for_restore {
            coord.retry_needed.store(false, Ordering::Release);
        }
        let mut ops = post_unlock::PostUnlockOps::default();
        ops.intents.push(post_unlock::PostUnlockIntent::WeaponEffectsCheckpoint(Box::new(op)));
        let sent = post_unlock::send_post_unlock_item(
            tx,
            in_flight,
            post_unlock::PostUnlockWorkItem {
                db_opt: db_arc_opt,
                sde_opt: None,
                ops,
                replies: Vec::new(),
                in_flight_guard: None,
                work_seq: 0,
            },
        );
        if sent.is_err() {
            if let Some(ref coord) = coord_for_restore {
                coord.retry_needed.store(true, Ordering::Release);
            }
        }
        false
    } else {
        false
    };
    if persisted {
        let (bp, mut st) = (
            ballpark_actor.read().unwrap_or_else(|e| e.into_inner()),
            actor_state_actor.write().unwrap_or_else(|e| e.into_inner()),
        );
        let flushed = st.weapon_effects_coord.flushed_seq.load(Ordering::Acquire);
        if flushed >= st.weapon_effects_seq
            && !st.weapon_effects_coord.retry_needed.load(Ordering::Acquire)
            && !st
                .weapon_effects_coord
                .has_pending_legacy_prune
                .load(Ordering::Acquire)
        {
            st.weapon_effects_dirty = false;
            st.weapon_effects_coord.dirty.store(false, Ordering::Release);
        }
        publish_weapon_effects_view(
            &bp,
            &st,
            view_tx_actor,
            tidi_factor_bits_actor,
            active_subscribers,
        );
    }
}

fn publish_weapon_effects_view(
    bp: &eve_destiny::Ballpark,
    st: &SystemActorState,
    view_tx_actor: &tokio::sync::watch::Sender<Arc<SystemView>>,
    tidi_factor_bits_actor: &std::sync::atomic::AtomicU64,
    active_subscribers: usize,
) {
    celestials::update_system_hibernation_flag(st, bp, active_subscribers);
    let cur_tidi = celestials::clamp_tidi_factor_bits(tidi_factor_bits_actor);
    let next = SystemView::from_ballpark(bp, Some(st), cur_tidi, None);
    celestials::send_view_preserving_atomic_tidi(view_tx_actor, tidi_factor_bits_actor, next);
}

fn upsert_entropic_spool_activation_in_state(
    state: &mut SystemActorState,
    solar_system_id: SolarSystemId,
    spool: EntropicSpoolState,
    is_active_in_db: bool,
) -> Option<(u32, f64)> {
    let spool = spool.with_solar_system(solar_system_id);
    let key = (spool.attacker_ship_id, spool.module_id);
    if let Some(entry) = state.weapon_effects.entropic_spools.get_mut(&key) {
        if entry.target_id == spool.target_id && is_active_in_db {
            return None;
        }
        *entry = EntropicSpoolState {
            cycles: 0,
            elapsed_in_cycle: 0.0,
            current_multiplier: 1.0,
            ..spool
        };
    } else {
        state.weapon_effects.entropic_spools.insert(
            key,
            EntropicSpoolState {
                cycles: 0,
                elapsed_in_cycle: 0.0,
                current_multiplier: 1.0,
                ..spool
            },
        );
    }
    mark_weapon_effects_dirty_in_state(state);
    Some((0, 1.0))
}

fn advance_entropic_spool_cycle_in_state(
    state: &mut SystemActorState,
    ship_id: i64,
    module_id: i64,
) -> (u32, f64) {
    if let Some(entry) = state
        .weapon_effects
        .entropic_spools
        .get_mut(&(ship_id, module_id))
    {
        entry.cycles = entry.cycles.saturating_add(1);
        let mult = 1.0 + (entry.cycles as f64 * entry.per_cycle_bonus).min(entry.max_bonus);
        entry.current_multiplier = mult;
        let out = (entry.cycles, mult);
        mark_weapon_effects_dirty_in_state(state);
        out
    } else {
        (0, 1.0)
    }
}

fn update_entropic_spool_in_state(
    state: &mut SystemActorState,
    solar_system_id: SolarSystemId,
    spool: EntropicSpoolState,
) {
    let spool = spool.with_solar_system(solar_system_id);
    let key = (spool.attacker_ship_id, spool.module_id);
    state.weapon_effects.entropic_spools.insert(key, spool);
    mark_weapon_effects_dirty_in_state(state);
}

fn remove_entropic_spool_in_state(
    state: &mut SystemActorState,
    ship_id: i64,
    module_id: i64,
) -> bool {
    let key = (ship_id, module_id);
    let mut removed = state
        .weapon_effects
        .entropic_spools
        .remove(&key)
        .is_some();
    if state.weapon_effects_needs_sqlite_hydration {
        removed |= state
            .weapon_effects_removed_pre_hydration
            .spools
            .insert(key);
    }
    if removed {
        mark_weapon_effects_dirty_in_state(state);
    }
    removed
}

fn collect_and_clear_entropic_spools_for_ship_in_state(
    state: &mut SystemActorState,
    ship_id: i64,
    target_id_opt: Option<i64>,
) -> Vec<i64> {
    let matched: Vec<i64> = state
        .weapon_effects
        .entropic_spools
        .iter()
        .filter(|((s, _), st)| *s == ship_id && target_id_opt.is_none_or(|tid| st.target_id == tid))
        .map(|((_, m), _)| *m)
        .collect();
    let mut changed = false;
    for &mod_id in &matched {
        if state
            .weapon_effects
            .entropic_spools
            .remove(&(ship_id, mod_id))
            .is_some()
        {
            changed = true;
        }
    }
    if state.weapon_effects_needs_sqlite_hydration {
        if let Some(tid) = target_id_opt {
            changed |= state
                .weapon_effects_removed_pre_hydration
                .spool_ship_targets
                .insert((ship_id, tid));
        } else {
            changed |= state
                .weapon_effects_removed_pre_hydration
                .spool_ships
                .insert(ship_id);
        }
    }
    if changed {
        mark_weapon_effects_dirty_in_state(state);
    }
    matched
}

fn upsert_in_flight_missile_in_state(
    state: &mut SystemActorState,
    solar_system_id: SolarSystemId,
    missile: InFlightMissileState,
) {
    let missile = missile.with_solar_system(solar_system_id);
    let mid = missile.missile_ball_id;
    let is_term = missile.is_terminal();
    state
        .weapon_effects
        .in_flight_missiles
        .insert(mid, missile);
    if is_term {
        state
            .weapon_effects
            .prune_excess_terminal_missiles(Some(mid));
    }
    mark_weapon_effects_dirty_in_state(state);
}

fn remove_in_flight_missile_in_state(state: &mut SystemActorState, missile_ball_id: i64) -> bool {
    let mut removed = state
        .weapon_effects
        .in_flight_missiles
        .remove(&missile_ball_id)
        .is_some();
    if state.weapon_effects_needs_sqlite_hydration {
        removed |= state
            .weapon_effects_removed_pre_hydration
            .missiles
            .insert(missile_ball_id);
    }
    if removed {
        mark_weapon_effects_dirty_in_state(state);
    }
    removed
}

fn damage_in_flight_missile_in_state(
    state: &mut SystemActorState,
    missile_ball_id: i64,
    damage: f64,
) -> Option<bool> {
    let ms = state
        .weapon_effects
        .in_flight_missiles
        .get_mut(&missile_ball_id)?;
    if ms.is_terminal() {
        return None;
    }
    ms.hp -= damage;
    let destroyed_now = if ms.hp <= 0.0 {
        ms.hp = 0.0;
        ms.destroyed = true;
        ms.applied_damage = 0.0;
        true
    } else {
        false
    };
    if destroyed_now {
        state
            .weapon_effects
            .prune_excess_terminal_missiles(Some(missile_ball_id));
    }
    mark_weapon_effects_dirty_in_state(state);
    Some(destroyed_now)
}

fn intercept_in_flight_missile_in_state(
    state: &mut SystemActorState,
    target_missile_id_opt: Option<i64>,
) -> Option<i64> {
    let cand_id = target_missile_id_opt
        .filter(|tid| {
            state
                .weapon_effects
                .in_flight_missiles
                .get(tid)
                .is_some_and(|ms| !ms.is_terminal())
        })
        .or_else(|| {
            state
                .weapon_effects
                .in_flight_missiles
                .values()
                .filter(|ms| !ms.is_terminal())
                .min_by_key(|ms| ms.missile_ball_id)
                .map(|ms| ms.missile_ball_id)
        });
    if let Some(mid) = cand_id {
        if let Some(ms) = state.weapon_effects.in_flight_missiles.get_mut(&mid) {
            ms.hp = 0.0;
            ms.destroyed = true;
            ms.applied_damage = 0.0;
        }
        state
            .weapon_effects
            .prune_excess_terminal_missiles(Some(mid));
        mark_weapon_effects_dirty_in_state(state);
    }
    cand_id
}

fn upsert_active_breacher_dot_in_state(
    state: &mut SystemActorState,
    solar_system_id: SolarSystemId,
    dot: ActiveBreacherDot,
) {
    let dot = dot.with_solar_system(solar_system_id);
    state
        .weapon_effects
        .active_breacher_dots
        .insert(dot.target_ball_id, dot);
    mark_weapon_effects_dirty_in_state(state);
}

fn remove_active_breacher_dot_in_state(
    state: &mut SystemActorState,
    target_ball_id: i64,
) -> bool {
    let mut removed = state
        .weapon_effects
        .active_breacher_dots
        .remove(&target_ball_id)
        .is_some();
    if state.weapon_effects_needs_sqlite_hydration {
        removed |= state
            .weapon_effects_removed_pre_hydration
            .breachers
            .insert(target_ball_id);
    }
    if removed {
        mark_weapon_effects_dirty_in_state(state);
    }
    removed
}

pub(in crate::system_actor) fn handle_weapon_effects_command(
    exec_ctx: &mut ActorExecCtx<'_>,
    cmd: SystemCommand,
) -> Option<SystemCommand> {
    match cmd {
        SystemCommand::HydrateWeaponEffects {
            spools,
            missiles,
            breachers,
            legacy_prune,
            reply,
        } => {
            let applied = if exec_ctx.actor_st.weapon_effects_needs_sqlite_hydration {
                let data = LoadedWeaponEffectsData {
                    spools,
                    missiles,
                    breachers,
                    legacy_prune,
                };
                if let Some(prune) = merge_loaded_weapon_effects_in_state(
                    exec_ctx.actor_st,
                    exec_ctx.solar_system_id,
                    data,
                ) {
                    let op = build_checkpoint_op(
                        exec_ctx.solar_system_id,
                        exec_ctx.actor_st,
                        Some(prune),
                    );
                    exec_ctx.post_unlock_ops.push(
                        post_unlock::PostUnlockIntent::WeaponEffectsCheckpoint(Box::new(op)),
                    );
                }
                true
            } else {
                false
            };
            defer_reply(exec_ctx.deferred_replies, reply, Ok(applied));
            None
        }
        SystemCommand::UpsertEntropicSpoolActivation {
            spool,
            is_active_in_db,
            reply,
        } => {
            let res = upsert_entropic_spool_activation_in_state(
                exec_ctx.actor_st,
                exec_ctx.solar_system_id,
                spool,
                is_active_in_db,
            );
            defer_reply(exec_ctx.deferred_replies, reply, Ok(res));
            None
        }
        SystemCommand::AdvanceEntropicSpoolCycle {
            ship_id,
            module_id,
            reply,
        } => {
            let res =
                advance_entropic_spool_cycle_in_state(exec_ctx.actor_st, ship_id, module_id);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(res));
            None
        }
        SystemCommand::UpdateEntropicSpool { spool, reply } => {
            update_entropic_spool_in_state(exec_ctx.actor_st, exec_ctx.solar_system_id, spool);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::RemoveEntropicSpool {
            ship_id,
            module_id,
            reply,
        } => {
            let removed = remove_entropic_spool_in_state(exec_ctx.actor_st, ship_id, module_id);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(removed));
            None
        }
        SystemCommand::CollectAndClearEntropicSpoolsForShip {
            ship_id,
            target_id_opt,
            reply,
        } => {
            let cleared = collect_and_clear_entropic_spools_for_ship_in_state(
                exec_ctx.actor_st,
                ship_id,
                target_id_opt,
            );
            defer_reply(exec_ctx.deferred_replies, reply, Ok(cleared));
            None
        }
        SystemCommand::UpsertInFlightMissile { missile, reply } => {
            upsert_in_flight_missile_in_state(exec_ctx.actor_st, exec_ctx.solar_system_id, missile);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::RemoveInFlightMissile {
            missile_ball_id,
            reply,
        } => {
            let removed = remove_in_flight_missile_in_state(exec_ctx.actor_st, missile_ball_id);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(removed));
            None
        }
        SystemCommand::DamageInFlightMissile {
            missile_ball_id,
            damage,
            reply,
        } => {
            let res = damage_in_flight_missile_in_state(exec_ctx.actor_st, missile_ball_id, damage);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(res));
            None
        }
        SystemCommand::InterceptInFlightMissile {
            target_missile_id_opt,
            reply,
        } => {
            let res =
                intercept_in_flight_missile_in_state(exec_ctx.actor_st, target_missile_id_opt);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(res));
            None
        }
        SystemCommand::UpsertActiveBreacherDot { dot, reply } => {
            upsert_active_breacher_dot_in_state(exec_ctx.actor_st, exec_ctx.solar_system_id, dot);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(()));
            None
        }
        SystemCommand::RemoveActiveBreacherDot {
            target_ball_id,
            reply,
        } => {
            let removed = remove_active_breacher_dot_in_state(exec_ctx.actor_st, target_ball_id);
            defer_reply(exec_ctx.deferred_replies, reply, Ok(removed));
            None
        }
        SystemCommand::FlushWeaponEffectsCheckpoint { reply } => {
            let dispatched = collect_dirty_weapon_effects_post_unlock_op_inner(
                exec_ctx.solar_system_id,
                exec_ctx.actor_st,
                exec_ctx.post_unlock_ops,
                true,
            );
            let target_seq = exec_ctx.actor_st.weapon_effects_seq;
            let coord = Arc::clone(&exec_ctx.actor_st.weapon_effects_coord);
            let check_persist = exec_ctx.actor_st.db_opt.is_some()
                && (dispatched
                    || exec_ctx.actor_st.weapon_effects_dirty
                    || coord.flushed_seq.load(Ordering::Acquire) < target_seq
                    || coord.retry_needed.load(Ordering::Acquire)
                    || coord.has_pending_legacy_prune.load(Ordering::Acquire));
            if check_persist {
                exec_ctx.deferred_db_replies.push(Box::new(move || {
                    let flushed = coord.flushed_seq.load(Ordering::Acquire);
                    let retry = coord.retry_needed.load(Ordering::Acquire);
                    let pending_prune = coord.has_pending_legacy_prune.load(Ordering::Acquire);
                    if flushed >= target_seq && !retry && !pending_prune {
                        let _ = reply.send(Ok(true));
                    } else {
                        let _ = reply.send(Err(format!(
                            "weapon_effects checkpoint flush incomplete (flushed={flushed}, target={target_seq}, retry={retry}, pending_prune={pending_prune})"
                        )));
                    }
                }));
            } else {
                defer_reply(exec_ctx.deferred_db_replies, reply, Ok(true));
            }
            None
        }
        other => Some(other),
    }
}

impl SystemView {
    pub fn entropic_spool(&self, ship_id: i64, module_id: i64) -> Option<EntropicSpoolState> {
        self.weapon_effects
            .entropic_spools
            .get(&(ship_id, module_id))
            .cloned()
    }

    pub fn in_flight_missile(&self, missile_ball_id: i64) -> Option<InFlightMissileState> {
        self.weapon_effects
            .in_flight_missiles
            .get(&missile_ball_id)
            .cloned()
    }

    pub fn active_breacher_dot(&self, target_ball_id: i64) -> Option<ActiveBreacherDot> {
        self.weapon_effects
            .active_breacher_dots
            .get(&target_ball_id)
            .cloned()
    }
}

impl SystemHandle {
    pub fn weapon_effects(&self) -> ActorWeaponEffectsState {
        self.view().weapon_effects.clone()
    }

    pub fn entropic_spool(&self, ship_id: i64, module_id: i64) -> Option<EntropicSpoolState> {
        self.view().entropic_spool(ship_id, module_id)
    }

    pub fn in_flight_missile(&self, missile_ball_id: i64) -> Option<InFlightMissileState> {
        self.view().in_flight_missile(missile_ball_id)
    }

    pub fn active_breacher_dot(&self, target_ball_id: i64) -> Option<ActiveBreacherDot> {
        self.view().active_breacher_dot(target_ball_id)
    }

    pub async fn ensure_weapon_effects_hydrated(&self) {
        let view = self.view();
        if !view.weapon_effects_needs_sqlite_hydration || !held_lock_ranks().is_empty() {
            return;
        }
        let Some(coord) = view.weapon_effects_coord.clone() else {
            return;
        };
        let Some(ref db) = coord.db_opt else {
            return;
        };
        let db_cl = Arc::clone(db);
        let mig_cl = coord.migrated_legacy_systems.clone();
        let live_ball_ids: HashSet<BallId> = view.balls.keys().copied().collect();
        let sys_id = self.solar_system_id;
        let Ok(Ok(loaded)) = tokio::task::spawn_blocking(move || {
            read_sqlite_and_legacy_for_system(
                &db_cl,
                sys_id,
                Some(&live_ball_ids),
                mig_cl.as_ref(),
                false,
            )
        })
        .await
        else {
            return;
        };
        let _ = call_actor_result_with_deadline(
            self,
            "HydrateWeaponEffects",
            ACTOR_CMD_TIMEOUT,
            move |reply| SystemCommand::HydrateWeaponEffects {
                spools: loaded.spools,
                missiles: loaded.missiles,
                breachers: loaded.breachers,
                legacy_prune: loaded.legacy_prune,
                reply,
            },
        )
        .await;
    }

    pub fn ensure_weapon_effects_hydrated_sync(&self) {
        self.ensure_weapon_effects_hydrated_sync_inner(false);
    }

    fn ensure_weapon_effects_hydrated_sync_inner(&self, nonblocking: bool) {
        let view = self.view();
        if !view.weapon_effects_needs_sqlite_hydration || !held_lock_ranks().is_empty() {
            return;
        }
        let Some(coord) = view.weapon_effects_coord.clone() else {
            return;
        };
        let Some(ref db) = coord.db_opt else {
            return;
        };
        let live_ball_ids: HashSet<BallId> = view.balls.keys().copied().collect();
        let Ok(loaded) = read_sqlite_and_legacy_for_system(
            db,
            self.solar_system_id,
            Some(&live_ball_ids),
            coord.migrated_legacy_systems.as_ref(),
            nonblocking,
        ) else {
            return;
        };
        let _ = dispatch_actor_sync_with_deadline(
            self,
            "HydrateWeaponEffects",
            ACTOR_CMD_TIMEOUT,
            loaded,
            |loaded, reply| SystemCommand::HydrateWeaponEffects {
                spools: loaded.spools,
                missiles: loaded.missiles,
                breachers: loaded.breachers,
                legacy_prune: loaded.legacy_prune,
                reply,
            },
            |ctx: &mut BallOpsCtx<'_>, loaded| {
                if ctx.actor_st.weapon_effects_needs_sqlite_hydration {
                    if let Some(prune) = merge_loaded_weapon_effects_in_state(
                        ctx.actor_st,
                        ctx.solar_system_id,
                        loaded,
                    ) {
                        let op = build_checkpoint_op(
                            ctx.solar_system_id,
                            ctx.actor_st,
                            Some(prune),
                        );
                        ctx.post_unlock_ops.push(
                            post_unlock::PostUnlockIntent::WeaponEffectsCheckpoint(Box::new(op)),
                        );
                    }
                }
                Ok(true)
            },
        );
    }

    pub async fn flush_weapon_effects_checkpoint(&self) -> Result<(), String> {
        let view = self.view();
        let has_pending = view
            .weapon_effects_coord
            .as_ref()
            .is_some_and(|c| c.has_pending_work());
        if !has_pending {
            return Ok(());
        }
        if view.weapon_effects_needs_sqlite_hydration {
            self.ensure_weapon_effects_hydrated().await;
            if self.view().weapon_effects_needs_sqlite_hydration {
                return Err("weapon_effects SQLite hydration incomplete before flush".to_string());
            }
        }
        let ok = call_actor_result_with_deadline(
            self,
            "FlushWeaponEffectsCheckpoint",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::FlushWeaponEffectsCheckpoint { reply },
        )
        .await?;
        if !ok {
            return Err("weapon_effects SQLite checkpoint flush failed".to_string());
        }
        Ok(())
    }

    pub fn flush_weapon_effects_checkpoint_sync(&self) -> Result<(), String> {
        let view = self.view();
        let has_pending = view
            .weapon_effects_coord
            .as_ref()
            .is_some_and(|c| c.has_pending_work());
        if !has_pending {
            return Ok(());
        }
        if view.weapon_effects_needs_sqlite_hydration {
            self.ensure_weapon_effects_hydrated_sync();
            if self.view().weapon_effects_needs_sqlite_hydration {
                return Err("weapon_effects SQLite hydration incomplete before flush".to_string());
            }
        }
        let ok = dispatch_actor_sync_with_deadline(
            self,
            "FlushWeaponEffectsCheckpoint",
            ACTOR_CMD_TIMEOUT,
            (),
            |(), reply| SystemCommand::FlushWeaponEffectsCheckpoint { reply },
            |ctx, ()| {
                let _ = collect_dirty_weapon_effects_post_unlock_op_inner(
                    ctx.solar_system_id,
                    ctx.actor_st,
                    ctx.post_unlock_ops,
                    true,
                );
                Ok(true)
            },
        )?;
        if !ok {
            return Err("weapon_effects SQLite checkpoint flush failed".to_string());
        }
        if let Some(coord) = self.view().weapon_effects_coord.as_ref() {
            let flushed = coord.flushed_seq.load(Ordering::Acquire);
            let seq = coord.seq.load(Ordering::Acquire);
            let retry = coord.retry_needed.load(Ordering::Acquire);
            if coord.db_opt.is_some() && (flushed < seq || retry) {
                return Err(format!(
                    "weapon_effects sync checkpoint flush incomplete (flushed={flushed}, target={seq}, retry={retry})"
                ));
            }
        }
        Ok(())
    }

    pub async fn upsert_in_flight_missile(&self, missile: InFlightMissileState) -> Result<(), String> {
        call_actor_result_with_deadline(
            self,
            "UpsertInFlightMissile",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::UpsertInFlightMissile { missile, reply },
        )
        .await
    }

    pub fn upsert_in_flight_missile_sync(&self, missile: InFlightMissileState) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "UpsertInFlightMissile",
            ACTOR_CMD_TIMEOUT,
            missile,
            |missile, reply| SystemCommand::UpsertInFlightMissile { missile, reply },
            |ctx, missile| {
                upsert_in_flight_missile_in_state(ctx.actor_st, ctx.solar_system_id, missile);
                Ok(())
            },
        )
    }

    pub fn remove_in_flight_missile_sync(&self, missile_ball_id: i64) -> Result<bool, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "RemoveInFlightMissile",
            ACTOR_CMD_TIMEOUT,
            missile_ball_id,
            |missile_ball_id, reply| SystemCommand::RemoveInFlightMissile {
                missile_ball_id,
                reply,
            },
            |ctx, missile_ball_id| {
                Ok(remove_in_flight_missile_in_state(
                    ctx.actor_st,
                    missile_ball_id,
                ))
            },
        )
    }

    pub async fn damage_in_flight_missile(
        &self,
        missile_ball_id: i64,
        damage: f64,
    ) -> Result<Option<bool>, String> {
        self.ensure_weapon_effects_hydrated().await;
        call_actor_result_with_deadline(
            self,
            "DamageInFlightMissile",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::DamageInFlightMissile {
                missile_ball_id,
                damage,
                reply,
            },
        )
        .await
    }

    pub async fn intercept_in_flight_missile(
        &self,
        target_missile_id_opt: Option<i64>,
    ) -> Result<Option<i64>, String> {
        self.ensure_weapon_effects_hydrated().await;
        call_actor_result_with_deadline(
            self,
            "InterceptInFlightMissile",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::InterceptInFlightMissile {
                target_missile_id_opt,
                reply,
            },
        )
        .await
    }

    pub async fn step_active_missiles_snapshot(&self) -> Vec<InFlightMissileState> {
        self.ensure_weapon_effects_hydrated().await;
        let mut out: Vec<InFlightMissileState> = self
            .view()
            .weapon_effects
            .in_flight_missiles
            .values()
            .filter(|m| !m.is_terminal())
            .cloned()
            .collect();
        out.sort_by_key(|m| m.missile_ball_id);
        out
    }

    pub async fn get_in_flight_missile(&self, missile_ball_id: i64) -> Option<InFlightMissileState> {
        self.ensure_weapon_effects_hydrated().await;
        self.in_flight_missile(missile_ball_id)
    }

    pub async fn upsert_active_breacher_dot(&self, dot: ActiveBreacherDot) -> Result<(), String> {
        call_actor_result_with_deadline(
            self,
            "UpsertActiveBreacherDot",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::UpsertActiveBreacherDot { dot, reply },
        )
        .await
    }

    pub fn upsert_active_breacher_dot_sync(&self, dot: ActiveBreacherDot) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "UpsertActiveBreacherDot",
            ACTOR_CMD_TIMEOUT,
            dot,
            |dot, reply| SystemCommand::UpsertActiveBreacherDot { dot, reply },
            |ctx, dot| {
                upsert_active_breacher_dot_in_state(ctx.actor_st, ctx.solar_system_id, dot);
                Ok(())
            },
        )
    }

    pub async fn remove_active_breacher_dot(&self, target_ball_id: i64) -> Result<bool, String> {
        self.ensure_weapon_effects_hydrated().await;
        call_actor_result_with_deadline(
            self,
            "RemoveActiveBreacherDot",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::RemoveActiveBreacherDot {
                target_ball_id,
                reply,
            },
        )
        .await
    }

    pub fn remove_active_breacher_dot_sync(&self, target_ball_id: i64) -> Result<bool, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "RemoveActiveBreacherDot",
            ACTOR_CMD_TIMEOUT,
            target_ball_id,
            |target_ball_id, reply| SystemCommand::RemoveActiveBreacherDot {
                target_ball_id,
                reply,
            },
            |ctx, target_ball_id| {
                Ok(remove_active_breacher_dot_in_state(
                    ctx.actor_st,
                    target_ball_id,
                ))
            },
        )
    }

    pub async fn active_breacher_dots_snapshot(&self) -> Vec<ActiveBreacherDot> {
        self.ensure_weapon_effects_hydrated().await;
        let mut out: Vec<ActiveBreacherDot> = self
            .view()
            .weapon_effects
            .active_breacher_dots
            .values()
            .cloned()
            .collect();
        out.sort_by_key(|d| d.target_ball_id);
        out
    }

    pub async fn get_active_breacher_dot(&self, target_ball_id: i64) -> Option<ActiveBreacherDot> {
        self.ensure_weapon_effects_hydrated().await;
        self.active_breacher_dot(target_ball_id)
    }

    pub async fn active_entropic_spools_snapshot(&self) -> Vec<EntropicSpoolState> {
        self.ensure_weapon_effects_hydrated().await;
        let mut out: Vec<EntropicSpoolState> = self
            .view()
            .weapon_effects
            .entropic_spools
            .values()
            .cloned()
            .collect();
        out.sort_by_key(|s| (s.attacker_ship_id, s.module_id));
        out
    }

    pub async fn get_entropic_spool(
        &self,
        ship_id: i64,
        module_id: i64,
    ) -> Option<EntropicSpoolState> {
        self.ensure_weapon_effects_hydrated().await;
        self.entropic_spool(ship_id, module_id)
    }

    pub async fn update_entropic_spool(&self, spool: EntropicSpoolState) -> Result<(), String> {
        call_actor_result_with_deadline(
            self,
            "UpdateEntropicSpool",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::UpdateEntropicSpool { spool, reply },
        )
        .await
    }

    pub fn update_entropic_spool_sync(&self, spool: EntropicSpoolState) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "UpdateEntropicSpool",
            ACTOR_CMD_TIMEOUT,
            spool,
            |spool, reply| SystemCommand::UpdateEntropicSpool { spool, reply },
            |ctx, spool| {
                update_entropic_spool_in_state(ctx.actor_st, ctx.solar_system_id, spool);
                Ok(())
            },
        )
    }

    pub fn upsert_entropic_spool_activation_sync(
        &self,
        spool: EntropicSpoolState,
        is_active_in_db: bool,
    ) -> Result<Option<(u32, f64)>, String> {
        self.ensure_weapon_effects_hydrated_sync();
        dispatch_actor_sync_with_deadline(
            self,
            "UpsertEntropicSpoolActivation",
            ACTOR_CMD_TIMEOUT,
            (spool, is_active_in_db),
            |(spool, is_active_in_db), reply| SystemCommand::UpsertEntropicSpoolActivation {
                spool,
                is_active_in_db,
                reply,
            },
            |ctx, (spool, is_active_in_db)| {
                Ok(upsert_entropic_spool_activation_in_state(
                    ctx.actor_st,
                    ctx.solar_system_id,
                    spool,
                    is_active_in_db,
                ))
            },
        )
    }

    pub fn advance_entropic_spool_cycle_sync(
        &self,
        ship_id: i64,
        module_id: i64,
    ) -> Result<(u32, f64), String> {
        self.ensure_weapon_effects_hydrated_sync();
        dispatch_actor_sync_with_deadline(
            self,
            "AdvanceEntropicSpoolCycle",
            ACTOR_CMD_TIMEOUT,
            (ship_id, module_id),
            |(ship_id, module_id), reply| SystemCommand::AdvanceEntropicSpoolCycle {
                ship_id,
                module_id,
                reply,
            },
            |ctx, (ship_id, module_id)| {
                Ok(advance_entropic_spool_cycle_in_state(
                    ctx.actor_st,
                    ship_id,
                    module_id,
                ))
            },
        )
    }

    pub fn remove_entropic_spool_sync(&self, ship_id: i64, module_id: i64) -> Result<bool, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "RemoveEntropicSpool",
            ACTOR_CMD_TIMEOUT,
            (ship_id, module_id),
            |(ship_id, module_id), reply| SystemCommand::RemoveEntropicSpool {
                ship_id,
                module_id,
                reply,
            },
            |ctx, (ship_id, module_id)| {
                Ok(remove_entropic_spool_in_state(
                    ctx.actor_st,
                    ship_id,
                    module_id,
                ))
            },
        )
    }

    pub fn collect_and_clear_entropic_spools_for_ship_sync(
        &self,
        ship_id: i64,
        target_id_opt: Option<i64>,
    ) -> Result<Vec<i64>, String> {
        self.ensure_weapon_effects_hydrated_sync_inner(true);
        dispatch_actor_sync_with_deadline(
            self,
            "CollectAndClearEntropicSpoolsForShip",
            ACTOR_CMD_TIMEOUT,
            (ship_id, target_id_opt),
            |(ship_id, target_id_opt), reply| {
                SystemCommand::CollectAndClearEntropicSpoolsForShip {
                    ship_id,
                    target_id_opt,
                    reply,
                }
            },
            |ctx, (ship_id, target_id_opt)| {
                Ok(collect_and_clear_entropic_spools_for_ship_in_state(
                    ctx.actor_st,
                    ship_id,
                    target_id_opt,
                ))
            },
        )
    }
}

impl SystemRegistry {
    pub(crate) fn try_wake_for_weapon_effects_seed(
        &self,
        solar_system_id: SolarSystemId,
    ) -> Result<SystemHandle, SystemActivationError> {
        if let Some(handle) = self.get_handle(solar_system_id) {
            return Ok(handle);
        }
        let reason = SystemWakeReason::ClusterTick(ClusterTickWakeReason::ScheduledWork);
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
            return self.wake_with_reason(solar_system_id, reason);
        }
        self.wake_with_reason(solar_system_id, reason)
    }

    pub fn active_entropic_spool(&self, ship_id: i64, module_id: i64) -> Option<EntropicSpoolState> {
        for (_, handle) in self.active_system_handles() {
            let view = handle.view();
            if let Some(s) = view.entropic_spool(ship_id, module_id) {
                return Some(s);
            }
            if view.weapon_effects_needs_sqlite_hydration {
                handle.ensure_weapon_effects_hydrated_sync();
                if let Some(s) = handle.entropic_spool(ship_id, module_id) {
                    return Some(s);
                }
            }
        }
        None
    }

    pub fn advance_entropic_spool_cycle_sync(
        &self,
        ship_id: i64,
        module_id: i64,
    ) -> (u32, f64) {
        for (_, handle) in self.active_system_handles() {
            let view = handle.view();
            let candidate = view
                .weapon_effects
                .entropic_spools
                .contains_key(&(ship_id, module_id))
                || view.weapon_effects_needs_sqlite_hydration;
            if candidate {
                if let Ok(out) = handle.advance_entropic_spool_cycle_sync(ship_id, module_id) {
                    if out.0 > 0 {
                        return out;
                    }
                }
            }
        }
        (0, 1.0)
    }

    pub fn remove_entropic_spool_sync(&self, ship_id: i64, module_id: i64) -> bool {
        let mut any = false;
        for (_, handle) in self.active_system_handles() {
            let view = handle.view();
            if view
                .weapon_effects
                .entropic_spools
                .contains_key(&(ship_id, module_id))
                || view.weapon_effects_needs_sqlite_hydration
            {
                if let Ok(true) = handle.remove_entropic_spool_sync(ship_id, module_id) {
                    any = true;
                }
            }
        }
        any
    }

    pub fn collect_and_clear_entropic_spools_for_ship_sync(
        &self,
        ship_id: i64,
        target_id_opt: Option<i64>,
    ) -> Vec<i64> {
        let mut out = Vec::new();
        for (_, handle) in self.active_system_handles() {
            let view = handle.view();
            let might_match = view
                .weapon_effects
                .entropic_spools
                .iter()
                .any(|(&(s, _), st)| {
                    s == ship_id && target_id_opt.is_none_or(|tid| st.target_id == tid)
                })
                || view.weapon_effects_needs_sqlite_hydration;
            if might_match {
                if let Ok(mods) =
                    handle.collect_and_clear_entropic_spools_for_ship_sync(ship_id, target_id_opt)
                {
                    out.extend(mods);
                }
            }
        }
        out
    }

    pub fn active_in_flight_missile(&self, missile_ball_id: i64) -> Option<InFlightMissileState> {
        for (_, handle) in self.active_system_handles() {
            let mut view = handle.view();
            if view.weapon_effects_needs_sqlite_hydration {
                handle.ensure_weapon_effects_hydrated_sync();
                view = handle.view();
            }
            if let Some(m) = view.in_flight_missile(missile_ball_id) {
                return Some(m);
            }
        }
        None
    }

    pub fn active_breacher_dot(&self, target_ball_id: i64) -> Option<ActiveBreacherDot> {
        for (_, handle) in self.active_system_handles() {
            let mut view = handle.view();
            if view.weapon_effects_needs_sqlite_hydration {
                handle.ensure_weapon_effects_hydrated_sync();
                view = handle.view();
            }
            if let Some(d) = view.active_breacher_dot(target_ball_id) {
                return Some(d);
            }
        }
        None
    }

    pub fn active_breacher_dot_count(&self) -> usize {
        self.active_system_handles()
            .into_iter()
            .map(|(_, h)| {
                if h.view().weapon_effects_needs_sqlite_hydration {
                    h.ensure_weapon_effects_hydrated_sync();
                }
                h.view().weapon_effects.active_breacher_dots.len()
            })
            .sum()
    }

    pub fn active_entropic_spools_matching(
        &self,
        mut pred: impl FnMut(&(i64, i64), &EntropicSpoolState) -> bool,
    ) -> Vec<((i64, i64), EntropicSpoolState)> {
        let mut out = Vec::new();
        for (_, handle) in self.active_system_handles() {
            let mut view = handle.view();
            if view.weapon_effects_needs_sqlite_hydration {
                handle.ensure_weapon_effects_hydrated_sync();
                view = handle.view();
            }
            for (k, v) in &view.weapon_effects.entropic_spools {
                if pred(k, v) {
                    out.push((*k, v.clone()));
                }
            }
        }
        out
    }

    pub fn active_in_flight_missiles_matching(
        &self,
        mut pred: impl FnMut(&i64, &InFlightMissileState) -> bool,
    ) -> Vec<(i64, InFlightMissileState)> {
        let mut out = Vec::new();
        for (_, handle) in self.active_system_handles() {
            let mut view = handle.view();
            if view.weapon_effects_needs_sqlite_hydration {
                handle.ensure_weapon_effects_hydrated_sync();
                view = handle.view();
            }
            for (k, v) in &view.weapon_effects.in_flight_missiles {
                if pred(k, v) {
                    out.push((*k, v.clone()));
                }
            }
        }
        out
    }

    pub fn active_breacher_dots_matching(
        &self,
        mut pred: impl FnMut(&i64, &ActiveBreacherDot) -> bool,
    ) -> Vec<(i64, ActiveBreacherDot)> {
        let mut out = Vec::new();
        for (_, handle) in self.active_system_handles() {
            let mut view = handle.view();
            if view.weapon_effects_needs_sqlite_hydration {
                handle.ensure_weapon_effects_hydrated_sync();
                view = handle.view();
            }
            for (k, v) in &view.weapon_effects.active_breacher_dots {
                if pred(k, v) {
                    out.push((*k, v.clone()));
                }
            }
        }
        out
    }
}

pub(in crate::system_actor) fn flush_from_handle(handle: &SystemHandle) {
    if held_lock_ranks().is_empty()
        && handle
            .view()
            .weapon_effects_coord
            .as_ref()
            .is_some_and(|c| c.has_pending_work())
    {
        let _ = handle.flush_weapon_effects_checkpoint_sync();
    }
}

#[cfg(test)]
#[path = "weapon_effects_tests.rs"]
mod weapon_effects_tests;
