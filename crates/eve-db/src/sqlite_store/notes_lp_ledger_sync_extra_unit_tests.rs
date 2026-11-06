//! Deep branch-coverage and mutation-killing unit tests for `notes_lp_sequence_store`,
//! `ledger_store`, and `sync_ext`.

use super::fingerprints::TableFingerprints;
use super::ledger_store::{
    apply_ledger_migration, compute_ledger_fingerprints, load_ledger, rollback_ledger,
    sync_ledger, LEDGER_SCHEMA_VERSION,
};
use super::load::load_snapshot_from_sqlite;
use super::notes_lp_sequence_store::{
    apply_d6_migration, compute_d6_fingerprints, load_d6, rollback_d6, sync_d6, D6_SCHEMA_VERSION,
};
use super::schema::apply_migrations;
use super::sync_ext::sync_extension_tables;
use crate::ledger::{JournalContext, JournalMeta, MarketTransactionRecord};
use crate::snapshot_ext::*;
use crate::types::*;
use rusqlite::Connection;

#[test]
fn notes_lp_sequence_store_granular_mutations_deletions_and_selective_rollbacks() {
    assert_eq!(D6_SCHEMA_VERSION, 3);
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_d6_migration(&conn).unwrap();

    // Default world_meta values loaded into a fresh snapshot
    let mut init_snap = WorldStateSnapshot::default();
    load_d6(&conn, &mut init_snap);
    assert!(init_snap.next_alliance_id >= 99_000_001);
    assert!(init_snap.next_corporation_id >= 98_000_001);
    assert!(init_snap.next_notification_id >= 940_000_001);
    assert!(init_snap.next_report_id >= 1);

    let mut snap = init_snap;
    let mut fp = TableFingerprints::default();
    fp.d6 = compute_d6_fingerprints(&snap);

    // 1. Mutate each ID counter individually and verify sync_d6 returns true
    for (field_idx, new_val) in [
        (0, 99_111_222i64),
        (1, 98_222_333i64),
        (2, 940_333_444i64),
        (3, 555_666i64),
    ] {
        match field_idx {
            0 => snap.next_alliance_id = new_val,
            1 => snap.next_corporation_id = new_val,
            2 => snap.next_notification_id = new_val,
            _ => snap.next_report_id = new_val,
        }
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // 2. Insert two owner_notes, two corp_lp_balances, two char_lp_logs, and sp6_lifecycle
    snap.owner_notes.insert(
        10,
        OwnerNoteRecord {
            note_id: 10,
            character_id: 90_000_001,
            item_id: 60_003_760,
            label: "N1".into(),
            note: "Body1".into(),
            timestamp_unix: 1_000,
        },
    );
    snap.owner_notes.insert(
        20,
        OwnerNoteRecord {
            note_id: 20,
            character_id: 90_000_002,
            item_id: 60_003_760,
            label: "N2".into(),
            note: "Body2".into(),
            timestamp_unix: 2_000,
        },
    );
    snap.corp_lp_balances.insert((98_000_001, 1_000_125), 1_500);
    snap.corp_lp_balances.insert((98_000_002, 1_000_125), 2_500);
    snap.char_lp_logs.insert(
        90_000_001,
        vec![LpRewardLogEntry {
            log_id: 1,
            corp_id: 1_000_125,
            amount: 300,
            reason: "fw".into(),
            timestamp_unix: 1_500,
        }],
    );
    snap.char_lp_logs.insert(
        90_000_002,
        vec![LpRewardLogEntry {
            log_id: 2,
            corp_id: 1_000_125,
            amount: 600,
            reason: "mission".into(),
            timestamp_unix: 2_500,
        }],
    );
    snap.sp6_lifecycle.last_downtime_day_idx = Some(20_100);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // 3. Test selective rollback_d6 for each individual dirty flag:
    // 3a. Only owner_notes mutated in-place (same len, different content!)
    snap.owner_notes.get_mut(&10).unwrap().note = "ModifiedInPlace".into();
    rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.owner_notes[&10].note, "Body1");

    // 3b. Only corp_lp_balances mutated
    snap.corp_lp_balances.insert((98_000_001, 1_000_125), 9_999);
    rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.corp_lp_balances[&(98_000_001, 1_000_125)], 1_500);

    // 3c. Only char_lp_logs mutated in-place (same len, different content!)
    snap.char_lp_logs.get_mut(&90_000_001).unwrap()[0].amount = 9_999;
    rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.char_lp_logs[&90_000_001][0].amount, 300);

    // 3d. Only sp6_lifecycle mutated
    snap.sp6_lifecycle.last_downtime_day_idx = Some(99_999);
    rollback_d6(&conn, &mut snap, &fp);
    assert_eq!(snap.sp6_lifecycle.last_downtime_day_idx, Some(20_100));

    // 4. Delete one key from each collection and verify the other key remains in SQLite
    snap.owner_notes.remove(&10);
    snap.corp_lp_balances.remove(&(98_000_001, 1_000_125));
    snap.char_lp_logs.remove(&90_000_001);
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_d6(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    let mut reloaded = WorldStateSnapshot::default();
    load_d6(&conn, &mut reloaded);
    assert!(!reloaded.owner_notes.contains_key(&10));
    assert_eq!(reloaded.owner_notes[&20].label, "N2");
    assert!(!reloaded.corp_lp_balances.contains_key(&(98_000_001, 1_000_125)));
    assert_eq!(reloaded.corp_lp_balances[&(98_000_002, 1_000_125)], 2_500);
    assert!(!reloaded.char_lp_logs.contains_key(&90_000_001));
    assert_eq!(reloaded.char_lp_logs[&90_000_002][0].amount, 600);
}

