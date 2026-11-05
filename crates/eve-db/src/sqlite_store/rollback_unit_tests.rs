//! Unit and mutation-killing tests for `sqlite_store::rollback`.

use super::*;
use crate::persistence::WorldRepository;
use crate::sqlite_store::load::load_snapshot_from_sqlite;
use crate::sqlite_store::schema::apply_migrations;
use crate::sqlite_store::sync::replace_sqlite_from_snapshot;
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

fn seed_full_snapshot_for_rollback() -> WorldStateSnapshot {
    let mut snap = WorldRepository::create_demo_snapshot();
    let cid = 90_000_001;
    snap.next_entity_id = 222_000_000;
    snap.wal_seq = 19;
    snap.markets_seeded = true;
    snap.accounts.insert(
        "pilot".into(),
        AccountRecord {
            user_id: 1,
            username: "pilot".into(),
            password_token: "tok".into(),
            role: 1,
            banned_until_unix: None,
            ban_reason: None,
        },
    );

    snap.skills.entry(cid).or_default().insert(
        3300,
        TrainedSkillRecord {
            type_id: 3300,
            level: 3,
            skill_points: 8_000.0,
        },
    );
    snap.skill_queues.insert(
        cid,
        vec![SkillQueueEntry {
            type_id: 3300,
            target_level: 4,
            start_sp: 8_000.0,
            end_sp: 45_255.0,
            duration_seconds: 1200.0,
            started_at_unix: Some(1_700_000_000),
        }],
    );
    snap.jump_clones.insert(
        cid,
        vec![JumpCloneRecord {
            jump_clone_id: 400_010,
            character_id: cid,
            location_id: 60_003_760,
            implants: vec![9899],
        }],
    );
    snap.active_implants.insert(cid, vec![9899]);
    snap.saved_fittings.insert(
        500_010,
        SavedFittingRecord {
            fitting_id: 500_010,
            owner_id: cid,
            name: "FitA".into(),
            ship_type_id: 587,
            description: "Desc".into(),
            items: vec![],
        },
    );
    snap.market_orders.insert(
        700_010,
        MarketOrderRecord {
            order_id: 700_010,
            owner_id: cid,
            station_id: 60_003_760,
            region_id: 10_000_002,
            type_id: 34,
            is_bid: false,
            price: IskCents(500),
            vol_entered: 100,
            vol_remaining: 90,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents(0),
        },
    );
    snap.wallet_journal.push(WalletJournalEntry {
        entry_id: 800_010,
        character_id: cid,
        entry_type: "Bounty".into(),
        amount: IskCents(1000),
        balance_after: IskCents(2000),
        reason: "Rat".into(),
    });
    snap.contracts.insert(
        600_010,
        ContractRecord {
            contract_id: 600_010,
            issuer_id: cid,
            contract_type: "Courier".into(),
            status: "Outstanding".into(),
            start_station_id: 60_003_760,
            end_station_id: 60_003_760,
            price: IskCents(100),
            reward: IskCents(200),
            collateral: IskCents(300),
            ..Default::default()
        },
    );
    snap.industry_jobs.insert(
        610_010,
        IndustryJobRecord {
            job_id: 610_010,
            installer_id: cid,
            facility_id: 60_003_760,
            blueprint_item_id: 100_000_001,
            blueprint_type_id: 683,
            activity_id: 1,
            runs: 2,
            product_type_id: 587,
            status: "Active".into(),
            duration_seconds: 600,
            start_unix: 1_700_000_000,
            ..Default::default()
        },
    );
    snap.planetary_colonies.insert(
        (cid, 40_009_082),
        PlanetaryColonyRecord {
            planet_id: 40_009_082,
            owner_id: cid,
            solar_system_id: 30_000_142,
            command_center_level: 2,
            pins_count: 3,
            extracted_commodities: HashMap::new(),
            last_extracted_unix: 1_700_000_000,
        },
    );
    snap.active_missions.insert(
        (cid, 102),
        ActiveMissionRecord {
            mission_id: 102,
            character_id: cid,
            agent_id: 3_018_922,
            solar_system_id: 30_000_142,
            state: "Offered".into(),
            current_room_id: 0,
            npcs_remaining: 5,
            spawned_npc_ball_ids: vec![],
            bookmark_id: None,
        },
    );
    snap.mission_tally.insert(cid, 4);
    snap.standing_transactions.insert(
        cid,
        vec![StandingTransactionRecord {
            event_id: 10,
            from_id: 1_000_125,
            to_id: cid,
            modification: 0.5,
            event_type_id: 83,
        }],
    );
    snap.npe_progress.insert(
        cid,
        NpeProgressRecord {
            character_id: cid,
            node_graph_id: 1,
            current_node_id: 2,
            completed: false,
            career_goal_progress: HashMap::new(),
            claimed_goals: HashSet::new(),
        },
    );
    snap.bookmarks.insert(
        620_010,
        BookmarkRecord {
            bookmark_id: 620_010,
            owner_id: cid,
            label: "Gate".into(),
            solar_system_id: 30_000_142,
            x: 10.0,
            y: 20.0,
            z: 30.0,
            item_id: None,
            memo: String::new(),
            folder_id: None,
            is_shared: false,
        },
    );
    snap.mails.push(MailRecord {
        message_id: 630_010,
        sender_id: cid,
        recipient_id: 90_000_002,
        subject: "Subj".into(),
        body: "Body".into(),
    });
    snap.fleets.insert(
        640_010,
        FleetRecord {
            fleet_id: 640_010,
            boss_id: cid,
            members: vec![cid],
            ..Default::default()
        },
    );
    snap.alliances.insert(
        99_000_010,
        AllianceRecord {
            alliance_id: 99_000_010,
            name: "Ally".into(),
            short_name: "ALY".into(),
            executor_corp_id: 98_000_001,
            member_corp_ids: HashSet::from([98_000_001]),
        },
    );
    snap.wars.insert(
        650_010,
        WarRecord {
            war_id: 650_010,
            aggressor_corp_id: 98_000_001,
            defender_corp_id: 98_000_002,
            mutual: false,
            started_unix: 1_700_000_000,
            active: true,
        },
    );
    snap.corp_projects.insert(
        660_010,
        CorpProjectRecord {
            project_id: 660_010,
            corporation_id: 98_000_001,
            name: "Proj".into(),
            target_quantity: 100,
            current_quantity: 10,
            isk_per_unit: IskCents(50),
            escrow_remaining: IskCents(4500),
        },
    );
    snap.killmails.insert(
        670_010,
        KillmailRecord {
            kill_id: 670_010,
            victim_character_id: 90_000_002,
            attacker_character_id: cid,
            solar_system_id: 30_000_142,
            ship_type_id: 587,
            total_value_isk: 1_000.0,
            killed_unix: 1_700_000_000,
            victim_corp_id: 98_000_002,
            victim_alliance_id: None,
            attacker_corp_id: 98_000_001,
            attacker_alliance_id: None,
        },
    );
    snap.kill_rights.insert(
        680_010,
        KillRightRecord {
            kill_right_id: 680_010,
            owner_id: cid,
            target_id: 90_000_002,
            activation_fee: IskCents(1000),
            is_active_suspect: false,
        },
    );
    snap.structures.insert(
        1_000_000_000_010,
        UpwellStructureRecord {
            structure_id: 1_000_000_000_010,
            type_id: 35832,
            owner_corp_id: 98_000_001,
            solar_system_id: 30_000_142,
            name: "Citadel".into(),
            position: [0.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 100,
            shield_hp: 5000.0,
            armor_hp: 5000.0,
            hull_hp: 5000.0,
            ..Default::default()
        },
    );
    snap.committed_idempotency_keys.insert((cid, "k1".into()));
    snap.licensed_skins.insert(cid, vec![11]);
    snap.applied_ship_skins.insert(100_000_001, 11);
    snap.asteroids.insert(
        70_000_010,
        AsteroidRecord {
            asteroid_id: 70_000_010,
            solar_system_id: 30_000_142,
            belt_id: 40_009_077,
            type_id: 1230,
            quantity: 500,
            initial_quantity: 1000,
            position: (1.0, 2.0, 3.0),
            radius: 50.0,
        },
    );
    snap.character_last_jump_unix.insert(cid, 111);
    snap.character_cspa_charges.insert(cid, IskCents(222));
    snap.mail_timestamps.insert(630_010, 333);
    snap.character_login_stats.insert(
        cid,
        CharacterLoginStats {
            last_login_unix: 444,
            last_logout_unix: 555,
            logins_count: 6,
        },
    );
    snap.item_custom_names
        .insert(100_000_001, "NamedShip".into());
    snap.item_positions.insert(100_000_001, [1.0, 2.0, 3.0]);
    snap.structure_vulnerability_windows
        .insert(1_000_000_000_010, (10, 20));
    snap.skill_history.insert(
        cid,
        vec![SkillHistoryEntry {
            log_date_ft: 100,
            event_type_id: 33,
            skill_type_id: 3300,
            skill_level: 3,
            absolute_points: 8_000.0,
        }],
    );
    snap.character_doll_info
        .insert(cid, ("dna".into(), "meta".into()));
    snap
}

#[test]
fn rollback_dirty_from_sqlite_reverts_mutations_deletions_and_additions_across_all_tables() {
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    let base = seed_full_snapshot_for_rollback();
    replace_sqlite_from_snapshot(&mut conn, &base).unwrap();
    let (clean, fp) = load_snapshot_from_sqlite(&conn).unwrap();

    let cid = 90_000_001;
    let mut dirty = clean.clone();

    // Corrupt scalar meta, add extra bogus keys, and mutate/delete existing keys
    dirty.next_entity_id = 999_999_999;
    dirty.wal_seq = 999;
    dirty.markets_seeded = false;

    dirty.accounts.get_mut("pilot").unwrap().role = 999;
    dirty.accounts.insert(
        "rogue".into(),
        AccountRecord {
            user_id: 777,
            username: "rogue".into(),
            password_token: "x".into(),
            role: 0,
            banned_until_unix: None,
            ban_reason: None,
        },
    );
    dirty.characters.get_mut(&cid).unwrap().balance = IskCents(0);
    dirty.items.get_mut(&100_000_001).unwrap().quantity = 9999;
    dirty.market_orders.get_mut(&700_010).unwrap().price = IskCents(999_999);
    dirty.wallet_journal.push(WalletJournalEntry {
        entry_id: 999_999,
        character_id: cid,
        entry_type: "Dirty".into(),
        amount: IskCents(1),
        balance_after: IskCents(1),
        reason: "Dirty".into(),
    });
    dirty.mails.push(MailRecord {
        message_id: 999_999,
        sender_id: cid,
        recipient_id: cid,
        subject: "Dirty".into(),
        body: "Dirty".into(),
    });
    dirty.committed_idempotency_keys.clear();
    dirty.mission_tally.insert(cid, 999);
    dirty.applied_ship_skins.clear();
    // Mutate skill points in place (same map len, different hash)
    dirty
        .skills
        .get_mut(&cid)
        .unwrap()
        .get_mut(&3300)
        .unwrap()
        .skill_points = 1.0;
    dirty.skill_queues.clear();
    dirty.jump_clones.clear();
    dirty.active_implants.clear();
    dirty.saved_fittings.clear();
    dirty.contracts.clear();
    dirty.industry_jobs.clear();
    dirty.planetary_colonies.clear();
    dirty.active_missions.clear();
    dirty.standing_transactions.clear();
    dirty.npe_progress.clear();
    dirty.bookmarks.clear();
    dirty.fleets.clear();
    dirty.corporations.clear();
    dirty.alliances.clear();
    dirty.wars.clear();
    dirty.corp_projects.clear();
    dirty.killmails.clear();
    dirty.kill_rights.clear();
    dirty.structures.clear();
    dirty.system_states.clear();
    dirty.licensed_skins.clear();
    dirty.asteroids.clear();
    dirty.character_last_jump_unix.clear();
    dirty.character_cspa_charges.clear();
    dirty.mail_timestamps.clear();
    dirty.character_login_stats.clear();
    dirty.item_custom_names.clear();
    dirty.item_positions.clear();
    dirty.structure_vulnerability_windows.clear();
    dirty.skill_history.clear();
    dirty.character_doll_info.clear();

    rollback_dirty_from_sqlite(&conn, &mut dirty, &fp).unwrap();

    assert_eq!(dirty.next_entity_id, fp.next_entity_id);
    assert_eq!(dirty.wal_seq, 19);
    assert!(dirty.markets_seeded);
    assert!(!dirty.accounts.contains_key("rogue"));
    assert_eq!(dirty.accounts["pilot"], clean.accounts["pilot"]);
    assert_eq!(dirty.characters[&cid].balance, clean.characters[&cid].balance);
    assert_eq!(dirty.items[&100_000_001].quantity, clean.items[&100_000_001].quantity);
    assert_eq!(dirty.market_orders[&700_010].price, IskCents(500));
    assert_eq!(dirty.wallet_journal.len(), clean.wallet_journal.len());
    assert_eq!(dirty.mails.len(), clean.mails.len());
    assert!(dirty.committed_idempotency_keys.contains(&(cid, "k1".into())));
    assert_eq!(dirty.mission_tally[&cid], 4);
    assert_eq!(dirty.applied_ship_skins[&100_000_001], 11);
    assert_eq!(dirty.skills[&cid][&3300].skill_points, 8_000.0);
    assert_eq!(dirty.skill_queues[&cid].len(), 1);
    assert_eq!(dirty.jump_clones[&cid].len(), 1);
    assert_eq!(dirty.active_implants[&cid], vec![9899]);
    assert!(dirty.saved_fittings.contains_key(&500_010));
    assert!(dirty.contracts.contains_key(&600_010));
    assert!(dirty.industry_jobs.contains_key(&610_010));
    assert!(dirty.planetary_colonies.contains_key(&(cid, 40_009_082)));
    assert!(dirty.active_missions.contains_key(&(cid, 102)));
    assert_eq!(dirty.standing_transactions[&cid].len(), 1);
    assert!(dirty.npe_progress.contains_key(&cid));
    assert!(dirty.bookmarks.contains_key(&620_010));
    assert!(dirty.fleets.contains_key(&640_010));
    assert!(!dirty.corporations.is_empty());
    assert!(dirty.alliances.contains_key(&99_000_010));
    assert!(dirty.wars.contains_key(&650_010));
    assert!(dirty.corp_projects.contains_key(&660_010));
    assert!(dirty.killmails.contains_key(&670_010));
    assert!(dirty.kill_rights.contains_key(&680_010));
    assert!(dirty.structures.contains_key(&1_000_000_000_010));
    assert!(!dirty.system_states.is_empty());
    assert_eq!(dirty.licensed_skins[&cid], vec![11]);
    assert!(dirty.asteroids.contains_key(&70_000_010));
    assert_eq!(dirty.character_last_jump_unix[&cid], 111);
    assert_eq!(dirty.character_cspa_charges[&cid], IskCents(222));
    assert_eq!(dirty.mail_timestamps[&630_010], 333);
    assert_eq!(dirty.character_login_stats[&cid].logins_count, 6);
    assert_eq!(dirty.item_custom_names[&100_000_001], "NamedShip");
    assert_eq!(dirty.item_positions[&100_000_001], [1.0, 2.0, 3.0]);
    assert_eq!(dirty.structure_vulnerability_windows[&1_000_000_000_010], (10, 20));
    assert_eq!(dirty.skill_history[&cid].len(), 1);
    assert_eq!(dirty.character_doll_info[&cid].0, "dna");

    // Also test the `len < fp_len` SQLite reload path for wallet_journal and mails, and missing skills map
    dirty.wallet_journal.clear();
    dirty.mails.clear();
    dirty.skills.clear();
    rollback_dirty_from_sqlite(&conn, &mut dirty, &fp).unwrap();
    assert_eq!(dirty.wallet_journal, clean.wallet_journal);
    assert_eq!(dirty.mails, clean.mails);
    assert_eq!(dirty.skills[&cid][&3300].level, 3);
}
