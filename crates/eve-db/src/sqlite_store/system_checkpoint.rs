//! Per-system runtime checkpoints (`system_checkpoint`), virtual clock offset (`world_meta.clock_offset_secs`),
//! crash-loss policy classification, WAL truncation, and legacy `server_runtime_state` retirement
//! (`#920`, `#899` §3.6, `#864` C2, `#875` C13).

use crate::sqlite_store::domain_rows::{from_checked_domain_json, to_checked_domain_json};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Synthetic `system_id` used for cluster-wide checkpoint kinds not tied to a single solar system.
pub const CLUSTER_SYSTEM_ID: u32 = 0;

/// `world_meta` key storing the persisted virtual clock offset in seconds.
pub const WORLD_META_CLOCK_OFFSET_SECS_KEY: &str = "clock_offset_secs";

/// `world_meta` key recording whether the previous shutdown completed cleanly (`1`) or crashed (`0`).
pub const WORLD_META_CLEAN_SHUTDOWN_KEY: &str = "clean_shutdown";

/// Retired legacy `service_state_blobs` key replaced by per-system `system_checkpoint` rows.
pub const LEGACY_SERVER_RUNTIME_STATE_BLOB_KEY: &str = "server_runtime_state";

// Canonical `system_checkpoint.kind` identifiers.
pub const KIND_SHIP_CONDITION: &str = "ship_condition";
pub const KIND_MOON_PROBES: &str = "moon_probes";
pub const KIND_SCAN_SIGNATURES: &str = "scan_signatures";
pub const KIND_SCAN_PROGRESS: &str = "scan_progress";
pub const KIND_PHASED_ASTEROID_FIELDS: &str = "phased_asteroid_fields";
pub const KIND_MAP_LANDMARKS: &str = "map_landmarks";
pub const KIND_SYSTEM_SIM_TIME: &str = "system_sim_time";
pub const KIND_FIGHTERS: &str = "fighters";
pub const KIND_SESSION_CHANGE_TIMERS: &str = "session_change_timers";
pub const KIND_LOCATION_CHANGE_INVULN: &str = "location_change_invuln";
pub const KIND_TACTICAL_DURABLE: &str = "tactical_durable";
pub const KIND_MODULE_COOLDOWNS: &str = "module_reactivation_cooldowns";
pub const KIND_FLEET_EXTRAS: &str = "fleet_extras";
pub const KIND_SOCIAL_EXTRAS: &str = "social_extras";

// Explicitly crash-lost runtime kinds (`#920`, `#899` §3.6).
pub const KIND_MODULE_CYCLES: &str = "module_cycles";
pub const KIND_TARGET_LOCKS: &str = "target_locks";
pub const KIND_ACTIVE_WARPS: &str = "active_warps";
pub const KIND_NPCS: &str = "npcs";

/// Declares whether a runtime state kind is checkpointed across restarts or intentionally lost on crash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RuntimePersistencePolicy {
    /// Persisted in `system_checkpoint(system_id, kind, key, value)` and restored on restart.
    Checkpointed,
    /// Intentionally ephemeral: NOT periodically checkpointed and lost across crash recovery (`kill -9`).
    CrashLost,
}

/// Static classification metadata for a single runtime state kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeKindDescriptor {
    pub kind: &'static str,
    pub policy: RuntimePersistencePolicy,
    pub system_scoped: bool,
    pub description: &'static str,
}

