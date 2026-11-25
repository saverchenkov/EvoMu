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
    snap.missions
        .try_insert(MissionInstance {
            instance_id: 200_000_101,
            character_id: cid,
            agent_id: 3_018_922,
            template_id: 101,
            state: MissionState::Accepted,
            kind: MissionKind::Encounter,
            solar_system_id: 30_000_142,
            current_room_id: 2,
            bookmark_id: Some(620_001),
            origin_station_id: None,
            offered_level: None,
            is_anomic_burner: false,
            offered_at_unix: 0,
            offer_expiry_unix: 0,
            accepted_at_unix: None,
            accept_expiry_unix: None,
            bonus_expiry_unix: None,
            objective: MissionObjective::Encounter(EncounterObjective {
                npcs_remaining: 4,
                spawned_npc_ball_ids: vec![910_001],
                initial_wave_count: None,
                initial_wave_types: Vec::new(),
                awaiting_downtime_respawn: false,
            }),
        })
        .expect("insert mission");
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
    let loaded = load_snapshot_from_sqlite(&conn).unwrap();

    assert!(loaded.next_entity_id >= 250_000_000);
    assert_eq!(loaded.wal_seq, 77);
    assert!(loaded.markets_seeded);

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
    assert_eq!(
        loaded
            .missions
            .get_by_character_template(cid, 101)
            .expect("mission 101 loaded")
            .npcs_remaining(),
        4
    );
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

