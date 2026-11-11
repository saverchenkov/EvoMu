//! SQLite persistence for `D-6` Batch 4 (`feat:lifecycle.industry-pi-durable`,
//! `feat:lifecycle.market-wallet-history-durable`, `feat:lifecycle.pve-progress-durable`,
//! `feat:lifecycle.abyssal-restart`, `feat:lifecycle.ship-and-item-config-durable`,
//! `feat:lifecycle.account-progress-durable`).

use crate::sqlite_store::fingerprints::{fp_json, FnvHasher, TableFingerprints};
use crate::types::WorldStateSnapshot;
use rusqlite::{params, Connection, Transaction};
use serde::{de::DeserializeOwned, Serialize};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

pub const D6_BATCH4_SCHEMA_VERSION: i64 = 5;

const D6_BATCH4_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS world_kv_blobs (
    blob_key TEXT PRIMARY KEY,
    blob_json TEXT NOT NULL
);
"#;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct D6Batch4Fingerprints {
    pub blobs: HashMap<&'static str, u64>,
    pub mining_ledger: u64,
}

pub fn apply_d6_batch4_migration(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(D6_BATCH4_SCHEMA_SQL)
        .map_err(|e| e.to_string())?;
    let now_unix = crate::clock::host_wall_unix_secs_and_nanos().0 as i64;
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, name, applied_unix) VALUES (?1, ?2, ?3)",
        params![
            D6_BATCH4_SCHEMA_VERSION,
            "0005_d6_industry_market_pve_ship_account",
            now_unix
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn fp_str_map(map: &HashMap<String, String>) -> u64 {
    use std::hash::Hasher;
    let mut xor = 0u64;
    let mut sum = 0u64;
    for (k, v) in map {
        let mut eh = std::collections::hash_map::DefaultHasher::new();
        eh.write_usize(k.len());
        eh.write(k.as_bytes());
        eh.write_usize(v.len());
        eh.write(v.as_bytes());
        let entry_fp = eh.finish();
        xor ^= entry_fp;
        sum = sum.wrapping_add(entry_fp.rotate_left(13));
    }
    let mut out = FnvHasher::new();
    out.write_u64(map.len() as u64);
    out.write_u64(xor);
    out.write_u64(sum);
    out.finish()
}

fn fp_map<K: Serialize, V: Serialize>(map: &HashMap<K, V>) -> u64 {
    super::fingerprints::canonical_fp(map)
}

fn fp_set<T: Serialize>(set: &HashSet<T>) -> u64 {
    let mut xor = 0u64;
    let mut sum = 0u64;
    for v in set {
        let eh = fp_json(v);
        xor ^= eh;
        sum = sum.wrapping_add(eh.rotate_left(13));
    }
    let mut out = FnvHasher::new();
    out.write_u64(set.len() as u64);
    out.write_u64(xor);
    out.write_u64(sum);
    out.finish()
}

fn fp_nested_map<K1: Serialize, K2: Serialize, V: Serialize>(
    map: &HashMap<K1, HashMap<K2, V>>,
) -> u64 {
    super::fingerprints::canonical_fp(map)
}

fn map_to_json<K: Serialize, V: Serialize>(map: &HashMap<K, V>) -> Result<String, String> {
    let entries: Vec<(&K, &V)> = map.iter().collect();
    super::fingerprints::to_checked_json_string(&entries)
}

fn map_from_json<K: DeserializeOwned + Eq + Hash, V: DeserializeOwned>(
    s: &str,
) -> Result<HashMap<K, V>, String> {
    let entries: Vec<(K, V)> = serde_json::from_str(s).map_err(|e| e.to_string())?;
    Ok(entries.into_iter().collect())
}

fn nested_map_to_json<K1: Serialize + Copy, K2: Serialize + Copy, V: Serialize>(
    map: &HashMap<K1, HashMap<K2, V>>,
) -> Result<String, String> {
    let entries: Vec<(K1, Vec<(K2, &V)>)> = map
        .iter()
        .map(|(&k1, inner)| (k1, inner.iter().map(|(&k2, v)| (k2, v)).collect()))
        .collect();
    super::fingerprints::to_checked_json_string(&entries)
}

fn nested_map_from_json<
    K1: DeserializeOwned + Eq + Hash,
    K2: DeserializeOwned + Eq + Hash,
    V: DeserializeOwned,
>(
    s: &str,
) -> Result<HashMap<K1, HashMap<K2, V>>, String> {
    let entries: Vec<(K1, Vec<(K2, V)>)> = serde_json::from_str(s).map_err(|e| e.to_string())?;
    Ok(
        entries
            .into_iter()
            .map(|(k1, inner)| (k1, inner.into_iter().collect()))
            .collect(),
    )
}

fn compute_blob_hashes(snap: &WorldStateSnapshot) -> HashMap<&'static str, u64> {
    let mut m = HashMap::with_capacity(24);
    m.insert("moon_extractions", fp_map(&snap.moon_extractions));
    m.insert("auto_moon_miners", fp_map(&snap.auto_moon_miners));
    m.insert("observer_mining_ledger", fp_map(&snap.observer_mining_ledger));
    m.insert("char_research_agents", fp_map(&snap.char_research_agents));
    m.insert("planetary_colony_layouts", fp_map(&snap.planetary_colony_layouts));
    m.insert("corp_customs_offices", fp_map(&snap.corp_customs_offices));
    m.insert("orbital_provision_types", fp_map(&snap.orbital_provision_types));
    m.insert("wallet_journal_timestamps", fp_map(&snap.wallet_journal_timestamps));
    m.insert("char_wallet_transactions", fp_map(&snap.char_wallet_transactions));
    m.insert("corp_wallet_journals", fp_map(&snap.corp_wallet_journals));
    m.insert("corp_wallet_transactions", fp_map(&snap.corp_wallet_transactions));
    m.insert("market_order_meta", fp_map(&snap.market_order_meta));
    m.insert("market_history", fp_map(&snap.market_history));
    m.insert("cosmic_signatures", fp_map(&snap.cosmic_signatures));
    m.insert("cosmic_anomalies", fp_map(&snap.cosmic_anomalies));
    m.insert("incursions", fp_map(&snap.incursions));
    m.insert("last_mission_decline_times", fp_map(&snap.last_mission_decline_times));
    m.insert("wh_polarizations", fp_map(&snap.wh_polarizations));
    m.insert("module_online_states", fp_map(&snap.module_online_states));
    m.insert("module_active_states", fp_set(&snap.module_active_states));
    m.insert("module_active_charges", fp_map(&snap.module_active_charges));
    m.insert("weapon_banks", fp_nested_map(&snap.weapon_banks));
    m.insert("paused_skill_queues", fp_set(&snap.paused_skill_queues));
    m.insert("starbase_settings", fp_map(&snap.starbase_settings));
    m.insert("corp_container_logs", fp_map(&snap.corp_container_logs));
    m.insert("killmail_details", fp_map(&snap.killmail_details));
    m.insert("station_office_rental_costs", fp_map(&snap.station_office_rental_costs));
    m.insert("structure_esi_timer_meta", fp_map(&snap.structure_esi_timer_meta));
    m.insert("corp_project_contributors", fp_map(&snap.corp_project_contributors));
    m.insert("loyalty_store_offers", fp_map(&snap.loyalty_store_offers));
    m.insert("system_jump_events", fp_json(&snap.system_jump_events));
    m.insert("system_kill_events", fp_json(&snap.system_kill_events));
    m.insert("quasar_state_blobs", fp_str_map(&snap.quasar_state_blobs));
    m.insert("service_state_blobs", fp_str_map(&snap.service_state_blobs));
    m.insert("insurance_contracts", fp_map(&snap.insurance_contracts));
    m
}