/// Complete explicit classification of all runtime state kinds replacing the `server_runtime_state` monolith.
pub const RUNTIME_KIND_CATALOG: &[RuntimeKindDescriptor] = &[
    RuntimeKindDescriptor {
        kind: KIND_SHIP_CONDITION,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: true,
        description: "In-space ship/structure position, shield/armor/hull HP, and capacitor",
    },
    RuntimeKindDescriptor {
        kind: KIND_MOON_PROBES,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: true,
        description: "Launched moon survey probes queued for arrival",
    },
    RuntimeKindDescriptor {
        kind: KIND_SCAN_SIGNATURES,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: true,
        description: "Character resolved cosmic signatures per solar system",
    },
    RuntimeKindDescriptor {
        kind: KIND_SCAN_PROGRESS,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: true,
        description: "Partial cosmic signature scan signal progress per character/system",
    },
    RuntimeKindDescriptor {
        kind: KIND_PHASED_ASTEROID_FIELDS,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: true,
        description: "Phased asteroid mining field state per solar system",
    },
    RuntimeKindDescriptor {
        kind: KIND_MAP_LANDMARKS,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: true,
        description: "Dynamic map landmarks associated with solar systems",
    },
    RuntimeKindDescriptor {
        kind: KIND_SYSTEM_SIM_TIME,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: true,
        description: "Per-system tactical simulation clock seconds",
    },
    RuntimeKindDescriptor {
        kind: KIND_FIGHTERS,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: true,
        description: "Carrier fighter squadrons, tube loads, cooldowns, and charges",
    },
    RuntimeKindDescriptor {
        kind: KIND_SESSION_CHANGE_TIMERS,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: false,
        description: "Character session-change cooldown deadlines",
    },
    RuntimeKindDescriptor {
        kind: KIND_LOCATION_CHANGE_INVULN,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: false,
        description: "Post-jump / undock invulnerability deadlines",
    },
    RuntimeKindDescriptor {
        kind: KIND_TACTICAL_DURABLE,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: false,
        description: "Durable tactical cooldowns/unlocks (lance/TSB timers, clone vat ships)",
    },
    RuntimeKindDescriptor {
        kind: KIND_MODULE_COOLDOWNS,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: true,
        description: "Active module reactivation delay cooldowns",
    },
    RuntimeKindDescriptor {
        kind: KIND_FLEET_EXTRAS,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: false,
        description: "Fleet adverts, setups, formations, invites, and hierarchy extras",
    },
    RuntimeKindDescriptor {
        kind: KIND_SOCIAL_EXTRAS,
        policy: RuntimePersistencePolicy::Checkpointed,
        system_scoped: false,
        description: "Delayed local speakers, active social probe lists, forced corp/ship sync flags",
    },
    RuntimeKindDescriptor {
        kind: KIND_MODULE_CYCLES,
        policy: RuntimePersistencePolicy::CrashLost,
        system_scoped: true,
        description: "Active module/weapon/mining cycles, launcher stamps, entosis cycles (intentionally lost on crash)",
    },
    RuntimeKindDescriptor {
        kind: KIND_TARGET_LOCKS,
        policy: RuntimePersistencePolicy::CrashLost,
        system_scoped: true,
        description: "Target locks, projected/active EWAR, remote assistance, active/pending bubbles (intentionally lost on crash)",
    },
    RuntimeKindDescriptor {
        kind: KIND_ACTIVE_WARPS,
        policy: RuntimePersistencePolicy::CrashLost,
        system_scoped: true,
        description: "In-flight warp completion timers (intentionally lost on crash)",
    },
    RuntimeKindDescriptor {
        kind: KIND_NPCS,
        policy: RuntimePersistencePolicy::CrashLost,
        system_scoped: true,
        description: "Dynamic NPC spawns and aggro/AI runtime state (intentionally lost on crash)",
    },
];

/// Looks up the static policy descriptor for a runtime checkpoint `kind`.
pub fn runtime_kind_descriptor(kind: &str) -> Option<&'static RuntimeKindDescriptor> {
    RUNTIME_KIND_CATALOG.iter().find(|d| d.kind == kind)
}

/// Returns `true` when `kind` is declared [`RuntimePersistencePolicy::Checkpointed`].
pub fn is_kind_checkpointed(kind: &str) -> bool {
    runtime_kind_descriptor(kind)
        .is_some_and(|d| d.policy == RuntimePersistencePolicy::Checkpointed)
}

/// Returns `true` when `kind` is declared [`RuntimePersistencePolicy::CrashLost`].
pub fn is_kind_crash_lost(kind: &str) -> bool {
    runtime_kind_descriptor(kind).is_some_and(|d| d.policy == RuntimePersistencePolicy::CrashLost)
}

/// Persisted per-ship/structure condition payload stored under `kind = "ship_condition"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShipConditionCheckpoint {
    pub ball_id: i64,
    pub space_entry_epoch: u64,
    pub position: [f64; 3],
    pub shield_hp: Option<f64>,
    pub armor_hp: Option<f64>,
    pub structure_hp: Option<f64>,
    pub capacitor: Option<f64>,
}

/// Raw `system_checkpoint` row representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemCheckpointRow {
    pub system_id: u32,
    pub kind: String,
    pub key: String,
    pub value: String,
    pub updated_unix: u64,
}