#[test]
fn ledger_store_and_sync_ext_in_place_updates_order_and_selective_rollbacks() {
    assert_eq!(LEDGER_SCHEMA_VERSION, 2);
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    apply_ledger_migration(&conn).unwrap();

    let mut snap = WorldStateSnapshot::default();
    snap.wallet_journal_meta.insert(
        1,
        JournalMeta::new(10, 90_000_001, 90_000_002, 1_000)
            .with_context(JournalContext::Character, 90_000_001),
    );
    snap.wallet_journal_meta.insert(
        2,
        JournalMeta::new(85, 1_000_125, 90_000_001, 2_000),
    );
    // Insert market_transactions out of order (id 200 then id 100) to verify ORDER BY transaction_id ASC on load
    snap.market_transactions.push(MarketTransactionRecord {
        transaction_id: 200,
        date_unix: 2_000,
        type_id: 34,
        quantity: 50,
        price: IskCents(600),
        location_id: 60_003_760,
        buyer_id: 90_000_001,
        seller_id: 90_000_002,
        buyer_account_key: 1000,
        seller_account_key: 1000,
        journal_ref_id: 2,
    });
    snap.market_transactions.push(MarketTransactionRecord {
        transaction_id: 100,
        date_unix: 1_000,
        type_id: 35,
        quantity: 25,
        price: IskCents(1_200),
        location_id: 60_003_760,
        buyer_id: 90_000_002,
        seller_id: 90_000_001,
        buyer_account_key: 1000,
        seller_account_key: 1000,
        journal_ref_id: 1,
    });
    snap.account_plex_vault.insert(1, 500);
    snap.account_plex_vault.insert(2, 1_000);

    let mut fp = TableFingerprints::default();
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_ledger(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }
    assert_eq!(fp.ledger, compute_ledger_fingerprints(&snap));

    // Verify load_ledger sorts market_transactions by transaction_id ASC (100 then 200)
    let mut loaded_ledger = WorldStateSnapshot::default();
    load_ledger(&conn, &mut loaded_ledger);
    assert_eq!(loaded_ledger.market_transactions[0].transaction_id, 100);
    assert_eq!(loaded_ledger.market_transactions[1].transaction_id, 200);

    // Selective rollback when same len but modified value in wallet_journal_meta or market_transactions
    loaded_ledger.wallet_journal_meta.get_mut(&1).unwrap().date_unix = 9_999;
    rollback_ledger(&conn, &mut loaded_ledger, &fp);
    assert_eq!(loaded_ledger.wallet_journal_meta[&1].date_unix, 1_000);

    loaded_ledger.market_transactions[0].quantity = 9_999;
    rollback_ledger(&conn, &mut loaded_ledger, &fp);
    assert_eq!(loaded_ledger.market_transactions[0].quantity, 25);

    // Sync extension tables: insert 2 entries per table, update 1 in place, delete 1, and reload from SQLite
    snap.character_last_jump_unix.insert(1, 100);
    snap.character_last_jump_unix.insert(2, 200);
    snap.character_cspa_charges.insert(1, IskCents(100));
    snap.character_cspa_charges.insert(2, IskCents(200));
    snap.mail_timestamps.insert(1, 300);
    snap.mail_timestamps.insert(2, 400);
    snap.character_login_stats.insert(
        1,
        CharacterLoginStats {
            last_login_unix: 10,
            last_logout_unix: 20,
            logins_count: 1,
        },
    );
    snap.character_login_stats.insert(
        2,
        CharacterLoginStats {
            last_login_unix: 30,
            last_logout_unix: 40,
            logins_count: 2,
        },
    );
    snap.item_custom_names.insert(1, "Ship1".into());
    snap.item_custom_names.insert(2, "Ship2".into());
    snap.item_positions.insert(1, [1.0, 2.0, 3.0]);
    snap.item_positions.insert(2, [4.0, 5.0, 6.0]);
    snap.structure_vulnerability_windows.insert(1, (10, 20));
    snap.structure_vulnerability_windows.insert(2, (30, 40));
    snap.skill_history.insert(
        1,
        vec![SkillHistoryEntry {
            log_date_ft: 100,
            event_type_id: 33,
            skill_type_id: 3300,
            skill_level: 1,
            absolute_points: 250.0,
        }],
    );
    snap.skill_history.insert(
        2,
        vec![SkillHistoryEntry {
            log_date_ft: 200,
            event_type_id: 33,
            skill_type_id: 3300,
            skill_level: 2,
            absolute_points: 1_415.0,
        }],
    );
    snap.character_doll_info.insert(1, ("d1".into(), "m1".into()));
    snap.character_doll_info.insert(2, ("d2".into(), "m2".into()));

    {
        let tx = conn.transaction().unwrap();
        assert!(sync_extension_tables(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    // Remove key 1 and mutate key 2 in-place across all 9 extension tables
    snap.character_last_jump_unix.remove(&1);
    snap.character_last_jump_unix.insert(2, 201);
    snap.character_cspa_charges.remove(&1);
    snap.character_cspa_charges.insert(2, IskCents(201));
    snap.mail_timestamps.remove(&1);
    snap.mail_timestamps.insert(2, 401);
    snap.character_login_stats.remove(&1);
    snap.character_login_stats.get_mut(&2).unwrap().logins_count = 99;
    snap.item_custom_names.remove(&1);
    snap.item_custom_names.insert(2, "Ship2Renamed".into());
    snap.item_positions.remove(&1);
    snap.item_positions.insert(2, [7.0, 8.0, 9.0]);
    snap.structure_vulnerability_windows.remove(&1);
    snap.structure_vulnerability_windows.insert(2, (50, 60));
    snap.skill_history.remove(&1);
    snap.skill_history.get_mut(&2).unwrap()[0].skill_level = 5;
    snap.character_doll_info.remove(&1);
    snap.character_doll_info.insert(2, ("d2_new".into(), "m2_new".into()));

    {
        let tx = conn.transaction().unwrap();
        assert!(sync_extension_tables(&tx, &snap, &mut fp).unwrap());
        tx.commit().unwrap();
    }

    let (reloaded, _) = load_snapshot_from_sqlite(&conn).unwrap();
    assert!(!reloaded.character_last_jump_unix.contains_key(&1));
    assert_eq!(reloaded.character_last_jump_unix.get(&2), Some(&201));
    assert!(!reloaded.character_cspa_charges.contains_key(&1));
    assert_eq!(reloaded.character_cspa_charges.get(&2), Some(&IskCents(201)));
    assert!(!reloaded.mail_timestamps.contains_key(&1));
    assert_eq!(reloaded.mail_timestamps.get(&2), Some(&401));
    assert!(!reloaded.character_login_stats.contains_key(&1));
    assert_eq!(reloaded.character_login_stats[&2].logins_count, 99);
    assert!(!reloaded.item_custom_names.contains_key(&1));
    assert_eq!(reloaded.item_custom_names.get(&2), Some(&"Ship2Renamed".to_string()));
    assert!(!reloaded.item_positions.contains_key(&1));
    assert_eq!(reloaded.item_positions.get(&2), Some(&[7.0, 8.0, 9.0]));
    assert!(!reloaded.structure_vulnerability_windows.contains_key(&1));
    assert_eq!(reloaded.structure_vulnerability_windows.get(&2), Some(&(50, 60)));
    assert!(!reloaded.skill_history.contains_key(&1));
    assert_eq!(reloaded.skill_history[&2][0].skill_level, 5);
    assert!(!reloaded.character_doll_info.contains_key(&1));
    assert_eq!(
        reloaded.character_doll_info.get(&2),
        Some(&("d2_new".into(), "m2_new".into()))
    );
}
