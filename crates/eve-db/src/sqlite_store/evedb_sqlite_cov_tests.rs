//! Mutation-killing and branch-coverage unit tests for `sqlite_store`
//! (`fingerprints`, `load`, `rollback`, `sync`, and `sync_ext`).

#![forbid(unsafe_code)]

use crate::persistence::WorldRepository;
use crate::sqlite_store::fingerprints::{compute_fingerprints, TableFingerprints};
use crate::sqlite_store::load::load_snapshot_from_sqlite;
use crate::sqlite_store::schema::apply_migrations;
use crate::sqlite_store::sync::{replace_sqlite_from_snapshot, sync_dirty_to_sqlite};
use crate::types::{
    ActiveMissionRecord, AllianceRecord, AsteroidRecord, BookmarkRecord, CharacterLoginStats,
    ContractRecord, CorpProjectRecord, FleetRecord, IndustryJobRecord, JumpCloneRecord,
    KillRightRecord, KillmailRecord, MailRecord, MarketOrderRecord, MiningLedgerRecord,
    PlanetaryColonyRecord, SavedFittingRecord, SkillHistoryEntry, SkillQueueEntry,
    StandingTransactionRecord, UpwellStructureRecord, WalletJournalEntry, WarRecord,
    WorldStateSnapshot,
};
use crate::{IskCents, LpRewardLogEntry, OwnerNoteRecord};
use eve_sde::{SolarSystemId, TypeId};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

fn open_migrated_conn() -> Connection {
    let conn = Connection::open_in_memory().expect("open in-memory sqlite");
    apply_migrations(&conn).expect("apply migrations");
    conn
}

