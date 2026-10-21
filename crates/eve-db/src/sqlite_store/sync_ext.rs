//! Incremental sync of the per-key extension tables (character jump times, CSPA charges, mail
//! timestamps, login stats, custom names, positions, vulnerability windows, skill history, doll info).
//! Split out of `sync.rs` unchanged (behaviour-neutral) to keep files under the size limit.

use crate::sqlite_store::fingerprints::*;
use crate::types::*;
use rusqlite::{params, Transaction};

/// Returns `true` when any cold table changed (callers then emit a full-snapshot WAL delta).
pub(crate) fn sync_extension_tables(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
) -> Result<bool, String> {
    let mut cold_changed = false;
    // 29. character_last_jump_unix
    let removed_jumps: Vec<i64> = fp
        .character_last_jump_unix
        .keys()
        .filter(|k| !snap.character_last_jump_unix.contains_key(*k))
        .copied()
        .collect();
    for k in removed_jumps {
        cold_changed = true;
        tx.execute("DELETE FROM character_last_jump_unix WHERE character_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.character_last_jump_unix.remove(&k);
    }
    for (&k, &v) in &snap.character_last_jump_unix {
        if fp.character_last_jump_unix.get(&k) != Some(&v) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO character_last_jump_unix (character_id, last_jump_unix) VALUES (?1, ?2)",
                params![k, v as i64],
            )
            .map_err(|e| e.to_string())?;
            fp.character_last_jump_unix.insert(k, v);
        }
    }

    // 30. character_cspa_charges
    let removed_cspa: Vec<i64> = fp
        .character_cspa_charges
        .keys()
        .filter(|k| !snap.character_cspa_charges.contains_key(*k))
        .copied()
        .collect();
    for k in removed_cspa {
        cold_changed = true;
        tx.execute("DELETE FROM character_cspa_charges WHERE character_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.character_cspa_charges.remove(&k);
    }
    for (&k, &cents) in &snap.character_cspa_charges {
        let v = cents.0;
        if fp.character_cspa_charges.get(&k) != Some(&v) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO character_cspa_charges (character_id, cspa_charge_cents) VALUES (?1, ?2)",
                params![k, v],
            )
            .map_err(|e| e.to_string())?;
            fp.character_cspa_charges.insert(k, v);
        }
    }

    // 31. mail_timestamps
    let removed_mail_ts: Vec<i64> = fp
        .mail_timestamps
        .keys()
        .filter(|k| !snap.mail_timestamps.contains_key(*k))
        .copied()
        .collect();
    for k in removed_mail_ts {
        cold_changed = true;
        tx.execute("DELETE FROM mail_timestamps WHERE mail_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.mail_timestamps.remove(&k);
    }
    for (&k, &v) in &snap.mail_timestamps {
        if fp.mail_timestamps.get(&k) != Some(&v) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO mail_timestamps (mail_id, timestamp_unix) VALUES (?1, ?2)",
                params![k, v as i64],
            )
            .map_err(|e| e.to_string())?;
            fp.mail_timestamps.insert(k, v);
        }
    }

    // 32. character_login_stats
    let removed_login_stats: Vec<i64> = fp
        .character_login_stats
        .keys()
        .filter(|k| !snap.character_login_stats.contains_key(*k))
        .copied()
        .collect();
    for k in removed_login_stats {
        cold_changed = true;
        tx.execute("DELETE FROM character_login_stats WHERE character_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.character_login_stats.remove(&k);
    }
    for (&k, v) in &snap.character_login_stats {
        let h = fp_json(v);
        if fp.character_login_stats.get(&k) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO character_login_stats (character_id, last_login_unix, last_logout_unix, logins_count) VALUES (?1, ?2, ?3, ?4)",
                params![k, v.last_login_unix as i64, v.last_logout_unix as i64, v.logins_count as i64],
            )
            .map_err(|e| e.to_string())?;
            fp.character_login_stats.insert(k, h);
        }
    }

    // 33. item_custom_names
    let removed_custom_names: Vec<i64> = fp
        .item_custom_names
        .keys()
        .filter(|k| !snap.item_custom_names.contains_key(*k))
        .copied()
        .collect();
    for k in removed_custom_names {
        cold_changed = true;
        tx.execute("DELETE FROM item_custom_names WHERE item_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.item_custom_names.remove(&k);
    }
    for (&k, v) in &snap.item_custom_names {
        let h = fp_json(v);
        if fp.item_custom_names.get(&k) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO item_custom_names (item_id, custom_name) VALUES (?1, ?2)",
                params![k, v],
            )
            .map_err(|e| e.to_string())?;
            fp.item_custom_names.insert(k, h);
        }
    }

    // 34. item_positions
    let removed_item_pos: Vec<i64> = fp
        .item_positions
        .keys()
        .filter(|k| !snap.item_positions.contains_key(*k))
        .copied()
        .collect();
    for k in removed_item_pos {
        cold_changed = true;
        tx.execute("DELETE FROM item_positions WHERE item_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.item_positions.remove(&k);
    }
    for (&k, v) in &snap.item_positions {
        let h = fp_json(v);
        if fp.item_positions.get(&k) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO item_positions (item_id, x, y, z) VALUES (?1, ?2, ?3, ?4)",
                params![k, v[0], v[1], v[2]],
            )
            .map_err(|e| e.to_string())?;
            fp.item_positions.insert(k, h);
        }
    }

    // 35. structure_vulnerability_windows
    let removed_vuln: Vec<i64> = fp
        .structure_vulnerability_windows
        .keys()
        .filter(|k| !snap.structure_vulnerability_windows.contains_key(*k))
        .copied()
        .collect();
    for k in removed_vuln {
        cold_changed = true;
        tx.execute("DELETE FROM structure_vulnerability_windows WHERE structure_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.structure_vulnerability_windows.remove(&k);
    }
    for (&k, &v) in &snap.structure_vulnerability_windows {
        if fp.structure_vulnerability_windows.get(&k) != Some(&v) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO structure_vulnerability_windows (structure_id, window_start_unix, window_end_unix) VALUES (?1, ?2, ?3)",
                params![k, v.0 as i64, v.1 as i64],
            )
            .map_err(|e| e.to_string())?;
            fp.structure_vulnerability_windows.insert(k, v);
        }
    }

    // 36. skill_history
    let removed_skill_hist: Vec<i64> = fp
        .skill_history
        .keys()
        .filter(|k| !snap.skill_history.contains_key(*k))
        .copied()
        .collect();
    for k in removed_skill_hist {
        cold_changed = true;
        tx.execute("DELETE FROM skill_history WHERE character_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.skill_history.remove(&k);
    }
    for (&k, v) in &snap.skill_history {
        let h = fp_json(v);
        if fp.skill_history.get(&k) != Some(&h) {
            cold_changed = true;
            let js = serde_json::to_string(v).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO skill_history (character_id, history_json) VALUES (?1, ?2)",
                params![k, js],
            )
            .map_err(|e| e.to_string())?;
            fp.skill_history.insert(k, h);
        }
    }

    // 37. character_doll_info
    let removed_doll_info: Vec<i64> = fp
        .character_doll_info
        .keys()
        .filter(|k| !snap.character_doll_info.contains_key(*k))
        .copied()
        .collect();
    for k in removed_doll_info {
        cold_changed = true;
        tx.execute("DELETE FROM character_doll_info WHERE character_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        fp.character_doll_info.remove(&k);
    }
    for (&k, v) in &snap.character_doll_info {
        let h = fp_json(v);
        if fp.character_doll_info.get(&k) != Some(&h) {
            cold_changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO character_doll_info (character_id, doll_dna, portrait_meta) VALUES (?1, ?2, ?3)",
                params![k, v.0, v.1],
            )
            .map_err(|e| e.to_string())?;
            fp.character_doll_info.insert(k, h);
        }
    }
    Ok(cold_changed)
}
