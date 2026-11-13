//! Edge-case, WAL/snapshot conflict, and SQLite error-path tests for `sqlite_store::sync`.

use super::*;
use crate::persistence::WorldRepository;
use crate::sqlite_store::load::load_snapshot_from_sqlite;
use crate::sqlite_store::schema::apply_migrations;
use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[test]
fn sync_dirty_metadata_only_and_shared_arc_market_orders() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldRepository::create_demo_snapshot();
    let cid = 90_000_001;
    snap.market_orders.insert(
        700_500,
        MarketOrderRecord {
            order_id: 700_500,
            owner_id: cid,
            station_id: 60_003_760,
            region_id: 10_000_002,
            type_id: 34,
            is_bid: false,
            price: IskCents(1_000),
            vol_entered: 100,
            vol_remaining: 100,
            min_volume: 1,
            is_npc: true,
            escrow: IskCents(0),
        },
    );
    let fp = replace_sqlite_from_snapshot(&mut conn, &snap).unwrap();

    // Clone fp so Arc::make_mut(&mut fp.market_orders) must copy-on-write
    let mut fp_cow = fp.clone();
    assert!(Arc::strong_count(&fp_cow.market_orders) >= 2);

    // No-op sync produces empty delta and does not set full_snapshot
    {
        let tx = conn.transaction().unwrap();
        let delta = sync_dirty_to_sqlite(&tx, &snap, &mut fp_cow, true).unwrap();
        tx.commit().unwrap();
        assert!(delta.full_snapshot.is_none());
        assert!(delta.upserted_characters.is_empty());
        assert!(delta.upserted_market_orders.is_empty());
    }

    // Only next_entity_id and wal_seq change -> hot-only, no cold_changed
    snap.next_entity_id += 1;
    snap.wal_seq += 1;
    snap.market_orders.get_mut(&700_500).unwrap().vol_remaining = 80;
    {
        let tx = conn.transaction().unwrap();
        let delta = sync_dirty_to_sqlite(&tx, &snap, &mut fp_cow, true).unwrap();
        tx.commit().unwrap();
        assert!(delta.full_snapshot.is_none());
        assert_eq!(delta.upserted_market_orders.len(), 1);
        assert_eq!(delta.upserted_market_orders[0].vol_remaining, 80);
    }
    // Original fp still has the old fingerprint for 700_500
    assert_ne!(fp.market_orders.get(&700_500), fp_cow.market_orders.get(&700_500));

    // Only markets_seeded flips -> cold_changed = true -> emits full_snapshot when need_wal_delta = true
    snap.markets_seeded = !snap.markets_seeded;
    {
        let tx = conn.transaction().unwrap();
        let delta = sync_dirty_to_sqlite(&tx, &snap, &mut fp_cow, true).unwrap();
        tx.commit().unwrap();
        assert!(delta.full_snapshot.is_some());
    }
}

#[test]
fn sync_dirty_wallet_journal_and_mail_truncation_and_prefix_mutation_rewrite() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldRepository::create_demo_snapshot();
    let cid = 90_000_001;
    snap.wallet_journal = vec![
        WalletJournalEntry {
            entry_id: 1,
            character_id: cid,
            entry_type: "Bounty".into(),
            amount: IskCents(100),
            balance_after: IskCents(100),
            reason: "r1".into(),
        },
        WalletJournalEntry {
            entry_id: 2,
            character_id: cid,
            entry_type: "Bounty".into(),
            amount: IskCents(200),
            balance_after: IskCents(300),
            reason: "r2".into(),
        },
    ];
    snap.mails = vec![
        MailRecord {
            message_id: 10,
            sender_id: cid,
            recipient_id: cid,
            subject: "Subj1".into(),
            body: "Body1".into(),
        },
        MailRecord {
            message_id: 11,
            sender_id: cid,
            recipient_id: cid,
            subject: "Subj2".into(),
            body: "Body2".into(),
        },
    ];
    let mut fp = replace_sqlite_from_snapshot(&mut conn, &snap).unwrap();

    // Mutate prefix item[0] in both wallet_journal and mails (same len) -> triggers DELETE + re-insert
    snap.wallet_journal[0].reason = "mutated_prefix".into();
    snap.mails[0].body = "mutated_body".into();
    {
        let tx = conn.transaction().unwrap();
        let delta = sync_dirty_to_sqlite(&tx, &snap, &mut fp, true).unwrap();
        tx.commit().unwrap();
        assert!(delta.full_snapshot.is_some());
        assert_eq!(delta.appended_wallet_journal.len(), 2);
    }

    // Truncate to len=1 (non-empty) -> triggers DELETE + re-insert of remaining 1 entry
    snap.wallet_journal.truncate(1);
    snap.mails.truncate(1);
    {
        let tx = conn.transaction().unwrap();
        let delta = sync_dirty_to_sqlite(&tx, &snap, &mut fp, true).unwrap();
        tx.commit().unwrap();
        assert!(delta.full_snapshot.is_some());
        assert_eq!(delta.appended_wallet_journal.len(), 1);
        assert_eq!(fp.wallet_journal_len, 1);
        assert_eq!(fp.mails_len, 1);
    }

    let (loaded, _) = load_snapshot_from_sqlite(&conn).unwrap();
    assert_eq!(loaded.wallet_journal.len(), 1);
    assert_eq!(loaded.wallet_journal[0].reason, "mutated_prefix");
    assert_eq!(loaded.mails.len(), 1);
    assert_eq!(loaded.mails[0].body, "mutated_body");
}

