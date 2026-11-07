//! Semantic unit tests split from `world_snapshot_extended_sync_tests`.

use super::world_snapshot_extended_sync_tests::*;
use crate::persistence::WorldRepository;
use crate::types::*;
use crate::{DbError, IskCents};

#[test]
fn full_snapshot_sync_rollback_and_reload_roundtrip() {
    let path = temp_db_path("full_sync_rollback");
    let _ = std::fs::remove_file(&path);

    let repo = WorldRepository::open_sqlite(&path).expect("open sqlite");
    repo.with_mut_snapshot(|snap| {
        super::world_snapshot_sync_rollback_tests::populate_world_core_entities(snap);
        populate_world_extended_state(snap);
    });

    let err = repo.try_with_mut_snapshot(|snap| {
        snap.wal_seq = 999;
        snap.accounts.get_mut("alice").unwrap().role = 99;
        snap.characters.get_mut(&2001).unwrap().balance = IskCents(42);
        snap.items.get_mut(&5001).unwrap().quantity = 99;
        snap.industry_jobs.get_mut(&9301).unwrap().runs = 77;
        snap.contracts.get_mut(&9201).unwrap().reward = IskCents(123);
        snap.bookmarks.get_mut(&9401).unwrap().label = "Modified".into();
        snap.structures.get_mut(&1_020_000_000_001).unwrap().fuel_blocks = 12;
        snap.character_last_jump_unix.insert(2001, 9999);
        snap.crimewatch_states
            .get_mut(&2001)
            .unwrap()
            .criminal_flag_until_unix = 8888;
        Err::<(), DbError>(DbError::ValidationError("abort update".into()))
    });
    assert!(matches!(err, Err(DbError::ValidationError(_))));
    repo.with_snapshot(|snap| {
        assert_eq!(snap.accounts["alice"].role, 2);
        assert_eq!(snap.characters[&2001].balance, IskCents(1_500_000));
        assert_eq!(snap.industry_jobs[&9301].runs, 2);
        assert_eq!(snap.bookmarks[&9401].label, "Safe");
        assert_eq!(snap.structures[&1_020_000_000_001].fuel_blocks, 500);
        assert_eq!(snap.character_last_jump_unix[&2001], 2000);
    });

    let err2 = repo.try_with_mut_snapshot(|snap| {
        snap.accounts.clear();
        snap.characters.clear();
        snap.items.clear();
        snap.industry_jobs.clear();
        snap.planetary_colonies.clear();
        snap.active_missions.clear();
        snap.standing_transactions.clear();
        snap.npe_progress.clear();
        snap.bookmarks.clear();
        snap.mails.clear();
        snap.fleets.clear();
        snap.corporations.clear();
        snap.alliances.clear();
        snap.wars.clear();
        snap.corp_projects.clear();
        snap.killmails.clear();
        snap.kill_rights.clear();
        snap.structures.clear();
        snap.asteroids.clear();
        snap.moon_extractions.clear();
        snap.auto_moon_miners.clear();
        snap.character_last_jump_unix.clear();
        snap.item_custom_names.clear();
        snap.skill_history.clear();
        snap.crimewatch_states.clear();
        snap.mailing_lists.clear();
        snap.calendar_events.clear();
        snap.char_contacts.clear();
        snap.wallet_journal.clear();
        Err::<(), DbError>(DbError::ValidationError("abort clear".into()))
    });
    assert!(matches!(err2, Err(DbError::ValidationError(_))));
    repo.with_snapshot(|snap| {
        assert_eq!(snap.accounts.len(), 1);
        assert_eq!(snap.characters.len(), 1);
        assert_eq!(snap.industry_jobs.len(), 1);
        assert_eq!(snap.planetary_colonies.len(), 1);
        assert_eq!(snap.bookmarks.len(), 1);
        assert_eq!(snap.corporations.len(), 1);
        assert_eq!(snap.structures.len(), 1);
        assert_eq!(snap.asteroids.len(), 1);
        assert_eq!(snap.mailing_lists.len(), 1);
        assert_eq!(snap.char_contacts.len(), 1);
    });

    let err_all = repo.try_with_mut_snapshot(|snap| {
        *snap = WorldStateSnapshot::default();
        Err::<(), DbError>(DbError::ValidationError("abort full default".into()))
    });
    assert!(matches!(err_all, Err(DbError::ValidationError(_))));
    repo.with_snapshot(|snap| {
        assert_eq!(snap.accounts.len(), 1);
        assert_eq!(snap.mail_labels.len(), 1);
        assert_eq!(snap.corp_shareholders.len(), 1);
        assert_eq!(snap.corp_customs_offices.len(), 1);
        assert_eq!(snap.corp_vote_cases.len(), 1);
    });

    repo.with_mut_snapshot(|snap| {
        snap.wal_seq = 55;
        snap.characters.get_mut(&2001).unwrap().balance = IskCents(1_750_000);
        snap.industry_jobs.get_mut(&9301).unwrap().runs = 5;
        snap.structures.get_mut(&1_020_000_000_001).unwrap().fuel_blocks = 750;
        snap.character_last_jump_unix.insert(2001, 3000);
        snap.character_cspa_charges.clear();
        snap.item_custom_names.insert(5001, "Renamed Ship".into());
        snap.war_lifecycle_meta
            .get_mut(&9701)
            .unwrap()
            .open_for_allies = Some(false);
    });
    drop(repo);

    let reopened = WorldRepository::open_sqlite(&path).expect("reopen sqlite");
    reopened.with_snapshot(|loaded| {
        assert!(loaded.wal_seq >= 55);
        assert_eq!(loaded.accounts["alice"].user_id, 1001);
        assert_eq!(loaded.characters[&2001].balance, IskCents(1_750_000));
        assert_eq!(loaded.industry_jobs[&9301].runs, 5);
        assert_eq!(loaded.structures[&1_020_000_000_001].fuel_blocks, 750);
        assert_eq!(loaded.character_last_jump_unix[&2001], 3000);
        assert!(loaded.character_cspa_charges.is_empty());
        assert_eq!(loaded.item_custom_names[&5001], "Renamed Ship");
        assert_eq!(loaded.asteroids[&71001].type_id, 1230);
        assert_eq!(loaded.mailing_lists[&1].name, "All");
        assert_eq!(loaded.calendar_events[&1].title, "Op");
        assert_eq!(loaded.char_contacts[&2001][0].standing, 5.0);
        assert_eq!(loaded.crimewatch_states[&2001].criminal_flag_until_unix, 2000);
    });

    let _ = std::fs::remove_file(&path);
}

