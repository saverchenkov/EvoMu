use crate::accounts::*;
use crate::persistence::*;
use crate::types::*;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};

#[test]
fn test_move_item_ownership_idempotency_and_version_conflict() {
    let repo = WorldRepository::with_demo_seed();

    let res = repo.move_item_transactional(
        90_000_002,
        100_000_003,
        60_003_760,
        InventoryFlag::Cargo,
        None,
        Some("shared-key-1"),
    );
    assert!(matches!(res, Err(DbError::ItemOwnershipViolation { .. })));

    // Moving the currently boarded active ship (100_000_001) must be rejected
    let active_ship_move = repo.move_item_transactional(
        90_000_001,
        100_000_001,
        60_003_760,
        InventoryFlag::Cargo,
        Some(1),
        None,
    );
    assert!(matches!(
        active_ship_move,
        Err(DbError::InvalidDestinationLocation { .. })
    ));

    let moved1 = repo
        .move_item_transactional(
            90_000_001,
            100_000_003,
            100_000_001,
            InventoryFlag::Cargo,
            Some(1),
            Some("move-ab-to-cargo"),
        )
        .unwrap();
    assert_eq!(moved1.flag, InventoryFlag::Cargo);
    assert_eq!(moved1.version, 2);

    let moved2 = repo
        .move_item_transactional(
            90_000_001,
            100_000_003,
            100_000_001,
            InventoryFlag::Cargo,
            Some(1),
            Some("move-ab-to-cargo"),
        )
        .unwrap();
    assert_eq!(moved2.version, 2);

    let stale = repo.move_item_transactional(
        90_000_001,
        100_000_003,
        60_003_760,
        InventoryFlag::Hangar,
        Some(1),
        Some("new-key-stale-version"),
    );
    assert!(matches!(
        stale,
        Err(DbError::VersionConflict {
            expected: 1,
            actual: 2
        })
    ));
}

#[test]
fn test_atomic_undock_and_rollback() {
    let repo = WorldRepository::with_demo_seed();
    let (station_id, ch, ship) = repo.undock_character(1, 90_000_001).unwrap();
    assert_eq!(station_id, 60_003_760);
    assert_eq!(ch.station_id, None);
    assert_eq!(ship.location_id, 30_000_142);
    assert_eq!(ship.flag, InventoryFlag::None);

    assert!(matches!(
        repo.undock_character(1, 90_000_001),
        Err(DbError::NotDocked(90_000_001))
    ));

    repo.rollback_undock(90_000_001, station_id, ship.item_id);
    let restored = repo.select_character_for_user(1, 90_000_001).unwrap();
    assert_eq!(restored.station_id, Some(60_003_760));
}

