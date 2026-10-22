//! SQLite persistence for `D-3` / `D-6`:
//! - `owner_notes` (`MarketOrderMeta`, `IndustryJobCostMeta`, `PiColonyNetworkState`, user notes, reports)
//! - `corp_lp_balances` and `char_lp_logs` (`feat:lifecycle.lp-durable`)
//! - `world_meta` monotonic ID sequence counters (`feat:invariant.ids-monotonic-across-restart`)

use crate::snapshot_ext::LpRewardLogEntry;
use crate::sqlite_store::fingerprints::{fp_json, TableFingerprints};
use crate::types::{OwnerNoteRecord, WorldStateSnapshot};
use rusqlite::{params, Connection, Transaction};
use std::collections::HashMap;

pub const D6_SCHEMA_VERSION: i64 = 3;

const D6_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS owner_notes (
    note_id INTEGER PRIMARY KEY,
    character_id INTEGER NOT NULL,
    item_id INTEGER NOT NULL,
    label TEXT NOT NULL,
    note TEXT NOT NULL,
    timestamp_unix INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS corp_lp_balances (
    corporation_id INTEGER NOT NULL,
    issuer_corp_id INTEGER NOT NULL,
    lp_balance INTEGER NOT NULL,
    PRIMARY KEY (corporation_id, issuer_corp_id)
);
CREATE TABLE IF NOT EXISTS char_lp_logs (
    character_id INTEGER PRIMARY KEY,
    logs_json TEXT NOT NULL
);
"#;

/// Fingerprints of the `D-6` persistence tables and monotonic ID counters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct D6Fingerprints {
    pub owner_notes: HashMap<i64, u64>,
    pub corp_lp_balances: HashMap<(i64, i64), i64>,
    pub char_lp_logs: HashMap<i64, u64>,
    pub next_alliance_id: i64,
    pub next_corporation_id: i64,
    pub next_notification_id: i64,
    pub next_report_id: i64,
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
        owner_notes: snap.owner_notes.iter().map(|(&k, v)| (k, fp_json(v))).collect(),
        corp_lp_balances: snap.corp_lp_balances.clone(),
        char_lp_logs: snap.char_lp_logs.iter().map(|(&k, v)| (k, fp_json(v))).collect(),
        next_alliance_id: snap.next_alliance_id,
        next_corporation_id: snap.next_corporation_id,
        next_notification_id: snap.next_notification_id,
        next_report_id: snap.next_report_id,
    }
}

pub fn load_d6(conn: &Connection, snap: &mut WorldStateSnapshot) {
    if let Ok(mut stmt) = conn.prepare("SELECT meta_key, meta_value FROM world_meta") {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
            for (k, v) in rows.flatten() {
                match k.as_str() {
                    "next_alliance_id" => snap.next_alliance_id = v,
                    "next_corporation_id" => snap.next_corporation_id = v,
                    "next_notification_id" => snap.next_notification_id = v,
                    "next_report_id" => snap.next_report_id = v,
                    _ => {}
                }
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare(
        "SELECT note_id, character_id, item_id, label, note, timestamp_unix FROM owner_notes",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(OwnerNoteRecord {
                note_id: r.get(0)?,
                character_id: r.get(1)?,
                item_id: r.get(2)?,
                label: r.get(3)?,
                note: r.get(4)?,
                timestamp_unix: r.get::<_, i64>(5)? as u64,
            })
        }) {
            for note in rows.flatten() {
                snap.owner_notes.insert(note.note_id, note);
            }
        }
    }
    if let Ok(mut stmt) =
        conn.prepare("SELECT corporation_id, issuer_corp_id, lp_balance FROM corp_lp_balances")
    {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
        }) {
            for (corp_id, issuer_corp_id, lp) in rows.flatten() {
                snap.corp_lp_balances.insert((corp_id, issuer_corp_id), lp);
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare("SELECT character_id, logs_json FROM char_lp_logs") {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
            for (cid, js) in rows.flatten() {
                if let Ok(entries) = serde_json::from_str::<Vec<LpRewardLogEntry>>(&js) {
                    snap.char_lp_logs.insert(cid, entries);
                }
            }
        }
    }
    snap.reconcile_id_counters();
}