fn serialize_blob(snap: &WorldStateSnapshot, key: &str) -> Result<String, String> {
    match key {
        "moon_extractions" => map_to_json(&snap.moon_extractions),
        "auto_moon_miners" => map_to_json(&snap.auto_moon_miners),
        "observer_mining_ledger" => map_to_json(&snap.observer_mining_ledger),
        "char_research_agents" => map_to_json(&snap.char_research_agents),
        "planetary_colony_layouts" => map_to_json(&snap.planetary_colony_layouts),
        "corp_customs_offices" => map_to_json(&snap.corp_customs_offices),
        "orbital_provision_types" => map_to_json(&snap.orbital_provision_types),
        "wallet_journal_timestamps" => map_to_json(&snap.wallet_journal_timestamps),
        "char_wallet_transactions" => map_to_json(&snap.char_wallet_transactions),
        "corp_wallet_journals" => map_to_json(&snap.corp_wallet_journals),
        "corp_wallet_transactions" => map_to_json(&snap.corp_wallet_transactions),
        "market_order_meta" => map_to_json(&snap.market_order_meta),
        "market_history" => map_to_json(&snap.market_history),
        "cosmic_signatures" => map_to_json(&snap.cosmic_signatures),
        "cosmic_anomalies" => map_to_json(&snap.cosmic_anomalies),
        "incursions" => map_to_json(&snap.incursions),
        "last_mission_decline_times" => map_to_json(&snap.last_mission_decline_times),
        "wh_polarizations" => map_to_json(&snap.wh_polarizations),
        "module_online_states" => map_to_json(&snap.module_online_states),
        "module_active_states" => {
            let mut sorted: Vec<i64> = snap.module_active_states.iter().copied().collect();
            sorted.sort_unstable();
            super::fingerprints::to_checked_json_string(&sorted)
        }
        "module_active_charges" => map_to_json(&snap.module_active_charges),
        "weapon_banks" => nested_map_to_json(&snap.weapon_banks),
        "paused_skill_queues" => {
            let mut sorted: Vec<i64> = snap.paused_skill_queues.iter().copied().collect();
            sorted.sort_unstable();
            super::fingerprints::to_checked_json_string(&sorted)
        }
        "starbase_settings" => map_to_json(&snap.starbase_settings),
        "corp_container_logs" => map_to_json(&snap.corp_container_logs),
        "killmail_details" => map_to_json(&snap.killmail_details),
        "station_office_rental_costs" => map_to_json(&snap.station_office_rental_costs),
        "structure_esi_timer_meta" => map_to_json(&snap.structure_esi_timer_meta),
        "corp_project_contributors" => map_to_json(&snap.corp_project_contributors),
        "loyalty_store_offers" => map_to_json(&snap.loyalty_store_offers),
        "system_jump_events" => {
            super::fingerprints::to_checked_json_string(&snap.system_jump_events)
        }
        "system_kill_events" => {
            super::fingerprints::to_checked_json_string(&snap.system_kill_events)
        }
        "quasar_state_blobs" => map_to_json(&snap.quasar_state_blobs),
        "service_state_blobs" => map_to_json(&snap.service_state_blobs),
        "insurance_contracts" => map_to_json(&snap.insurance_contracts),
        _ => Ok("[]".into()),
    }
}

