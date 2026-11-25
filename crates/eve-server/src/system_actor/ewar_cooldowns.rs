//! Per-system in-space EWAR projection/active-start state, module reactivation cooldowns,
//! and sim-clock module reload deadlines owned by [`SystemActor`] (`#1541`).

#![forbid(unsafe_code)]

use super::{
    call_actor_result_with_deadline, defer_reply, dispatch_actor_sync_with_deadline,
    ActorExecCtx, BallOpsCtx, SystemCommand, SystemHandle, SystemRegistry, SystemView,
    ACTOR_CMD_TIMEOUT,
};
use eve_db::lock_rank::{held_lock_ranks, RANK_WORLD_INNER};
use eve_db::sqlite_store::system_checkpoint::{
    current_unix_secs, ensure_system_checkpoint_schema, load_all_checkpoints_for_kind,
    load_system_checkpoints, replace_system_checkpoints_for_kind, CLUSTER_SYSTEM_ID,
    KIND_MODULE_COOLDOWNS,
};
use eve_db::{WorldRepository, WorldStateSnapshot};
use eve_sde::SolarSystemId;
use std::collections::{BTreeMap, HashMap};
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

/// Per-system durable module reactivation cooldowns and sim-clock reload deadlines owned
/// by [`SystemActor`] (`#1541`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActorModuleCooldownState {
    pub reactivation_cooldowns: HashMap<(i64, i64, u32), ModuleCooldownEntry>,
    pub reload_sim_deadlines: HashMap<i64, ModuleReloadSimDeadline>,
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

