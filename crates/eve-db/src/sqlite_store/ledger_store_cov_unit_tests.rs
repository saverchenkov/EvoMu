//! Comprehensive unit and mutation-killing tests for `sqlite_store::ledger_store`
//! (`apply_ledger_migration`, `compute_ledger_fingerprints`, `load_ledger`, `sync_ledger`, `rollback_ledger`).

use super::fingerprints::{fp_json, TableFingerprints};
use super::ledger_store::{
    apply_ledger_migration, compute_ledger_fingerprints, load_ledger, rollback_ledger,
    sync_ledger, LedgerFingerprints, LEDGER_SCHEMA_VERSION,
};
use super::schema::apply_migrations;
use crate::ledger::{JournalContext, JournalMeta, MarketTransactionRecord};
use crate::types::{IskCents, WorldStateSnapshot};
use rusqlite::{params, Connection};

fn sample_tx(tx_id: i64, qty: i64) -> MarketTransactionRecord {
    MarketTransactionRecord {
        transaction_id: tx_id,
        date_unix: 1_700_000_000 + tx_id as u64,
        type_id: 34,
        quantity: qty,
        price: IskCents(500),
        location_id: 60_003_760,
        buyer_id: 90_000_001,
        seller_id: 90_000_002,
        buyer_account_key: 1000,
        seller_account_key: 1000,
        journal_ref_id: tx_id,
    }
}

