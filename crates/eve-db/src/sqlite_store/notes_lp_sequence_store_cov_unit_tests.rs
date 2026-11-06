//! Comprehensive unit and mutation-killing tests for `sqlite_store::notes_lp_sequence_store`
//! (`apply_d6_migration`, `compute_d6_fingerprints`, `load_d6`, `sync_d6`, and `rollback_d6`).

use super::fingerprints::{fp_json, fp_sp6_lifecycle, TableFingerprints};
use super::notes_lp_sequence_store::{
    apply_d6_migration, compute_d6_fingerprints, load_d6, rollback_d6, sync_d6, D6Fingerprints,
    D6_SCHEMA_VERSION,
};
use super::schema::apply_migrations;
use crate::snapshot_ext::LpRewardLogEntry;
use crate::types::{AllianceRecord, OwnerNoteRecord, WorldStateSnapshot};
use rusqlite::{params, Connection};

#[test]
fn apply_d6_migration_idempotency_and_error_paths() {
    assert_eq!(D6_SCHEMA_VERSION, 3);

    // 1. Read-only connection fails `D6_SCHEMA_SQL` batch execution.
    let ro_conn = Connection::open_in_memory().unwrap();
    ro_conn.execute_batch("PRAGMA query_only = ON;").unwrap();
    assert!(apply_d6_migration(&ro_conn).is_err());

    // 2. Connection missing `world_meta` fails the monotonic ID seed step.
    let no_meta_conn = Connection::open_in_memory().unwrap();
    assert!(apply_d6_migration(&no_meta_conn).is_err());

    // 3. Connection with `world_meta` but missing `schema_migrations` fails migration recording.
    let no_mig_conn = Connection::open_in_memory().unwrap();
    no_mig_conn
        .execute_batch(
            "CREATE TABLE world_meta (meta_key TEXT PRIMARY KEY, meta_value INTEGER NOT NULL);",
        )
        .unwrap();
    assert!(apply_d6_migration(&no_mig_conn).is_err());

    // 4. Full schema migration succeeds and `INSERT OR IGNORE` preserves elevated ID counters.
    let conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_d6_migration(&conn).unwrap();

    conn.execute(
        "UPDATE world_meta SET meta_value = 99888777 WHERE meta_key = 'next_alliance_id'",
        [],
    )
    .unwrap();
    apply_d6_migration(&conn).unwrap();
    let persisted_alliance_seq: i64 = conn
        .query_row(
            "SELECT meta_value FROM world_meta WHERE meta_key = 'next_alliance_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(persisted_alliance_seq, 99_888_777);

    let mig_name: String = conn
        .query_row(
            "SELECT name FROM schema_migrations WHERE version = ?1",
            params![D6_SCHEMA_VERSION],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mig_name, "0003_d6_owner_notes_lp_ids");
}

#[test]
fn load_d6_reads_elevated_counters_skips_corrupt_json_and_reconciles_ids() {
    // 1. Empty connection without tables is a safe no-op that still reconciles entity ID floor.
    let empty_conn = Connection::open_in_memory().unwrap();
    let mut empty_snap = WorldStateSnapshot::default();
    empty_snap.next_entity_id = 0;
    load_d6(&empty_conn, &mut empty_snap);
    assert!(empty_snap.next_entity_id >= 200_000_000);

    // 2. Populated connection with elevated counters, unknown world_meta key, valid + corrupt JSON rows.
    let conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_d6_migration(&conn).unwrap();

    conn.execute_batch(
        "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES
            ('next_alliance_id', 99765432),
            ('next_corporation_id', 98654321),
            ('next_notification_id', 940543210),
            ('next_report_id', 43210),
            ('unknown_custom_meta_key', 777);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO owner_notes (note_id, character_id, item_id, label, note, timestamp_unix)
         VALUES (501, 90000011, 60003760, 'FitLabel', 'NoteText', 1700000123)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO corp_lp_balances (corporation_id, issuer_corp_id, lp_balance)
         VALUES (98000010, 1000125, 4200)",
        [],
    )
    .unwrap();
    let valid_log_js = serde_json::to_string(&vec![LpRewardLogEntry {
        log_id: 7,
        corp_id: 1_000_125,
        amount: 850,
        reason: "plex_capture".into(),
        timestamp_unix: 1_700_000_200,
    }])
    .unwrap();
    conn.execute(
        "INSERT INTO char_lp_logs (character_id, logs_json) VALUES (90000011, ?1)",
        params![valid_log_js],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO char_lp_logs (character_id, logs_json) VALUES (90000099, '{corrupt-json')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO sp6_lifecycle_state (singleton_id, state_json) VALUES (1, '{bad-sp6')",
        [],
    )
    .unwrap();

    let mut snap = WorldStateSnapshot::default();
    snap.sp6_lifecycle.last_downtime_day_idx = Some(123);
    load_d6(&conn, &mut snap);

    // Exact elevated counters prove each match arm in `load_d6` ran (above default floors).
    assert_eq!(snap.next_alliance_id, 99_765_432);
    assert_eq!(snap.next_corporation_id, 98_654_321);
    assert_eq!(snap.next_notification_id, 940_543_210);
    assert_eq!(snap.next_report_id, 43_210);

    let note = &snap.owner_notes[&501];
    assert_eq!(note.character_id, 90_000_011);
    assert_eq!(note.item_id, 60_003_760);
    assert_eq!(note.label, "FitLabel");
    assert_eq!(note.note, "NoteText");
    assert_eq!(note.timestamp_unix, 1_700_000_123);
    assert_eq!(snap.corp_lp_balances[&(98_000_010, 1_000_125)], 4_200);
    assert_eq!(snap.char_lp_logs[&90_000_011][0].amount, 850);
    assert!(!snap.char_lp_logs.contains_key(&90_000_099));
    // Corrupt sp6_lifecycle JSON leaves prior field untouched.
    assert_eq!(snap.sp6_lifecycle.last_downtime_day_idx, Some(123));

    // Verify `reconcile_id_counters()` at the end of `load_d6` advances counters past existing entities.
    let mut snap_with_higher_alliance = WorldStateSnapshot::default();
    snap_with_higher_alliance.alliances.insert(
        99_900_000,
        AllianceRecord {
            alliance_id: 99_900_000,
            name: "High ID Alliance".into(),
            short_name: "HIID".into(),
            executor_corp_id: 98_000_001,
            ..AllianceRecord::default()
        },
    );
    load_d6(&conn, &mut snap_with_higher_alliance);
    assert_eq!(snap_with_higher_alliance.next_alliance_id, 99_900_000);
}

#[test]
fn sync_d6_isolated_change_flags_noop_idempotency_and_sql_errors() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_d6_migration(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    load_d6(&conn, &mut snap);
    let mut fp = TableFingerprints::default();
    fp.d6 = compute_d6_fingerprints(&snap);

    // Unchanged snapshot must return `Ok(false)`.
    {
        let tx = conn.transaction().unwrap();
        assert!(!sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // 1. Isolated `owner_notes`: insert -> true, noop -> false, update -> true, delete -> true, noop -> false.
    snap.owner_notes.insert(
        1,
        OwnerNoteRecord {
            note_id: 1,
            character_id: 90_000_001,
            item_id: 60_003_760,
            label: "L1".into(),
            note: "B1".into(),
            timestamp_unix: 100,
        },
    );
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    snap.owner_notes.get_mut(&1).unwrap().note = "B1_updated".into();
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    snap.owner_notes.remove(&1);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // 2. Isolated `corp_lp_balances`: insert -> true, noop -> false, update -> true, delete -> true, noop -> false.
    snap.corp_lp_balances.insert((98_000_001, 1_000_125), 500);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    snap.corp_lp_balances.insert((98_000_001, 1_000_125), 750);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    snap.corp_lp_balances.remove(&(98_000_001, 1_000_125));
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // 3. Isolated `char_lp_logs`: insert -> true, noop -> false, update -> true, delete -> true, noop -> false.
    snap.char_lp_logs.insert(
        90_000_001,
        vec![LpRewardLogEntry {
            log_id: 1,
            corp_id: 1_000_125,
            amount: 100,
            reason: "r1".into(),
            timestamp_unix: 1_000,
        }],
    );
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    snap.char_lp_logs.get_mut(&90_000_001).unwrap()[0].amount = 250;
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    snap.char_lp_logs.remove(&90_000_001);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // 4. Isolated `sp6_lifecycle`: update -> true, noop -> false.
    snap.sp6_lifecycle.pvp_timers_until.insert(90_000_001, 5_000);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        assert!(!sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    assert_eq!(
        fp.d6,
        D6Fingerprints {
            owner_notes: Default::default(),
            corp_lp_balances: Default::default(),
            char_lp_logs: Default::default(),
            next_alliance_id: snap.next_alliance_id,
            next_corporation_id: snap.next_corporation_id,
            next_notification_id: snap.next_notification_id,
            next_report_id: snap.next_report_id,
            sp6_lifecycle_fp: fp_sp6_lifecycle(&snap.sp6_lifecycle),
        }
    );

    // 5. SQL error propagation on missing tables for each `sync_d6` write statement.
    let mut bad_conn = Connection::open_in_memory().unwrap();
    {
        let mut s = snap.clone();
        s.next_report_id += 1;
        let mut f = fp.clone();
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_d6(&tx, &s, &mut f).is_err());
    }
    {
        // Delete error on `owner_notes`
        let mut f = fp.clone();
        f.d6.owner_notes.insert(99, 123);
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut f).is_err());
    }
    {
        // Insert error on `owner_notes`
        let mut s = snap.clone();
        s.owner_notes.insert(
            99,
            OwnerNoteRecord {
                note_id: 99,
                character_id: 1,
                item_id: 2,
                label: "l".into(),
                note: "n".into(),
                timestamp_unix: 3,
            },
        );
        let mut f = fp.clone();
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_d6(&tx, &s, &mut f).is_err());
    }
    {
        // Delete & insert errors on `corp_lp_balances`, `char_lp_logs`, and `sp6_lifecycle_state`
        let mut f = fp.clone();
        f.d6.corp_lp_balances.insert((1, 2), 10);
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut f).is_err());
    }
    {
        let mut s = snap.clone();
        s.corp_lp_balances.insert((1, 2), 10);
        let mut f = fp.clone();
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_d6(&tx, &s, &mut f).is_err());
    }
    {
        let mut f = fp.clone();
        f.d6.char_lp_logs.insert(1, 10);
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut f).is_err());
    }
    {
        let mut s = snap.clone();
        s.char_lp_logs.insert(1, vec![]);
        let mut f = fp.clone();
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_d6(&tx, &s, &mut f).is_err());
    }
    {
        let mut s = snap.clone();
        s.sp6_lifecycle.last_downtime_day_idx = Some(777);
        let mut f = fp.clone();
        let tx = bad_conn.transaction().unwrap();
        assert!(sync_d6(&tx, &s, &mut f).is_err());
    }
}