#[derive(Debug, Clone)]
pub struct ModuleCooldownPersistPayload {
    pub db_opt: Option<Arc<WorldRepository>>,
    pub rows: Vec<ModuleCooldownCheckpointRow>,
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
    let has_sim = entry.solar_system_id.is_some()
        && entry.solar_system_id == sys_opt
        && cur_sys_sim_s.is_finite()
        && entry.recorded_sys_sim_s.is_finite()
        && cur_sys_sim_s >= entry.recorded_sys_sim_s;
    let sim_elapsed = if has_sim {
        (cur_sys_sim_s - entry.recorded_sys_sim_s).max(0.0)
    } else {
        0.0
    };
    let elapsed = if has_sim && (has_tidi_history || tidi_factor < 0.999_999) {
        if cur_sys_sim_s > 0.0 || entry.recorded_sys_sim_s > 0.0 {
            sim_elapsed
        } else {
            (wall_elapsed * tidi_factor.clamp(0.10, 1.0)).max(sim_elapsed)
        }
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
) -> HashMap<(i64, i64, u32), ModuleCooldownEntry> {
    if solar_system_id == 0 {
        return HashMap::new();
    }
    let now_unix_f64 = db.clock().now_unix_secs() as f64;
    let raw = load_cold_system_module_cooldowns_from_db(db, solar_system_id);
    raw.into_iter()
        .filter(|(_, entry)| {
            remaining_cooldown_secs_with_tidi(
                entry,
                now_unix_f64,
                Some(solar_system_id),
                0.0,
                tidi_factor,
                has_tidi_history,
            ) > 1e-6
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_cold_system_module_cooldown_in_db(
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
    if solar_system_id == 0 || !delay_s.is_finite() || delay_s <= 0.0 {
        return;
    }
    let mut map = load_cold_system_module_cooldowns_from_db(db, solar_system_id);
    let key = (ship_id, module_id, module_type_id);
    let effective_delay = map
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
            .max(delay_s)
        })
        .unwrap_or(delay_s);
    map.insert(
        key,
        ModuleCooldownEntry {
            recorded_unix_f64: now_unix_f64,
            delay_s: effective_delay,
            solar_system_id: Some(solar_system_id),
            recorded_sys_sim_s: 0.0,
        },
    );
    let rows = serialize_module_cooldown_rows(&map);
    let _ = persist_system_module_cooldowns_in_db(db, solar_system_id, &rows);
}

pub(crate) fn seed_cold_system_module_cooldown_in_db(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
    key: (i64, i64, u32),
    entry: ModuleCooldownEntry,
) {
    if solar_system_id == 0 {
        return;
    }
    let mut map = load_cold_system_module_cooldowns_from_db(db, solar_system_id);
    map.insert(key, entry);
    let rows = serialize_module_cooldown_rows(&map);
    let _ = persist_system_module_cooldowns_in_db(db, solar_system_id, &rows);
}

pub(crate) fn remove_cold_system_module_cooldown_in_db(
    db: &WorldRepository,
    solar_system_id: SolarSystemId,
    key: (i64, i64, u32),
) -> bool {
    if solar_system_id == 0 {
        return false;
    }
    let mut map = load_cold_system_module_cooldowns_from_db(db, solar_system_id);
    if map.remove(&key).is_none() {
        return false;
    }
    let rows = serialize_module_cooldown_rows(&map);
    persist_system_module_cooldowns_in_db(db, solar_system_id, &rows)
}

pub(crate) fn clear_cold_ship_module_cooldowns_in_db(
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
    let mut by_sys: BTreeMap<u32, HashMap<(i64, i64, u32), ModuleCooldownEntry>> = BTreeMap::new();
    let mut dirty_systems: Vec<u32> = Vec::new();
    for (sys_id, _, (key @ (sid, _, _), entry)) in all_rows {
        let bucket = by_sys.entry(sys_id).or_default();
        if sid == ship_id {
            dirty_systems.push(sys_id);
        } else {
            bucket.insert(key, entry);
        }
    }
    if dirty_systems.is_empty() {
        return;
    }
    dirty_systems.sort_unstable();
    dirty_systems.dedup();
    let now_unix = current_unix_secs();
    let _ = db.with_sqlite_conn(|conn| {
        for sys_id in dirty_systems {
            let rows = by_sys
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
        Ok::<(), String>(())
    });
}

pub(crate) fn clear_all_cold_module_cooldowns_in_db(db: &WorldRepository) {
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
    let mut sys_ids: Vec<u32> = all_rows.into_iter().map(|(sys_id, _, _)| sys_id).collect();
    sys_ids.sort_unstable();
    sys_ids.dedup();
    if sys_ids.is_empty() {
        return;
    }
    let now_unix = current_unix_secs();
    let _ = db.with_sqlite_conn(|conn| {
        for sys_id in sys_ids {
            replace_system_checkpoints_for_kind::<((i64, i64, u32), ModuleCooldownEntry)>(
                conn,
                sys_id,
                KIND_MODULE_COOLDOWNS,
                &[],
                now_unix,
            )?;
        }
        Ok::<(), String>(())
    });
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
    // Fallback for ships docked/jumped between cold systems or unmigrated legacy CLUSTER_SYSTEM_ID rows.
    let Ok(all_rows) = db.with_sqlite_conn(|conn| {
        ensure_system_checkpoint_schema(conn)?;
        load_all_checkpoints_for_kind::<((i64, i64, u32), ModuleCooldownEntry)>(
            conn,
            KIND_MODULE_COOLDOWNS,
        )
    }) else {
        return None;
    };
    for (sys_id, _, (k, entry)) in all_rows {
        if k == key {
            return Some((sys_id, entry));
        }
    }
    None
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

fn flush_module_ewar_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    source_ship_id: i64,
    source_module_id: i64,
    allow_db_source_fallback: bool,
) -> Result<ModuleEwarFlushSnapshot, String> {
    let ewar = &mut ctx.actor_st.ewar_state;
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
    Ok(ModuleEwarFlushSnapshot {
        started_targets,
        projected_targets,
        affected_ships,
    })
}

fn record_module_cooldown_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    req: RecordModuleCooldownReq,
) -> Result<ModuleCooldownPersistPayload, String> {
    if !req.delay_s.is_finite() || req.delay_s <= 0.0 {
        return Ok(ModuleCooldownPersistPayload {
            db_opt: ctx.actor_st.db_opt.clone(),
            rows: serialize_module_cooldown_rows(&ctx.actor_st.module_cooldowns.reactivation_cooldowns),
        });
    }
    let cur_sys_sim_s = ctx.actor_st.sim_elapsed_secs;
    let key = (req.ship_id, req.module_id, req.module_type_id);
    let effective_delay = ctx
        .actor_st
        .module_cooldowns
        .reactivation_cooldowns
        .get(&key)
        .map(|e| {
            remaining_cooldown_secs_with_tidi(
                e,
                req.now_unix_f64,
                Some(ctx.solar_system_id),
                cur_sys_sim_s,
                req.tidi_factor,
                req.has_tidi_history,
            )
            .max(req.delay_s)
        })
        .unwrap_or(req.delay_s);
    ctx.actor_st.module_cooldowns.reactivation_cooldowns.insert(
        key,
        ModuleCooldownEntry {
            recorded_unix_f64: req.now_unix_f64,
            delay_s: effective_delay,
            solar_system_id: Some(ctx.solar_system_id),
            recorded_sys_sim_s: cur_sys_sim_s,
        },
    );
    ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    Ok(ModuleCooldownPersistPayload {
        db_opt: ctx.actor_st.db_opt.clone(),
        rows: serialize_module_cooldown_rows(&ctx.actor_st.module_cooldowns.reactivation_cooldowns),
    })
}

fn seed_module_cooldown_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    key: (i64, i64, u32),
    entry: ModuleCooldownEntry,
) -> Result<ModuleCooldownPersistPayload, String> {
    ctx.actor_st
        .module_cooldowns
        .reactivation_cooldowns
        .insert(key, entry);
    ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    Ok(ModuleCooldownPersistPayload {
        db_opt: ctx.actor_st.db_opt.clone(),
        rows: serialize_module_cooldown_rows(&ctx.actor_st.module_cooldowns.reactivation_cooldowns),
    })
}

fn remove_module_cooldown_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    key: (i64, i64, u32),
) -> Result<Option<ModuleCooldownPersistPayload>, String> {
    if ctx
        .actor_st
        .module_cooldowns
        .reactivation_cooldowns
        .remove(&key)
        .is_none()
    {
        return Ok(None);
    }
    ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    Ok(Some(ModuleCooldownPersistPayload {
        db_opt: ctx.actor_st.db_opt.clone(),
        rows: serialize_module_cooldown_rows(&ctx.actor_st.module_cooldowns.reactivation_cooldowns),
    }))
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
    if ctx.actor_st.module_cooldowns.reactivation_cooldowns.len() == before {
        return Ok(None);
    }
    ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    Ok(Some(ModuleCooldownPersistPayload {
        db_opt: ctx.actor_st.db_opt.clone(),
        rows: serialize_module_cooldown_rows(&ctx.actor_st.module_cooldowns.reactivation_cooldowns),
    }))
}