#[test]
fn test_durable_snapshot_persistence_and_domain_mutations() {
    // Verify FIPS 180-4 SHA-256 test vector ("abc") and salted credential hashing (OF-24)
    let abc_digest = sha256_bytes(b"abc");
    let mut abc_hex = String::new();
    for b in abc_digest {
        use std::fmt::Write as _;
        let _ = write!(&mut abc_hex, "{b:02x}");
    }
    assert_eq!(
        abc_hex,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );

    let sde = SdeRegistry::with_canonical_seed();
    let repo = WorldRepository::with_demo_seed();
    let stored_demo_digest = repo.with_snapshot(|snap| {
        let demo_acc = &snap.accounts["demo"];
        let other_acc = &snap.accounts["other"];
        assert!(demo_acc
            .password_token
            .starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
        assert_ne!(demo_acc.password_token, "demo-secret");
        let demo_salt = demo_acc.password_token.split('$').nth(4).unwrap();
        let other_salt = other_acc.password_token.split('$').nth(4).unwrap();
        assert_eq!(demo_salt.len(), 32);
        assert_ne!(demo_salt, other_salt);
        demo_acc.password_token.clone()
    });
    assert!(repo.authenticate("demo", "demo-secret").is_ok());
    assert_eq!(
        repo.authenticate("demo", "wrong-secret"),
        Err(DbError::InvalidCredentials)
    );
    // R21-P1-3: Pass-the-hash must be rejected (stored digest is not accepted as a login token)
    assert_eq!(
        repo.authenticate("demo", &stored_demo_digest),
        Err(DbError::InvalidCredentials)
    );

    let created = repo
        .create_character(1, "Snapshot Pilot", 1, 1, 11, 17, &sde)
        .unwrap();
    // Verify Caldari pilot spawns in Ibis (601) at State War Academy starting station in Kisogo (30000141)
    let created_items = repo.get_items_for_character(created.character_id);
    assert_eq!(created_items[0].type_id, 601);
    assert_eq!(created.solar_system_id, 30_000_141);
    assert_eq!(created.station_id, Some(60_015_157));

    // Verify IskCents rejects negative debits and insufficient funds
    assert_eq!(
        IskCents::from_isk(100).checked_sub(IskCents::from_isk(150)),
        None
    );
    assert_eq!(
        IskCents::from_isk(100).checked_sub(IskCents::from_isk(-10)),
        None
    );
    assert_eq!(
        IskCents::from_isk(100).checked_add(IskCents::from_isk(-10)),
        None
    );
    assert_eq!(IskCents::from_isk(100).checked_mul_qty(-5), None);

    // Populate tuple-keyed maps (planetary_colonies & active_missions) and idempotency keys
    repo.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (created.character_id, 40009082),
            PlanetaryColonyRecord {
                planet_id: 40009082,
                owner_id: created.character_id,
                solar_system_id: 30000142,
                command_center_level: 1,
                pins_count: 2,
                extracted_commodities: HashMap::from([(2393, 500)]),
                last_extracted_unix: 0,
            },
        );
        snap.active_missions.insert(
            (created.character_id, 1001),
            ActiveMissionRecord {
                mission_id: 1001,
                character_id: created.character_id,
                agent_id: 3018924,
                solar_system_id: 30000142,
                state: "Accepted".to_string(),
                current_room_id: 1,
                npcs_remaining: 3,
                spawned_npc_ball_ids: Vec::new(),
            },
        );
        snap.committed_idempotency_keys
            .insert((created.character_id, "idem-1".to_string()));
    });

    // Verify try_with_mut_snapshot discards mutations when closure returns Err
    let err_res: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.items.clear();
        Err("simulated failure".to_string())
    });
    assert!(err_res.is_err());
    assert!(!repo
        .get_items_for_character(created.character_id)
        .is_empty());

    let pid = std::process::id();
    let snap_path = std::env::temp_dir().join(format!("evomu_test_snapshot_{pid}.json"));
    repo.save_snapshot_to_file(&snap_path).unwrap();

    // Verify incremental WAL delta append and replay before checkpoint compaction (OF-06 / OF-23)
    let old_snap = repo.with_snapshot(|s| s.clone());
    let _ = repo
        .move_item_transactional(
            90_000_001,
            100_000_003,
            100_000_001,
            InventoryFlag::Cargo,
            Some(1),
            Some("wal-move-1"),
        )
        .unwrap();
    let new_snap = repo.with_snapshot(|s| s.clone());
    let wal_delta = WorldStateWalDelta::from_diff(&old_snap, &new_snap);
    assert!(wal_delta.full_snapshot.is_none());
    assert_eq!(wal_delta.upserted_items.len(), 1);
    repo.append_wal_delta_locked(&snap_path, &new_snap, &wal_delta)
        .unwrap();
    assert!(WorldRepository::wal_path_for(&snap_path).exists());

    let repo2 = WorldRepository::with_demo_seed();
    repo2.load_snapshot_from_file(&snap_path).unwrap();
    let loaded = repo2
        .select_character_for_user(1, created.character_id)
        .unwrap();
    assert_eq!(loaded.name, "Snapshot Pilot");
    let replayed_item = repo2.get_item(100_000_003).unwrap();
    assert_eq!(replayed_item.flag, InventoryFlag::Cargo);
    assert_eq!(replayed_item.version, 2);
    let has_colony = repo2.with_snapshot(|snap| {
        snap.planetary_colonies
            .contains_key(&(created.character_id, 40009082))
            && snap
                .active_missions
                .contains_key(&(created.character_id, 1001))
    });
    assert!(has_colony);

    // R23-P0-1, R23-P1-4, & R24-P1-1: Verify `new_empty_with_snapshot_path` preserves all persisted accounts,
    // characters, balances, skills, and items across restarts even when a torn/truncated trailing line is
    // appended to `.json.wal` before an ungraceful crash (`std::mem::forget`), and that `provision_player_account`
    // and `provision_gm_account` are idempotent.
    let prod_snap_path = std::env::temp_dir().join(format!("evomu_test_prod_restart_{pid}.json"));
    let prod_wal_path = WorldRepository::wal_path_for(&prod_snap_path);
    let _ = std::fs::remove_file(&prod_snap_path);
    let _ = std::fs::remove_file(&prod_wal_path);
    let prod1 = WorldRepository::new_empty_with_snapshot_path(Some(prod_snap_path.clone()))
        .expect("initial new_empty_with_snapshot_path");
    assert!(prod1.with_snapshot(|s| s.accounts.is_empty() && s.characters.is_empty()));
    let player_uid = prod1.provision_player_account("prod_pilot", "prod_pass_1");
    let prod_char = prod1
        .create_character(player_uid, "Prod Capsuleer", 1, 1, 11, 17, &sde)
        .unwrap();
    prod1.provision_gm_account("gm_admin", "gm_pass_1", 90_000_003, "ISD Polaris");
    prod1.with_mut_snapshot(|snap| {
        if let Some(gm_ch) = snap.characters.get_mut(&90_000_003) {
            gm_ch.balance = IskCents::from_isk(42_000_000);
        }
    });
    let gm_items_before = prod1.get_items_for_character(90_000_003);
    assert_eq!(gm_items_before.len(), 1);

    // R25-P1-1: (a) Verify `sync_data` failure after `write_all` truncates the uncommitted WAL record
    // back to `prev_len` on disk and aborts in memory, so a subsequent committed transaction reuses
    // the sequence number cleanly and survives restart without reviving the aborted transaction.
    let wal_len_before_abort = std::fs::metadata(&prod_wal_path).unwrap().len();
    prod1
        .wal_fault_mode
        .store(2, std::sync::atomic::Ordering::SeqCst);
    let sync_fail_res: Result<(), DbError> = prod1.try_with_mut_snapshot(|snap| {
        if let Some(gm_ch) = snap.characters.get_mut(&90_000_003) {
            gm_ch.balance = IskCents::from_isk(999_999_999);
        }
        Ok(())
    });
    assert!(matches!(sync_fail_res, Err(DbError::PersistenceFailure(_))));
    assert_eq!(
        prod1.get_character(90_000_003).unwrap().balance,
        IskCents::from_isk(42_000_000)
    );
    assert_eq!(
        std::fs::metadata(&prod_wal_path).unwrap().len(),
        wal_len_before_abort,
        "sync_data failure after write_all must truncate .json.wal back to prev_len (R25-P1-1)"
    );

    // R25-P1-1: (b) Verify partial `write_all` failure truncates partial bytes back to `prev_len`.
    prod1
        .wal_fault_mode
        .store(1, std::sync::atomic::Ordering::SeqCst);
    let partial_fail_res: Result<(), DbError> = prod1.try_with_mut_snapshot(|snap| {
        if let Some(gm_ch) = snap.characters.get_mut(&90_000_003) {
            gm_ch.balance = IskCents::from_isk(888_888_888);
        }
        Ok(())
    });
    assert!(matches!(
        partial_fail_res,
        Err(DbError::PersistenceFailure(_))
    ));
    assert_eq!(
        std::fs::metadata(&prod_wal_path).unwrap().len(),
        wal_len_before_abort,
        "partial write_all failure must truncate .json.wal back to prev_len (R25-P1-1)"
    );

    // R25-P1-1: (c) Simulate an uncleaned partial write left on disk in `.json.wal` followed by a
    // committed append, verifying the partial bytes are truncated before appending so the committed
    // record is never merged with the partial line and survives restart.
    {
        use std::io::Write as _;
        let mut wal_file = std::fs::OpenOptions::new()
            .append(true)
            .open(&prod_wal_path)
            .unwrap();
        wal_file
            .write_all(b"{\"seq\":777,\"partial_uncleaned_tail")
            .unwrap();
        wal_file.sync_data().unwrap();
    }
    prod1.with_mut_snapshot(|snap| {
        if let Some(gm_ch) = snap.characters.get_mut(&90_000_003) {
            gm_ch.balance = IskCents::from_isk(55_000_000);
        }
    });

    // R25-P1-1: (d) Verify a checkpoint (`save_snapshot_locked`) failure after a durable WAL write
    // treats checkpoint compaction as retryable, commits the durable transaction, and preserves it across restart.
    let checkpoint_tmp_dir = prod_snap_path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&checkpoint_tmp_dir);
    std::fs::create_dir_all(&checkpoint_tmp_dir).unwrap();
    prod1.pending_wal_records.store(
        WAL_CHECKPOINT_INTERVAL - 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    prod1
        .try_with_mut_snapshot::<_, (), DbError>(|snap| {
            if let Some(gm_ch) = snap.characters.get_mut(&90_000_003) {
                gm_ch.balance = IskCents::from_isk(64_000_000);
            }
            Ok(())
        })
        .expect("durable WAL write must commit even when background checkpoint fails (R25-P1-1)");
    let _ = std::fs::remove_dir_all(&checkpoint_tmp_dir);

    // R24-P1-1: Append a torn/truncated trailing line to `.json.wal` and simulate an ungraceful process crash (`std::mem::forget`)
    {
        use std::io::Write as _;
        let mut wal_file = std::fs::OpenOptions::new()
            .append(true)
            .open(&prod_wal_path)
            .expect("open prod_wal_path to append torn line");
        wal_file
            .write_all(b"{\"seq\":999,\"full_snapshot\":null,\"upserted_items\":[{\"item_id\":")
            .expect("write torn WAL line");
        wal_file.sync_data().unwrap();
    }
    std::mem::forget(prod1);

    // Restart production repository from the same snapshot path + torn WAL
    let prod2 = WorldRepository::new_empty_with_snapshot_path(Some(prod_snap_path.clone()))
        .expect("restart must tolerate and heal torn trailing WAL line (R24-P1-1, R25-P1-1)");
    let restarted_char = prod2
            .select_character_for_user(player_uid, prod_char.character_id)
            .expect("persisted player character must survive production restart after torn WAL (R23-P0-1, R24-P1-1)");
    assert_eq!(restarted_char.name, "Prod Capsuleer");
    assert!(!prod2
        .get_items_for_character(prod_char.character_id)
        .is_empty());

    // Re-running account provisioning on startup must preserve `user_id`, GM balance (`64_000_000` ISK), and GM items (R23-P1-4, R25-P1-1)
    let same_uid = prod2.provision_player_account("prod_pilot", "prod_pass_1");
    assert_eq!(same_uid, player_uid);
    let rotated_uid = prod2.provision_player_account("prod_pilot", "prod_pass_2");
    assert_eq!(rotated_uid, player_uid);
    assert!(prod2.authenticate("prod_pilot", "prod_pass_2").is_ok());

    prod2.provision_gm_account("gm_admin", "gm_pass_1", 90_000_003, "ISD Polaris");
    let gm_after = prod2.get_character(90_000_003).unwrap();
    assert_eq!(gm_after.balance, IskCents::from_isk(64_000_000));
    assert_eq!(prod2.get_items_for_character(90_000_003).len(), 1);

    // R24-P1-1: Verify that if the WAL path is unwriteable (e.g., replaced by a directory), `try_with_mut_snapshot`
    // returns `Err(DbError::PersistenceFailure(_))` and does NOT commit the mutation in memory.
    let _ = std::fs::remove_file(&prod_wal_path);
    std::fs::create_dir_all(&prod_wal_path).unwrap();
    let wal_err_res: Result<(), DbError> = prod2.try_with_mut_snapshot(|snap| {
        if let Some(gm_ch) = snap.characters.get_mut(&90_000_003) {
            gm_ch.balance = IskCents::from_isk(1);
        }
        Ok(())
    });
    assert!(matches!(wal_err_res, Err(DbError::PersistenceFailure(_))));
    assert_eq!(
        prod2.get_character(90_000_003).unwrap().balance,
        IskCents::from_isk(64_000_000)
    );
    let _ = std::fs::remove_dir_all(&prod_wal_path);

    // R25-P1-1: (e) Verify duplicate `seq` numbers in `.json.wal` are rejected on replay.
    prod2.save_snapshot_to_file(&prod_snap_path).unwrap();
    let dup_line = format!("{}\n", serde_json::to_string(&wal_delta).unwrap());
    std::fs::write(&prod_wal_path, format!("{dup_line}{dup_line}")).unwrap();
    assert!(
        prod2.load_snapshot_from_file(&prod_snap_path).is_err(),
        "duplicate WAL sequence numbers must be rejected on replay (R25-P1-1)"
    );
    let _ = std::fs::remove_file(&prod_wal_path);
    drop(prod2);

    // R24-P1-1: Verify that a corrupted base snapshot file returns `Err` from `new_empty_with_snapshot_path`
    // and is NEVER overwritten on disk.
    let corrupt_snap_path =
        std::env::temp_dir().join(format!("evomu_test_corrupt_snapshot_{pid}.json"));
    std::fs::write(&corrupt_snap_path, b"{not-valid-json").unwrap();
    let corrupt_load =
        WorldRepository::new_empty_with_snapshot_path(Some(corrupt_snap_path.clone()));
    assert!(corrupt_load.is_err());
    assert_eq!(
        std::fs::read(&corrupt_snap_path).unwrap(),
        b"{not-valid-json"
    );
    let _ = std::fs::remove_file(&corrupt_snap_path);

    let _ = std::fs::remove_file(&prod_snap_path);
    let _ = std::fs::remove_file(&prod_wal_path);
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(WorldRepository::wal_path_for(&snap_path));
}