pub(super) fn build_populated_snapshot() -> WorldStateSnapshot {
    let mut snap = WorldRepository::create_demo_snapshot();
    let cid = 90_000_001i64;
    snap.next_entity_id = 91_234_567;
    snap.wal_seq = 42;
    snap.markets_seeded = true;
    snap.skill_queues.insert(
        cid,
        vec![SkillQueueEntry {
            type_id: 3300,
            target_level: 3,
            start_sp: 1415.0,
            end_sp: 8000.0,
            duration_seconds: 900.0,
            started_at_unix: Some(1_760_000_000),
        }],
    );
    snap.jump_clones.insert(
        cid,
        vec![JumpCloneRecord {
            jump_clone_id: 400_200,
            character_id: cid,
            location_id: 60_003_760,
            implants: vec![9899],
        }],
    );
    snap.active_implants.insert(cid, vec![9899]);
    snap.saved_fittings.insert(
        500_200,
        SavedFittingRecord {
            fitting_id: 500_200,
            owner_id: cid,
            name: "Fit".into(),
            ship_type_id: 587,
            description: String::new(),
            items: vec![],
        },
    );
    snap.market_orders.insert(
        700_100,
        MarketOrderRecord {
            order_id: 700_100,
            owner_id: cid,
            station_id: 60_003_760,
            region_id: 10_000_002,
            type_id: 34,
            is_bid: false,
            price: IskCents(400),
            vol_entered: 50,
            vol_remaining: 50,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents(0),
        },
    );
    snap.wallet_journal.push(WalletJournalEntry {
        entry_id: 800_200,
        character_id: cid,
        entry_type: "Initial".into(),
        amount: IskCents(100),
        balance_after: IskCents(100),
        reason: "r1".into(),
    });
    snap.contracts.insert(
        600_200,
        ContractRecord {
            contract_id: 600_200,
            issuer_id: cid,
            contract_type: "Courier".into(),
            status: "Outstanding".into(),
            start_station_id: 60_003_760,
            end_station_id: 60_003_760,
            ..Default::default()
        },
    );
    snap.industry_jobs.insert(
        610_200,
        IndustryJobRecord {
            job_id: 610_200,
            installer_id: cid,
            facility_id: 60_003_760,
            blueprint_item_id: 100_000_001,
            blueprint_type_id: 683,
            activity_id: 1,
            runs: 1,
            product_type_id: 587,
            status: "Active".into(),
            duration_seconds: 300,
            start_unix: 1000,
            ..Default::default()
        },
    );
    snap.planetary_colonies.insert(
        (cid, 40_009_082),
        PlanetaryColonyRecord {
            planet_id: 40_009_082,
            owner_id: cid,
            solar_system_id: 30_000_142,
            command_center_level: 1,
            pins_count: 1,
            extracted_commodities: HashMap::new(),
            last_extracted_unix: 1000,
        },
    );
    snap.active_missions.insert(
        (cid, 200),
        ActiveMissionRecord {
            mission_id: 200,
            character_id: cid,
            agent_id: 3_018_922,
            solar_system_id: 30_000_142,
            state: "Offered".into(),
            current_room_id: 0,
            npcs_remaining: 1,
            spawned_npc_ball_ids: vec![],
            bookmark_id: None,
        },
    );
    snap.mission_tally.insert(cid, 7);
    snap.standing_transactions.insert(
        cid,
        vec![StandingTransactionRecord {
            event_id: 1,
            from_id: 1,
            to_id: cid,
            modification: 1.0,
            event_type_id: 1,
        }],
    );
    snap.bookmarks.insert(
        620_200,
        BookmarkRecord {
            bookmark_id: 620_200,
            owner_id: cid,
            label: "BM".into(),
            solar_system_id: 30_000_142,
            x: 1.0,
            y: 2.0,
            z: 3.0,
            item_id: None,
            memo: String::new(),
            folder_id: None,
            is_shared: false,
        },
    );
    snap.mails.push(MailRecord {
        message_id: 630_200,
        sender_id: cid,
        recipient_id: cid,
        subject: "S1".into(),
        body: "B1".into(),
    });
    snap.fleets.insert(
        640_200,
        FleetRecord {
            fleet_id: 640_200,
            boss_id: cid,
            ..Default::default()
        },
    );
    snap.alliances.insert(
        99_000_200,
        AllianceRecord {
            alliance_id: 99_000_200,
            name: "A".into(),
            short_name: "A".into(),
            executor_corp_id: 1_000_170,
            member_corp_ids: HashSet::new(),
        },
    );
    snap.wars.insert(
        650_200,
        WarRecord {
            war_id: 650_200,
            aggressor_corp_id: 1_000_170,
            defender_corp_id: 1_000_045,
            mutual: false,
            started_unix: 1000,
            active: true,
        },
    );
    snap.corp_projects.insert(
        660_200,
        CorpProjectRecord {
            project_id: 660_200,
            corporation_id: 1_000_170,
            name: "P".into(),
            target_quantity: 10,
            current_quantity: 0,
            isk_per_unit: IskCents(10),
            escrow_remaining: IskCents(100),
        },
    );
    snap.killmails.insert(
        670_200,
        KillmailRecord {
            kill_id: 670_200,
            victim_character_id: cid,
            attacker_character_id: cid,
            solar_system_id: 30_000_142,
            ship_type_id: 587,
            total_value_isk: 100.0,
            ..Default::default()
        },
    );
    snap.kill_rights.insert(
        680_200,
        KillRightRecord {
            kill_right_id: 680_200,
            owner_id: cid,
            target_id: cid,
            activation_fee: IskCents(100),
            is_active_suspect: false,
        },
    );
    snap.structures.insert(
        1_000_000_000_200,
        UpwellStructureRecord {
            structure_id: 1_000_000_000_200,
            type_id: 35832,
            owner_corp_id: 1_000_170,
            solar_system_id: 30_000_142,
            name: "S".into(),
            position: [0.0; 3],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 10,
            ..Default::default()
        },
    );
    snap.committed_idempotency_keys
        .insert((cid, "idem-cov-1".to_string()));
    snap.applied_ship_skins.insert(100_000_001, 210_001);
    snap.licensed_skins.insert(cid, vec![210_001, 210_002]);
    snap.mining_ledger.push(MiningLedgerRecord {
        character_id: cid,
        solar_system_id: 30_000_142 as SolarSystemId,
        type_id: 1230 as TypeId,
        quantity: 250,
        date: "2026-11-06".to_string(),
        timestamp_unix: 1_760_000_100,
    });
    snap.asteroids.insert(
        700_000_001,
        AsteroidRecord {
            asteroid_id: 700_000_001,
            solar_system_id: 30_000_142 as SolarSystemId,
            belt_id: 40_000_010,
            type_id: 1230 as TypeId,
            quantity: 5_000,
            initial_quantity: 5_000,
            position: (10.0, 20.0, 30.0),
            radius: 125.0,
        },
    );
    snap.character_last_jump_unix.insert(cid, 1_760_000_200);
    snap.character_cspa_charges.insert(cid, IskCents(295_000));
    snap.mail_timestamps.insert(800_000_001, 1_760_000_300);
    snap.character_login_stats.insert(
        cid,
        CharacterLoginStats {
            last_login_unix: 1_760_000_000,
            last_logout_unix: 1_760_000_100,
            logins_count: 3,
        },
    );
    snap.item_custom_names
        .insert(100_000_001, "Raven Navy".to_string());
    snap.item_positions
        .insert(100_000_001, [100.0, 200.0, -50.0]);
    snap.structure_vulnerability_windows
        .insert(1_000_000_001, (1_760_001_000, 1_760_004_600));
    snap.skill_history.insert(
        cid,
        vec![SkillHistoryEntry {
            log_date_ft: 1_760_000_400,
            event_type_id: 33,
            skill_type_id: 3300,
            skill_level: 3,
            absolute_points: 8_000.0,
        }],
    );
    snap.character_doll_info
        .insert(cid, ("dna_blob".to_string(), "meta_blob".to_string()));
    snap.owner_notes.insert(
        500_001,
        OwnerNoteRecord {
            note_id: 500_001,
            character_id: cid,
            item_id: 60_003_760,
            label: "CovNote".to_string(),
            note: "Coverage note body".to_string(),
            timestamp_unix: 1_760_000_500,
        },
    );
    snap.corp_lp_balances.insert((1_000_170, cid), 4_200);
    snap.char_lp_logs.insert(
        cid,
        vec![LpRewardLogEntry {
            log_id: 1,
            corp_id: 1_000_170,
            amount: 4_200,
            reason: "Mission".to_string(),
            timestamp_unix: 1_760_000_550,
        }],
    );
    snap
}