#[test]
fn sqlite_legacy_missions_conversion_idempotency_and_unknown_state_quarantine() {
    use crate::sqlite_store::domain_rows::KeyCodec;
    use crate::sqlite_store::quarantine::list_quarantined_rows;

    let tmp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let db_path = tmp_dir.join(format!("evomu_t3_legacy_missions_{pid}.sqlite"));
    let bad_db_path = tmp_dir.join(format!("evomu_t3_legacy_missions_bad_{pid}.sqlite"));
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&bad_db_path);

    let cid1 = 90_000_001_i64;
    let cid2 = 90_000_002_i64;

    let seed_legacy_db = |path: &std::path::Path, include_bad_state: bool| {
        let mut snap = WorldRepository::create_demo_snapshot();
        if !snap.characters.contains_key(&cid2) {
            let mut c2 = snap.characters[&cid1].clone();
            c2.character_id = cid2;
            c2.name = "Legacy Char 2".into();
            snap.characters.insert(cid2, c2);
        }
        snap.missions.clear();
        snap.next_entity_id = 200_000_000;
        {
            let mut conn = Connection::open(path).expect("open seed sqlite");
            apply_migrations(&conn).expect("apply migrations");
            replace_sqlite_from_snapshot(&mut conn, &snap).expect("seed snapshot");
            conn.execute_batch(
                "DROP TABLE IF EXISTS mission_instances;
                 CREATE TABLE missions (
                     character_id INTEGER NOT NULL,
                     mission_id INTEGER NOT NULL,
                     agent_id INTEGER NOT NULL,
                     solar_system_id INTEGER NOT NULL,
                     state TEXT NOT NULL,
                     current_room_id INTEGER NOT NULL,
                     npcs_remaining INTEGER NOT NULL,
                     record_json TEXT NOT NULL,
                     PRIMARY KEY (character_id, mission_id),
                     FOREIGN KEY (character_id) REFERENCES characters(character_id) ON DELETE CASCADE
                 );",
            )
            .expect("recreate legacy missions table");

            // Insert (cid2, 100) BEFORE (cid1, 200) in SQLite to prove deterministic (character_id, mission_id) ordering.
            let enc_json = serde_json::json!({
                "mission_id": 100,
                "character_id": cid2,
                "agent_id": 3_018_418,
                "solar_system_id": 30_000_142,
                "state": "Accepted",
                "current_room_id": 2,
                "npcs_remaining": 3,
                "spawned_npc_ball_ids": [200_000_001],
                "bookmark_id": 77_001
            })
            .to_string();
            conn.execute(
                "INSERT INTO missions (character_id, mission_id, agent_id, solar_system_id, state, current_room_id, npcs_remaining, record_json)
                 VALUES (?1, 100, 3018418, 30000142, 'Accepted', 2, 3, ?2)",
                rusqlite::params![cid2, enc_json],
            )
            .expect("insert legacy encounter row");

            let courier_json = serde_json::json!({
                "mission_id": 200,
                "character_id": cid1,
                "agent_id": 3_018_418,
                "solar_system_id": 30_000_142,
                "state": "Offered",
                "current_room_id": 1,
                "npcs_remaining": 0,
                "spawned_npc_ball_ids": [],
                "bookmark_id": null
            })
            .to_string();
            conn.execute(
                "INSERT INTO missions (character_id, mission_id, agent_id, solar_system_id, state, current_room_id, npcs_remaining, record_json)
                 VALUES (?1, 200, 3018418, 30000142, 'Offered', 1, 0, ?2)",
                rusqlite::params![cid1, courier_json],
            )
            .expect("insert legacy courier row");

            if include_bad_state {
                let bad_json = serde_json::json!({
                    "mission_id": 999,
                    "character_id": cid1,
                    "agent_id": 3_018_418,
                    "solar_system_id": 30_000_142,
                    "state": "BogusState",
                    "current_room_id": 1,
                    "npcs_remaining": 0,
                    "spawned_npc_ball_ids": [],
                    "bookmark_id": null
                })
                .to_string();
                conn.execute(
                    "INSERT INTO missions (character_id, mission_id, agent_id, solar_system_id, state, current_room_id, npcs_remaining, record_json)
                     VALUES (?1, 999, 3018418, 30000142, 'BogusState', 1, 0, ?2)",
                    rusqlite::params![cid1, bad_json],
                )
                .expect("insert legacy unknown-state row");
            }

            let meta1_nid = legacy_mission_meta_note_id(cid1, 200);
            let meta1_note = OwnerNoteRecord {
                note_id: meta1_nid,
                character_id: -5,
                item_id: 200,
                label: format!("mission_meta:{cid1}:200"),
                note: serde_json::json!({
                    "offered_at_unix": 1_700_000_000u64,
                    "offer_expiry_unix": 1_700_086_400u64,
                    "required_delivery_type_id": 3810,
                    "required_delivery_qty": 15,
                    "granted_courier_item_id": 888_001,
                    "granted_courier_qty": 15,
                    "is_anomic_burner": false,
                    "destination_station_id": 60_003_761,
                    "origin_station_id": 60_003_760,
                    "offered_level": 3,
                    "completed_at_unix": 1_700_000_999u64,
                    "chosen_branch": "DropMe"
                })
                .to_string(),
                timestamp_unix: 1_700_000_000,
            };
            conn.execute(
                "INSERT OR REPLACE INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    DOMAIN_OWNER_NOTES,
                    meta1_nid.encode_key(),
                    serde_json::to_string(&meta1_note).unwrap()
                ],
            )
            .expect("insert meta1 note");

            let meta2_nid = legacy_mission_meta_note_id(cid2, 100);
            let meta2_note = OwnerNoteRecord {
                note_id: meta2_nid,
                character_id: -5,
                item_id: 100,
                label: format!("mission_meta:{cid2}:100"),
                note: serde_json::json!({
                    "offered_at_unix": 1_700_000_100u64,
                    "offer_expiry_unix": 1_700_086_500u64,
                    "accepted_at_unix": 1_700_000_200u64,
                    "is_anomic_burner": true,
                    "origin_station_id": 60_003_760,
                    "offered_level": 4
                })
                .to_string(),
                timestamp_unix: 1_700_000_100,
            };
            conn.execute(
                "INSERT OR REPLACE INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    DOMAIN_OWNER_NOTES,
                    meta2_nid.encode_key(),
                    serde_json::to_string(&meta2_note).unwrap()
                ],
            )
            .expect("insert meta2 note");

            let wave_key = format!("{cid2}:100");
            conn.execute(
                "INSERT OR REPLACE INTO domain_rows (domain, key, value) VALUES (?1, 'missions_awaiting_downtime_respawn', ?2)",
                rusqlite::params![
                    DOMAIN_SP6_LIFECYCLE,
                    serde_json::to_string(&vec![(cid2, 100u32)]).unwrap()
                ],
            )
            .expect("insert respawn list");
            conn.execute(
                "INSERT OR REPLACE INTO domain_rows (domain, key, value) VALUES (?1, ?2, '3')",
                rusqlite::params![
                    DOMAIN_SP6_LIFECYCLE,
                    format!("mission_initial_npc_counts/{}", wave_key.encode_key())
                ],
            )
            .expect("insert initial npc count");
            conn.execute(
                "INSERT OR REPLACE INTO domain_rows (domain, key, value) VALUES (?1, ?2, '[587,588]')",
                rusqlite::params![
                    DOMAIN_SP6_LIFECYCLE,
                    format!("mission_initial_npc_types/{}", wave_key.encode_key())
                ],
            )
            .expect("insert initial npc types");
        }
    };

    // 1. Clean migration + idempotent second load
    seed_legacy_db(&db_path, false);
    let repo1 = WorldRepository::open_sqlite(&db_path).expect("first open_sqlite converts legacy missions");
    let (m1_id, m2_id, hwm_after_first) = repo1.with_snapshot(|snap| {
        let m1 = snap
            .missions
            .get_by_character_template(cid1, 200)
            .expect("migrated courier mission");
        assert_eq!(m1.state, MissionState::Offered);
        assert_eq!(m1.kind, MissionKind::Courier);
        assert_eq!(m1.destination_station_id(), Some(60_003_761));
        assert_eq!(m1.granted_courier(), Some((Some(888_001), 15)));
        assert_eq!(m1.offered_level, Some(3));
        assert_eq!(m1.offered_at_unix, 1_700_000_000);

        let m2 = snap
            .missions
            .get_by_character_template(cid2, 100)
            .expect("migrated encounter mission");
        assert_eq!(m2.state, MissionState::Accepted);
        assert_eq!(m2.kind, MissionKind::Encounter);
        assert!(m2.is_anomic_burner);
        assert_eq!(m2.npcs_remaining(), 3);
        assert_eq!(m2.spawned_npc_ball_ids(), &[200_000_001]);
        assert_eq!(m2.initial_wave_count(), Some(3));
        assert_eq!(m2.initial_wave_types(), &[587, 588]);
        assert!(m2.awaiting_downtime_respawn());
        assert!(m1.instance_id < m2.instance_id, "must allocate in (character_id, mission_id) order");
        assert_ne!(m1.instance_id, 200_000_001);
        assert_ne!(m2.instance_id, 200_000_001);

        assert!(!snap.owner_notes.values().any(|n| n.label.starts_with("mission_meta:")));
        (m1.instance_id, m2.instance_id, snap.next_entity_id)
    });
    repo1.with_sqlite_conn(|conn| {
        assert!(!table_exists(conn, "missions").unwrap());
        assert!(table_exists(conn, "mission_instances").unwrap());
        let leftover_waves: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM domain_rows WHERE domain = ?1 AND (key = 'missions_awaiting_downtime_respawn' OR key LIKE 'mission_initial_npc_counts/%' OR key LIKE 'mission_initial_npc_types/%')",
                rusqlite::params![DOMAIN_SP6_LIFECYCLE],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leftover_waves, 0);
        let leftover_notes: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM domain_rows WHERE domain = ?1 AND value LIKE '%mission_meta:%'",
                rusqlite::params![DOMAIN_OWNER_NOTES],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(leftover_notes, 0);
    });
    let expected_missions = repo1.with_snapshot(|s| s.missions.clone());
    drop(repo1);

    let repo2 = WorldRepository::open_sqlite(&db_path).expect("second open_sqlite must be idempotent");
    repo2.with_snapshot(|snap| {
        assert_eq!(snap.missions, expected_missions);
        assert_eq!(snap.missions.get_by_character_template(cid1, 200).unwrap().instance_id, m1_id);
        assert_eq!(snap.missions.get_by_character_template(cid2, 100).unwrap().instance_id, m2_id);
        assert_eq!(snap.next_entity_id, hwm_after_first);
    });
    drop(repo2);

    // 2. Unknown state row: fail closed vs quarantine
    seed_legacy_db(&bad_db_path, true);
    let fail_closed = WorldRepository::open_sqlite_with_options(&bad_db_path, false);
    assert!(fail_closed.is_err(), "unknown legacy mission state must fail closed");

    let repo_q = WorldRepository::open_sqlite_with_options(&bad_db_path, true)
        .expect("quarantine mode opens with valid rows");
    repo_q.with_snapshot(|snap| {
        assert_eq!(snap.missions.len(), 2);
        assert!(snap.missions.get_by_character_template(cid1, 999).is_none());
    });
    repo_q.with_sqlite_conn(|conn| {
        assert!(!table_exists(conn, "missions").unwrap());
        let quarantined = list_quarantined_rows(conn).expect("quarantine_rows readable");
        assert!(
            quarantined.iter().any(|q| q.source_table == "missions" && q.key.contains("999")),
            "unknown state row must be quarantined: {quarantined:?}"
        );
    });
    drop(repo_q);

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&bad_db_path);
}