#[test]
fn test_d3_sqlite_relational_world_db_and_concurrent_sessions() {
    use std::sync::Arc;
    let sde = SdeRegistry::with_canonical_seed();
    let pid = std::process::id();
    let sqlite_path = std::env::temp_dir().join(format!("evomu_test_d3_world_{pid}.sqlite"));
    let _ = std::fs::remove_file(&sqlite_path);

    let repo =
        Arc::new(WorldRepository::open_sqlite(&sqlite_path).expect("open_sqlite must succeed"));
    assert_eq!(repo.sqlite_path(), Some(sqlite_path.as_path()));

    // Verify PRAGMA journal_mode = wal and PRAGMA foreign_keys = 1 are active
    {
        let conn = rusqlite::Connection::open(&sqlite_path).unwrap();
        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    }

    let u1 = repo.provision_player_account("pilot_one", "pass_one_123");
    let u2 = repo.provision_player_account("pilot_two", "pass_two_456");
    let c1 = repo
        .create_character(u1, "Sqlite Alpha", 1, 1, 11, 17, &sde)
        .unwrap();
    let c2 = repo
        .create_character(u2, "Sqlite Bravo", 2, 3, 19, 14, &sde)
        .unwrap();

    // Populate all relational domain tables (skills, queue, orders, journal, contracts, jobs,
    // missions, standings, mail, fleets, corps, bookmarks, structures)
    repo.with_mut_snapshot(|snap| {
        snap.skills.entry(c1.character_id).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.skill_queues.insert(
            c1.character_id,
            vec![SkillQueueEntry {
                type_id: 3302,
                target_level: 4,
                start_sp: 8_000.0,
                end_sp: 45_255.0,
                duration_seconds: 1200.0,
            }],
        );
        snap.market_orders.insert(
            800_001,
            MarketOrderRecord {
                order_id: 800_001,
                owner_id: c1.character_id,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(6),
                vol_entered: 1_000,
                vol_remaining: 1_000,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(6_000),
            },
        );
        snap.wallet_journal.push(WalletJournalEntry {
            entry_id: 900_001,
            character_id: c1.character_id,
            entry_type: "market_escrow".to_string(),
            amount: IskCents::from_isk(-6_000),
            balance_after: IskCents::from_isk(4_000),
            reason: "Market order escrow".to_string(),
        });
        snap.contracts.insert(
            910_001,
            ContractRecord {
                contract_id: 910_001,
                issuer_id: c1.character_id,
                acceptor_id: None,
                contract_type: "ItemExchange".to_string(),
                status: "Outstanding".to_string(),
                price: IskCents::from_isk(100_000),
                reward: IskCents(0),
                collateral: IskCents(0),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                item_ids: vec![],
                expiration_unix: 2_000_000_000,
            },
        );
        snap.industry_jobs.insert(
            920_001,
            IndustryJobRecord {
                job_id: 920_001,
                installer_id: c1.character_id,
                facility_id: 60_003_760,
                activity_id: 1,
                blueprint_item_id: 100_000_007,
                blueprint_type_id: 691,
                product_type_id: 587,
                runs: 1,
                status: "Active".to_string(),
                duration_seconds: 3600,
                start_unix: 1_700_000_000,
            },
        );
        snap.active_missions.insert(
            (c1.character_id, 501),
            ActiveMissionRecord {
                mission_id: 501,
                character_id: c1.character_id,
                agent_id: 3018924,
                solar_system_id: 30_000_142,
                state: "Accepted".to_string(),
                current_room_id: 0,
                npcs_remaining: 2,
                spawned_npc_ball_ids: vec![],
            },
        );
        snap.standing_transactions.insert(
            c1.character_id,
            vec![StandingTransactionRecord {
                event_id: 1,
                from_id: 1_000_035,
                to_id: c1.character_id,
                modification: 0.25,
                event_type_id: 33,
            }],
        );
        snap.mails.push(MailRecord {
            message_id: 930_001,
            sender_id: c1.character_id,
            recipient_id: c2.character_id,
            subject: "Hello".to_string(),
            body: "World".to_string(),
        });
        snap.fleets.insert(
            940_001,
            FleetRecord {
                fleet_id: 940_001,
                boss_id: c1.character_id,
                members: vec![c1.character_id],
                wings: vec![],
                squads: vec![],
                free_move: true,
                is_registered: true,
            },
        );
        snap.corporations.insert(
            950_001,
            CorporationRecord {
                corporation_id: 950_001,
                name: "Sqlite Corp".to_string(),
                ticker: "SQLT".to_string(),
                ceo_id: c1.character_id,
                alliance_id: None,
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(1_000_000),
                members: HashSet::from([c1.character_id]),
            },
        );
        snap.bookmarks.insert(
            960_001,
            BookmarkRecord {
                bookmark_id: 960_001,
                owner_id: c1.character_id,
                label: "Jita Undock".to_string(),
                solar_system_id: 30_000_142,
                x: 100.0,
                y: 200.0,
                z: 300.0,
            },
        );
        snap.structures.insert(
            970_001,
            UpwellStructureRecord {
                structure_id: 970_001,
                type_id: 35832,
                owner_corp_id: 950_001,
                solar_system_id: 30_000_142,
                name: "Jita - Astrahus".to_string(),
                position: [1_000.0, 2_000.0, 3_000.0],
                state: "ShieldVulnerable".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    // Verify selective dirty-row rollback on try_with_mut_snapshot Err restores in-memory state cleanly
    let abort_res: Result<(), DbError> = repo.try_with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&c1.character_id) {
            ch.balance = IskCents::from_isk(999_999_999);
        }
        snap.market_orders.remove(&800_001);
        Err(DbError::InvalidCredentials)
    });
    assert_eq!(abort_res, Err(DbError::InvalidCredentials));
    assert_eq!(
        repo.get_character(c1.character_id).unwrap().balance,
        IskCents::from_isk(5_000)
    );
    assert!(repo.with_snapshot(|s| s.market_orders.contains_key(&800_001)));

    // Concurrent writes from two sessions must not lose updates
    let r1 = Arc::clone(&repo);
    let r2 = Arc::clone(&repo);
    let c1_id = c1.character_id;
    let c2_id = c2.character_id;
    let t1 = std::thread::spawn(move || {
        for _ in 0..25 {
            r1.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&c1_id) {
                    ch.balance = ch.balance.checked_add(IskCents::from_isk(1_000)).unwrap();
                }
            });
        }
    });
    let t2 = std::thread::spawn(move || {
        for _ in 0..25 {
            r2.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&c2_id) {
                    ch.balance = ch.balance.checked_add(IskCents::from_isk(2_000)).unwrap();
                }
            });
        }
    });
    t1.join().unwrap();
    t2.join().unwrap();
    drop(repo);

    // Restart server from the SQLite database file and verify all state survived
    let restarted = WorldRepository::open_sqlite(&sqlite_path).expect("reopen SQLite world DB");
    let c1_after = restarted.get_character(c1_id).unwrap();
    let c2_after = restarted.get_character(c2_id).unwrap();
    assert_eq!(c1_after.balance, IskCents::from_isk(5_000 + 25_000));
    assert_eq!(c2_after.balance, IskCents::from_isk(5_000 + 50_000));

    restarted.with_snapshot(|snap| {
        assert_eq!(snap.skills[&c1_id][&3300].level, 5);
        assert_eq!(snap.skill_queues[&c1_id].len(), 1);
        assert!(snap.market_orders.contains_key(&800_001));
        assert_eq!(snap.wallet_journal.len(), 1);
        assert!(snap.contracts.contains_key(&910_001));
        assert!(snap.industry_jobs.contains_key(&920_001));
        assert!(snap.active_missions.contains_key(&(c1_id, 501)));
        assert_eq!(snap.standing_transactions[&c1_id].len(), 1);
        assert_eq!(snap.mails.len(), 1);
        assert!(snap.fleets.contains_key(&940_001));
        assert!(snap.corporations.contains_key(&950_001));
        assert!(snap.bookmarks.contains_key(&960_001));
        assert!(snap.structures.contains_key(&970_001));
    });

    let _ = std::fs::remove_file(&sqlite_path);
    let _ = std::fs::remove_file(sqlite_path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(sqlite_path.with_extension("sqlite-shm"));
}

