//! SQLite persistence for `D-3` / `D-6`:
//! - `sp6_lifecycle_state` (`Sp6PersistedState`)
//! - `world_meta` monotonic ID sequence counters (`feat:invariant.ids-monotonic-across-restart`)
//!
//! (`owner_notes`, `corp_lp_balances`, and `char_lp_logs` moved to `domain_rows` in `#916`.)

use crate::snapshot_ext::Sp6PersistedState;
use crate::sqlite_store::fingerprints::TableFingerprints;
use crate::types::WorldStateSnapshot;
use rusqlite::{params, Connection, Transaction};

pub const D6_SCHEMA_VERSION: i64 = 3;

const D6_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS sp6_lifecycle_state (
    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    state_json TEXT NOT NULL
);
"#;

/// Fingerprints of the `D-6` sequence counters and `sp6_lifecycle_state`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct D6Fingerprints {
    pub next_alliance_id: i64,
    pub next_corporation_id: i64,
    pub next_notification_id: i64,
    pub next_report_id: i64,
    pub sp6_lifecycle_fp: u64,
}

pub fn apply_d6_migration(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(D6_SCHEMA_SQL).map_err(|e| e.to_string())?;
    conn.execute_batch(
        "INSERT OR IGNORE INTO world_meta (meta_key, meta_value) VALUES
            ('next_alliance_id', 99000001),
            ('next_corporation_id', 98000001),
            ('next_notification_id', 940000001),
            ('next_report_id', 1);",
    )
    .map_err(|e| e.to_string())?;
    let now_unix = crate::clock::host_wall_unix_secs_and_nanos().0 as i64;
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, name, applied_unix) VALUES (?1, ?2, ?3)",
        params![D6_SCHEMA_VERSION, "0003_d6_owner_notes_lp_ids", now_unix],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn compute_d6_fingerprints(snap: &WorldStateSnapshot) -> D6Fingerprints {
    D6Fingerprints {
        next_alliance_id: snap.next_alliance_id,
        next_corporation_id: snap.next_corporation_id,
        next_notification_id: snap.next_notification_id,
        next_report_id: snap.next_report_id,
        sp6_lifecycle_fp: super::fingerprints::fp_sp6_lifecycle(&snap.sp6_lifecycle),
    }
}

fn parse_sp6_lifecycle_row(r: &rusqlite::Row<'_>) -> Result<Sp6PersistedState, String> {
    r.get::<_, String>(2)
        .map_err(|e| e.to_string())
        .and_then(|js| serde_json::from_str::<Sp6PersistedState>(&js).map_err(|e| e.to_string()))
}

pub fn load_d6_checked(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<(), String> {
    if let Ok(mut stmt) = conn.prepare("SELECT rowid, meta_key, meta_value FROM world_meta") {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = super::quarantine::row_col_string(r, 1);
                let payload = super::quarantine::row_payload_summary(r, 1, 3);
                match (|| -> Result<(String, i64), String> {
                    Ok((
                        r.get(1).map_err(|e| e.to_string())?,
                        r.get(2).map_err(|e| e.to_string())?,
                    ))
                })() {
                    Ok((k, v)) => match k.as_str() {
                        "next_alliance_id" => snap.next_alliance_id = v,
                        "next_corporation_id" => snap.next_corporation_id = v,
                        "next_notification_id" => snap.next_notification_id = v,
                        "next_report_id" => snap.next_report_id = v,
                        _ => {}
                    },
                    Err(err) => {
                        ctx.record_bad_row("world_meta", "world_meta", key, payload, err, rowid)?;
                    }
                }
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare(
        "SELECT rowid, singleton_id, state_json FROM sp6_lifecycle_state WHERE singleton_id = 1",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            if let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = super::quarantine::row_col_string(r, 1);
                let payload = super::quarantine::row_col_string(r, 2);
                match parse_sp6_lifecycle_row(r) {
                    Ok(st) => {
                        snap.sp6_lifecycle = st;
                    }
                    Err(err) => {
                        ctx.record_bad_row(
                            "sp6_lifecycle_state",
                            "sp6_lifecycle_state",
                            key,
                            payload,
                            err,
                            rowid,
                        )?;
                    }
                }
            }
        }
    }
    snap.reconcile_id_counters();
    Ok(())
}