#[test]
fn sqlite_legacy_missions_anomic_delivery_direct_load_ddl_and_key_mismatch() {
    use crate::sqlite_store::domain_rows::KeyCodec;
    use crate::sqlite_store::quarantine::list_quarantined_rows;

    let cid1 = 90_000_001_i64;
    let cid2 = 90_000_002_i64;

    let make_unmigrated_conn = |populated: bool, with_mismatch: bool| -> Connection {
        let mut snap = WorldRepository::create_demo_snapshot();
        if !snap.characters.contains_key(&cid2) {
            let mut c2 = snap.characters[&cid1].clone();
            c2.character_id = cid2;
            c2.name = "Legacy Char 2".into();
            snap.characters.insert(cid2, c2);
        }
        snap.missions.clear();
        snap.next_entity_id = 200_000_000;

        let mut conn = Connection::open_in_memory().expect("open in-memory sqlite");
        apply_migrations(&conn).expect("apply migrations");
        replace_sqlite_from_snapshot(&mut conn, &snap).expect("seed snapshot");
        conn.execute_batch(
            "DROP TABLE IF EXISTS mission_instances;
             CREATE TABLE missions (
                 character_id INTEGER NOT NULL,
                 mission_id INTEGER NOT NULL,
                 agent_id INTEGER NOT NULL,
                 solar_system_id INTEGER NOT NULL,
                 state TEXT NOT NULL,
                 current_room_id INTEGER NOT NULL,
                 npcs_remaining INTEGER NOT NULL,
                 record_json TEXT NOT NULL,
                 PRIMARY KEY (character_id, mission_id),
                 FOREIGN KEY (character_id) REFERENCES characters(character_id) ON DELETE CASCADE
             );",
        )
        .expect("replace mission_instances with legacy missions table");

        if populated {
            let delivery_json = serde_json::json!({
                "mission_id": 250,
                "character_id": cid1,
                "agent_id": 3_018_418,
                "solar_system_id": 30_000_142,
                "state": "Accepted",
                "current_room_id": 1,
                "npcs_remaining": 0,
                "spawned_npc_ball_ids": [],
                "bookmark_id": null
            })
            .to_string();
            conn.execute(
                "INSERT INTO missions (character_id, mission_id, agent_id, solar_system_id, state, current_room_id, npcs_remaining, record_json)
                 VALUES (?1, 250, 3018418, 30000142, 'Accepted', 1, 0, ?2)",
                rusqlite::params![cid1, delivery_json],
            )
            .expect("insert legacy delivery row");

            let meta_nid = legacy_mission_meta_note_id(cid1, 250);
            let meta_note = OwnerNoteRecord {
                note_id: meta_nid,
                character_id: -5,
                item_id: 250,
                label: format!("mission_meta:{cid1}:250"),
                note: serde_json::json!({
                    "offered_at_unix": 1_700_000_000u64,
                    "offer_expiry_unix": 1_700_086_400u64,
                    "required_delivery_type_id": 34,
                    "required_delivery_qty": 250,
                    "is_anomic_burner": true,
                    "origin_station_id": 60_003_760,
                    "offered_level": 4
                })
                .to_string(),
                timestamp_unix: 1_700_000_000,
            };
            conn.execute(
                "INSERT OR REPLACE INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    DOMAIN_OWNER_NOTES,
                    meta_nid.encode_key(),
                    serde_json::to_string(&meta_note).unwrap()
                ],
            )
            .expect("insert anomic delivery meta note");
        }

        if with_mismatch {
            let mismatch_json = serde_json::json!({
                "mission_id": 302,
                "character_id": cid2,
                "agent_id": 3_018_418,
                "solar_system_id": 30_000_142,
                "state": "Accepted",
                "current_room_id": 1,
                "npcs_remaining": 0,
                "spawned_npc_ball_ids": [],
                "bookmark_id": null
            })
            .to_string();
            conn.execute(
                "INSERT INTO missions (character_id, mission_id, agent_id, solar_system_id, state, current_room_id, npcs_remaining, record_json)
                 VALUES (?1, 301, 3018418, 30000142, 'Accepted', 1, 0, ?2)",
                rusqlite::params![cid1, mismatch_json],
            )
            .expect("insert mismatched legacy mission row");
        }

        conn
    };

    // 1. Direct load_snapshot_from_sqlite on empty unmigrated legacy missions table creates DDL + index.
    let conn_empty = make_unmigrated_conn(false, false);
    let snap_empty =
        load_snapshot_from_sqlite(&conn_empty).expect("direct load on empty legacy missions");
    assert!(snap_empty.missions.is_empty());
    assert!(!table_exists(&conn_empty, "missions").unwrap());
    assert!(table_exists(&conn_empty, "mission_instances").unwrap());
    let idx_exists: i64 = conn_empty
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_mission_instances_character_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(idx_exists, 1);
    let snap_empty_reload =
        load_snapshot_from_sqlite(&conn_empty).expect("direct reload after empty legacy migration");
    assert!(snap_empty_reload.missions.is_empty());

    // 2. Direct load_snapshot_from_sqlite on populated legacy DB preserves is_anomic_burner delivery objectives (BF-01 & BF-02).
    let conn_pop = make_unmigrated_conn(true, false);
    let snap_pop =
        load_snapshot_from_sqlite(&conn_pop).expect("direct load on populated legacy missions");
    assert!(!table_exists(&conn_pop, "missions").unwrap());
    assert!(table_exists(&conn_pop, "mission_instances").unwrap());
    let migrated = snap_pop
        .missions
        .get_by_character_template(cid1, 250)
        .expect("anomic burner delivery mission must migrate");
    assert!(migrated.is_anomic_burner);
    assert_ne!(migrated.kind, MissionKind::Encounter);
    assert_eq!(migrated.required_delivery(), Some((34, 250)));
    let snap_pop_reload =
        load_snapshot_from_sqlite(&conn_pop).expect("reload persisted migrated mission_instances");
    assert_eq!(snap_pop_reload.missions, snap_pop.missions);

    // 3. Column vs JSON key mismatch fails closed without quarantine and quarantines under quarantine mode (BF-02).
    let conn_bad_fc = make_unmigrated_conn(true, true);
    let err = load_snapshot_from_sqlite_with_options(&conn_bad_fc, false)
        .expect_err("column vs JSON key mismatch must fail closed");
    assert!(err.contains("legacy mission key"), "unexpected err: {err}");
    assert!(table_exists(&conn_bad_fc, "missions").unwrap());
    assert!(!table_exists(&conn_bad_fc, "mission_instances").unwrap());

    let conn_bad_q = make_unmigrated_conn(true, true);
    let snap_q = load_snapshot_from_sqlite_with_options(&conn_bad_q, true)
        .expect("quarantine mode succeeds and preserves valid rows");
    assert_eq!(snap_q.missions.len(), 1);
    assert!(snap_q.missions.get_by_character_template(cid1, 250).is_some());
    assert!(snap_q.missions.get_by_character_template(cid1, 301).is_none());
    assert!(snap_q.missions.get_by_character_template(cid2, 302).is_none());
    assert!(!table_exists(&conn_bad_q, "missions").unwrap());
    assert!(table_exists(&conn_bad_q, "mission_instances").unwrap());
    let quarantined = list_quarantined_rows(&conn_bad_q).expect("quarantine rows readable");
    assert!(
        quarantined
            .iter()
            .any(|q| q.source_table == "missions" && q.key == format!("{cid1}:301")),
        "key mismatch row must be quarantined: {quarantined:?}"
    );
}