fn apply_blob_json_checked(snap: &mut WorldStateSnapshot, key: &str, js: &str) -> Result<(), String> {
    match key {
        "moon_extractions" => snap.moon_extractions = map_from_json(js)?,
        "auto_moon_miners" => snap.auto_moon_miners = map_from_json(js)?,
        "observer_mining_ledger" => snap.observer_mining_ledger = map_from_json(js)?,
        "char_research_agents" => snap.char_research_agents = map_from_json(js)?,
        "planetary_colony_layouts" => snap.planetary_colony_layouts = map_from_json(js)?,
        "corp_customs_offices" => snap.corp_customs_offices = map_from_json(js)?,
        "orbital_provision_types" => snap.orbital_provision_types = map_from_json(js)?,
        "wallet_journal_timestamps" => snap.wallet_journal_timestamps = map_from_json(js)?,
        "char_wallet_transactions" => snap.char_wallet_transactions = map_from_json(js)?,
        "corp_wallet_journals" => snap.corp_wallet_journals = map_from_json(js)?,
        "corp_wallet_transactions" => snap.corp_wallet_transactions = map_from_json(js)?,
        "market_order_meta" => snap.market_order_meta = map_from_json(js)?,
        "market_history" => snap.market_history = map_from_json(js)?,
        "cosmic_signatures" => snap.cosmic_signatures = map_from_json(js)?,
        "cosmic_anomalies" => snap.cosmic_anomalies = map_from_json(js)?,
        "incursions" => snap.incursions = map_from_json(js)?,
        "last_mission_decline_times" => snap.last_mission_decline_times = map_from_json(js)?,
        "wh_polarizations" => snap.wh_polarizations = map_from_json(js)?,
        "module_online_states" => snap.module_online_states = map_from_json(js)?,
        "module_active_states" => {
            snap.module_active_states = serde_json::from_str(js).map_err(|e| e.to_string())?;
        }
        "module_active_charges" => snap.module_active_charges = map_from_json(js)?,
        "weapon_banks" => snap.weapon_banks = nested_map_from_json(js)?,
        "paused_skill_queues" => {
            snap.paused_skill_queues = serde_json::from_str(js).map_err(|e| e.to_string())?;
        }
        "starbase_settings" => snap.starbase_settings = map_from_json(js)?,
        "corp_container_logs" => snap.corp_container_logs = map_from_json(js)?,
        "killmail_details" => snap.killmail_details = map_from_json(js)?,
        "station_office_rental_costs" => snap.station_office_rental_costs = map_from_json(js)?,
        "structure_esi_timer_meta" => snap.structure_esi_timer_meta = map_from_json(js)?,
        "corp_project_contributors" => snap.corp_project_contributors = map_from_json(js)?,
        "loyalty_store_offers" => snap.loyalty_store_offers = map_from_json(js)?,
        "system_jump_events" => {
            snap.system_jump_events = serde_json::from_str(js).map_err(|e| e.to_string())?;
        }
        "system_kill_events" => {
            snap.system_kill_events = serde_json::from_str(js).map_err(|e| e.to_string())?;
        }
        "quasar_state_blobs" => snap.quasar_state_blobs = map_from_json(js)?,
        "service_state_blobs" => snap.service_state_blobs = map_from_json(js)?,
        "insurance_contracts" => snap.insurance_contracts = map_from_json(js)?,
        _ => {}
    }
    Ok(())
}

