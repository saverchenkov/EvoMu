//! SQLite persistence for the wallet ledger extensions (`crate::ledger`): journal metadata,
//! market transactions and the account-wide PLEX Vault. Tables are created idempotently by
//! [`apply_ledger_migration`] (schema migration `0002_wallet_ledger`), so existing world DBs gain
//! them on next open; legacy per-character PLEX balances are folded into account vaults on load.

use crate::ledger::{JournalMeta, MarketTransactionRecord};
use crate::sqlite_store::fingerprints::{fp_json, TableFingerprints};
use crate::types::WorldStateSnapshot;
use rusqlite::{params, Connection, Transaction};
use std::collections::HashMap;

pub const LEDGER_SCHEMA_VERSION: i64 = 2;

const LEDGER_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS wallet_journal_meta (
    entry_id INTEGER PRIMARY KEY,
    meta_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS market_transactions (
    transaction_id INTEGER PRIMARY KEY,
    tx_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS account_plex_vault (
    user_id INTEGER PRIMARY KEY,
    plex INTEGER NOT NULL
);
"#;

/// Fingerprints of the ledger tables (row hashes keyed by primary key).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LedgerFingerprints {
    pub journal_meta: HashMap<i64, u64>,
    pub market_transactions: HashMap<i64, u64>,
    pub account_plex_vault: HashMap<u64, i64>,
}

pub fn apply_ledger_migration(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(LEDGER_SCHEMA_SQL).map_err(|e| e.to_string())?;
    let now_unix = crate::clock::host_wall_unix_secs_and_nanos().0 as i64;
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, name, applied_unix) VALUES (?1, ?2, ?3)",
        params![LEDGER_SCHEMA_VERSION, "0002_wallet_ledger", now_unix],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn compute_ledger_fingerprints(snap: &WorldStateSnapshot) -> LedgerFingerprints {
    LedgerFingerprints {
        journal_meta: snap.wallet_journal_meta.iter().map(|(k, v)| (*k, fp_json(v))).collect(),
        market_transactions: snap
            .market_transactions
            .iter()
            .map(|t| (t.transaction_id, fp_json(t)))
            .collect(),
        account_plex_vault: snap.account_plex_vault.clone(),
    }
}

pub fn load_ledger(conn: &Connection, snap: &mut WorldStateSnapshot) {
    if let Ok(mut stmt) = conn.prepare("SELECT entry_id, meta_json FROM wallet_journal_meta") {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
            for (id, js) in rows.flatten() {
                if let Ok(m) = serde_json::from_str::<JournalMeta>(&js) {
                    snap.wallet_journal_meta.insert(id, m);
                }
            }
        }
    }
    if let Ok(mut stmt) =
        conn.prepare("SELECT tx_json FROM market_transactions ORDER BY transaction_id ASC")
    {
        if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
            for js in rows.flatten() {
                if let Ok(t) = serde_json::from_str::<MarketTransactionRecord>(&js) {
                    snap.market_transactions.push(t);
                }
            }
        }
    }
    if let Ok(mut stmt) = conn.prepare("SELECT user_id, plex FROM account_plex_vault") {
        if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))) {
            for (uid, plex) in rows.flatten() {
                snap.account_plex_vault.insert(uid as u64, plex);
            }
        }
    }
}

/// Writes changed ledger rows; returns `true` if anything changed.
pub fn sync_ledger(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
) -> Result<bool, String> {
    let mut changed = false;
    let lf = &mut fp.ledger;
    let gone: Vec<i64> =
        lf.journal_meta.keys().filter(|k| !snap.wallet_journal_meta.contains_key(*k)).copied().collect();
    for k in gone {
        changed = true;
        tx.execute("DELETE FROM wallet_journal_meta WHERE entry_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        lf.journal_meta.remove(&k);
    }
    for (&k, v) in &snap.wallet_journal_meta {
        let h = fp_json(v);
        if lf.journal_meta.get(&k) != Some(&h) {
            changed = true;
            let js = serde_json::to_string(v).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO wallet_journal_meta (entry_id, meta_json) VALUES (?1, ?2)",
                params![k, js],
            )
            .map_err(|e| e.to_string())?;
            lf.journal_meta.insert(k, h);
        }
    }
    let live: std::collections::HashSet<i64> =
        snap.market_transactions.iter().map(|t| t.transaction_id).collect();
    let gone: Vec<i64> = lf.market_transactions.keys().filter(|k| !live.contains(k)).copied().collect();
    for k in gone {
        changed = true;
        tx.execute("DELETE FROM market_transactions WHERE transaction_id = ?1", params![k])
            .map_err(|e| e.to_string())?;
        lf.market_transactions.remove(&k);
    }
    for t in &snap.market_transactions {
        let h = fp_json(t);
        if lf.market_transactions.get(&t.transaction_id) != Some(&h) {
            changed = true;
            let js = serde_json::to_string(t).map_err(|e| e.to_string())?;
            tx.execute(
                "INSERT OR REPLACE INTO market_transactions (transaction_id, tx_json) VALUES (?1, ?2)",
                params![t.transaction_id, js],
            )
            .map_err(|e| e.to_string())?;
            lf.market_transactions.insert(t.transaction_id, h);
        }
    }
    let gone: Vec<u64> =
        lf.account_plex_vault.keys().filter(|k| !snap.account_plex_vault.contains_key(*k)).copied().collect();
    for k in gone {
        changed = true;
        tx.execute("DELETE FROM account_plex_vault WHERE user_id = ?1", params![k as i64])
            .map_err(|e| e.to_string())?;
        lf.account_plex_vault.remove(&k);
    }
    for (&uid, &plex) in &snap.account_plex_vault {
        if lf.account_plex_vault.get(&uid) != Some(&plex) {
            changed = true;
            tx.execute(
                "INSERT OR REPLACE INTO account_plex_vault (user_id, plex) VALUES (?1, ?2)",
                params![uid as i64, plex],
            )
            .map_err(|e| e.to_string())?;
            lf.account_plex_vault.insert(uid, plex);
        }
    }
    Ok(changed)
}

/// Restores ledger state from SQLite after a failed write (mirrors `rollback_dirty_from_sqlite`).
pub fn rollback_ledger(conn: &Connection, snap: &mut WorldStateSnapshot, fp: &TableFingerprints) {
    let lf = &fp.ledger;
    let dirty_meta = snap.wallet_journal_meta.len() != lf.journal_meta.len()
        || snap.wallet_journal_meta.iter().any(|(k, v)| lf.journal_meta.get(k) != Some(&fp_json(v)));
    let dirty_tx = snap.market_transactions.len() != lf.market_transactions.len()
        || snap
            .market_transactions
            .iter()
            .any(|t| lf.market_transactions.get(&t.transaction_id) != Some(&fp_json(t)));
    let dirty_vault = snap.account_plex_vault != lf.account_plex_vault;
    if !(dirty_meta || dirty_tx || dirty_vault) {
        return;
    }
    let mut fresh = WorldStateSnapshot::default();
    load_ledger(conn, &mut fresh);
    if dirty_meta {
        snap.wallet_journal_meta = fresh.wallet_journal_meta;
    }
    if dirty_tx {
        snap.market_transactions = fresh.market_transactions;
    }
    if dirty_vault {
        snap.account_plex_vault = fresh.account_plex_vault;
    }
}