#[test]
fn sqlite_store_kills_all_mutants_and_verifies_default_entity_id() {
    let empty_conn = open_migrated_conn();
    let (empty_loaded, _) =
        load_snapshot_from_sqlite(&empty_conn).expect("load empty migrated sqlite");
    assert_eq!(empty_loaded.next_entity_id, 200_000_000);

    let snap = build_populated_snapshot();
    let fp = compute_fingerprints(&snap);
    assert_eq!(fp.next_entity_id, 91_234_567);
    assert_eq!(fp.wal_seq, 42);
    assert!(fp.markets_seeded);
    assert_eq!(fp.wallet_journal_len, snap.wallet_journal.len());
    assert!(fp.wallet_journal_len > 0);
    assert_ne!(fp.wallet_journal_hash, 0);
    assert_eq!(fp.mails_len, snap.mails.len());
    assert!(fp.mails_len > 0);
    assert_ne!(fp.mails_hash, 0);
    assert!(fp
        .committed_idempotency_keys
        .contains(&(90_000_001, "idem-cov-1".to_string())));
    assert_eq!(fp.mission_tally.get(&90_000_001), Some(&7));
    assert_eq!(fp.applied_ship_skins.get(&100_000_001), Some(&210_001));

    let mut conn = open_migrated_conn();
    let mut sync_fp = TableFingerprints::default();
    let delta = {
        let tx = conn.transaction().expect("begin tx");
        let d = sync_dirty_to_sqlite(&tx, &snap, &mut sync_fp, true).expect("sync populated");
        tx.commit().expect("commit tx");
        d
    };
    assert_eq!(delta.seq, 42);
    assert_eq!(delta.next_entity_id, 91_234_567);
    assert!(delta.full_snapshot.is_some());
}

