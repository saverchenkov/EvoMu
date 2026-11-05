//! Unit and mutation-killing tests for `sqlite_store::sync`.

use super::*;
use crate::persistence::WorldRepository;
use crate::sqlite_store::load::load_snapshot_from_sqlite;
use crate::sqlite_store::schema::apply_migrations;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

#[test]
fn sync_dirty_to_sqlite_hot_path_emits_incremental_wal_delta_without_full_snapshot() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldRepository::create_demo_snapshot();
    let cid = 90_000_001;
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
    let mut fp = replace_sqlite_from_snapshot(&mut conn, &snap).unwrap();

    // Perform strictly hot-path mutations:
    // -wal_seq, next_entity_id
    // -character update
    // -item upsert + item remove
    // -market_order upsert + market_order remove
    // -wallet_journal append
    // -contract upsert
    // -system_state upsert
    // -idempotency_key add
    snap.wal_seq += 1;
    snap.next_entity_id += 5;
    snap.characters.get_mut(&cid).unwrap().balance.0 += 50_000;
    snap.items.remove(&100_000_002);
    snap.items.get_mut(&100_000_001).unwrap().quantity += 10;
    snap.market_orders.remove(&700_100);
    snap.market_orders.insert(
        700_101,
        MarketOrderRecord {
            order_id: 700_101,
            owner_id: cid,
            station_id: 60_003_760,
            region_id: 10_000_002,
            type_id: 35,
            is_bid: true,
            price: IskCents(600),
            vol_entered: 20,
            vol_remaining: 20,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents(12_000),
        },
    );
    snap.wallet_journal.push(WalletJournalEntry {
        entry_id: 800_100,
        character_id: cid,
        entry_type: "MarketEscrow".into(),
        amount: IskCents(-12_000),
        balance_after: IskCents(99_988_000),
        reason: "Order".into(),
    });
    snap.contracts.insert(
        600_100,
        ContractRecord {
            contract_id: 600_100,
            issuer_id: cid,
            contract_type: "ItemExchange".into(),
            status: "Outstanding".into(),
            start_station_id: 60_003_760,
            end_station_id: 60_003_760,
            price: IskCents(5000),
            ..Default::default()
        },
    );
    snap.system_states
        .get_mut(&30_000_142)
        .unwrap()
        .ess_main_bank
        .0 += 1_000;
    snap.committed_idempotency_keys
        .insert((cid, "hot_idem_1".into()));

    let tx = conn.transaction().unwrap();
    let delta = sync_dirty_to_sqlite(&tx, &snap, &mut fp, true).unwrap();
    tx.commit().unwrap();

    assert!(
        delta.full_snapshot.is_none(),
        "hot-only changes must not trigger full_snapshot"
    );
    assert_eq!(delta.seq, snap.wal_seq);
    assert_eq!(delta.next_entity_id, snap.next_entity_id);
    assert_eq!(delta.upserted_characters.len(), 1);
    assert_eq!(delta.upserted_characters[0].character_id, cid);
    assert_eq!(delta.removed_items, vec![100_000_002]);
    assert_eq!(delta.upserted_items.len(), 1);
    assert_eq!(delta.upserted_items[0].item_id, 100_000_001);
    assert_eq!(delta.removed_market_orders, vec![700_100]);
    assert_eq!(delta.upserted_market_orders.len(), 1);
    assert_eq!(delta.upserted_market_orders[0].order_id, 700_101);
    assert_eq!(delta.appended_wallet_journal.len(), 1);
    assert_eq!(delta.appended_wallet_journal[0].entry_id, 800_100);
    assert_eq!(delta.upserted_contracts.len(), 1);
    assert_eq!(delta.upserted_contracts[0].contract_id, 600_100);
    assert_eq!(delta.upserted_system_states.len(), 1);
    assert_eq!(delta.added_idempotency_keys, vec![(cid, "hot_idem_1".into())]);
}