#[test]
fn sqlite_legacy_mission_instances_upgrade_terminal_and_heraldry() {
    let cid = 90_000_001_i64;
    let db_path = std::env::temp_dir().join(format!(
        "evomu_legacy_mission_instances_upgrade_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db_path);

    let mut snap = WorldRepository::create_demo_snapshot();
    snap.missions.clear();
    // Simulate pre-Track-4 SQLite schema/data before Track 4 migration.
    {
        let mut conn = Connection::open(&db_path).expect("open seed sqlite");
        apply_migrations(&conn).expect("apply migrations");
        replace_sqlite_from_snapshot(&mut conn, &snap).expect("seed snapshot");
        conn.execute_batch("DROP TABLE IF EXISTS mission_history;")
            .expect("drop mission_history to simulate pre-Track-4 schema");

        let legacy_completed_json = serde_json::json!({
            "instance_id": 200_000_111_i64,
            "character_id": cid,
            "agent_id": 3_018_418_i64,
            "template_id": 501_u32,
            "state": "Completed",
            "kind": "Encounter",
            "solar_system_id": 30_000_142_i64,
            "current_room_id": 1_u32,
            "offered_at_unix": 1_700_000_000_u64,
            "offer_expiry_unix": 1_700_086_400_u64,
            "accepted_at_unix": 1_700_000_500_u64,
            "objective": {
                "Encounter": {
                    "npcs_remaining": 0,
                    "spawned_npc_ball_ids": [],
                    "initial_wave_count": null,
                    "initial_wave_types": [],
                    "awaiting_downtime_respawn": false
                }
            }
        })
        .to_string();
        conn.execute(
            "INSERT INTO mission_instances (instance_id, character_id, agent_id, mission_id, state, record_json)
             VALUES (200000111, ?1, 3018418, 501, 'Completed', ?2)",
            rusqlite::params![cid, legacy_completed_json],
        )
        .expect("insert legacy terminal row into mission_instances");

        let legacy_heraldry_json = serde_json::json!({
            "instance_id": 200_000_112_i64,
            "character_id": cid,
            "agent_id": 3_019_500_i64,
            "template_id": 17_013_u32,
            "state": "Accepted",
            "kind": "Heraldry",
            "solar_system_id": 30_000_142_i64,
            "current_room_id": 1_u32,
            "offered_at_unix": 1_700_001_000_u64,
            "offer_expiry_unix": 1_700_087_400_u64,
            "accepted_at_unix": 1_700_001_100_u64,
            "objective": "None"
        })
        .to_string();
        conn.execute(
            "INSERT INTO mission_instances (instance_id, character_id, agent_id, mission_id, state, record_json)
             VALUES (200000112, ?1, 3019500, 17013, 'Accepted', ?2)",
            rusqlite::params![cid, legacy_heraldry_json],
        )
        .expect("insert legacy Heraldry None row into mission_instances");
    }

    let repo = WorldRepository::open_sqlite(&db_path).expect("upgrade legacy DB cleanly");
    repo.with_snapshot(|loaded| {
        assert_eq!(loaded.missions.len(), 1);
        assert_eq!(loaded.missions.history_len(), 1);
        assert!(loaded.missions.has_history_for_agent_state(
            cid,
            3_018_418,
            crate::MissionState::Completed
        ));
        let heraldry = loaded
            .missions
            .get_by_character_template(cid, 17_013)
            .expect("active heraldry mission present");
        assert_eq!(heraldry.kind, crate::MissionKind::Heraldry);
        assert!(matches!(
            heraldry.objective,
            crate::MissionObjective::Delivery(_)
        ));
        assert!(heraldry.is_objective_consistent());
        loaded.missions.assert_invariants().expect("valid store");
    });
    repo.with_sqlite_conn(|conn| {
        assert!(table_exists(conn, "mission_history").unwrap());
        let active_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM mission_instances", [], |r| r.get(0))
            .unwrap();
        let history_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM mission_history", [], |r| r.get(0))
            .unwrap();
        assert_eq!(active_count, 1);
        assert_eq!(history_count, 1);
    });
    let expected_missions = repo.with_snapshot(|s| s.missions.clone());
    drop(repo);

    let repo2 = WorldRepository::open_sqlite(&db_path).expect("reopen must be idempotent");
    repo2.with_snapshot(|reloaded| {
        assert_eq!(reloaded.missions, expected_missions);
    });
    drop(repo2);
    let _ = std::fs::remove_file(&db_path);
}