#[test]
fn sqlite_store_sync_deletions_and_full_rollback_roundtrip() {
    let mut conn = open_migrated_conn();
    let base = build_populated_snapshot();
    let mut fp = replace_sqlite_from_snapshot(&mut conn, &base).expect("replace snapshot");

    let mut working = base.clone();
    working.wal_seq = 999;
    working.next_entity_id = 99_999_999;
    working.markets_seeded = false;
    working.accounts.clear();
    working.characters.clear();
    working.skills.clear();
    working.skill_queues.clear();
    working.jump_clones.clear();
    working.active_implants.clear();
    working.items.clear();
    working.saved_fittings.clear();
    working.market_orders.clear();
    working.wallet_journal.clear();
    working.contracts.clear();
    working.industry_jobs.clear();
    working.planetary_colonies.clear();
    working.active_missions.clear();
    working.mission_tally.clear();
    working.standing_transactions.clear();
    working.npe_progress.clear();
    working.bookmarks.clear();
    working.mails.clear();
    working.fleets.clear();
    working.corporations.clear();
    working.alliances.clear();
    working.wars.clear();
    working.corp_projects.clear();
    working.killmails.clear();
    working.kill_rights.clear();
    working.structures.clear();
    working.system_states.clear();
    working.committed_idempotency_keys.clear();
    working.licensed_skins.clear();
    working.applied_ship_skins.clear();
    working.mining_ledger.clear();
    working.asteroids.clear();
    working.character_last_jump_unix.clear();
    working.character_cspa_charges.clear();
    working.mail_timestamps.clear();
    working.character_login_stats.clear();
    working.item_custom_names.clear();
    working.item_positions.clear();
    working.structure_vulnerability_windows.clear();
    working.skill_history.clear();
    working.character_doll_info.clear();

    let (reloaded, _) = load_snapshot_from_sqlite(&conn).expect("reload full snapshot");
    let mut expected_reconciled = base.clone();
    expected_reconciled.reconcile_id_counters();
    assert_eq!(reloaded.next_entity_id, expected_reconciled.next_entity_id);
    assert_eq!(reloaded.wal_seq, base.wal_seq);
    assert_eq!(reloaded.markets_seeded, base.markets_seeded);
    assert_eq!(reloaded.accounts.len(), base.accounts.len());
    assert_eq!(reloaded.characters.len(), base.characters.len());
    assert_eq!(reloaded.items.len(), base.items.len());
    assert_eq!(reloaded.mission_tally, base.mission_tally);
    assert_eq!(reloaded.applied_ship_skins, base.applied_ship_skins);
    assert_eq!(reloaded.mining_ledger.len(), base.mining_ledger.len());
    assert_eq!(reloaded.asteroids.len(), base.asteroids.len());
    assert_eq!(reloaded.character_doll_info, base.character_doll_info);

    // Sync deletion across all populated tables to exercise DELETE branches cleanly
    let empty = WorldStateSnapshot::default();
    let tx = conn.transaction().expect("begin tx");
    sync_dirty_to_sqlite(&tx, &empty, &mut fp, false).expect("sync deletions");
    tx.commit().expect("commit tx");
    let (after_delete, _) = load_snapshot_from_sqlite(&conn).expect("load after delete");
    assert!(after_delete.accounts.is_empty());
    assert!(after_delete.asteroids.is_empty());
    assert!(after_delete.mining_ledger.is_empty());
}

#[test]
fn sqlite_store_sync_insert_and_delete_error_paths_on_dropped_tables() {
    let snap = build_populated_snapshot();
    let base_fp = compute_fingerprints(&snap);

    let tables = [
        "world_meta",
        "accounts",
        "characters",
        "skills",
        "skill_queues",
        "jump_clones",
        "active_implants",
        "items",
        "saved_fittings",
        "market_orders",
        "wallet_journal",
        "contracts",
        "industry_jobs",
        "planetary_colonies",
        "missions",
        "mission_tally",
        "standings",
        "npe_progress",
        "bookmarks",
        "mail",
        "fleets",
        "corps",
        "alliances",
        "wars",
        "corp_projects",
        "killmails",
        "kill_rights",
        "structures",
        "system_states",
        "idempotency_keys",
        "licensed_skins",
        "applied_ship_skins",
        "char_mining_ledger",
        "asteroids",
        "character_last_jump_unix",
        "character_cspa_charges",
        "mail_timestamps",
        "character_login_stats",
        "item_custom_names",
        "item_positions",
        "structure_vulnerability_windows",
        "skill_history",
        "character_doll_info",
    ];

    for tbl in tables {
        let mut insert_conn = open_migrated_conn();
        insert_conn
            .execute(&format!("DROP TABLE {tbl}"), [])
            .unwrap_or_else(|e| panic!("drop {tbl}: {e}"));
        let mut empty_fp = TableFingerprints::default();
        let tx = insert_conn.transaction().expect("begin tx");
        assert!(
            sync_dirty_to_sqlite(&tx, &snap, &mut empty_fp, false).is_err(),
            "expected insert sync error when table {tbl} is missing"
        );
    }

    let empty_snap = WorldStateSnapshot {
        next_entity_id: 1,
        wal_seq: 1,
        markets_seeded: false,
        ..Default::default()
    };
    for tbl in &tables[1..] {
        let mut del_conn = open_migrated_conn();
        del_conn
            .execute(&format!("DROP TABLE {tbl}"), [])
            .unwrap_or_else(|e| panic!("drop {tbl}: {e}"));
        let mut fp = base_fp.clone();
        let tx = del_conn.transaction().expect("begin tx");
        assert!(
            sync_dirty_to_sqlite(&tx, &empty_snap, &mut fp, false).is_err(),
            "expected delete sync error when table {tbl} is missing"
        );
    }
}