/// Returns the current host UNIX timestamp in seconds for `updated_unix` metadata.
pub fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Idempotent schema DDL for `system_checkpoint` and `world_meta.clock_offset_secs`.
pub fn apply_system_checkpoint_migration(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS system_checkpoint (
            system_id INTEGER NOT NULL,
            kind TEXT NOT NULL,
            key TEXT NOT NULL,
            value TEXT NOT NULL,
            updated_unix INTEGER NOT NULL,
            PRIMARY KEY (system_id, kind, key)
        ) WITHOUT ROWID;
        CREATE INDEX IF NOT EXISTS idx_system_checkpoint_kind
            ON system_checkpoint (kind, system_id);",
    )
    .map_err(|e| format!("system_checkpoint migration error: {e}"))?;

    if sqlite_table_exists(conn, "world_meta")? {
        conn.execute(
            "INSERT OR IGNORE INTO world_meta (meta_key, meta_value) VALUES (?1, 0)",
            params![WORLD_META_CLOCK_OFFSET_SECS_KEY],
        )
        .map_err(|e| format!("seed world_meta.clock_offset_secs error: {e}"))?;
        conn.execute(
            "INSERT OR IGNORE INTO world_meta (meta_key, meta_value) VALUES (?1, 0)",
            params![WORLD_META_CLEAN_SHUTDOWN_KEY],
        )
        .map_err(|e| format!("seed world_meta.clean_shutdown error: {e}"))?;
    }
    Ok(())
}

/// Ensures `system_checkpoint` exists on `conn`, applying the DDL only if missing.
pub fn ensure_system_checkpoint_schema(conn: &Connection) -> Result<(), String> {
    if !sqlite_table_exists(conn, "system_checkpoint")? {
        apply_system_checkpoint_migration(conn)?;
    }
    Ok(())
}

fn sqlite_table_exists(conn: &Connection, table_name: &str) -> Result<bool, String> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            params![table_name],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("sqlite_master lookup for {table_name}: {e}"))?;
    Ok(found.is_some())
}

fn validate_checkpoint_kind(kind: &str) -> Result<&'static str, String> {
    match runtime_kind_descriptor(kind) {
        Some(desc) if desc.policy == RuntimePersistencePolicy::Checkpointed => Ok(desc.kind),
        Some(desc) => Err(format!(
            "runtime kind '{}' is classified {:?} and cannot be written to system_checkpoint",
            desc.kind, desc.policy
        )),
        None => Err(format!(
            "unclassified system_checkpoint runtime kind '{kind}'"
        )),
    }
}

/// Upserts a single `system_checkpoint(system_id, kind, key, value)` row inside an open SQLite transaction.
pub fn upsert_system_checkpoint_in_tx<T: Serialize + ?Sized>(
    tx: &Transaction<'_>,
    system_id: u32,
    kind: &str,
    key: &str,
    value: &T,
    updated_unix: u64,
) -> Result<(), String> {
    let kind_static = validate_checkpoint_kind(kind)?;
    let json = to_checked_domain_json(kind_static, key, value).map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO system_checkpoint (system_id, kind, key, value, updated_unix)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(system_id, kind, key) DO UPDATE SET
             value = excluded.value,
             updated_unix = excluded.updated_unix",
        params![
            i64::from(system_id),
            kind,
            key,
            json,
            i64::try_from(updated_unix).unwrap_or(i64::MAX)
        ],
    )
    .map_err(|e| format!("upsert system_checkpoint ({system_id}, {kind}, {key}): {e}"))?;
    Ok(())
}

/// Upserts a single `system_checkpoint(system_id, kind, key, value)` row on `conn`.
pub fn upsert_system_checkpoint<T: Serialize + ?Sized>(
    conn: &Connection,
    system_id: u32,
    kind: &str,
    key: &str,
    value: &T,
    updated_unix: u64,
) -> Result<(), String> {
    ensure_system_checkpoint_schema(conn)?;
    let kind_static = validate_checkpoint_kind(kind)?;
    let json = to_checked_domain_json(kind_static, key, value).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO system_checkpoint (system_id, kind, key, value, updated_unix)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(system_id, kind, key) DO UPDATE SET
             value = excluded.value,
             updated_unix = excluded.updated_unix",
        params![
            i64::from(system_id),
            kind,
            key,
            json,
            i64::try_from(updated_unix).unwrap_or(i64::MAX)
        ],
    )
    .map_err(|e| format!("upsert system_checkpoint ({system_id}, {kind}, {key}): {e}"))?;
    Ok(())
}

/// Atomically replaces every `(key, value)` entry for `(system_id, kind)`.
pub fn replace_system_checkpoints_for_kind<T: Serialize>(
    conn: &mut Connection,
    system_id: u32,
    kind: &str,
    rows: &[(String, T)],
    updated_unix: u64,
) -> Result<(), String> {
    ensure_system_checkpoint_schema(conn)?;
    validate_checkpoint_kind(kind)?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("begin replace_system_checkpoints_for_kind tx: {e}"))?;
    tx.execute(
        "DELETE FROM system_checkpoint WHERE system_id = ?1 AND kind = ?2",
        params![i64::from(system_id), kind],
    )
    .map_err(|e| format!("delete old system_checkpoint ({system_id}, {kind}): {e}"))?;
    for (key, value) in rows {
        upsert_system_checkpoint_in_tx(&tx, system_id, kind, key, value, updated_unix)?;
    }
    tx.commit()
        .map_err(|e| format!("commit replace_system_checkpoints_for_kind tx: {e}"))?;
    Ok(())
}

