//! Unit and mutation-killing tests for `sqlite_store::load`.

use super::*;
use crate::persistence::WorldRepository;
use crate::sqlite_store::schema::apply_migrations;
use crate::sqlite_store::sync::replace_sqlite_from_snapshot;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

fn build_populated_snapshot() -> WorldStateSnapshot {
    let mut snap = WorldRepository::create_demo_snapshot();
    snap.next_entity_id = 250_000_000;
    snap.wal_seq = 77;
    snap.markets_seeded = true;

    let cid = 90_000_001;
    snap.accounts.insert(
        "banned_user".into(),
        AccountRecord {
            user_id: 99,
            username: "banned_user".into(),
            password_token: "pw_hash".into(),
            role: 1,
            banned_until_unix: Some(1_900_000_000),
            ban_reason: Some("RMT".into()),
        },
    );
    snap.skills.entry(cid).or_default().insert(
        3300,
        TrainedSkillRecord {
            type_id: 3300,
            level: 4,
            skill_points: 45_255.0,
        },
    );
    snap.skill_queues.insert(
        cid,
        vec![
            SkillQueueEntry {
                type_id: 3300,
                target_level: 5,
                start_sp: 45_255.0,
                end_sp: 256_000.0,
                duration_seconds: 3600.0,
                started_at_unix: Some(1_700_000_000),
            },
            SkillQueueEntry {
                type_id: 3301,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 600.0,
                started_at_unix: None,
            },
        ],
    );
    snap.jump_clones.insert(
        cid,
        vec![JumpCloneRecord {
            jump_clone_id: 400_001,
            character_id: cid,
            location_id: 60_003_760,
            implants: vec![9899],
        }],
    );
    snap.active_implants.insert(cid, vec![9899, 9941]);
    snap.saved_fittings.insert(
        500_001,
        SavedFittingRecord {
            fitting_id: 500_001,
            owner_id: cid,
            name: "PvP Rifter".into(),
            ship_type_id: 587,
            description: " Fast tackle".into(),
            items: vec![(2881, InventoryFlag::HiSlot0, 1)],
        },
    );
    snap.market_orders.insert(
        700_001,
        MarketOrderRecord {
            order_id: 700_001,
            owner_id: cid,
            station_id: 60_003_760,
            region_id: 10_000_002,
            type_id: 34,
            is_bid: true,
            price: IskCents(550),
            vol_entered: 1000,
            vol_remaining: 800,
            min_volume: 10,
            is_npc: true,
            escrow: IskCents(440_000),
        },
    );
    snap.wallet_journal.push(WalletJournalEntry {
        entry_id: 800_001,
        character_id: cid,
        entry_type: "Bounty".into(),
        amount: IskCents(125_000),
        balance_after: IskCents(100_125_000),
        reason: "NPC kill".into(),
    });
    snap.contracts.insert(
        600_001,
        ContractRecord {
            contract_id: 600_001,
            issuer_id: cid,
            acceptor_id: Some(90_000_002),
            contract_type: "ItemExchange".into(),
            status: "Outstanding".into(),
            start_station_id: 60_003_760,
            end_station_id: 60_003_760,
            price: IskCents(10_000),
            reward: IskCents(500),
            collateral: IskCents(20_000),
            expiration_unix: 1_800_000_000,
            ..Default::default()
        },
    );
    snap.industry_jobs.insert(
        610_001,
        IndustryJobRecord {
            job_id: 610_001,
            installer_id: cid,
            facility_id: 60_003_760,
            blueprint_item_id: 100_000_001,
            blueprint_type_id: 683,
            activity_id: 8,
            runs: 3,
            product_type_id: 587,
            status: "Delivered".into(),
            duration_seconds: 7200,
            start_unix: 1_700_000_000,
            blueprint_location_id: 60_003_760,
            output_location_id: 60_003_760,
            cost: Some(12_500.5),
            licensed_runs: Some(10),
            probability: Some(0.45),
            completed_unix: Some(1_700_007_200),
            completed_character_id: Some(cid),
        },
    );
    snap.planetary_colonies.insert(
        (cid, 40_009_082),
        PlanetaryColonyRecord {
            planet_id: 40_009_082,
            owner_id: cid,
            solar_system_id: 30_000_142,
            command_center_level: 4,
            pins_count: 6,
            extracted_commodities: HashMap::from([(2393, 1200)]),
            last_extracted_unix: 1_700_001_000,
        },
    );
    snap.active_missions.insert(
        (cid, 101),
        ActiveMissionRecord {
            mission_id: 101,
            character_id: cid,
            agent_id: 3_018_922,
            solar_system_id: 30_000_142,
            state: "Accepted".into(),
            current_room_id: 2,
            npcs_remaining: 4,
            spawned_npc_ball_ids: vec![910_001],
            bookmark_id: Some(620_001),
        },
    );
    snap.mission_tally.insert(cid, 12);
    snap.standing_transactions.insert(
        cid,
        vec![StandingTransactionRecord {
            event_id: 1,
            from_id: 1_000_125,
            to_id: cid,
            modification: 0.25,
            event_type_id: 83,
        }],
    );
    snap.npe_progress.insert(
        cid,
        NpeProgressRecord {
            character_id: cid,
            node_graph_id: 10,
            current_node_id: 3,
            completed: true,
            career_goal_progress: HashMap::from([(1, 5)]),
            claimed_goals: HashSet::from([1]),
        },
    );
    snap.bookmarks.insert(
        620_001,
        BookmarkRecord {
            bookmark_id: 620_001,
            owner_id: cid,
            label: "Safe Spot".into(),
            solar_system_id: 30_000_142,
            x: 100.0,
            y: 200.0,
            z: -300.0,
            item_id: Some(60_003_760),
            memo: "Deep safe".into(),
            folder_id: Some(7),
            is_shared: true,
        },
    );
    snap.mails.push(MailRecord {
        message_id: 630_001,
        sender_id: cid,
        recipient_id: 90_000_002,
        subject: "Welcome".into(),
        body: "Fly safe".into(),
    });
    snap.fleets.insert(
        640_001,
        FleetRecord {
            fleet_id: 640_001,
            boss_id: cid,
            members: vec![cid],
            free_move: true,
            is_registered: true,
            motd: "Armor fleet".into(),
            ..Default::default()
        },
    );
    snap.alliances.insert(
        99_000_001,
        AllianceRecord {
            alliance_id: 99_000_001,
            name: "Test Alliance".into(),
            short_name: "TEST".into(),
            executor_corp_id: 98_000_001,
            member_corp_ids: HashSet::from([98_000_001]),
        },
    );
    snap.wars.insert(
        650_001,
        WarRecord {
            war_id: 650_001,
            aggressor_corp_id: 98_000_001,
            defender_corp_id: 98_000_002,
            mutual: true,
            started_unix: 1_700_000_000,
            active: true,
        },
    );
    snap.corp_projects.insert(
        660_001,
        CorpProjectRecord {
            project_id: 660_001,
            corporation_id: 98_000_001,
            name: "Ore Buyback".into(),
            target_quantity: 10_000,
            current_quantity: 2_500,
            isk_per_unit: IskCents(100),
            escrow_remaining: IskCents(750_000),
        },
    );
    snap.killmails.insert(
        670_001,
        KillmailRecord {
            kill_id: 670_001,
            victim_character_id: 90_000_002,
            attacker_character_id: cid,
            solar_system_id: 30_000_142,
            ship_type_id: 587,
            total_value_isk: 4_500_000.0,
            killed_unix: 1_700_005_000,
            victim_corp_id: 98_000_002,
            victim_alliance_id: None,
            attacker_corp_id: 98_000_001,
            attacker_alliance_id: Some(99_000_001),
        },
    );
    snap.kill_rights.insert(
        680_001,
        KillRightRecord {
            kill_right_id: 680_001,
            owner_id: cid,
            target_id: 90_000_002,
            activation_fee: IskCents(5_000_000_00),
            is_active_suspect: true,
        },
    );
    snap.structures.insert(
        1_000_000_000_001,
        UpwellStructureRecord {
            structure_id: 1_000_000_000_001,
            type_id: 35832,
            owner_corp_id: 98_000_001,
            solar_system_id: 30_000_142,
            name: "Perimeter Hub".into(),
            position: [1.0, 2.0, 3.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            shield_hp: 5000.0,
            armor_hp: 5000.0,
            hull_hp: 5000.0,
            ..Default::default()
        },
    );
    snap.committed_idempotency_keys
        .insert((cid, "idem_tx_1".into()));
    snap.licensed_skins.insert(cid, vec![101, 202]);
    snap.applied_ship_skins.insert(100_000_001, 101);
    snap.mining_ledger.push(MiningLedgerRecord {
        character_id: cid,
        solar_system_id: 30_000_142,
        type_id: 1230,
        quantity: 750,
        date: "2026-01-15".into(),
        timestamp_unix: 1_768_435_200,
    });
    snap.asteroids.insert(
        70_000_001,
        AsteroidRecord {
            asteroid_id: 70_000_001,
            solar_system_id: 30_000_142,
            belt_id: 40_009_077,
            type_id: 1230,
            quantity: 15_000,
            initial_quantity: 20_000,
            position: (1000.0, 2000.0, 3000.0),
            radius: 120.0,
        },
    );
    snap.character_last_jump_unix.insert(cid, 1_700_009_000);
    snap.character_cspa_charges.insert(cid, IskCents(295_000));
    snap.mail_timestamps.insert(630_001, 1_700_009_100);
    snap.character_login_stats.insert(
        cid,
        CharacterLoginStats {
            last_login_unix: 1_700_009_200,
            last_logout_unix: 1_700_009_300,
            logins_count: 14,
        },
    );
    snap.item_custom_names
        .insert(100_000_001, "Golden Rifter".into());
    snap.item_positions
        .insert(100_000_001, [11.0, 22.0, 33.0]);
    snap.structure_vulnerability_windows
        .insert(1_000_000_000_001, (100, 200));
    snap.skill_history.insert(
        cid,
        vec![SkillHistoryEntry {
            log_date_ft: 133_485_408_000_000_000,
            event_type_id: 33,
            skill_type_id: 3300,
            skill_level: 4,
            absolute_points: 45_255.0,
        }],
    );
    snap.character_doll_info
        .insert(cid, ("dna_blob".into(), "portrait_blob".into()));
    snap
}

#[test]
fn load_snapshot_from_sqlite_restores_all_37_tables_and_post_processes_state() {
    let mut conn = Connection::open_in_memory().unwrap();
    // Unmigrated DB must return an error
    assert!(load_snapshot_from_sqlite(&conn).is_err());

    apply_migrations(&conn).unwrap();
    let mut orig = build_populated_snapshot();
    // Set legacy character plex_balance to verify fold_legacy_plex_balances on load
    if let Some(ch) = orig.characters.get_mut(&90_000_001) {
        ch.plex_balance = 333;
    }
    orig.account_plex_vault.clear();

    replace_sqlite_from_snapshot(&mut conn, &orig).unwrap();
    let (loaded, fp) = load_snapshot_from_sqlite(&conn).unwrap();

    assert!(loaded.next_entity_id >= 250_000_000);
    assert_eq!(loaded.wal_seq, 77);
    assert!(loaded.markets_seeded);
    assert_eq!(fp.wal_seq, 77);
    assert!(fp.markets_seeded);

    let banned = &loaded.accounts["banned_user"];
    assert_eq!(banned.user_id, 99);
    assert_eq!(banned.role, 1);
    assert_eq!(banned.banned_until_unix, Some(1_900_000_000));
    assert_eq!(banned.ban_reason.as_deref(), Some("RMT"));

    let cid = 90_000_001;
    // fold_legacy_plex_balances folded character plex_balance into account_plex_vault
    let uid = loaded.characters[&cid].user_id;
    assert_eq!(loaded.characters[&cid].plex_balance, 0);
    assert!(loaded.account_plex_vault.get(&uid).copied().unwrap_or(0) >= 333);

    assert_eq!(loaded.skills[&cid][&3300].level, 4);
    assert_eq!(loaded.skill_queues[&cid].len(), 2);
    assert_eq!(loaded.skill_queues[&cid][0].started_at_unix, Some(1_700_000_000));
    assert_eq!(loaded.skill_queues[&cid][1].started_at_unix, None);
    assert_eq!(loaded.jump_clones[&cid][0].jump_clone_id, 400_001);
    assert_eq!(loaded.active_implants[&cid], vec![9899, 9941]);
    assert_eq!(loaded.saved_fittings[&500_001].name, "PvP Rifter");

    let ord = &loaded.market_orders[&700_001];
    assert!(ord.is_bid);
    assert!(ord.is_npc);
    assert_eq!(ord.price, IskCents(550));
    assert_eq!(ord.vol_entered, 1000);
    assert_eq!(ord.vol_remaining, 800);
    assert_eq!(ord.min_volume, 10);
    assert_eq!(ord.escrow, IskCents(440_000));

    assert!(loaded.wallet_journal.iter().any(|w| w.entry_id == 800_001 && w.amount == IskCents(125_000)));
    assert_eq!(loaded.contracts[&600_001].price, IskCents(10_000));

    let job = &loaded.industry_jobs[&610_001];
    assert_eq!(job.activity_id, 8);
    assert_eq!(job.runs, 3);
    assert_eq!(job.cost, Some(12_500.5));
    assert_eq!(job.licensed_runs, Some(10));
    assert!((job.probability.unwrap() - 0.45).abs() < 1e-5);
    assert_eq!(job.completed_unix, Some(1_700_007_200));
    assert_eq!(job.completed_character_id, Some(cid));

    assert_eq!(loaded.planetary_colonies[&(cid, 40_009_082)].command_center_level, 4);
    assert_eq!(loaded.active_missions[&(cid, 101)].npcs_remaining, 4);
    assert_eq!(loaded.mission_tally[&cid], 12);
    assert_eq!(loaded.standing_transactions[&cid][0].modification, 0.25);
    assert!(loaded.npe_progress[&cid].completed);

    let bm = &loaded.bookmarks[&620_001];
    assert_eq!(bm.memo, "Deep safe");
    assert_eq!(bm.folder_id, Some(7));
    assert!(bm.is_shared);

    assert!(loaded.mails.iter().any(|m| m.message_id == 630_001 && m.subject == "Welcome"));
    assert!(loaded.fleets[&640_001].free_move);
    assert_eq!(loaded.alliances[&99_000_001].short_name, "TEST");
    assert!(loaded.wars[&650_001].mutual);
    assert!(loaded.wars[&650_001].active);
    assert_eq!(loaded.corp_projects[&660_001].target_quantity, 10_000);
    assert_eq!(loaded.killmails[&670_001].attacker_alliance_id, Some(99_000_001));
    assert!(loaded.kill_rights[&680_001].is_active_suspect);
    assert_eq!(loaded.structures[&1_000_000_000_001].fuel_blocks, 500);
    assert!(loaded.committed_idempotency_keys.contains(&(cid, "idem_tx_1".into())));
    assert_eq!(loaded.licensed_skins[&cid], vec![101, 202]);
    assert_eq!(loaded.applied_ship_skins[&100_000_001], 101);
    assert_eq!(loaded.mining_ledger.len(), 1);
    assert_eq!(loaded.asteroids[&70_000_001].quantity, 15_000);
    assert_eq!(loaded.character_last_jump_unix[&cid], 1_700_009_000);
    assert_eq!(loaded.character_cspa_charges[&cid], IskCents(295_000));
    assert_eq!(loaded.mail_timestamps[&630_001], 1_700_009_100);
    assert_eq!(loaded.character_login_stats[&cid].logins_count, 14);
    assert_eq!(loaded.item_custom_names[&100_000_001], "Golden Rifter");
    assert_eq!(loaded.item_positions[&100_000_001], [11.0, 22.0, 33.0]);
    assert_eq!(loaded.structure_vulnerability_windows[&1_000_000_000_001], (100, 200));
    assert_eq!(loaded.skill_history[&cid][0].skill_level, 4);
    assert_eq!(loaded.character_doll_info[&cid].0, "dna_blob");
    assert!(!loaded.fw_system_occupations.is_empty());
}

#[test]
fn startup_migrations_persist_before_baseline_fingerprints_and_survive_first_tx_rollback() {
    let tmp_path = std::env::temp_dir().join(format!(
        "evomu_startup_mig_rollback_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp_path);

    let cid = 90_000_001i64;
    let uid: u64;
    {
        let mut conn = Connection::open(&tmp_path).unwrap();
        apply_migrations(&conn).unwrap();

        let mut pre_mig = build_populated_snapshot();
        uid = pre_mig.characters[&cid].user_id;
        // 1. Legacy per-character PLEX balance with empty account_plex_vault
        pre_mig.account_plex_vault.clear();
        pre_mig.characters.get_mut(&cid).unwrap().plex_balance = 450;
        // 2. Stale next_entity_id smaller than an existing item_id (760_000_099)
        pre_mig.next_entity_id = 100;
        pre_mig.items.insert(
            760_000_099,
            InventoryItemRecord {
                item_id: 760_000_099,
                type_id: 34,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                ..Default::default()
            },
        );
        // 3. Empty FW system occupations requiring startup backfill
        pre_mig.fw_system_occupations.clear();
        // 4. Legacy service_state_blobs entry requiring typed service domain migration
        pre_mig
            .service_state_blobs
            .insert("contract_deposit:777".into(), "125000".into());

        replace_sqlite_from_snapshot(&mut conn, &pre_mig).unwrap();
    }

    let repo = WorldRepository::open_sqlite(&tmp_path).expect("open_sqlite with pending migrations");
    repo.with_snapshot(|snap| {
        assert_eq!(snap.account_plex_vault.get(&uid), Some(&450));
        assert_eq!(snap.characters[&cid].plex_balance, 0);
        assert_eq!(snap.next_entity_id, 760_000_099);
        assert!(snap.fw_system_occupations.len() >= 175);
        assert_eq!(
            snap.fleet_and_social_ops.contract_deposits.get(&777),
            Some(&125_000)
        );
        assert!(!snap.service_state_blobs.contains_key("contract_deposit:777"));
    });

    // Execute a failing first transaction that mutates all 4 migrated domains and returns Err.
    // Rollback must restore the POST-migration baseline rather than reverting to pre-migration rows.
    let abort_res: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(uid, 999_999);
        snap.characters.get_mut(&cid).unwrap().plex_balance = 888;
        snap.next_entity_id = 899_999_999;
        snap.fw_system_occupations.clear();
        snap.fleet_and_social_ops.contract_deposits.clear();
        Err("simulated first-tx failure".into())
    });
    assert!(abort_res.is_err());

    repo.with_snapshot(|snap| {
        assert_eq!(
            snap.account_plex_vault.get(&uid),
            Some(&450),
            "rollback must preserve folded account_plex_vault"
        );
        assert_eq!(snap.characters[&cid].plex_balance, 0);
        assert_eq!(
            snap.next_entity_id, 760_000_099,
            "rollback must preserve reconciled next_entity_id"
        );
        assert!(
            snap.fw_system_occupations.len() >= 175,
            "rollback must preserve backfilled fw_system_occupations"
        );
        assert_eq!(
            snap.fleet_and_social_ops.contract_deposits.get(&777),
            Some(&125_000),
            "rollback must preserve migrated service domain state"
        );
    });

    drop(repo);
    let _ = std::fs::remove_file(&tmp_path);
}