#[test]
fn rollback_d6_restores_counters_on_clean_maps_and_detects_deleted_keys() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_d6_migration(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    load_d6(&conn, &mut snap);
    snap.next_alliance_id = 99_100_001;
    snap.next_corporation_id = 98_200_002;
    snap.next_notification_id = 940_300_003;
    snap.next_report_id = 400_004;
    snap.owner_notes.insert(
        1,
        OwnerNoteRecord {
            note_id: 1,
            character_id: 90_000_001,
            item_id: 60_003_760,
            label: "A".into(),
            note: "N_A".into(),
            timestamp_unix: 10,
        },
    );
    snap.owner_notes.insert(
        2,
        OwnerNoteRecord {
            note_id: 2,
            character_id: 90_000_001,
            item_id: 60_003_760,
            label: "B".into(),
            note: "N_B".into(),
            timestamp_unix: 20,
        },
    );
    snap.corp_lp_balances.insert((98_000_001, 1_000_125), 900);
    snap.char_lp_logs.insert(
        90_000_001,
        vec![LpRewardLogEntry {
            log_id: 1,
            corp_id: 1_000_125,
            amount: 50,
            reason: "a".into(),
            timestamp_unix: 10,
        }],
    );
    snap.char_lp_logs.insert(
        90_000_002,
        vec![LpRewardLogEntry {
            log_id: 2,
            corp_id: 1_000_125,
            amount: 75,
            reason: "b".into(),
            timestamp_unix: 20,
        }],
    );
    snap.sp6_lifecycle.last_downtime_day_idx = Some(42);

    let mut fp = TableFingerprints::default();
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    assert_eq!(fp.d6.owner_notes[&1], fp_json(&snap.owner_notes[&1]));

    // 1. When ONLY ID counters are mutated (`!(dirty_notes || dirty_lp || dirty_logs || dirty_sp6)`),
    // `rollback_d6` restores all 4 counters and returns early without reloading SQLite tables.
    snap.next_alliance_id = 1;
    snap.next_corporation_id = 2;
    snap.next_notification_id = 3;
    snap.next_report_id = 4;
    rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.next_alliance_id, 99_100_001);
    assert_eq!(snap.next_corporation_id, 98_200_002);
    assert_eq!(snap.next_notification_id, 940_300_003);
    assert_eq!(snap.next_report_id, 400_004);

    // 2. Deleting a key from `owner_notes` (remaining keys match `df.owner_notes`, only `len()` differs!)
    // must trigger `dirty_notes` and restore the deleted note from SQLite.
    snap.owner_notes.remove(&1);
    assert_eq!(snap.owner_notes.len(), 1);
    rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.owner_notes.len(), 2);
    assert_eq!(snap.owner_notes[&1].note, "N_A");

    // 3. Deleting a key from `char_lp_logs` (only `len()` differs!) must trigger `dirty_logs` and restore it.
    snap.char_lp_logs.remove(&90_000_001);
    assert_eq!(snap.char_lp_logs.len(), 1);
    rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.char_lp_logs.len(), 2);
    assert_eq!(snap.char_lp_logs[&90_000_001][0].amount, 50);

    // 4. Isolated in-place mutations across ALL entries (preserving `len()`) must each trigger rollback.
    for v in snap.owner_notes.values_mut() {
        v.note = "MutatedAll".into();
    }
    rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.owner_notes[&1].note, "N_A");
    assert_eq!(snap.owner_notes[&2].note, "N_B");

    snap.corp_lp_balances.insert((98_000_001, 1_000_125), 12_345);
    rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.corp_lp_balances[&(98_000_001, 1_000_125)], 900);

    for logs in snap.char_lp_logs.values_mut() {
        logs[0].amount = 99_999;
    }
    rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.char_lp_logs[&90_000_001][0].amount, 50);
    assert_eq!(snap.char_lp_logs[&90_000_002][0].amount, 75);

    snap.sp6_lifecycle.last_downtime_day_idx = Some(999);
    rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.sp6_lifecycle.last_downtime_day_idx, Some(42));
}