fn build_social_populated_snapshot() -> WorldStateSnapshot {
    use crate::durable_domain_types::SocialContactNotificationRecord;
    use crate::types::{
        BookmarkFolderRecord, CalendarEventRecord, ChatChannelAclRecord, ChatChannelRecord,
        MailLabelRecord, MailingListRecord, SocialContactRecord,
    };
    let mut snap = WorldStateSnapshot::default();
    let cid = 90_000_001_i64;
    snap.read_mails.insert((cid, 1));
    snap.deleted_mails.insert((cid, 2));
    snap.trashed_mails.insert((cid, 3));
    snap.mail_labels.insert(cid, vec![MailLabelRecord { label_id: 1, name: "L".into(), color: "#000000".into() }]);
    snap.mail_assigned_labels.insert((cid, 1), vec![1]);
    snap.mailing_lists.insert(
        10,
        MailingListRecord {
            list_id: 10, name: "ML".into(), owner_id: cid, default_access: 1, default_role: 0,
            cost: 0, members: HashMap::from([(cid, 1)]), entity_access: HashMap::new(),
            welcome_mail: None, mail_ids: HashSet::new(),
        },
    );
    snap.calendar_events.insert(
        20,
        CalendarEventRecord {
            event_id: 20, owner_id: cid, creator_id: cid, event_date_time: 100, duration: 30,
            importance: 0, title: "Ev".into(), description: "D".into(), scope: "Personal".into(),
            responses: HashMap::new(), is_deleted: false,
        },
    );
    snap.bookmarks.insert(
        30,
        BookmarkRecord {
            bookmark_id: 30, owner_id: cid, label: "B".into(), solar_system_id: 30_000_142,
            x: 1.0, y: 2.0, z: 3.0, item_id: None, memo: String::new(), folder_id: Some(40), is_shared: true,
        },
    );
    snap.bookmark_folders.insert(40, BookmarkFolderRecord { folder_id: 40, owner_id: cid, folder_name: "F".into(), ..Default::default() });
    snap.bookmark_subfolders.insert(30, 40);
    snap.bookmark_expiries.insert(30, 1_900_000_000);
    snap.access_groups.insert(50, vec![cid]);
    snap.chat_channels.insert(
        "ch1".into(),
        ChatChannelRecord {
            channel_id: "ch1".into(), name: "Ch".into(), motd: String::new(), owner_id: Some(cid),
            members: HashSet::from([cid]), is_system_local: false, solar_system_id: None,
            is_private: false, operators: HashSet::new(), password: None, is_delayed_local: false,
        },
    );
    let c = SocialContactRecord {
        contact_id: 90_000_002, contact_type: "character".into(), standing: 1.0,
        is_watched: false, is_blocked: false, label_ids: vec![],
    };
    snap.char_contacts.insert(cid, vec![c.clone()]);
    snap.char_contact_notifications.insert(
        cid,
        vec![SocialContactNotificationRecord {
            notification_id: 1, sender_character_id: 90_000_002,
            send_date_unix: 100, standing_level: 1.0, message: "Hi".into(),
        }],
    );
    snap.char_contact_labels.insert(cid, vec![(1, "F".into())]);
    snap.corp_contacts.insert(98_000_001, vec![c.clone()]);
    snap.corp_contact_labels.insert(98_000_001, vec![(1, "C".into(), 1)]);
    snap.alliance_contacts.insert(99_000_001, vec![c]);
    snap.alliance_contact_labels.insert(99_000_001, vec![(1, "A".into())]);
    snap.char_notes.insert((cid, 90_000_002), "note".into());
    snap.chat_channel_acl.insert(
        "ch1".into(),
        ChatChannelAclRecord { allow_list: HashSet::new(), deny_list: HashSet::new(), muted_until: HashMap::new() },
    );
    snap
}