#[test]
fn apply_ledger_migration_idempotency_and_error_paths() {
    assert_eq!(LEDGER_SCHEMA_VERSION, 2);

    // 1. Read-only connection fails `LEDGER_SCHEMA_SQL`.
    let ro_conn = Connection::open_in_memory().unwrap();
    ro_conn.execute_batch("PRAGMA query_only = ON;").unwrap();
    assert!(apply_ledger_migration(&ro_conn).is_err());

    // 2. Connection without `schema_migrations` table fails recording migration version.
    let no_mig_conn = Connection::open_in_memory().unwrap();
    assert!(apply_ledger_migration(&no_mig_conn).is_err());

    // 3. Full schema migration succeeds and is idempotent on repeated invocation.
    let conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_ledger_migration(&conn).unwrap();
    apply_ledger_migration(&conn).unwrap();

    let name: String = conn
        .query_row(
            "SELECT name FROM schema_migrations WHERE version = ?1",
            params![LEDGER_SCHEMA_VERSION],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(name, "0002_wallet_ledger");
}

#[test]
fn load_ledger_handles_missing_tables_corrupt_json_and_ascending_tx_order() {
    // 1. Empty connection without ledger tables is a safe no-op.
    let empty_conn = Connection::open_in_memory().unwrap();
    let mut empty_snap = WorldStateSnapshot::default();
    load_ledger(&empty_conn, &mut empty_snap);
    assert!(empty_snap.wallet_journal_meta.is_empty());
    assert!(empty_snap.market_transactions.is_empty());
    assert!(empty_snap.account_plex_vault.is_empty());

    // 2. Populated connection with valid + corrupt JSON rows and out-of-order transaction IDs.
    let conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_ledger_migration(&conn).unwrap();

    let m1 = JournalMeta::new(10, 90_000_001, 90_000_002, 1_500)
        .with_context(JournalContext::MarketTransaction, 300);
    conn.execute(
        "INSERT INTO wallet_journal_meta (entry_id, meta_json) VALUES (1, ?1)",
        params![serde_json::to_string(&m1).unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO wallet_journal_meta (entry_id, meta_json) VALUES (2, '{corrupt')",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO market_transactions (transaction_id, tx_json) VALUES (300, ?1)",
        params![serde_json::to_string(&sample_tx(300, 10)).unwrap()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO market_transactions (transaction_id, tx_json) VALUES (200, '{corrupt')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO market_transactions (transaction_id, tx_json) VALUES (100, ?1)",
        params![serde_json::to_string(&sample_tx(100, 5)).unwrap()],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO account_plex_vault (user_id, plex) VALUES (42, 1250)",
        [],
    )
    .unwrap();

    let mut snap = WorldStateSnapshot::default();
    load_ledger(&conn, &mut snap);
    assert_eq!(snap.wallet_journal_meta.len(), 1);
    assert_eq!(snap.wallet_journal_meta[&1], m1);
    assert_eq!(snap.market_transactions.len(), 2);
    assert_eq!(snap.market_transactions[0].transaction_id, 100);
    assert_eq!(snap.market_transactions[1].transaction_id, 300);
    assert_eq!(snap.account_plex_vault.get(&42), Some(&1_250));
}

#[test]
fn sync_ledger_isolated_table_lifecycle_noop_flags_and_sql_errors() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_ledger_migration(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    let mut fp = TableFingerprints::default();

    // Baseline unchanged snapshot returns `Ok(false)`.
    {
        let tx = conn.transaction().unwrap();
        assert!(!sync_ledger(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // 1. Isolated `wallet_journal_meta`: insert -> true, noop -> false, update -> true, delete -> true, noop -> false.
    snap.wallet_journal_meta
        .insert(10, JournalMeta::new(10, 1, 2, 100));
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_ledger(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    snap.wallet_journal_meta.get_mut(&10).unwrap().date_unix = 250;
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_ledger(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    snap.wallet_journal_meta.remove(&10);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_ledger(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // 2. Isolated `market_transactions`: insert -> true, noop -> false, update -> true, delete -> true, noop -> false.
    snap.market_transactions.push(sample_tx(50, 12));
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_ledger(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    snap.market_transactions[0].quantity = 99;
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_ledger(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    snap.market_transactions.clear();
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_ledger(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // 3. Isolated `account_plex_vault`: insert -> true, noop -> false, update -> true, delete -> true, noop -> false.
    snap.account_plex_vault.insert(7, 500);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_ledger(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    snap.account_plex_vault.insert(7, 800);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_ledger(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    snap.account_plex_vault.remove(&7);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_ledger(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    assert_eq!(fp.ledger, LedgerFingerprints::default());

    // 4. SQL errors on missing tables across all 6 `sync_ledger` SQL statements.
    let mut bad_conn = Connection::open_in_memory().unwrap();
    {
        let mut f = fp.clone();
        f.ledger.journal_meta.insert(1, 1);
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut f).is_err());
    }
    {
        let mut s = snap.clone();
        s.wallet_journal_meta
            .insert(1, JournalMeta::new(10, 1, 2, 3));
        let mut f = fp.clone();
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &s, &mut f).is_err());
    }
    {
        let mut f = fp.clone();
        f.ledger.market_transactions.insert(1, 1);
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut f).is_err());
    }
    {
        let mut s = snap.clone();
        s.market_transactions.push(sample_tx(1, 1));
        let mut f = fp.clone();
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &s, &mut f).is_err());
    }
    {
        let mut f = fp.clone();
        f.ledger.account_plex_vault.insert(1, 100);
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut f).is_err());
    }
    {
        let mut s = snap.clone();
        s.account_plex_vault.insert(1, 100);
        let mut f = fp.clone();
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &s, &mut f).is_err());
    }
}

#[test]
fn rollback_ledger_clean_noop_and_selective_deletion_restoration() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_ledger_migration(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    snap.wallet_journal_meta
        .insert(1, JournalMeta::new(10, 1, 2, 100));
    snap.wallet_journal_meta
        .insert(2, JournalMeta::new(85, 3, 4, 200));
    snap.market_transactions.push(sample_tx(10, 5));
    snap.market_transactions.push(sample_tx(20, 15));
    snap.account_plex_vault.insert(1, 300);
    snap.account_plex_vault.insert(2, 600);

    let mut fp = TableFingerprints::default();
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    assert_eq!(fp.ledger, compute_ledger_fingerprints(&snap));
    assert_eq!(
        fp.ledger.journal_meta[&1],
        fp_json(&snap.wallet_journal_meta[&1])
    );

    // 1. Clean snapshot hits early return in `rollback_ledger` without touching SQLite.
    // Prove early return by mutating SQLite behind `fp`'s back and confirming `snap` stays intact!
    conn.execute(
        "UPDATE account_plex_vault SET plex = 999999 WHERE user_id = 1",
        [],
    )
    .unwrap();
    rollback_ledger(&conn, &mut snap, &fp);
    assert_eq!(snap.account_plex_vault[&1], 300);
    // Restore SQLite row to 300 for subsequent rollback checks.
    conn.execute(
        "UPDATE account_plex_vault SET plex = 300 WHERE user_id = 1",
        [],
    )
    .unwrap();

    // 2. Removing an entry from `wallet_journal_meta` (`len()` mismatch while remaining keys match!)
    // triggers `dirty_meta` and restores only `wallet_journal_meta`.
    snap.wallet_journal_meta.remove(&1);
    assert_eq!(snap.wallet_journal_meta.len(), 1);
    rollback_ledger(&conn, &mut snap, &fp);
    assert_eq!(snap.wallet_journal_meta.len(), 2);
    assert_eq!(snap.wallet_journal_meta[&1].date_unix, 100);

    // 3. Removing an entry from `market_transactions` (`len()` mismatch while remaining txs match!)
    // triggers `dirty_tx` and restores `market_transactions`.
    snap.market_transactions.pop();
    assert_eq!(snap.market_transactions.len(), 1);
    rollback_ledger(&conn, &mut snap, &fp);
    assert_eq!(snap.market_transactions.len(), 2);
    assert_eq!(snap.market_transactions[1].transaction_id, 20);

    // 4. Mutating `account_plex_vault` triggers `dirty_vault` and restores `account_plex_vault`.
    snap.account_plex_vault.remove(&2);
    rollback_ledger(&conn, &mut snap, &fp);
    assert_eq!(snap.account_plex_vault[&2], 600);

    // 5. Isolated in-place mutations across ALL entries (preserving `len()`) must trigger rollback.
    for m in snap.wallet_journal_meta.values_mut() {
        m.date_unix = 888_888;
    }
    rollback_ledger(&conn, &mut snap, &fp);
    assert_eq!(snap.wallet_journal_meta[&1].date_unix, 100);
    assert_eq!(snap.wallet_journal_meta[&2].date_unix, 200);

    for t in &mut snap.market_transactions {
        t.quantity = 77_777;
    }
    rollback_ledger(&conn, &mut snap, &fp);
    assert_eq!(snap.market_transactions[0].quantity, 5);
    assert_eq!(snap.market_transactions[1].quantity, 15);
}