pub fn compute_d6_batch4_fingerprints(snap: &WorldStateSnapshot) -> D6Batch4Fingerprints {
    D6Batch4Fingerprints {
        blobs: compute_blob_hashes(snap),
        mining_ledger: fp_json(&snap.mining_ledger),
    }
}

pub fn load_d6_batch4_checked(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<(), String> {
    if let Ok(mut stmt) =
        conn.prepare("SELECT rowid, blob_key, blob_json FROM world_kv_blobs")
    {
        if let Ok(mut rows) = stmt.query([]) {
            while let Some(r) = rows.next().map_err(|e| e.to_string())? {
                let rowid = r.get::<_, i64>(0).ok();
                let key = super::quarantine::row_col_string(r, 1);
                let payload = super::quarantine::row_col_string(r, 2);
                match r
                    .get::<_, String>(2)
                    .map_err(|e| e.to_string())
                    .and_then(|js| apply_blob_json_checked(snap, &key, &js))
                {
                    Ok(()) => {}
                    Err(err) => {
                        ctx.record_bad_row(
                            "world_kv_blobs",
                            "world_kv_blobs",
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
    Ok(())
}

pub fn load_d6_batch4(conn: &Connection, snap: &mut WorldStateSnapshot) {
    let _ = load_d6_batch4_checked(
        conn,
        snap,
        &mut super::quarantine::LoadContext::quarantine_in_memory(),
    );
}

pub fn sync_d6_batch4(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
) -> Result<bool, String> {
    let mut changed = false;
    let new_hashes = compute_blob_hashes(snap);
    for (key, new_h) in new_hashes {
        if fp.d6_batch4.blobs.get(key) != Some(&new_h) {
            if key == "service_state_blobs" {
                if let Err(msg) =
                    crate::service_domains::validate_service_state_blobs(&snap.service_state_blobs)
                {
                    crate::service_domains::record_service_state_blob_limit_violation(&msg);
                    continue;
                }
            }
            changed = true;
            let js = serialize_blob(snap, key)?;
            tx.execute(
                "INSERT OR REPLACE INTO world_kv_blobs (blob_key, blob_json) VALUES (?1, ?2)",
                params![key, js],
            )
            .map_err(|e| e.to_string())?;
            fp.d6_batch4.blobs.insert(key, new_h);
        }
    }

    let ml_h = fp_json(&snap.mining_ledger);
    if fp.d6_batch4.mining_ledger != ml_h {
        changed = true;
        tx.execute("DELETE FROM char_mining_ledger", [])
            .map_err(|e| e.to_string())?;
        for entry in &snap.mining_ledger {
            tx.execute(
                "INSERT INTO char_mining_ledger (character_id, solar_system_id, type_id, quantity, date, timestamp_unix) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    entry.character_id,
                    entry.solar_system_id as i64,
                    entry.type_id as i64,
                    entry.quantity,
                    entry.date,
                    entry.timestamp_unix as i64,
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        fp.d6_batch4.mining_ledger = ml_h;
    }

    Ok(changed)
}

pub fn rollback_d6_batch4(conn: &Connection, snap: &mut WorldStateSnapshot, fp: &TableFingerprints) {
    let cur = compute_d6_batch4_fingerprints(snap);
    if cur == fp.d6_batch4 {
        return;
    }
    let mut fresh = WorldStateSnapshot::default();
    load_d6_batch4(conn, &mut fresh);
    if let Ok(mut stmt) = conn.prepare(
        "SELECT character_id, solar_system_id, type_id, quantity, date, timestamp_unix FROM char_mining_ledger ORDER BY id ASC",
    ) {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(crate::types::MiningLedgerRecord {
                character_id: r.get(0)?,
                solar_system_id: r.get::<_, i64>(1)? as u32,
                type_id: r.get::<_, i64>(2)? as u32,
                quantity: r.get(3)?,
                date: r.get(4)?,
                timestamp_unix: r.get::<_, i64>(5)? as u64,
            })
        }) {
            for r in rows.flatten() {
                fresh.mining_ledger.push(r);
            }
        }
    }
    snap.moon_extractions = fresh.moon_extractions;
    snap.auto_moon_miners = fresh.auto_moon_miners;
    snap.observer_mining_ledger = fresh.observer_mining_ledger;
    snap.mining_ledger = fresh.mining_ledger;
    snap.char_research_agents = fresh.char_research_agents;
    snap.planetary_colony_layouts = fresh.planetary_colony_layouts;
    snap.corp_customs_offices = fresh.corp_customs_offices;
    snap.orbital_provision_types = fresh.orbital_provision_types;
    snap.wallet_journal_timestamps = fresh.wallet_journal_timestamps;
    snap.char_wallet_transactions = fresh.char_wallet_transactions;
    snap.corp_wallet_journals = fresh.corp_wallet_journals;
    snap.corp_wallet_transactions = fresh.corp_wallet_transactions;
    snap.market_order_meta = fresh.market_order_meta;
    snap.market_history = fresh.market_history;
    snap.cosmic_signatures = fresh.cosmic_signatures;
    snap.cosmic_anomalies = fresh.cosmic_anomalies;
    snap.incursions = fresh.incursions;
    snap.last_mission_decline_times = fresh.last_mission_decline_times;
    snap.wh_polarizations = fresh.wh_polarizations;
    snap.module_online_states = fresh.module_online_states;
    snap.module_active_states = fresh.module_active_states;
    snap.module_active_charges = fresh.module_active_charges;
    snap.weapon_banks = fresh.weapon_banks;
    snap.paused_skill_queues = fresh.paused_skill_queues;
    snap.starbase_settings = fresh.starbase_settings;
    snap.corp_container_logs = fresh.corp_container_logs;
    snap.killmail_details = fresh.killmail_details;
    snap.station_office_rental_costs = fresh.station_office_rental_costs;
    snap.structure_esi_timer_meta = fresh.structure_esi_timer_meta;
    snap.corp_project_contributors = fresh.corp_project_contributors;
    snap.loyalty_store_offers = fresh.loyalty_store_offers;
    snap.system_jump_events = fresh.system_jump_events;
    snap.system_kill_events = fresh.system_kill_events;
    snap.quasar_state_blobs = fresh.quasar_state_blobs;
    snap.service_state_blobs = fresh.service_state_blobs;
    snap.insurance_contracts = fresh.insurance_contracts;
}
