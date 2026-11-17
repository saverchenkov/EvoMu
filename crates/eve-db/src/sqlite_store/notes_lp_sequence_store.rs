//! SQLite persistence for `D-6` (`world_meta` monotonic ID sequence counters:
//! `feat:invariant.ids-monotonic-across-restart`).
//!
//! (`owner_notes`, `corp_lp_balances`, `char_lp_logs`, and `sp6_lifecycle` moved to `domain_rows` in `#916`.)

use crate::sqlite_store::fingerprints::TableFingerprints;
use crate::types::WorldStateSnapshot;
use rusqlite::{params, Connection, Transaction};

pub const D6_SCHEMA_VERSION: i64 = 3;

/// Fingerprints of the `D-6` monotonic sequence counters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct D6Fingerprints {
    pub next_alliance_id: i64,
    pub next_corporation_id: i64,
    pub next_notification_id: i64,
    pub next_report_id: i64,
}

pub fn apply_d6_migration(conn: &Connection) -> Result<(), String> {
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
    }
}

pub fn load_d6_checked(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<(), String> {
    let mut next_alliance_id = snap.next_alliance_id;
    let mut next_corporation_id = snap.next_corporation_id;
    let mut next_notification_id = snap.next_notification_id;
    let mut next_report_id = snap.next_report_id;

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
                        "next_alliance_id" => next_alliance_id = v,
                        "next_corporation_id" => next_corporation_id = v,
                        "next_notification_id" => next_notification_id = v,
                        "next_report_id" => next_report_id = v,
                        _ => {}
                    },
                    Err(err) => {
                        ctx.record_bad_row("world_meta", "world_meta", key, payload, err, rowid)?;
                    }
                }
            }
        }
    }
    snap.next_alliance_id = next_alliance_id;
    snap.next_corporation_id = next_corporation_id;
    snap.next_notification_id = next_notification_id;
    snap.next_report_id = next_report_id;
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

/// Writes changed `D-6` ID counters; returns `true` if anything changed.
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

    Ok(changed)
}

/// Restores `D-6` monotonic ID counters from baseline fingerprints after a failed write (`try_with_mut_snapshot` rollback).
pub fn rollback_d6(
    _conn: &Connection,
    snap: &mut WorldStateSnapshot,
    fp: &TableFingerprints,
) -> Result<(), String> {
    let df = &fp.d6;
    snap.next_alliance_id = df.next_alliance_id;
    snap.next_corporation_id = df.next_corporation_id;
    snap.next_notification_id = df.next_notification_id;
    snap.next_report_id = df.next_report_id;
    Ok(())
}