#[test]
fn sync_dirty_skill_in_place_update_and_optional_field_paths() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let mut snap = WorldRepository::create_demo_snapshot();
    let cid = 90_000_001;
    let mut fp = replace_sqlite_from_snapshot(&mut conn, &snap).unwrap();

    // Insert banned account, skill queue with started_at_unix=Some, industry job with all Some optionals,
    // bookmark with Some(item_id) & Some(folder_id) & is_shared=true, killmail with corp/alliance IDs, etc.
    snap.accounts.insert(
        "banned_user".into(),
        AccountRecord {
            user_id: 999,
            username: "banned_user".into(),
            password_token: "hash".into(),
            role: 0,
            banned_until_unix: Some(1_900_000_000),
            ban_reason: Some("RMT".into()),
        },
    );
    snap.skills.entry(cid).or_default().insert(
        3412,
        TrainedSkillRecord {
            type_id: 3412,
            level: 3,
            skill_points: 24_000.0,
        },
    );
    snap.skill_queues.insert(
        cid,
        vec![SkillQueueEntry {
            type_id: 3412,
            target_level: 4,
            start_sp: 24_000.0,
            end_sp: 135_765.0,
            duration_seconds: 12_000.0,
            started_at_unix: Some(1_700_000_000),
        }],
    );
    snap.industry_jobs.insert(
        900_001,
        IndustryJobRecord {
            job_id: 900_001,
            installer_id: cid,
            facility_id: 60_003_760,
            blueprint_item_id: 100_000_001,
            blueprint_type_id: 683,
            activity_id: 8,
            runs: 5,
            product_type_id: 587,
            status: "Delivered".into(),
            duration_seconds: 600,
            start_unix: 100,
            blueprint_location_id: 60_003_760,
            output_location_id: 60_003_760,
            cost: Some(15_000.5),
            licensed_runs: Some(10),
            probability: Some(0.35),
            completed_unix: Some(700),
            completed_character_id: Some(cid),
        },
    );
    snap.bookmarks.insert(
        900_002,
        BookmarkRecord {
            bookmark_id: 900_002,
            owner_id: cid,
            label: "Shared BM".into(),
            solar_system_id: 30_000_142,
            x: 10.0,
            y: 20.0,
            z: 30.0,
            item_id: Some(60_003_760),
            memo: "memo".into(),
            folder_id: Some(1),
            is_shared: true,
        },
    );
    snap.killmails.insert(
        900_003,
        KillmailRecord {
            kill_id: 900_003,
            victim_character_id: cid,
            attacker_character_id: 90_000_002,
            solar_system_id: 30_000_142,
            ship_type_id: 587,
            total_value_isk: 1_234_567.0,
            killed_unix: 1_700_000_123,
            victim_corp_id: 98_000_001,
            victim_alliance_id: Some(99_000_001),
            attacker_corp_id: 98_000_002,
            attacker_alliance_id: None,
            ..Default::default()
        },
    );
    snap.kill_rights.insert(
        900_004,
        KillRightRecord {
            kill_right_id: 900_004,
            owner_id: cid,
            target_id: 90_000_002,
            activation_fee: IskCents(500_000),
            is_active_suspect: true,
        },
    );
    snap.wars.insert(
        900_005,
        WarRecord {
            war_id: 900_005,
            aggressor_corp_id: 98_000_001,
            defender_corp_id: 98_000_002,
            mutual: true,
            started_unix: 1_700_000_000,
            active: false,
        },
    );
    snap.npe_progress.insert(
        cid,
        NpeProgressRecord {
            character_id: cid,
            node_graph_id: 2,
            current_node_id: 99,
            completed: true,
            career_goal_progress: HashMap::from([(1, 10)]),
            claimed_goals: HashSet::from([1]),
        },
    );
    snap.fleets.insert(
        900_006,
        FleetRecord {
            fleet_id: 900_006,
            boss_id: cid,
            free_move: true,
            is_registered: true,
            ..Default::default()
        },
    );

    {
        let tx = conn.transaction().unwrap();
        let delta = sync_dirty_to_sqlite(&tx, &snap, &mut fp, true).unwrap();
        tx.commit().unwrap();
        assert!(delta.full_snapshot.is_some());
    }

    // Update skill SP in place, update existing character & existing item (ON CONFLICT / REPLACE)
    snap.skills.get_mut(&cid).unwrap().get_mut(&3412).unwrap().level = 4;
    snap.characters.get_mut(&cid).unwrap().activity_status = Some("InSpace".into());
    snap.items.get_mut(&100_000_001).unwrap().singleton = true;
    {
        let tx = conn.transaction().unwrap();
        sync_dirty_to_sqlite(&tx, &snap, &mut fp, false).unwrap();
        tx.commit().unwrap();
    }

    let (loaded, _) = load_snapshot_from_sqlite(&conn).unwrap();
    assert_eq!(loaded.skills[&cid][&3412].level, 4);
    assert_eq!(loaded.accounts["banned_user"].ban_reason.as_deref(), Some("RMT"));
    assert_eq!(loaded.industry_jobs[&900_001].licensed_runs, Some(10));
    assert!(loaded.bookmarks[&900_002].is_shared);
    assert!(loaded.kill_rights[&900_004].is_active_suspect);
    assert!(loaded.npe_progress[&cid].completed);
}