/// Atomically replaces all `(system_id, key, value)` entries for `kind` across every system.
pub fn replace_all_checkpoints_for_kind<T: Serialize>(
    conn: &mut Connection,
    kind: &str,
    rows: &[(u32, String, T)],
    updated_unix: u64,
) -> Result<(), String> {
    ensure_system_checkpoint_schema(conn)?;
    validate_checkpoint_kind(kind)?;
    let tx = conn
        .transaction()
        .map_err(|e| format!("begin replace_all_checkpoints_for_kind tx: {e}"))?;
    tx.execute(
        "DELETE FROM system_checkpoint WHERE kind = ?1",
        params![kind],
    )
    .map_err(|e| format!("delete old system_checkpoint for kind {kind}: {e}"))?;
    for (system_id, key, value) in rows {
        upsert_system_checkpoint_in_tx(&tx, *system_id, kind, key, value, updated_unix)?;
    }
    tx.commit()
        .map_err(|e| format!("commit replace_all_checkpoints_for_kind tx: {e}"))?;
    Ok(())
}

/// Atomically replaces a batch of full checkpoint kinds in a single SQLite transaction.
pub fn replace_checkpoint_kinds_batch(
    conn: &mut Connection,
    kinds: &[(&str, Vec<(u32, String, String)>)],
    updated_unix: u64,
) -> Result<(), String> {
    ensure_system_checkpoint_schema(conn)?;
    for (kind, _) in kinds {
        validate_checkpoint_kind(kind)?;
    }
    let ts = i64::try_from(updated_unix).unwrap_or(i64::MAX);
    let tx = conn
        .transaction()
        .map_err(|e| format!("begin replace_checkpoint_kinds_batch tx: {e}"))?;
    {
        let mut del_stmt = tx
            .prepare("DELETE FROM system_checkpoint WHERE kind = ?1")
            .map_err(|e| format!("prepare delete kind: {e}"))?;
        let mut ins_stmt = tx
            .prepare(
                "INSERT INTO system_checkpoint (system_id, kind, key, value, updated_unix)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(system_id, kind, key) DO UPDATE SET
                     value = excluded.value,
                     updated_unix = excluded.updated_unix",
            )
            .map_err(|e| format!("prepare insert checkpoint: {e}"))?;
        for (kind, rows) in kinds {
            del_stmt
                .execute(params![*kind])
                .map_err(|e| format!("delete system_checkpoint kind {kind}: {e}"))?;
            for (system_id, key, json_value) in rows {
                ins_stmt
                    .execute(params![i64::from(*system_id), *kind, key, json_value, ts])
                    .map_err(|e| {
                        format!("insert system_checkpoint ({system_id}, {kind}, {key}): {e}")
                    })?;
            }
        }
    }
    tx.commit()
        .map_err(|e| format!("commit replace_checkpoint_kinds_batch tx: {e}"))?;
    Ok(())
}

/// Deletes a single `system_checkpoint` row.
pub fn delete_system_checkpoint(
    conn: &Connection,
    system_id: u32,
    kind: &str,
    key: &str,
) -> Result<usize, String> {
    if !sqlite_table_exists(conn, "system_checkpoint")? {
        return Ok(0);
    }
    conn.execute(
        "DELETE FROM system_checkpoint WHERE system_id = ?1 AND kind = ?2 AND key = ?3",
        params![i64::from(system_id), kind, key],
    )
    .map_err(|e| format!("delete system_checkpoint ({system_id}, {kind}, {key}): {e}"))
}

/// Removes every `system_checkpoint` row belonging to `system_id`.
pub fn clear_system_checkpoints_for_system(
    conn: &Connection,
    system_id: u32,
) -> Result<usize, String> {
    if !sqlite_table_exists(conn, "system_checkpoint")? {
        return Ok(0);
    }
    conn.execute(
        "DELETE FROM system_checkpoint WHERE system_id = ?1",
        params![i64::from(system_id)],
    )
    .map_err(|e| format!("clear system_checkpoint for system {system_id}: {e}"))
}