/// Writes changed `D-6` rows (`owner_notes`, `corp_lp_balances`, `char_lp_logs`, and ID counters);
/// returns `true` if anything changed.
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

    let gone_notes: Vec<i64> = df
        .owner_notes
        .keys()
        .filter(|k| !snap.owner_notes.contains_key(*k))
        .copied()
        .collect();
    for nid in gone_notes {
        changed = true;
        tx.execute("DELETE FROM owner_notes WHERE note_id = ?1", params![nid])
            .map_err(|e| e.to_string())?;
        df.owner_notes.remove(&nid);
    }
    for (&nid, note) in &snap.owner_notes {
        let h = fp_json(note);
        if df.owner_notes.get(&nid) != Some(&h) {
            changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO owner_notes (note_id, character_id, item_id, label, note, timestamp_unix) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    note.note_id,
                    note.character_id,
                    note.item_id,
                    note.label,
                    note.note,
                    note.timestamp_unix as i64
                ],
            )
            .map_err(|e| e.to_string())?;
            df.owner_notes.insert(nid, h);
        }
    }

    let gone_lp: Vec<(i64, i64)> = df
        .corp_lp_balances
        .keys()
        .filter(|k| !snap.corp_lp_balances.contains_key(*k))
        .copied()
        .collect();
    for (corp_id, issuer_id) in gone_lp {
        changed = true;
        tx.execute(
            "DELETE FROM corp_lp_balances WHERE corporation_id = ?1 AND issuer_corp_id = ?2",
            params![corp_id, issuer_id],
        )
        .map_err(|e| e.to_string())?;
        df.corp_lp_balances.remove(&(corp_id, issuer_id));
    }
    for (&(corp_id, issuer_id), &lp) in &snap.corp_lp_balances {
        if df.corp_lp_balances.get(&(corp_id, issuer_id)) != Some(&lp) {
            changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO corp_lp_balances (corporation_id, issuer_corp_id, lp_balance) VALUES (?1, ?2, ?3)",
                params![corp_id, issuer_id, lp],
            )
            .map_err(|e| e.to_string())?;
            df.corp_lp_balances.insert((corp_id, issuer_id), lp);
        }
    }

    let gone_logs: Vec<i64> = df
        .char_lp_logs
        .keys()
        .filter(|k| !snap.char_lp_logs.contains_key(*k))
        .copied()
        .collect();
    for cid in gone_logs {
        changed = true;
        tx.execute("DELETE FROM char_lp_logs WHERE character_id = ?1", params![cid])
            .map_err(|e| e.to_string())?;
        df.char_lp_logs.remove(&cid);
    }
    for (&cid, entries) in &snap.char_lp_logs {
        let h = fp_json(entries);
        if df.char_lp_logs.get(&cid) != Some(&h) {
            changed = true;
            let js = serde_json::to_string(entries).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO char_lp_logs (character_id, logs_json) VALUES (?1, ?2)",
                params![cid, js],
            )
            .map_err(|e| e.to_string())?;
            df.char_lp_logs.insert(cid, h);
        }
    }

    Ok(changed)
}

/// Restores `D-6` state from SQLite after a failed write (`try_with_mut_snapshot` rollback).
pub fn rollback_d6(conn: &Connection, snap: &mut WorldStateSnapshot, fp: &TableFingerprints) {
    let df = &fp.d6;
    snap.next_alliance_id = df.next_alliance_id;
    snap.next_corporation_id = df.next_corporation_id;
    snap.next_notification_id = df.next_notification_id;
    snap.next_report_id = df.next_report_id;

    let dirty_notes = snap.owner_notes.len() != df.owner_notes.len()
        || snap
            .owner_notes
            .iter()
            .any(|(k, v)| df.owner_notes.get(k) != Some(&fp_json(v)));
    let dirty_lp = snap.corp_lp_balances != df.corp_lp_balances;
    let dirty_logs = snap.char_lp_logs.len() != df.char_lp_logs.len()
        || snap
            .char_lp_logs
            .iter()
            .any(|(k, v)| df.char_lp_logs.get(k) != Some(&fp_json(v)));
    if !(dirty_notes || dirty_lp || dirty_logs) {
        return;
    }
    let mut fresh = WorldStateSnapshot::default();
    load_d6(conn, &mut fresh);
    if dirty_notes {
        snap.owner_notes = fresh.owner_notes;
    }
    if dirty_lp {
        snap.corp_lp_balances = fresh.corp_lp_balances;
    }
    if dirty_logs {
        snap.char_lp_logs = fresh.char_lp_logs;
    }
}