#[test]
fn social_store_dropped_tables_corrupt_json_and_migration_error_paths() {
    use crate::sqlite_store::social_store::{
        apply_d6_social_migration, load_d6_social_into_snapshot, sync_d6_social,
    };

    let pop = build_social_populated_snapshot();
    let base_fp = compute_fingerprints(&pop);
    let empty = WorldStateSnapshot::default();
    let social_tables = [
        "read_mails",
        "deleted_mails",
        "trashed_mails",
        "mail_labels",
        "mail_assigned_labels",
        "mailing_lists",
        "calendar_events",
        "bookmark_meta",
        "bookmark_folders",
        "chat_channels",
        "char_contacts",
        "char_contact_notifications",
        "char_contact_labels",
        "corp_contacts",
        "corp_contact_labels",
        "alliance_contacts",
        "alliance_contact_labels",
        "char_notes",
        "access_groups",
    ];

    for tbl in social_tables {
        // 1. Insert sync error when table dropped
        let mut ins_conn = open_migrated_conn();
        ins_conn.execute(&format!("DROP TABLE {tbl}"), []).unwrap();
        let mut e_fp = TableFingerprints::default();
        {
            let tx = ins_conn.transaction().unwrap();
            assert!(sync_d6_social(&tx, &pop, &mut e_fp).is_err(), "insert missing {tbl}");
        }
        // 2. Load error when table dropped
        let mut s = WorldStateSnapshot::default();
        assert!(load_d6_social_into_snapshot(&ins_conn, &mut s).is_err(), "load missing {tbl}");

        // 3. Delete sync error when table dropped
        let mut del_conn = open_migrated_conn();
        del_conn.execute(&format!("DROP TABLE {tbl}"), []).unwrap();
        let mut p_fp = base_fp.clone();
        {
            let tx = del_conn.transaction().unwrap();
            assert!(sync_d6_social(&tx, &empty, &mut p_fp).is_err(), "delete missing {tbl}");
        }
    }

    // Corrupt JSON rows in each JSON-carrying social table fail load cleanly
    let json_corrupt_sql = [
        "INSERT INTO mail_labels (character_id, labels_json) VALUES (1, '{bad')",
        "INSERT INTO mail_assigned_labels (character_id, mail_id, labels_json) VALUES (1, 1, '{bad')",
        "INSERT INTO mailing_lists (list_id, record_json) VALUES (1, '{bad')",
        "INSERT INTO calendar_events (event_id, record_json) VALUES (1, '{bad')",
        "INSERT INTO bookmark_meta (bookmark_id, record_json) VALUES (1, '{bad')",
        "INSERT INTO bookmark_folders (folder_id, record_json) VALUES (1, '{bad')",
        "INSERT INTO chat_channels (channel_id, record_json) VALUES ('x', '{bad')",
        "INSERT INTO char_contacts (character_id, contacts_json) VALUES (1, '{bad')",
        "INSERT INTO char_contact_notifications (character_id, notifications_json) VALUES (1, '{bad')",
        "INSERT INTO char_contact_labels (character_id, labels_json) VALUES (1, '{bad')",
        "INSERT INTO corp_contacts (corporation_id, contacts_json) VALUES (1, '{bad')",
        "INSERT INTO corp_contact_labels (corporation_id, labels_json) VALUES (1, '{bad')",
        "INSERT INTO alliance_contacts (alliance_id, contacts_json) VALUES (1, '{bad')",
        "INSERT INTO alliance_contact_labels (alliance_id, labels_json) VALUES (1, '{bad')",
        "INSERT INTO access_groups (group_id, members_json) VALUES (1, '{bad')",
    ];
    for sql in json_corrupt_sql {
        let conn = open_migrated_conn();
        conn.execute(sql, []).unwrap();
        let mut s = WorldStateSnapshot::default();
        assert!(load_d6_social_into_snapshot(&conn, &mut s).is_err(), "expected json err for: {sql}");
    }

    // Migration error when conflicting view replaces read_mails or schema_migrations is dropped
    let bad_mig = Connection::open_in_memory().unwrap();
    bad_mig.execute("CREATE VIEW read_mails AS SELECT 1 AS character_id, 2 AS mail_id", []).unwrap();
    assert!(apply_d6_social_migration(&bad_mig).is_err());

    let conn2 = Connection::open_in_memory().unwrap();
    conn2.execute_batch("CREATE TABLE read_mails (character_id INTEGER, mail_id INTEGER); CREATE VIEW deleted_mails AS SELECT 1;").unwrap();
    assert!(apply_d6_social_migration(&conn2).is_err());
    drop(bad_mig);
}