/// Purges any rows in `system_checkpoint` whose `kind` is declared [`RuntimePersistencePolicy::CrashLost`].
pub fn purge_crash_lost_checkpoints(conn: &Connection) -> Result<usize, String> {
    if !sqlite_table_exists(conn, "system_checkpoint")? {
        return Ok(0);
    }
    let mut deleted = 0usize;
    for desc in RUNTIME_KIND_CATALOG {
        if desc.policy == RuntimePersistencePolicy::CrashLost {
            deleted += conn
                .execute(
                    "DELETE FROM system_checkpoint WHERE kind = ?1",
                    params![desc.kind],
                )
                .map_err(|e| format!("purge crash-lost kind {}: {e}", desc.kind))?;
        }
    }
    Ok(deleted)
}

/// Loads all `(key, value)` checkpoint entries for `(system_id, kind)`.
pub fn load_system_checkpoints<T: DeserializeOwned>(
    conn: &Connection,
    system_id: u32,
    kind: &str,
) -> Result<Vec<(String, T)>, String> {
    if !sqlite_table_exists(conn, "system_checkpoint")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT key, value FROM system_checkpoint
             WHERE system_id = ?1 AND kind = ?2
             ORDER BY key",
        )
        .map_err(|e| format!("prepare load_system_checkpoints: {e}"))?;
    let rows = stmt
        .query_map(params![i64::from(system_id), kind], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| format!("query load_system_checkpoints: {e}"))?;
    let mut out = Vec::new();
    for row_res in rows {
        let (key, raw_json) = row_res
            .map_err(|e| format!("read system_checkpoint row ({system_id}, {kind}): {e}"))?;
        let val = from_checked_domain_json::<T>(&raw_json)?;
        out.push((key, val));
    }
    Ok(out)
}

/// Loads all `(system_id, key, value)` checkpoint entries for `kind` across all systems.
pub fn load_all_checkpoints_for_kind<T: DeserializeOwned>(
    conn: &Connection,
    kind: &str,
) -> Result<Vec<(u32, String, T)>, String> {
    if !sqlite_table_exists(conn, "system_checkpoint")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT system_id, key, value FROM system_checkpoint
             WHERE kind = ?1
             ORDER BY system_id, key",
        )
        .map_err(|e| format!("prepare load_all_checkpoints_for_kind: {e}"))?;
    let rows = stmt
        .query_map(params![kind], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| format!("query load_all_checkpoints_for_kind: {e}"))?;
    let mut out = Vec::new();
    for row_res in rows {
        let (sys_raw, key, raw_json) =
            row_res.map_err(|e| format!("read system_checkpoint row ({kind}): {e}"))?;
        let system_id = u32::try_from(sys_raw)
            .map_err(|_| format!("invalid system_id {sys_raw} in system_checkpoint"))?;
        let val = from_checked_domain_json::<T>(&raw_json)?;
        out.push((system_id, key, val));
    }
    Ok(out)
}