fn clear_all_module_cooldowns_in_actor(
    ctx: &mut BallOpsCtx<'_>,
) -> Result<ModuleCooldownPersistPayload, String> {
    if !ctx
        .actor_st
        .module_cooldowns
        .reactivation_cooldowns
        .is_empty()
    {
        ctx.actor_st.module_cooldowns.reactivation_cooldowns.clear();
        ctx.actor_st.module_cooldown_checkpoint_dirty = true;
    }
    Ok(ModuleCooldownPersistPayload {
        db_opt: ctx.actor_st.db_opt.clone(),
        rows: Vec::new(),
    })
}

fn replace_module_cooldowns_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    cooldowns: HashMap<(i64, i64, u32), ModuleCooldownEntry>,
) -> Result<(), String> {
    ctx.actor_st.module_cooldowns.reactivation_cooldowns = cooldowns;
    ctx.actor_st.module_cooldown_checkpoint_dirty = false;
    Ok(())
}

fn snapshot_module_cooldown_checkpoint_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    only_if_dirty: bool,
) -> Result<Option<ModuleCooldownPersistPayload>, String> {
    if only_if_dirty
        && !ctx.actor_st.module_cooldown_checkpoint_dirty
        && ctx
            .actor_st
            .module_cooldowns
            .reactivation_cooldowns
            .is_empty()
    {
        return Ok(None);
    }
    ctx.actor_st.module_cooldown_checkpoint_dirty = false;
    Ok(Some(ModuleCooldownPersistPayload {
        db_opt: ctx.actor_st.db_opt.clone(),
        rows: serialize_module_cooldown_rows(&ctx.actor_st.module_cooldowns.reactivation_cooldowns),
    }))
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
            let res = clear_all_module_cooldowns_in_actor(&mut ctx).map(|_| ());
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

    /// Executes post-unlock EWAR flush directly against unlocked actor state inside
    /// `flush_post_unlock_actions`, publishing `SystemView` before returning (`#1541`).
    pub(crate) fn flush_module_ewar_unlocked_direct(
        &self,
        source_ship_id: i64,
        source_module_id: i64,
        allow_db_source_fallback: bool,
    ) -> ModuleEwarFlushSnapshot {
        let bp = self.ballpark.read().unwrap_or_else(|e| e.into_inner());
        let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
        let ewar = &mut st.ewar_state;
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
        self.publish_view_from(&bp, Some(&st));
        ModuleEwarFlushSnapshot {
            started_targets,
            projected_targets,
            affected_ships,
        }
    }

    fn persist_cooldown_payload(&self, payload: ModuleCooldownPersistPayload) {
        if let Some(ref db) = payload.db_opt {
            let _ = persist_system_module_cooldowns_in_db(
                db,
                self.solar_system_id,
                &payload.rows,
            );
        }
    }

    /// Executes post-unlock cooldown insert directly against unlocked actor state inside
    /// `flush_post_unlock_actions`, persisting SQLite checkpoints post-unlock (`#1541`).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_module_cooldown_unlocked_direct(
        &self,
        ship_id: i64,
        module_id: i64,
        module_type_id: u32,
        now_unix_f64: f64,
        delay_s: f64,
        tidi_factor: f64,
        has_tidi_history: bool,
    ) {
        if !delay_s.is_finite() || delay_s <= 0.0 {
            return;
        }
        let payload = {
            let bp = self.ballpark.read().unwrap_or_else(|e| e.into_inner());
            let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
            let cur_sys_sim_s = st.sim_elapsed_secs;
            let key = (ship_id, module_id, module_type_id);
            let effective_delay = st
                .module_cooldowns
                .reactivation_cooldowns
                .get(&key)
                .map(|e| {
                    remaining_cooldown_secs_with_tidi(
                        e,
                        now_unix_f64,
                        Some(self.solar_system_id),
                        cur_sys_sim_s,
                        tidi_factor,
                        has_tidi_history,
                    )
                    .max(delay_s)
                })
                .unwrap_or(delay_s);
            st.module_cooldowns.reactivation_cooldowns.insert(
                key,
                ModuleCooldownEntry {
                    recorded_unix_f64: now_unix_f64,
                    delay_s: effective_delay,
                    solar_system_id: Some(self.solar_system_id),
                    recorded_sys_sim_s: cur_sys_sim_s,
                },
            );
            st.module_cooldown_checkpoint_dirty = true;
            self.publish_view_from(&bp, Some(&st));
            ModuleCooldownPersistPayload {
                db_opt: st.db_opt.clone(),
                rows: serialize_module_cooldown_rows(&st.module_cooldowns.reactivation_cooldowns),
            }
        };
        self.persist_cooldown_payload(payload);
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
        dispatch_actor_sync_with_deadline(
            self,
            "ClearAllModuleCooldowns",
            ACTOR_CMD_TIMEOUT,
            (),
            |(), reply| SystemCommand::ClearAllModuleCooldowns { reply },
            |ctx, ()| clear_all_module_cooldowns_in_actor(ctx).map(|_| ()),
        )
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
    /// post-unlock (`#1541`). Called on eviction, actor stop, and runtime sync.
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
        let count = payload.rows.len();
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
        let count = payload.rows.len();
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
    use eve_db::{InventoryFlag, InventoryItemRecord};
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
}
