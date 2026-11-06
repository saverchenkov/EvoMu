//! Mutation-killing and branch-coverage unit tests for `sqlite_store`
//! (`fingerprints`, `load`, `rollback`, `sync`, and `sync_ext`).

#![forbid(unsafe_code)]

use crate::persistence::WorldRepository;
use crate::sqlite_store::fingerprints::{compute_fingerprints, TableFingerprints};
use crate::sqlite_store::load::load_snapshot_from_sqlite;
use crate::sqlite_store::rollback::rollback_dirty_from_sqlite;
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

    rollback_dirty_from_sqlite(&conn, &mut working, &fp).expect("rollback full snapshot");
    assert_eq!(working.next_entity_id, base.next_entity_id);
    assert_eq!(working.wal_seq, base.wal_seq);
    assert_eq!(working.markets_seeded, base.markets_seeded);
    assert_eq!(working.accounts.len(), base.accounts.len());
    assert_eq!(working.characters.len(), base.characters.len());
    assert_eq!(working.items.len(), base.items.len());
    assert_eq!(working.mission_tally, base.mission_tally);
    assert_eq!(working.applied_ship_skins, base.applied_ship_skins);
    assert_eq!(working.mining_ledger.len(), base.mining_ledger.len());
    assert_eq!(working.asteroids.len(), base.asteroids.len());
    assert_eq!(working.character_doll_info, base.character_doll_info);

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