#[test]
fn snapshot_contract_wrap_id_and_helper_edges() {
    let mut snap = WorldStateSnapshot::default();
    super::world_snapshot_sync_rollback_tests::populate_world_core_entities(&mut snap);
    populate_world_extended_state(&mut snap);

    let e1 = snap.alloc_entity_id();
    let e2 = snap.alloc_entity_id();
    assert_eq!(e2, e1 + 1);
    let a1 = snap.allocate_alliance_id();
    assert!(a1 > 99_000_000);
    let c1 = snap.allocate_corporation_id();
    assert!(c1 > 98_100_000);
    let n1 = snap.allocate_notification_id();
    assert!(n1 > 500_000);
    let r1 = snap.allocate_report_id();
    assert!(r1 > 700_000);
    snap.reconcile_id_counters();

    snap.items.insert(
        5002,
        InventoryItemRecord {
            item_id: 5002,
            type_id: 34,
            owner_id: 2001,
            location_id: 60003760,
            flag: InventoryFlag::Hangar,
            quantity: 10,
            ..InventoryItemRecord::default()
        },
    );
    snap.contracts.get_mut(&9201).unwrap().item_ids.clear();
    snap.industry_jobs.get_mut(&9301).unwrap().blueprint_item_id = 9999;
    let item = snap.items[&5002].clone();
    assert!(snap.is_item_consumable_by(&item, 2001, Some(60003760), Some(5001)));
    assert!(!snap.is_item_consumable_by(&item, 2002, Some(60003760), Some(5001)));

    // Plastic wrap unwrapping on courier contract failure
    snap.items.insert(
        6001,
        InventoryItemRecord {
            item_id: 6001,
            type_id: 3468,
            owner_id: 2002,
            location_id: 60003760,
            flag: InventoryFlag::Hangar,
            ..InventoryItemRecord::default()
        },
    );
    snap.items.get_mut(&5002).unwrap().location_id = 6001;
    snap.contracts.get_mut(&9201).unwrap().item_ids = vec![5002];
    snap.contracts.get_mut(&9201).unwrap().expiration_unix = 100;
    assert_eq!(snap.resolve_expired_contract(9201, 50), None);
    assert_eq!(snap.resolve_expired_contract(9201, 200), Some("Expired").filter(|_| false).or(Some("Failed")));
    assert_eq!(snap.contracts[&9201].status, "Failed");
    assert_eq!(snap.items[&5002].location_id, 60003760);
    assert_eq!(snap.items[&5002].owner_id, 2002);
    assert!(!snap.items.contains_key(&6001));

    let hist = &snap.market_history[&(10000002, 34)][0];
    assert_eq!((hist.open_isk(), hist.high_isk(), hist.low_isk(), hist.close_isk()), (5.8, 6.5, 5.5, 6.2));
}