#[test]
fn sync_dirty_wal_file_mode_and_busy_lock_conflict() {
    let dir = std::env::temp_dir().join(format!("evomu_wal_sync_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("wal_sync_test.sqlite");
    let _ = std::fs::remove_file(&db_path);

    let mut writer = Connection::open(&db_path).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer.busy_timeout(std::time::Duration::from_millis(0)).unwrap();
    apply_migrations(&writer).unwrap();

    let mut snap = WorldRepository::create_demo_snapshot();
    let mut fp = replace_sqlite_from_snapshot(&mut writer, &snap).unwrap();

    // Another connection holds an IMMEDIATE transaction lock -> writer transaction_with_behavior(Immediate) fails SQLITE_BUSY
    let mut locker = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .unwrap();
    let lock_tx = locker
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();

    snap.wal_seq += 1;
    let busy_err = replace_sqlite_from_snapshot(&mut writer, &snap).unwrap_err();
    assert!(
        busy_err.to_lowercase().contains("locked") || busy_err.to_lowercase().contains("busy"),
        "expected SQLITE_BUSY error, got: {busy_err}"
    );
    drop(lock_tx);

    // Once lock released, replace_sqlite_from_snapshot succeeds in WAL mode
    let fp2 = replace_sqlite_from_snapshot(&mut writer, &snap).unwrap();
    assert_eq!(fp2.wal_seq, snap.wal_seq);
    fp.wal_seq = snap.wal_seq;
    drop(locker);
    drop(writer);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn replace_sqlite_and_sync_dirty_propagate_sqlite_errors_on_all_table_paths() {
    // 1. Missing schema -> replace_sqlite_from_snapshot fails on execute_batch
    let mut empty_conn = Connection::open_in_memory().unwrap();
    let empty_snap = WorldStateSnapshot::default();
    assert!(replace_sqlite_from_snapshot(&mut empty_conn, &empty_snap).is_err());

    // 2. Read-only connection fails transaction start
    let dir = std::env::temp_dir().join(format!("evomu_ro_sync_test_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("ro.sqlite");
    let _ = std::fs::remove_file(&db_path);
    {
        let c = Connection::open(&db_path).unwrap();
        apply_migrations(&c).unwrap();
    }
    let mut ro = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert!(replace_sqlite_from_snapshot(&mut ro, &empty_snap).is_err());

    // 3. Inject BEFORE INSERT / BEFORE DELETE triggers on every table synced by sync.rs
    // to exercise every `.map_err(|e| e.to_string())?` branch cleanly.
    let mut snap = WorldRepository::create_demo_snapshot();
    let cid = 90_000_001;
    snap.accounts.insert(
        "trig_acc".into(),
        AccountRecord {
            user_id: 700,
            username: "trig_acc".into(),
            password_token: "p".into(),
            role: 0,
            banned_until_unix: None,
            ban_reason: None,
        },
    );
    snap.skills.entry(cid).or_default().insert(
        3300,
        TrainedSkillRecord {
            type_id: 3300,
            level: 1,
            skill_points: 250.0,
        },
    );
    snap.skill_queues.insert(
        cid,
        vec![SkillQueueEntry {
            type_id: 3300,
            target_level: 2,
            start_sp: 250.0,
            end_sp: 1414.0,
            duration_seconds: 100.0,
            started_at_unix: None,
        }],
    );
    snap.jump_clones.insert(
        cid,
        vec![JumpCloneRecord {
            jump_clone_id: 1,
            character_id: cid,
            location_id: 60_003_760,
            implants: vec![],
        }],
    );
    snap.active_implants.insert(cid, vec![9899]);
    snap.saved_fittings.insert(
        1,
        SavedFittingRecord {
            fitting_id: 1,
            owner_id: cid,
            name: "F".into(),
            ship_type_id: 587,
            description: String::new(),
            items: vec![],
        },
    );
    snap.market_orders.insert(
        1,
        MarketOrderRecord {
            order_id: 1,
            owner_id: cid,
            station_id: 60_003_760,
            region_id: 10_000_002,
            type_id: 34,
            is_bid: false,
            price: IskCents(100),
            vol_entered: 10,
            vol_remaining: 10,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents(0),
        },
    );
    snap.wallet_journal.push(WalletJournalEntry {
        entry_id: 1,
        character_id: cid,
        entry_type: "T".into(),
        amount: IskCents(10),
        balance_after: IskCents(10),
        reason: "R".into(),
    });
    snap.contracts.insert(
        1,
        ContractRecord {
            contract_id: 1,
            issuer_id: cid,
            ..Default::default()
        },
    );
    snap.industry_jobs.insert(
        1,
        IndustryJobRecord {
            job_id: 1,
            installer_id: cid,
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
            last_extracted_unix: 100,
        },
    );
    snap.active_missions.insert(
        (cid, 1),
        ActiveMissionRecord {
            mission_id: 1,
            character_id: cid,
            agent_id: 3_018_922,
            solar_system_id: 30_000_142,
            state: "Offered".into(),
            current_room_id: 0,
            npcs_remaining: 0,
            spawned_npc_ball_ids: vec![],
            bookmark_id: None,
        },
    );
    snap.mission_tally.insert(cid, 1);
    snap.standing_transactions.insert(
        cid,
        vec![StandingTransactionRecord {
            event_id: 1,
            from_id: 1,
            to_id: cid,
            modification: 0.5,
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
        1,
        BookmarkRecord {
            bookmark_id: 1,
            owner_id: cid,
            label: "B".into(),
            solar_system_id: 30_000_142,
            x: 0.0,
            y: 0.0,
            z: 0.0,
            item_id: None,
            memo: String::new(),
            folder_id: None,
            is_shared: false,
        },
    );
    snap.mails.push(MailRecord {
        message_id: 1,
        sender_id: cid,
        recipient_id: cid,
        subject: "S".into(),
        body: "B".into(),
    });
    snap.fleets.insert(
        1,
        FleetRecord {
            fleet_id: 1,
            boss_id: cid,
            ..Default::default()
        },
    );
    snap.alliances.insert(
        1,
        AllianceRecord {
            alliance_id: 1,
            name: "A".into(),
            short_name: "A".into(),
            executor_corp_id: 98_000_001,
            member_corp_ids: HashSet::new(),
        },
    );
    snap.wars.insert(
        1,
        WarRecord {
            war_id: 1,
            aggressor_corp_id: 98_000_001,
            defender_corp_id: 98_000_002,
            mutual: false,
            started_unix: 1,
            active: true,
        },
    );
    snap.corp_projects.insert(
        1,
        CorpProjectRecord {
            project_id: 1,
            corporation_id: 98_000_001,
            name: "P".into(),
            target_quantity: 1,
            current_quantity: 0,
            isk_per_unit: IskCents(1),
            escrow_remaining: IskCents(1),
        },
    );
    snap.killmails.insert(
        1,
        KillmailRecord {
            kill_id: 1,
            victim_character_id: cid,
            attacker_character_id: cid,
            solar_system_id: 30_000_142,
            ship_type_id: 587,
            total_value_isk: 1.0,
            ..Default::default()
        },
    );
    snap.kill_rights.insert(
        1,
        KillRightRecord {
            kill_right_id: 1,
            owner_id: cid,
            target_id: cid,
            activation_fee: IskCents(1),
            is_active_suspect: false,
        },
    );
    snap.structures.insert(
        1,
        UpwellStructureRecord {
            structure_id: 1,
            type_id: 35832,
            owner_corp_id: 98_000_001,
            solar_system_id: 30_000_142,
            name: "S".into(),
            state: "ShieldVulnerable".into(),
            ..Default::default()
        },
    );
    snap.committed_idempotency_keys.insert((cid, "k".into()));
    snap.licensed_skins.insert(cid, vec![1]);
    snap.applied_ship_skins.insert(100_000_001, 1);
    snap.asteroids.insert(
        1,
        AsteroidRecord {
            asteroid_id: 1,
            solar_system_id: 30_000_142,
            belt_id: 1,
            type_id: 1230,
            quantity: 10,
            initial_quantity: 10,
            position: (0.0, 0.0, 0.0),
            radius: 10.0,
        },
    );

    let tables_insert = [
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
        "asteroids",
    ];
    for tbl in tables_insert {
        let mut c = Connection::open_in_memory().unwrap();
        apply_migrations(&c).unwrap();
        c.execute_batch(&format!(
            "CREATE TRIGGER fail_ins_{tbl} BEFORE INSERT ON {tbl} BEGIN SELECT RAISE(ABORT, 'inj_ins_{tbl}'); END;"
        ))
        .unwrap();
        let tx = c.transaction().unwrap();
        let mut fp = TableFingerprints::default();
        let err = sync_dirty_to_sqlite(&tx, &snap, &mut fp, false).unwrap_err();
        assert!(err.contains(&format!("inj_ins_{tbl}")), "unexpected err for {tbl}: {err}");
    }

    // Also test DELETE triggers when clearing records from a populated DB
    let mut base_conn = Connection::open_in_memory().unwrap();
    apply_migrations(&base_conn).unwrap();
    let base_fp = replace_sqlite_from_snapshot(&mut base_conn, &snap).unwrap();
    let mut cleared = snap.clone();
    cleared.accounts.clear();
    cleared.characters.clear();
    cleared.skills.clear();
    cleared.skill_queues.clear();
    cleared.jump_clones.clear();
    cleared.active_implants.clear();
    cleared.items.clear();
    cleared.saved_fittings.clear();
    cleared.market_orders.clear();
    cleared.wallet_journal.clear();
    cleared.contracts.clear();
    cleared.industry_jobs.clear();
    cleared.planetary_colonies.clear();
    cleared.active_missions.clear();
    cleared.mission_tally.clear();
    cleared.standing_transactions.clear();
    cleared.npe_progress.clear();
    cleared.bookmarks.clear();
    cleared.mails.clear();
    cleared.fleets.clear();
    cleared.corporations.clear();
    cleared.alliances.clear();
    cleared.wars.clear();
    cleared.corp_projects.clear();
    cleared.killmails.clear();
    cleared.kill_rights.clear();
    cleared.structures.clear();
    cleared.system_states.clear();
    cleared.committed_idempotency_keys.clear();
    cleared.licensed_skins.clear();
    cleared.applied_ship_skins.clear();
    cleared.asteroids.clear();

    for tbl in tables_insert {
        if tbl == "world_meta" {
            continue;
        }
        let mut c = Connection::open_in_memory().unwrap();
        apply_migrations(&c).unwrap();
        let mut fp = replace_sqlite_from_snapshot(&mut c, &snap).unwrap();
        assert_eq!(fp.characters.len(), base_fp.characters.len());
        c.execute_batch(&format!(
            "CREATE TRIGGER fail_del_{tbl} BEFORE DELETE ON {tbl} BEGIN SELECT RAISE(ABORT, 'inj_del_{tbl}'); END;"
        ))
        .unwrap();
        let tx = c.transaction().unwrap();
        let err = sync_dirty_to_sqlite(&tx, &cleared, &mut fp, false).unwrap_err();
        assert!(err.contains(&format!("inj_del_{tbl}")), "unexpected del err for {tbl}: {err}");
    }
    drop(ro);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn changeset_and_selective_cold_domain_sync_branches() {
    let base = WorldRepository::create_demo_snapshot();
    let mut after;
    let cid1 = 90_000_001;
    let cid2 = 90_000_002;
    let item_keep = 100_000_001;
    let item_del = 100_000_777;
    let ord_upd = 710_001;
    let ord_del = 710_002;

    let mut snap = base;
    snap.items.insert(
        item_del,
        InventoryItemRecord {
            item_id: item_del,
            type_id: 34,
            owner_id: cid1,
            location_id: 60_003_760,
            flag: InventoryFlag::Hangar,
            quantity: 50,
            singleton: false,
            version: 1,
            ..Default::default()
        },
    );
    for (oid, price) in [(ord_upd, 1_000), (ord_del, 2_000)] {
        snap.market_orders.insert(
            oid,
            MarketOrderRecord {
                order_id: oid,
                owner_id: cid1,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents(price),
                vol_entered: 10,
                vol_remaining: 10,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    }
    snap.skills.entry(cid1).or_default().insert(
        3300,
        TrainedSkillRecord {
            type_id: 3300,
            level: 2,
            skill_points: 1_415.0,
        },
    );
    snap.skill_queues.insert(
        cid1,
        vec![SkillQueueEntry {
            type_id: 3300,
            target_level: 3,
            start_sp: 1_415.0,
            end_sp: 8_000.0,
            duration_seconds: 600.0,
            started_at_unix: Some(1_700_000_000),
        }],
    );

    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    let fp = replace_sqlite_from_snapshot(&mut conn, &snap).unwrap();

    after = snap.clone();
    after.characters.get_mut(&cid1).unwrap().balance = IskCents(999_888_777);
    after.skills.get_mut(&cid1).unwrap().get_mut(&3300).unwrap().skill_points = 5_000.0;
    after.skill_queues.get_mut(&cid1).unwrap()[0].start_sp = 5_000.0;
    after.items.get_mut(&item_keep).unwrap().quantity += 7;
    after.items.remove(&item_del);
    after.market_orders.get_mut(&ord_upd).unwrap().vol_remaining = 4;
    after.market_orders.remove(&ord_del);
    after.asteroids.insert(
        88_001,
        AsteroidRecord {
            asteroid_id: 88_001,
            solar_system_id: 30_000_142,
            belt_id: 40_000_001,
            type_id: 1230,
            quantity: 500,
            initial_quantity: 500,
            position: (1.0, 2.0, 3.0),
            radius: 50.0,
        },
    );
    after.crimewatch_states.insert(
        cid1,
        PersistedCrimeWatchState {
            character_id: cid1,
            weapon_timer_until_unix: 12_345,
            ..Default::default()
        },
    );

    let changes = crate::tx_engine::ChangeSet::diff(1, &snap, &after);
    assert_eq!(changes.characters.len(), 1);
    assert_eq!(changes.skills.len(), 1);
    assert_eq!(changes.skill_queues.len(), 1);
    assert_eq!(changes.items.len(), 2);
    assert_eq!(changes.market_orders.len(), 2);
    assert!(changes.cold_domains.part4_world_tail);
    assert!(changes.cold_domains.d6_corp_crimewatch);
    assert!(changes.cold_domains.crimewatch_changed);

    let mut staged_cold = fp.clone();
    {
        let tx = conn.transaction().unwrap();
        let mut exec = crate::tx_engine::TxSqlExecutor::new(&tx, crate::TxFaultPlan::None);
        let (delta, hot_fp) = crate::tx_engine::apply_changeset_to_sqlite(
            &mut exec,
            &after,
            &changes,
            &fp,
            &mut staged_cold,
            true,
        )
        .unwrap();
        tx.commit().unwrap();
        assert!(delta.dirty);
        assert_eq!(delta.removed_items, vec![item_del]);
        assert_eq!(delta.removed_market_orders, vec![ord_del]);
        let mut next_fp = staged_cold;
        hot_fp.apply_to(&mut next_fp);
        assert!(next_fp.asteroids.contains_key(&88_001));
    }

    let (reloaded, _) = load_snapshot_from_sqlite(&conn).unwrap();
    assert_eq!(reloaded.characters[&cid1].balance, IskCents(999_888_777));
    assert!(!reloaded.items.contains_key(&item_del));
    assert!(!reloaded.market_orders.contains_key(&ord_del));
    assert_eq!(reloaded.market_orders[&ord_upd].vol_remaining, 4);
    assert!(reloaded.asteroids.contains_key(&88_001));
    assert_eq!(reloaded.crimewatch_states[&cid1].weapon_timer_until_unix, 12_345);
    let _ = cid2;
}