pub fn load_d6(conn: &Connection, snap: &mut WorldStateSnapshot) {
    let _ = load_d6_checked(
        conn,
        snap,
        &mut super::quarantine::LoadContext::quarantine_in_memory(),
    );
}

/// Writes changed `D-6` ID counters and `sp6_lifecycle_state`; returns `true` if anything changed.
pub fn sync_d6(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
) -> Result<bool, String> {
    let mut changed = false;
    let df = &mut fp.d6;

    for (key, snap_val, fp_val) in [
        ("next_alliance_id", snap.next_alliance_id, &mut df.next_alliance_id),
        ("next_corporation_id", snap.next_corporation_id, &mut df.next_corporation_id),
        ("next_notification_id", snap.next_notification_id, &mut df.next_notification_id),
        ("next_report_id", snap.next_report_id, &mut df.next_report_id),
    ] {
        if *fp_val != snap_val {
            changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES (?1, ?2)",
                params![key, snap_val],
            )
            .map_err(|e| e.to_string())?;
            *fp_val = snap_val;
        }
    }

    let sp6_h = super::fingerprints::fp_sp6_lifecycle(&snap.sp6_lifecycle);
    if df.sp6_lifecycle_fp != sp6_h {
        changed = true;
        let js = super::fingerprints::to_checked_json_string(&snap.sp6_lifecycle)?;
        tx.execute(
            "INSERT OR REPLACE INTO sp6_lifecycle_state (singleton_id, state_json) VALUES (1, ?1)",
            params![js],
        )
        .map_err(|e| e.to_string())?;
        df.sp6_lifecycle_fp = sp6_h;
    }

    Ok(changed)
}

fn reload_sp6_lifecycle_strict(conn: &Connection, expected_fp: u64) -> Result<Sp6PersistedState, String> {
    let mut stmt = conn
        .prepare(
            "SELECT rowid, singleton_id, state_json FROM sp6_lifecycle_state WHERE singleton_id = 1",
        )
        .map_err(|e| format!("rollback sp6_lifecycle_state prepare failed: {e}"))?;
    let mut rows = stmt
        .query([])
        .map_err(|e| format!("rollback sp6_lifecycle_state query failed: {e}"))?;
    if let Some(r) = rows
        .next()
        .map_err(|e| format!("rollback sp6_lifecycle_state row failed: {e}"))?
    {
        parse_sp6_lifecycle_row(r)
            .map_err(|e| format!("rollback sp6_lifecycle_state parse failed: {e}"))
    } else {
        let default_fp = super::fingerprints::fp_sp6_lifecycle(&Sp6PersistedState::default());
        if expected_fp == 0 || expected_fp == default_fp {
            Ok(Sp6PersistedState::default())
        } else {
            Err("rollback sp6_lifecycle_state missing row for non-default baseline".to_string())
        }
    }
}

/// Restores `D-6` state from SQLite after a failed write (`try_with_mut_snapshot` rollback).
///
/// Builds every dirty table reload into temporaries before mutating `snap`, failing closed
/// without wiping live state or partially reverting counters if any SQLite query or parse fails.
pub fn rollback_d6(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    fp: &TableFingerprints,
) -> Result<(), String> {
    let df = &fp.d6;
    let dirty_sp6 = super::fingerprints::fp_sp6_lifecycle(&snap.sp6_lifecycle) != df.sp6_lifecycle_fp;
    if !dirty_sp6 {
        snap.next_alliance_id = df.next_alliance_id;
        snap.next_corporation_id = df.next_corporation_id;
        snap.next_notification_id = df.next_notification_id;
        snap.next_report_id = df.next_report_id;
        return Ok(());
    }

    let fresh_sp6 = match reload_sp6_lifecycle_strict(conn, df.sp6_lifecycle_fp) {
        Ok(staged) => staged,
        Err(err) => {
            crate::persistence::record_persist_failure();
            tracing::error!("rollback_d6 failed closed without mutating snapshot: {err}");
            return Err(err);
        }
    };

    snap.next_alliance_id = df.next_alliance_id;
    snap.next_corporation_id = df.next_corporation_id;
    snap.next_notification_id = df.next_notification_id;
    snap.next_report_id = df.next_report_id;
    snap.sp6_lifecycle = fresh_sp6;
    Ok(())
}