#[test]
fn schema_and_cold_stores_dropped_table_and_corrupt_paths() {
    use crate::sqlite_store::corp_crimewatch_store::{load_d6_corp_crimewatch, sync_d6_corp_crimewatch};
    use crate::sqlite_store::industry_market_pve_store::{load_d6_batch4, sync_d6_batch4};
    use crate::sqlite_store::notes_lp_sequence_store::{load_d6, sync_d6};
    use crate::sqlite_store::schema::{load_or_init_jwt_secret, save_jwt_secret};

    // 1. JWT secret invalid length, non-hex 64-char string, and missing table error paths
    let conn = open_migrated_conn();
    conn.execute(
        "INSERT OR REPLACE INTO auth_secrets (secret_key, secret_hex) VALUES ('jwt_signing_secret', 'short')",
        [],
    )
    .unwrap();
    let s1 = load_or_init_jwt_secret(&conn, || [0x11u8; 32]).unwrap();
    assert_eq!(s1, [0x11u8; 32]);

    conn.execute(
        "INSERT OR REPLACE INTO auth_secrets (secret_key, secret_hex) VALUES ('jwt_signing_secret', ?1)",
        rusqlite::params!["zz".repeat(32)],
    )
    .unwrap();
    let s2 = load_or_init_jwt_secret(&conn, || [0x22u8; 32]).unwrap();
    assert_eq!(s2, [0x22u8; 32]);

    conn.execute("DROP TABLE auth_secrets", []).unwrap();
    assert!(load_or_init_jwt_secret(&conn, || [0x33u8; 32]).is_err());
    assert!(save_jwt_secret(&conn, &[0x44u8; 32]).is_err());

    // 2. PRAGMA query_only triggers apply_migrations and submodule migration error paths cleanly
    let c_ro = Connection::open_in_memory().unwrap();
    c_ro.execute_batch("PRAGMA query_only = ON;").unwrap();
    assert!(apply_migrations(&c_ro).is_err());
    assert!(crate::sqlite_store::auth_store::apply_auth_migration(&c_ro).is_err());
    assert!(crate::sqlite_store::ledger_store::apply_ledger_migration(&c_ro).is_err());
    assert!(crate::sqlite_store::notes_lp_sequence_store::apply_d6_migration(&c_ro).is_err());
    assert!(crate::sqlite_store::social_store::apply_d6_social_migration(&c_ro).is_err());
    assert!(crate::sqlite_store::corp_crimewatch_store::apply_d6_corp_crimewatch_migration(&c_ro).is_err());
    assert!(crate::sqlite_store::industry_market_pve_store::apply_d6_batch4_migration(&c_ro).is_err());
    assert!(crate::sqlite_store::service_domains_store::apply_service_domains_migration(&c_ro).is_err());
    // View collision on items triggers index/alter/schema failure during apply_migrations
    let c_view = Connection::open_in_memory().unwrap();
    c_view.execute("CREATE VIEW schema_migrations AS SELECT 1 AS version", []).unwrap();
    let _ = apply_migrations(&c_view);

    // Read-only SQLite file DB triggers execute_batch/schema errors cleanly
    let tmp = std::env::temp_dir().join(format!("evomu_ro_mig_{}.sqlite", std::process::id()));
    {
        let rw = Connection::open(&tmp).unwrap();
        apply_migrations(&rw).unwrap();
    }
    if let Ok(ro) = Connection::open_with_flags(&tmp, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
        let _ = apply_migrations(&ro);
    }
    let _ = std::fs::remove_file(&tmp);

    // 3. Cold stores unknown keys, dropped tables on sync/load
    let mut c_cw = open_migrated_conn();
    c_cw.execute("INSERT INTO d6_corp_crimewatch_kv (store_key, payload_json) VALUES ('unknown_k', '{}')", []).unwrap();
    let mut cw_snap = WorldStateSnapshot::default();
    load_d6_corp_crimewatch(&c_cw, &mut cw_snap);
    c_cw.execute("DROP TABLE d6_corp_crimewatch_kv", []).unwrap();
    load_d6_corp_crimewatch(&c_cw, &mut cw_snap);
    let mut cw_pop = WorldStateSnapshot::default();
    cw_pop.wars.insert(1, WarRecord {
        war_id: 1, aggressor_corp_id: 1, defender_corp_id: 2, mutual: false, started_unix: 1, active: true,
    });
    let mut fp_cw = TableFingerprints::default();
    {
        let tx = c_cw.transaction().unwrap();
        assert!(sync_d6_corp_crimewatch(&tx, &cw_pop, &mut fp_cw).is_err());
    }

    // notes_lp_sequence_store dropped tables & unknown sequence key
    let c_notes = open_migrated_conn();
    c_notes.execute("INSERT INTO sp6_lifecycle_state (singleton_id, state_json) VALUES (1, '{\"next_alliance_id\":5}')", []).ok();
    c_notes.execute("INSERT INTO corp_lp_balances (corporation_id, issuer_corp_id, lp_balance) VALUES (1, 2, 10)", []).unwrap();
    c_notes.execute("INSERT INTO char_lp_logs (character_id, logs_json) VALUES (1, '[]')", []).unwrap();
    let mut n_snap = WorldStateSnapshot::default();
    load_d6(&c_notes, &mut n_snap);
    for t in ["owner_notes", "corp_lp_balances", "char_lp_logs", "sp6_lifecycle_state"] {
        let mut c2 = open_migrated_conn();
        c2.execute(&format!("DROP TABLE {t}"), []).unwrap();
        let mut s2 = WorldStateSnapshot::default();
        load_d6(&c2, &mut s2);
        let mut pop_n = WorldStateSnapshot::default();
        pop_n.owner_notes.insert(1, OwnerNoteRecord { note_id: 1, character_id: 1, item_id: 1, label: "L".into(), note: "N".into(), timestamp_unix: 1 });
        pop_n.corp_lp_balances.insert((1, 2), 10);
        pop_n.char_lp_logs.insert(1, vec![]);
        let mut fp_n = TableFingerprints::default();
        let tx = c2.transaction().unwrap();
        assert!(sync_d6(&tx, &pop_n, &mut fp_n).is_err());
    }

    // industry_market_pve_store unknown key + dropped tables on load/sync/rollback
    let c_b4 = open_migrated_conn();
    c_b4.execute("INSERT INTO world_kv_blobs (blob_key, blob_json) VALUES ('unknown_blob', '[]')", []).unwrap();
    let mut b4_snap = WorldStateSnapshot::default();
    load_d6_batch4(&c_b4, &mut b4_snap);
    for t in ["world_kv_blobs", "char_mining_ledger"] {
        let mut c2 = open_migrated_conn();
        c2.execute(&format!("DROP TABLE {t}"), []).unwrap();
        let mut s2 = WorldStateSnapshot::default();
        load_d6_batch4(&c2, &mut s2);
        let mut pop_b4 = WorldStateSnapshot::default();
        pop_b4.mining_ledger.push(MiningLedgerRecord {
            character_id: 1, solar_system_id: 30_000_142, type_id: 1230, quantity: 10, date: "d".into(), timestamp_unix: 1,
        });
        pop_b4.quasar_state_blobs.insert("k".into(), "v".into());
        let mut fp_b4 = TableFingerprints::default();
        crate::sqlite_store::industry_market_pve_store::rollback_d6_batch4(&c2, &mut pop_b4, &fp_b4);
        let tx = c2.transaction().unwrap();
        assert!(sync_d6_batch4(&tx, &pop_b4, &mut fp_b4).is_err());
    }
}