#[test]
fn sync_dirty_to_sqlite_cold_tables_upserts_rewrites_and_deletions() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldRepository::create_demo_snapshot();
    let cid = 90_000_001;
    let mut fp = replace_sqlite_from_snapshot(&mut conn, &snap).unwrap();

    // Populate all cold tables and sync with need_wal_delta = true
    snap.markets_seeded = true;
    snap.accounts.insert(
        "temp_acc".into(),
        AccountRecord {
            user_id: 500,
            username: "temp_acc".into(),
            password_token: "t".into(),
            role: 1,
            banned_until_unix: None,
            ban_reason: None,
        },
    );
    snap.skills.entry(cid).or_default().insert(
        3300,
        TrainedSkillRecord {
            type_id: 3300,
            level: 2,
            skill_points: 1415.0,
        },
    );
    snap.skills.entry(cid).or_default().insert(
        3301,
        TrainedSkillRecord {
            type_id: 3301,
            level: 1,
            skill_points: 250.0,
        },
    );
    snap.skill_queues.insert(
        cid,
        vec![SkillQueueEntry {
            type_id: 3300,
            target_level: 3,
            start_sp: 1415.0,
            end_sp: 8000.0,
            duration_seconds: 900.0,
            started_at_unix: None,
        }],
    );
    snap.jump_clones.insert(
        cid,
        vec![JumpCloneRecord {
            jump_clone_id: 400_200,
            character_id: cid,
            location_id: 60_003_760,
            implants: vec![],
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
    snap.mission_tally.insert(cid, 2);
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
    snap.npe_progress.insert(
        cid,
        NpeProgressRecord {
            character_id: cid,
            node_graph_id: 1,
            current_node_id: 1,
            completed: false,
            career_goal_progress: HashMap::new(),
            claimed_goals: HashSet::new(),
        },
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
            executor_corp_id: 98_000_001,
            member_corp_ids: HashSet::new(),
        },
    );
    snap.wars.insert(
        650_200,
        WarRecord {
            war_id: 650_200,
            aggressor_corp_id: 98_000_001,
            defender_corp_id: 98_000_002,
            mutual: false,
            started_unix: 1000,
            active: true,
        },
    );
    snap.corp_projects.insert(
        660_200,
        CorpProjectRecord {
            project_id: 660_200,
            corporation_id: 98_000_001,
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
            owner_corp_id: 98_000_001,
            solar_system_id: 30_000_142,
            name: "S".into(),
            position: [0.0; 3],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 10,
            ..Default::default()
        },
    );
    snap.committed_idempotency_keys.insert((cid, "idem_del".into()));
    snap.licensed_skins.insert(cid, vec![55]);
    snap.applied_ship_skins.insert(100_000_001, 55);
    snap.asteroids.insert(
        70_000_200,
        AsteroidRecord {
            asteroid_id: 70_000_200,
            solar_system_id: 30_000_142,
            belt_id: 1,
            type_id: 1230,
            quantity: 100,
            initial_quantity: 100,
            position: (0.0, 0.0, 0.0),
            radius: 10.0,
        },
    );

    {
        let tx = conn.transaction().unwrap();
        let delta = sync_dirty_to_sqlite(&tx, &snap, &mut fp, true).unwrap();
        assert!(delta.full_snapshot.is_some());
        tx.commit().unwrap();
    }

    // Remove a single skill (3301) while keeping character in `skills`, and rewrite wallet_journal & mails in place
    snap.skills.get_mut(&cid).unwrap().remove(&3301);
    snap.wallet_journal[0].reason = "rewritten_reason".into();
    snap.mails[0].subject = "rewritten_subj".into();
    {
        let tx = conn.transaction().unwrap();
        let delta = sync_dirty_to_sqlite(&tx, &snap, &mut fp, false).unwrap();
        assert!(delta.full_snapshot.is_none());
        tx.commit().unwrap();
    }

    // Now delete all cold records and remove character 90_000_002 to exercise all DELETE branches
    snap.accounts.remove("temp_acc");
    snap.characters.remove(&90_000_002);
    snap.skills.clear();
    snap.skill_queues.clear();
    snap.jump_clones.clear();
    snap.active_implants.clear();
    snap.saved_fittings.clear();
    snap.contracts.clear();
    snap.industry_jobs.clear();
    snap.planetary_colonies.clear();
    snap.active_missions.clear();
    snap.mission_tally.clear();
    snap.standing_transactions.clear();
    snap.npe_progress.clear();
    snap.bookmarks.clear();
    snap.mails.clear();
    snap.fleets.clear();
    snap.corporations.remove(&98_000_002);
    snap.alliances.clear();
    snap.wars.clear();
    snap.corp_projects.clear();
    snap.killmails.clear();
    snap.kill_rights.clear();
    snap.structures.clear();
    snap.system_states.remove(&30_002_187);
    snap.committed_idempotency_keys.clear();
    snap.licensed_skins.clear();
    snap.applied_ship_skins.clear();
    snap.asteroids.clear();

    {
        let tx = conn.transaction().unwrap();
        let delta = sync_dirty_to_sqlite(&tx, &snap, &mut fp, true).unwrap();
        assert!(delta.full_snapshot.is_some());
        tx.commit().unwrap();
    }

    let (loaded, _) = load_snapshot_from_sqlite(&conn).unwrap();
    assert!(!loaded.accounts.contains_key("temp_acc"));
    assert!(!loaded.characters.contains_key(&90_000_002));
    assert!(loaded.skills.is_empty());
    assert!(loaded.skill_queues.is_empty());
    assert!(loaded.jump_clones.is_empty());
    assert!(loaded.active_implants.is_empty());
    assert!(loaded.saved_fittings.is_empty());
    assert!(loaded.contracts.is_empty());
    assert!(loaded.industry_jobs.is_empty());
    assert!(loaded.planetary_colonies.is_empty());
    assert!(loaded.active_missions.is_empty());
    assert!(loaded.mission_tally.is_empty());
    assert!(loaded.standing_transactions.is_empty());
    assert!(loaded.npe_progress.is_empty());
    assert!(loaded.bookmarks.is_empty());
    assert!(loaded.mails.is_empty());
    assert!(loaded.fleets.is_empty());
    assert!(!loaded.corporations.contains_key(&98_000_002));
    assert!(loaded.alliances.is_empty());
    assert!(loaded.wars.is_empty());
    assert!(loaded.corp_projects.is_empty());
    assert!(loaded.killmails.is_empty());
    assert!(loaded.kill_rights.is_empty());
    assert!(loaded.structures.is_empty());
    assert!(!loaded.system_states.contains_key(&30_002_187));
    assert!(loaded.committed_idempotency_keys.is_empty());
    assert!(loaded.licensed_skins.is_empty());
    assert!(loaded.applied_ship_skins.is_empty());
    assert!(loaded.asteroids.is_empty());
    assert_eq!(loaded.wallet_journal[0].reason, "rewritten_reason");
}