#[test]
fn test_d4_argon2id_bans_online_lock_and_oauth2_pkce_sso() {
    let sde = SdeRegistry::with_canonical_seed();
    let pid = std::process::id();
    let sqlite_path = std::env::temp_dir().join(format!("evomu_test_d4_auth_{pid}.sqlite"));
    let _ = std::fs::remove_file(&sqlite_path);

    let repo = WorldRepository::open_sqlite(&sqlite_path).unwrap();
    let uid = repo.provision_player_account("capsuleer_auth", "S3cretP@ss!");
    let ch = repo
        .create_character(uid, "SSO Capsuleer", 1, 1, 11, 17, &sde)
        .unwrap();
    repo.provision_gm_account("gm_root", "gm_secret_123", 90_999_001, "ISD Root");

    // 1. Single active session lock (multiLoginBlocker) + check_session_revoked enforcement
    assert!(repo
        .acquire_online_lock(uid, 101, Some(ch.character_id), false, 1_700_000_000)
        .unwrap()
        .is_none());
    assert_eq!(
        repo.acquire_online_lock(uid, 202, Some(ch.character_id), false, 1_700_000_005),
        Err(DbError::AlreadyLoggedIn {
            user_id: uid,
            active_session_id: 101,
        })
    );
    // Active session 101 passes check_session_revoked; unlocked session 202 is blocked with MultiLoginBlocked (R34-P1-3)
    assert_eq!(repo.check_session_revoked(uid, 101, 1_700_000_006), None);
    let revoked_202 = repo.check_session_revoked(uid, 202, 1_700_000_006).unwrap();
    assert_eq!(revoked_202.0, "MultiLoginBlocked");
    // Re-acquiring with the same session_id is idempotent
    assert!(repo
        .acquire_online_lock(uid, 101, Some(ch.character_id), false, 1_700_000_010)
        .unwrap()
        .is_none());

    // 2. OAuth2 PKCE + signed JWT SSO token flow
    let code_verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let code_challenge = base64url_encode_no_pad(&sha256_bytes(code_verifier.as_bytes()));
    let code = repo
        .authorize_pkce(
            "capsuleer_auth",
            "S3cretP@ss!",
            ch.character_id,
            "eveLauncherTQ",
            "https://login.eveonline.com/launcher",
            vec![
                "esi-skills.read_skills.v1".to_string(),
                "esi-wallet.read_character_wallet.v1".to_string(),
            ],
            &code_challenge,
            "S256",
            1_700_000_100,
        )
        .unwrap();

    // Wrong verifier must fail and consume the single-use code
    assert_eq!(
        repo.exchange_pkce_code(&code, "eveLauncherTQ", "wrong-verifier", 1_700_000_105),
        Err(DbError::InvalidCredentials)
    );
    // Re-issue and exchange with valid verifier
    let code2 = repo
        .authorize_pkce(
            "capsuleer_auth",
            "S3cretP@ss!",
            ch.character_id,
            "eveLauncherTQ",
            "https://login.eveonline.com/launcher",
            vec!["esi-skills.read_skills.v1".to_string()],
            &code_challenge,
            "S256",
            1_700_000_110,
        )
        .unwrap();
    let bundle = repo
        .exchange_pkce_code(&code2, "eveLauncherTQ", code_verifier, 1_700_000_115)
        .unwrap();
    assert_eq!(bundle.token_type, "Bearer");
    assert_eq!(bundle.expires_in, 1_200);

    let claims = repo
        .verify_sso_access_token(&bundle.access_token, 1_700_000_200)
        .unwrap();
    assert_eq!(claims.character_id, ch.character_id);
    assert_eq!(claims.user_id, uid);
    assert_eq!(claims.sub, format!("CHARACTER:EVE:{}", ch.character_id));

    // R34-P1-4: Forged JWT signed with old hardcoded secret or wrong secret must be rejected
    let forged_header = base64url_encode_no_pad(br#"{"alg":"HS256","typ":"JWT"}"#);
    let forged_payload =
        base64url_encode_no_pad(serde_json::to_string(&claims).unwrap().as_bytes());
    let forged_signing_input = format!("{forged_header}.{forged_payload}");
    let forged_sig = base64url_encode_no_pad(&hmac_sha256(
        b"evomu-sso-hs256-signing-key-v24-01-local-default",
        forged_signing_input.as_bytes(),
    ));
    let forged_jwt = format!("{forged_signing_input}.{forged_sig}");
    assert_eq!(
        repo.verify_sso_access_token(&forged_jwt, 1_700_000_200),
        Err(DbError::InvalidCredentials)
    );
    assert_eq!(
        repo.authenticate_at("capsuleer_auth", &forged_jwt, 1_700_000_200),
        Err(DbError::InvalidCredentials)
    );

    // JWT with alg:none or future iat (> now_unix) must be rejected
    let none_header = base64url_encode_no_pad(br#"{"alg":"none","typ":"JWT"}"#);
    let none_input = format!("{none_header}.{forged_payload}");
    let none_sig = base64url_encode_no_pad(&hmac_sha256(
        &repo.auth_runtime.read().unwrap().jwt_signing_secret,
        none_input.as_bytes(),
    ));
    let none_jwt = format!("{none_input}.{none_sig}");
    assert_eq!(
        repo.verify_sso_access_token(&none_jwt, 1_700_000_200),
        Err(DbError::InvalidCredentials)
    );
    assert_eq!(
        repo.verify_sso_access_token(&bundle.access_token, 1_700_000_050),
        Err(DbError::InvalidCredentials),
        "JWT with iat in the future must be rejected"
    );
    assert_eq!(
        repo.verify_sso_access_token(&bundle.access_token, 1_700_001_315),
        Err(DbError::InvalidCredentials),
        "Expired JWT (now_unix == exp) must be rejected"
    );
    assert_eq!(
        repo.authenticate_at("capsuleer_auth", &bundle.access_token, 1_700_002_000),
        Err(DbError::InvalidCredentials),
        "Expired JWT (now_unix > exp) must be rejected by authenticate_at"
    );

    // Login with signed SSO JWT token works directly via authenticate_at
    let sso_acc = repo
        .authenticate_at("capsuleer_auth", &bundle.access_token, 1_700_000_200)
        .unwrap();
    assert_eq!(sso_acc.user_id, uid);

    // Refresh token rotation
    let rotated = repo
        .refresh_sso_token(&bundle.refresh_token, "eveLauncherTQ", 1_700_000_300)
        .unwrap();
    // Old refresh token is now revoked
    assert_eq!(
        repo.refresh_sso_token(&bundle.refresh_token, "eveLauncherTQ", 1_700_000_301),
        Err(DbError::InvalidCredentials)
    );

    // 3. Kick user invalidates active session and revokes prior JWT/refresh tokens
    let kicked_sess = repo
        .kick_user(uid, "GM kick test", 1_700_000_400)
        .expect("session 101 was active");
    assert_eq!(kicked_sess.session_id, 101);
    assert!(repo.get_online_session(uid).is_none());
    assert_eq!(
        repo.verify_sso_access_token(&rotated.access_token, 1_700_000_405),
        Err(DbError::InvalidCredentials)
    );

    // 4. ROLE_ADMIN accounts cannot be banned or kicked (role hierarchy check)
    assert!(matches!(
        repo.ban_account("gm_root", 1_700_086_400, "mutiny"),
        Err(DbError::ValidationError(_))
    ));
    let gm_uid = repo.with_snapshot(|s| s.accounts["gm_root"].user_id);
    repo.acquire_online_lock(gm_uid, 505, Some(90_999_001), true, 1_700_000_410)
        .unwrap();
    assert!(repo
        .kick_user(gm_uid, "try kick gm", 1_700_000_420)
        .is_none());
    assert!(repo.get_online_session(gm_uid).is_some());

    // 5. Account ban persists to SQLite and blocks login until unbanned or expired
    repo.ban_account("capsuleer_auth", 1_700_086_400, "EULA violation")
        .unwrap();
    assert_eq!(
        repo.authenticate_at("capsuleer_auth", "S3cretP@ss!", 1_700_001_000),
        Err(DbError::AccountBanned {
            until_unix: 1_700_086_400,
            reason: "EULA violation".to_string(),
        })
    );
    repo.unban_account("capsuleer_auth").unwrap();
    assert!(repo
        .authenticate_at("capsuleer_auth", "S3cretP@ss!", 1_700_001_000)
        .is_ok());

    let _ = std::fs::remove_file(&sqlite_path);
    let _ = std::fs::remove_file(sqlite_path.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(sqlite_path.with_extension("sqlite-shm"));
}