/// Loads all raw `(system_id, kind, key, value_json)` rows stored in `system_checkpoint`.
pub fn load_all_raw_checkpoints(conn: &Connection) -> Result<Vec<SystemCheckpointRow>, String> {
    if !sqlite_table_exists(conn, "system_checkpoint")? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare(
            "SELECT system_id, kind, key, value, updated_unix FROM system_checkpoint
             ORDER BY system_id, kind, key",
        )
        .map_err(|e| format!("prepare load_all_raw_checkpoints: {e}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(|e| format!("query load_all_raw_checkpoints: {e}"))?;
    let mut out = Vec::new();
    for row_res in rows {
        let (sys_raw, kind, key, value, ts_raw) =
            row_res.map_err(|e| format!("read system_checkpoint row: {e}"))?;
        let system_id = u32::try_from(sys_raw)
            .map_err(|_| format!("invalid system_id {sys_raw} in system_checkpoint"))?;
        let updated_unix = u64::try_from(ts_raw).unwrap_or(0);
        out.push(SystemCheckpointRow {
            system_id,
            kind,
            key,
            value,
            updated_unix,
        });
    }
    Ok(out)
}

/// Returns `true` when `system_checkpoint` contains at least one row.
pub fn has_any_system_checkpoints(conn: &Connection) -> Result<bool, String> {
    if !sqlite_table_exists(conn, "system_checkpoint")? {
        return Ok(false);
    }
    let found: Option<i64> = conn
        .query_row("SELECT 1 FROM system_checkpoint LIMIT 1", [], |r| r.get(0))
        .optional()
        .map_err(|e| format!("has_any_system_checkpoints: {e}"))?;
    Ok(found.is_some())
}

/// Loads `world_meta.clock_offset_secs` from SQLite, returning `0` if missing or non-positive.
pub fn load_clock_offset_secs(conn: &Connection) -> Result<u64, String> {
    if !sqlite_table_exists(conn, "world_meta")? {
        return Ok(0);
    }
    let val: Option<i64> = conn
        .query_row(
            "SELECT meta_value FROM world_meta WHERE meta_key = ?1",
            params![WORLD_META_CLOCK_OFFSET_SECS_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("load world_meta.clock_offset_secs: {e}"))?;
    Ok(val.and_then(|v| u64::try_from(v).ok()).unwrap_or(0))
}

/// Persists `world_meta.clock_offset_secs` inside an active SQLite transaction (`#920`, `#899` §3.6).
pub fn persist_clock_offset_secs_in_tx(
    tx: &Transaction<'_>,
    offset_secs: u64,
) -> Result<(), String> {
    let clamped = i64::try_from(offset_secs).unwrap_or(i64::MAX);
    tx.execute(
        "INSERT INTO world_meta (meta_key, meta_value) VALUES (?1, ?2)
         ON CONFLICT(meta_key) DO UPDATE SET meta_value = excluded.meta_value",
        params![WORLD_META_CLOCK_OFFSET_SECS_KEY, clamped],
    )
    .map_err(|e| format!("persist world_meta.clock_offset_secs in tx: {e}"))?;
    Ok(())
}

/// Persists `world_meta.clock_offset_secs` directly on `conn`.
pub fn persist_clock_offset_secs(conn: &Connection, offset_secs: u64) -> Result<(), String> {
    if !sqlite_table_exists(conn, "world_meta")? {
        return Ok(());
    }
    let clamped = i64::try_from(offset_secs).unwrap_or(i64::MAX);
    conn.execute(
        "INSERT INTO world_meta (meta_key, meta_value) VALUES (?1, ?2)
         ON CONFLICT(meta_key) DO UPDATE SET meta_value = excluded.meta_value",
        params![WORLD_META_CLOCK_OFFSET_SECS_KEY, clamped],
    )
    .map_err(|e| format!("persist world_meta.clock_offset_secs: {e}"))?;
    Ok(())
}

/// Reads `world_meta.clean_shutdown` (`true` when the previous stop finished cleanly).
pub fn load_clean_shutdown_marker(conn: &Connection) -> Result<bool, String> {
    if !sqlite_table_exists(conn, "world_meta")? {
        return Ok(false);
    }
    let val: Option<i64> = conn
        .query_row(
            "SELECT meta_value FROM world_meta WHERE meta_key = ?1",
            params![WORLD_META_CLEAN_SHUTDOWN_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| format!("load world_meta.clean_shutdown: {e}"))?;
    Ok(val.unwrap_or(0) != 0)
}

/// Updates `world_meta.clean_shutdown` (`1` on clean shutdown exit, `0` while running).
pub fn set_clean_shutdown_marker(conn: &Connection, clean: bool) -> Result<(), String> {
    if !sqlite_table_exists(conn, "world_meta")? {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO world_meta (meta_key, meta_value) VALUES (?1, ?2)
         ON CONFLICT(meta_key) DO UPDATE SET meta_value = excluded.meta_value",
        params![WORLD_META_CLEAN_SHUTDOWN_KEY, i64::from(clean)],
    )
    .map_err(|e| format!("set world_meta.clean_shutdown: {e}"))?;
    Ok(())
}

/// Executes `PRAGMA wal_checkpoint(TRUNCATE)` and returns `(busy, log_frames, checkpointed_frames)`.
pub fn wal_checkpoint_truncate(conn: &Connection) -> Result<(i64, i64, i64), String> {
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    })
    .map_err(|e| format!("PRAGMA wal_checkpoint(TRUNCATE) failed: {e}"))
}

/// Reads any legacy `service_state_blobs['server_runtime_state']` JSON payload before retirement.
pub fn read_legacy_server_runtime_blob(conn: &Connection) -> Result<Option<String>, String> {
    if !sqlite_table_exists(conn, "service_state_blobs")? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT state_json FROM service_state_blobs WHERE service_name = ?1",
        params![LEGACY_SERVER_RUNTIME_STATE_BLOB_KEY],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| format!("read legacy server_runtime_state blob: {e}"))
}

/// Deletes the retired `server_runtime_state` monolith entry from `service_state_blobs` and
/// clears `cluster_and_runtime.server_runtime_json` / `sp6_lifecycle.server_runtime_json`
/// in `service_domain_state` if present.
pub fn retire_legacy_server_runtime_blobs(conn: &Connection) -> Result<usize, String> {
    let mut removed = 0usize;
    if sqlite_table_exists(conn, "service_state_blobs")? {
        removed += conn
            .execute(
                "DELETE FROM service_state_blobs WHERE service_name = ?1",
                params![LEGACY_SERVER_RUNTIME_STATE_BLOB_KEY],
            )
            .map_err(|e| format!("delete legacy server_runtime_state blob: {e}"))?;
    }
    if sqlite_table_exists(conn, "service_domain_state")? {
        for (domain_key, json_field) in [
            ("cluster_and_runtime", "server_runtime_json"),
            ("sp6_lifecycle", "server_runtime_json"),
        ] {
            let existing: Option<String> = conn
                .query_row(
                    "SELECT state_json FROM service_domain_state WHERE domain_key = ?1",
                    params![domain_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| format!("read service_domain_state.{domain_key}: {e}"))?;
            if let Some(raw_json) = existing {
                if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&raw_json) {
                    if let Some(obj) = value.as_object_mut() {
                        if obj.get(json_field).is_some_and(|v| !v.is_null()) {
                            obj.insert(json_field.to_string(), serde_json::Value::Null);
                            if let Ok(updated) = serde_json::to_string(&value) {
                                conn.execute(
                                    "UPDATE service_domain_state SET state_json = ?2 WHERE domain_key = ?1",
                                    params![domain_key, updated],
                                )
                                .map_err(|e| {
                                    format!("clear {domain_key}.{json_field}: {e}")
                                })?;
                                removed += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn open_test_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(
            "CREATE TABLE world_meta (
                meta_key TEXT PRIMARY KEY,
                meta_value INTEGER NOT NULL
            );
            CREATE TABLE service_state_blobs (
                service_name TEXT PRIMARY KEY,
                state_json TEXT NOT NULL
            );
            CREATE TABLE service_domain_state (
                domain_key TEXT PRIMARY KEY,
                state_json TEXT NOT NULL
            );",
        )
        .expect("create base tables");
        apply_system_checkpoint_migration(&conn).expect("apply migration");
        conn
    }

    #[test]
    fn migration_is_idempotent_and_seeds_world_meta_keys() {
        let conn = open_test_db();
        apply_system_checkpoint_migration(&conn).expect("second migration succeeds");
        assert_eq!(load_clock_offset_secs(&conn).expect("load clock"), 0);
        assert!(!load_clean_shutdown_marker(&conn).expect("load clean marker"));
    }

    #[test]
    fn classification_covers_crash_lost_and_rejects_them_from_checkpoint_writes() {
        assert!(is_kind_checkpointed(KIND_SHIP_CONDITION));
        assert!(is_kind_checkpointed(KIND_SCAN_SIGNATURES));
        assert!(is_kind_checkpointed(KIND_FLEET_EXTRAS));
        assert!(is_kind_crash_lost(KIND_MODULE_CYCLES));
        assert!(is_kind_crash_lost(KIND_TARGET_LOCKS));
        assert!(is_kind_crash_lost(KIND_NPCS));
        assert!(is_kind_crash_lost(KIND_ACTIVE_WARPS));

        let conn = open_test_db();
        for bad_kind in [KIND_MODULE_CYCLES, KIND_TARGET_LOCKS, KIND_NPCS, "unknown"] {
            let err = upsert_system_checkpoint(&conn, 30_000_142, bad_kind, "k1", &123u64, 100)
                .expect_err("crash-lost or unknown kind must be rejected");
            assert!(
                err.contains("CrashLost") || err.contains("unclassified"),
                "unexpected error for {bad_kind}: {err}"
            );
        }

        // Even if legacy/corrupt rows exist for crash-lost kinds, purge removes them cleanly.
        conn.execute(
            "INSERT INTO system_checkpoint (system_id, kind, key, value, updated_unix)
             VALUES (30000142, 'module_cycles', 'm1', '{}', 10),
                    (30000142, 'target_locks', 't1', '{}', 10),
                    (30000142, 'npcs', 'n1', '{}', 10)",
            [],
        )
        .expect("insert raw crash-lost rows");
        let purged = purge_crash_lost_checkpoints(&conn).expect("purge crash-lost");
        assert_eq!(purged, 3);
        assert!(!has_any_system_checkpoints(&conn).expect("empty after purge"));
    }

    #[test]
    fn per_system_checkpoint_crud_replace_and_non_finite_rejection() {
        let mut conn = open_test_db();
        let ship = ShipConditionCheckpoint {
            ball_id: 100_000_001,
            space_entry_epoch: 7,
            position: [1250.5, -300.0, 900.25],
            shield_hp: Some(450.0),
            armor_hp: Some(320.0),
            structure_hp: Some(500.0),
            capacitor: Some(210.0),
        };
        upsert_system_checkpoint(
            &conn,
            30_000_142,
            KIND_SHIP_CONDITION,
            "100000001",
            &ship,
            1_700_000_010,
        )
        .expect("upsert ship condition");
        let loaded: Vec<(String, ShipConditionCheckpoint)> =
            load_system_checkpoints(&conn, 30_000_142, KIND_SHIP_CONDITION)
                .expect("load ship condition");
        assert_eq!(loaded, vec![("100000001".to_string(), ship.clone())]);

        // Non-finite floats must be rejected before writing corrupt JSON into SQLite.
        let bad_ship = ShipConditionCheckpoint {
            position: [f64::NAN, 0.0, 0.0],
            ..ship
        };
        assert!(upsert_system_checkpoint(
            &conn,
            30_000_142,
            KIND_SHIP_CONDITION,
            "bad",
            &bad_ship,
            10
        )
        .is_err());

        replace_all_checkpoints_for_kind(
            &mut conn,
            KIND_SCAN_SIGNATURES,
            &[
                (
                    30_000_142,
                    "90000001|1".to_string(),
                    (90_000_001i64, 30_000_142u32, 1i64),
                ),
                (
                    30_000_144,
                    "90000002|2".to_string(),
                    (90_000_002i64, 30_000_144u32, 2i64),
                ),
            ],
            1_700_000_020,
        )
        .expect("replace all scan signatures");
        let all_sigs: Vec<(u32, String, (i64, u32, i64))> =
            load_all_checkpoints_for_kind(&conn, KIND_SCAN_SIGNATURES).expect("load all sigs");
        assert_eq!(all_sigs.len(), 2);

        let cleared = clear_system_checkpoints_for_system(&conn, 30_000_142).expect("clear jita");
        assert_eq!(cleared, 2);
        let remaining: Vec<(u32, String, (i64, u32, i64))> =
            load_all_checkpoints_for_kind(&conn, KIND_SCAN_SIGNATURES).expect("remaining");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].0, 30_000_144);
    }

    #[test]
    fn clock_offset_clean_shutdown_wal_truncate_and_legacy_retirement_work() {
        let dir = std::env::temp_dir().join(format!(
            "evomu_sys_chk_test_{}_{}",
            std::process::id(),
            current_unix_secs()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let db_path: PathBuf = dir.join("world.db");
        let mut conn = Connection::open(&db_path).expect("open file sqlite");
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE world_meta (meta_key TEXT PRIMARY KEY, meta_value INTEGER NOT NULL);
             CREATE TABLE service_state_blobs (service_name TEXT PRIMARY KEY, state_json TEXT NOT NULL);
             CREATE TABLE service_domain_state (domain_key TEXT PRIMARY KEY, state_json TEXT NOT NULL);",
        )
        .expect("init wal db");
        apply_system_checkpoint_migration(&conn).expect("migrate");

        {
            let tx = conn.transaction().expect("begin tx");
            persist_clock_offset_secs_in_tx(&tx, 86_400).expect("persist clock offset in tx");
            tx.commit().expect("commit tx");
        }
        assert_eq!(load_clock_offset_secs(&conn).expect("load clock"), 86_400);

        set_clean_shutdown_marker(&conn, true).expect("set clean marker");
        assert!(load_clean_shutdown_marker(&conn).expect("read clean marker"));

        conn.execute(
            "INSERT INTO service_state_blobs (service_name, state_json) VALUES (?1, ?2)",
            params![
                LEGACY_SERVER_RUNTIME_STATE_BLOB_KEY,
                "{\"active_warps\":{}}"
            ],
        )
        .expect("seed legacy blob");
        conn.execute(
            "INSERT INTO service_domain_state (domain_key, state_json) VALUES (?1, ?2)",
            params![
                "cluster_and_runtime",
                "{\"server_runtime_json\":\"{}\",\"other\":1}"
            ],
        )
        .expect("seed legacy domain field");

        assert!(read_legacy_server_runtime_blob(&conn)
            .expect("read legacy blob")
            .is_some());
        let retired = retire_legacy_server_runtime_blobs(&conn).expect("retire legacy");
        assert_eq!(retired, 2);
        assert!(read_legacy_server_runtime_blob(&conn)
            .expect("legacy blob removed")
            .is_none());

        let (busy, _log, _ckpt) = wal_checkpoint_truncate(&conn).expect("wal checkpoint truncate");
        assert_eq!(busy, 0);
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
