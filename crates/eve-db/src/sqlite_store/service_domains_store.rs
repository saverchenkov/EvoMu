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

fn load_domain_row<T: Default + DeserializeOwned>(conn: &Connection, key: &str, dst: &mut T) {
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
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            *dst = T::default();
        }
        Err(_) => {}
    }
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
        let json = serde_json::to_string(val).map_err(|e| e.to_string())?;
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

pub fn load_service_domains(conn: &Connection, snap: &mut WorldStateSnapshot) {
    macro_rules! load_one {
        ($field:ident, $key:expr) => {
            load_domain_row(conn, $key, &mut snap.$field);
        };
    }
    for_each_service_domain!(load_one);
}

pub fn sync_service_domains(
    tx: &Transaction,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
) -> Result<bool, String> {
    crate::service_domains::validate_service_state_blobs(&snap.service_state_blobs)?;
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
