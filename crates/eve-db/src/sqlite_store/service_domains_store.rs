//! SQLite persistence for first-class typed service domain registries (`#588`).

use crate::sqlite_store::fingerprints::{canonical_fp, TableFingerprints};
use crate::types::WorldStateSnapshot;
use rusqlite::{params, Connection, Transaction};
use serde::{de::DeserializeOwned, Serialize};

pub const SERVICE_DOMAINS_SCHEMA_VERSION: i64 = 6;

const SERVICE_DOMAINS_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS service_domain_state (
    domain_key TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL
);
"#;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceDomainsFingerprints {
    pub pve_special_sites: u64,
    pub crab_beacons: u64,
    pub abyssal_state: u64,
    pub dungeon_runtime: u64,
    pub dungeon_editor: u64,
    pub fob_observatory_state: u64,
    pub mining_and_ess_state: u64,
    pub pvp_filament_state: u64,
    pub tourney_state: u64,
    pub deployable_state: u64,
    pub superweapon_runtime: u64,
    pub insurgency_complex_ops: u64,
    pub node_graph_state: u64,
    pub seasons_and_campaigns: u64,
    pub quasar_commerce: u64,
    pub fleet_and_social_ops: u64,
    pub cluster_and_runtime: u64,
}

pub fn apply_service_domains_migration(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(SERVICE_DOMAINS_SCHEMA_SQL)
        .map_err(|e| e.to_string())?;
    let now_unix = crate::clock::host_wall_unix_secs_and_nanos().0 as i64;
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, name, applied_unix) VALUES (?1, ?2, ?3)",
        params![
            SERVICE_DOMAINS_SCHEMA_VERSION,
            "0006_service_domain_state",
            now_unix
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn fp_domain<T: Default + PartialEq + Serialize>(val: &T) -> u64 {
    if *val == T::default() {
        return 0;
    }
    let out = canonical_fp(val);
    if out == 0 {
        1
    } else {
        out
    }
}

fn load_domain_row_checked<T: Default + DeserializeOwned>(
    conn: &Connection,
    key: &str,
    dst: &mut T,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<(), String> {
    let mut stmt = match conn
        .prepare("SELECT rowid, payload_json FROM service_domain_state WHERE domain_key = ?1")
    {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };
    let mut rows = stmt.query(params![key]).map_err(|e| e.to_string())?;
    if let Some(r) = rows.next().map_err(|e| e.to_string())? {
        let rowid = r.get::<_, i64>(0).ok();
        let raw_payload = super::quarantine::row_col_string(r, 1);
        match r
            .get::<_, String>(1)
            .map_err(|e| e.to_string())
            .and_then(|json| serde_json::from_str::<T>(&json).map_err(|e| e.to_string()))
        {
            Ok(parsed) => {
                *dst = parsed;
            }
            Err(err) => {
                *dst = T::default();
                ctx.record_bad_row(
                    "service_domain_state",
                    "service_domain_state (service_domain_blobs)",
                    key,
                    raw_payload,
                    err,
                    rowid,
                )?;
            }
        }
    } else {
        *dst = T::default();
    }
    Ok(())
}

fn rollback_domain_row<T: Default + DeserializeOwned>(
    conn: &Connection,
    key: &str,
    expected_fp: u64,
    dst: &mut T,
) {
    match conn.query_row(
        "SELECT payload_json FROM service_domain_state WHERE domain_key = ?1",
        params![key],
        |r| r.get::<_, String>(0),
    ) {
        Ok(json) => {
            if let Ok(parsed) = serde_json::from_str::<T>(&json) {
                *dst = parsed;
            }
        }
        Err(rusqlite::Error::QueryReturnedNoRows) if expected_fp == 0 => {
            // Committed baseline was `T::default()` (stored as an omitted row); revert dirty mutations.
            *dst = T::default();
        }
        Err(_) => {}
    }
}

fn sync_domain_row<T: Default + PartialEq + Serialize>(
    tx: &Transaction,
    key: &str,
    val: &T,
    old_fp: &mut u64,
) -> Result<bool, String> {
    let cur_fp = fp_domain(val);
    if cur_fp == *old_fp {
        return Ok(false);
    }
    if *val == T::default() {
        tx.execute(
            "DELETE FROM service_domain_state WHERE domain_key = ?1",
            params![key],
        )
        .map_err(|e| e.to_string())?;
    } else {
        let json = super::fingerprints::to_checked_json_string(val)?;
        tx.execute(
            "INSERT OR REPLACE INTO service_domain_state (domain_key, payload_json) VALUES (?1, ?2)",
            params![key, json],
        )
        .map_err(|e| e.to_string())?;
    }
    *old_fp = cur_fp;
    Ok(true)
}

macro_rules! for_each_service_domain {
    ($cb:ident) => {
        $cb!(pve_special_sites, "pve_special_sites");
        $cb!(crab_beacons, "crab_beacons");
        $cb!(abyssal_state, "abyssal_state");
        $cb!(dungeon_runtime, "dungeon_runtime");
        $cb!(dungeon_editor, "dungeon_editor");
        $cb!(fob_observatory_state, "fob_observatory_state");
        $cb!(mining_and_ess_state, "mining_and_ess_state");
        $cb!(pvp_filament_state, "pvp_filament_state");
        $cb!(tourney_state, "tourney_state");
        $cb!(deployable_state, "deployable_state");
        $cb!(superweapon_runtime, "superweapon_runtime");
        $cb!(insurgency_complex_ops, "insurgency_complex_ops");
        $cb!(node_graph_state, "node_graph_state");
        $cb!(seasons_and_campaigns, "seasons_and_campaigns");
        $cb!(quasar_commerce, "quasar_commerce");
        $cb!(fleet_and_social_ops, "fleet_and_social_ops");
        $cb!(cluster_and_runtime, "cluster_and_runtime");
    };
}

pub fn compute_service_domains_fingerprints(
    snap: &WorldStateSnapshot,
) -> ServiceDomainsFingerprints {
    let mut fp = ServiceDomainsFingerprints::default();
    macro_rules! compute_one {
        ($field:ident, $key:expr) => {
            fp.$field = fp_domain(&snap.$field);
        };
    }
    for_each_service_domain!(compute_one);
    fp
}

pub fn load_service_domains_checked(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<(), String> {
    let mut known_keys = std::collections::HashSet::with_capacity(20);
    macro_rules! load_one {
        ($field:ident, $key:expr) => {
            known_keys.insert($key);
            load_domain_row_checked(conn, $key, &mut snap.$field, ctx)?;
        };
    }
    for_each_service_domain!(load_one);

    // Validate any extra rows in `service_domain_state` or alias table `service_domain_blobs`.
    if let Ok(mut stmt) =
        conn.prepare("SELECT rowid, domain_key, payload_json FROM service_domain_state")
    {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = super::quarantine::row_col_string(r, 1);
                if known_keys.contains(key.as_str()) {
                    continue;
                }
                let raw_payload = super::quarantine::row_col_string(r, 2);
                if let Err(err) = r
                    .get::<_, String>(2)
                    .map_err(|e| e.to_string())
                    .and_then(|s| {
                        serde_json::from_str::<serde_json::Value>(&s).map_err(|e| e.to_string())
                    })
                {
                    ctx.record_bad_row(
                        "service_domain_state",
                        "service_domain_state (service_domain_blobs)",
                        key,
                        raw_payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    if let Ok(mut stmt) = conn.prepare(
        "SELECT rowid, domain_key, COALESCE(payload_json, domain_json) FROM service_domain_blobs",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = super::quarantine::row_col_string(r, 1);
                let raw_payload = super::quarantine::row_col_string(r, 2);
                if let Err(err) = r
                    .get::<_, String>(2)
                    .map_err(|e| e.to_string())
                    .and_then(|s| {
                        serde_json::from_str::<serde_json::Value>(&s).map_err(|e| e.to_string())
                    })
                {
                    ctx.record_bad_row(
                        "service_domain_blobs",
                        "service_domain_blobs (service_domain_state)",
                        key,
                        raw_payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    Ok(())
}

pub fn load_service_domains(conn: &Connection, snap: &mut WorldStateSnapshot) {
    let _ = load_service_domains_checked(
        conn,
        snap,
        &mut super::quarantine::LoadContext::quarantine_in_memory(),
    );
}

pub fn sync_service_domains(
    tx: &Transaction,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
) -> Result<bool, String> {
    let mut changed = false;
    macro_rules! sync_one {
        ($field:ident, $key:expr) => {
            changed |= sync_domain_row(
                tx,
                $key,
                &snap.$field,
                &mut fp.service_domains.$field,
            )?;
        };
    }
    for_each_service_domain!(sync_one);
    Ok(changed)
}

pub fn rollback_service_domains(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    fp: &TableFingerprints,
) {
    macro_rules! rollback_one {
        ($field:ident, $key:expr) => {
            if fp_domain(&snap.$field) != fp.service_domains.$field {
                rollback_domain_row(conn, $key, fp.service_domains.$field, &mut snap.$field);
            }
        };
    }
    for_each_service_domain!(rollback_one);
}
