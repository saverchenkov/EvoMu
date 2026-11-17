//! Incremental sync of the per-key extension tables (character jump times, CSPA charges, mail
//! timestamps, login stats, custom names, positions, vulnerability windows, skill history, doll info).

use crate::cow::{CowCell, CowDiffItem, CowMap};
use crate::sqlite_store::checked_json::to_checked_json_string;
use crate::types::*;
use rusqlite::{params, Transaction};
use std::collections::HashMap;

fn sync_cell_i64_map<V: PartialEq>(
    tx: &Transaction<'_>,
    before: Option<&CowCell<HashMap<i64, V>>>,
    after: &CowCell<HashMap<i64, V>>,
    delete_sql: &str,
    mut upsert_fn: impl FnMut(&Transaction<'_>, i64, &V) -> Result<(), String>,
) -> Result<bool, String> {
    if let Some(b) = before {
        if b.ptr_eq(after) {
            return Ok(false);
        }
        let mut changed = false;
        for &k in b.keys() {
            if !after.contains_key(&k) {
                changed = true;
                tx.execute(delete_sql, params![k])
                    .map_err(|e| e.to_string())?;
            }
        }
        for (&k, v) in after.iter() {
            if b.get(&k) != Some(v) {
                changed = true;
                upsert_fn(tx, k, v)?;
            }
        }
        return Ok(changed);
    }
    let mut changed = false;
    for (&k, v) in after.iter() {
        changed = true;
        upsert_fn(tx, k, v)?;
    }
    Ok(changed)
}

fn sync_cow_i64_map<V: Clone + PartialEq>(
    tx: &Transaction<'_>,
    before: Option<&CowMap<i64, V>>,
    after: &CowMap<i64, V>,
    delete_sql: &str,
    mut upsert_fn: impl FnMut(&Transaction<'_>, i64, &V) -> Result<(), String>,
) -> Result<bool, String> {
    if let Some(b) = before {
        if b.ptr_eq(after) {
            return Ok(false);
        }
        let mut changed = false;
        for diff in b.diff(after) {
            match diff {
                CowDiffItem::Add(k, v) | CowDiffItem::Update { new: (k, v), .. } => {
                    changed = true;
                    upsert_fn(tx, *k, v)?;
                }
                CowDiffItem::Remove(k, _) => {
                    changed = true;
                    tx.execute(delete_sql, params![*k])
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        return Ok(changed);
    }
    let mut changed = false;
    for (&k, v) in after {
        changed = true;
        upsert_fn(tx, k, v)?;
    }
    Ok(changed)
}

/// Syncs all 9 extension tables (including `mail_timestamps` and `item_custom_names`),
/// returning `true` if any row changed.
#[cfg(test)]
pub(crate) fn sync_extension_tables(
    tx: &Transaction<'_>,
    before: Option<&WorldStateSnapshot>,
    snap: &WorldStateSnapshot,
) -> Result<bool, String> {
    sync_extension_tables_inner(tx, before, snap, true)
}

/// Syncs the 7 cold extension tables; `mail_timestamps` and `item_custom_names` are synced via
/// `tx_engine` `ChangeSet`s when called from `sync_selected_cold_domains_to_sqlite`.
pub(crate) fn sync_extension_tables_cold_only(
    tx: &Transaction<'_>,
    before: Option<&WorldStateSnapshot>,
    snap: &WorldStateSnapshot,
) -> Result<bool, String> {
    sync_extension_tables_inner(tx, before, snap, false)
}

fn sync_extension_tables_inner(
    tx: &Transaction<'_>,
    before: Option<&WorldStateSnapshot>,
    snap: &WorldStateSnapshot,
    include_cow_maps: bool,
) -> Result<bool, String> {
    let mut cold_changed = false;

    // 29. character_last_jump_unix
    cold_changed |= sync_cell_i64_map(
        tx,
        before.map(|b| &b.character_last_jump_unix),
        &snap.character_last_jump_unix,
        "DELETE FROM character_last_jump_unix WHERE character_id = ?1",
        |tx, k, &v| {
            tx.execute(
                "INSERT OR REPLACE INTO character_last_jump_unix (character_id, last_jump_unix) VALUES (?1, ?2)",
                params![k, v as i64],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 30. character_cspa_charges
    cold_changed |= sync_cell_i64_map(
        tx,
        before.map(|b| &b.character_cspa_charges),
        &snap.character_cspa_charges,
        "DELETE FROM character_cspa_charges WHERE character_id = ?1",
        |tx, k, &cents| {
            tx.execute(
                "INSERT OR REPLACE INTO character_cspa_charges (character_id, cspa_charge_cents) VALUES (?1, ?2)",
                params![k, cents.0],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    if include_cow_maps {
        // 31. mail_timestamps
        cold_changed |= sync_cow_i64_map(
            tx,
            before.map(|b| &b.mail_timestamps),
            &snap.mail_timestamps,
            "DELETE FROM mail_timestamps WHERE mail_id = ?1",
            |tx, k, &v| {
                tx.execute(
                    "INSERT OR REPLACE INTO mail_timestamps (mail_id, timestamp_unix) VALUES (?1, ?2)",
                    params![k, v as i64],
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
            },
        )?;
    }

    // 32. character_login_stats
    cold_changed |= sync_cell_i64_map(
        tx,
        before.map(|b| &b.character_login_stats),
        &snap.character_login_stats,
        "DELETE FROM character_login_stats WHERE character_id = ?1",
        |tx, k, v| {
            tx.execute(
                "INSERT OR REPLACE INTO character_login_stats (character_id, last_login_unix, last_logout_unix, logins_count) VALUES (?1, ?2, ?3, ?4)",
                params![k, v.last_login_unix as i64, v.last_logout_unix as i64, v.logins_count as i64],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    if include_cow_maps {
        // 33. item_custom_names
        cold_changed |= sync_cow_i64_map(
            tx,
            before.map(|b| &b.item_custom_names),
            &snap.item_custom_names,
            "DELETE FROM item_custom_names WHERE item_id = ?1",
            |tx, k, v| {
                tx.execute(
                    "INSERT OR REPLACE INTO item_custom_names (item_id, custom_name) VALUES (?1, ?2)",
                    params![k, v],
                )
                .map(|_| ())
                .map_err(|e| e.to_string())
            },
        )?;
    }

    // 34. item_positions
    cold_changed |= sync_cell_i64_map(
        tx,
        before.map(|b| &b.item_positions),
        &snap.item_positions,
        "DELETE FROM item_positions WHERE item_id = ?1",
        |tx, k, v| {
            if !v[0].is_finite() || !v[1].is_finite() || !v[2].is_finite() {
                return Err(format!("non-finite float in item_positions[{k}]: {v:?}"));
            }
            tx.execute(
                "INSERT OR REPLACE INTO item_positions (item_id, x, y, z) VALUES (?1, ?2, ?3, ?4)",
                params![k, v[0], v[1], v[2]],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 35. structure_vulnerability_windows
    cold_changed |= sync_cell_i64_map(
        tx,
        before.map(|b| &b.structure_vulnerability_windows),
        &snap.structure_vulnerability_windows,
        "DELETE FROM structure_vulnerability_windows WHERE structure_id = ?1",
        |tx, k, &v| {
            tx.execute(
                "INSERT OR REPLACE INTO structure_vulnerability_windows (structure_id, window_start_unix, window_end_unix) VALUES (?1, ?2, ?3)",
                params![k, v.0 as i64, v.1 as i64],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 36. skill_history
    cold_changed |= sync_cell_i64_map(
        tx,
        before.map(|b| &b.skill_history),
        &snap.skill_history,
        "DELETE FROM skill_history WHERE character_id = ?1",
        |tx, k, v| {
            let js = to_checked_json_string(v)?;
            tx.execute(
                "INSERT OR REPLACE INTO skill_history (character_id, history_json) VALUES (?1, ?2)",
                params![k, js],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    // 37. character_doll_info
    cold_changed |= sync_cell_i64_map(
        tx,
        before.map(|b| &b.character_doll_info),
        &snap.character_doll_info,
        "DELETE FROM character_doll_info WHERE character_id = ?1",
        |tx, k, v| {
            tx.execute(
                "INSERT OR REPLACE INTO character_doll_info (character_id, doll_dna, portrait_meta) VALUES (?1, ?2, ?3)",
                params![k, v.0, v.1],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
        },
    )?;

    Ok(cold_changed)
}
