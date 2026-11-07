use crate::ledger::{JournalContext, JournalMeta, MarketTransactionRecord};
use crate::persistence::WorldRepository;
use crate::types::*;
use crate::{DbError, IskCents};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_db_path(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("evomu_{label}_{}_{nanos}.sqlite", std::process::id()))
}

fn populate_world_extended_state(snap: &mut WorldStateSnapshot) {
    snap.paused_skill_queues.insert(2002);
    snap.sov_prospecting_levels.insert(30000142, 4);
    snap.last_mission_decline_times.insert(2001, 1000);
    snap.character_last_jump_unix.insert(2001, 2000);
    snap.character_cspa_charges.insert(2001, IskCents(295_000));
    snap.character_login_stats.insert(
        2001,
        CharacterLoginStats {
            last_login_unix: 1000,
            last_logout_unix: 900,
            logins_count: 5,
        },
    );
    snap.item_custom_names.insert(5001, "My Ship".into());
    snap.item_positions.insert(5001, [10.0, 20.0, 30.0]);
    snap.structure_vulnerability_windows
        .insert(1_020_000_000_001, (18, 3));
    snap.skill_history.insert(
        2001,
        vec![SkillHistoryEntry {
            log_date_ft: 1000,
            event_type_id: 36,
            skill_type_id: 3300,
            skill_level: 2,
            absolute_points: 1000.0,
        }],
    );
    snap.character_doll_info
        .insert(2001, ("gender=1".into(), "pose=0".into()));
    snap.system_jump_events.push(SystemJumpEvent {
        solar_system_id: 30000142,
        character_id: 2001,
        timestamp_unix: 1000,
    });
    snap.system_kill_events.push(SystemKillEvent {
        solar_system_id: 30000142,
        ship_kills: 2,
        npc_kills: 15,
        pod_kills: 1,
        timestamp_unix: 1000,
    });
    snap.incursions.insert(
        20000020,
        IncursionRecord {
            constellation_id: 20000020,
            staging_solar_system_id: 30000142,
            infested_solar_systems: vec![30000142],
            faction_id: 500019,
            has_boss: true,
            influence: 0.75,
            state: "mobilizing".into(),
            incursion_type: "Incursion".into(),
        },
    );
    snap.sov_campaign_scores.insert(
        700,
        SovCampaignScoreRecord {
            structure_id: 1_020_000_000_001,
            attackers_score: 0.6,
            defender_score: 0.4,
        },
    );
    snap.fw_character_stats.insert(
        2001,
        FwCharacterStatsRecord {
            character_id: 2001,
            faction_id: Some(500_001),
            enlisted_on_unix: Some(1000),
            current_rank: 2,
            highest_rank: 3,
            kills: FwWindowedStat::default(),
            victory_points: FwWindowedStat::default(),
        },
    );
    snap.fw_corp_enlisted_on.insert(3001, 1000);
    snap.fw_system_occupations.insert(
        30002797,
        FwSystemOccupationRecord {
            solar_system_id: 30002797,
            owner_faction_id: 500_001,
            occupier_faction_id: 500_001,
            victory_points: 500,
            victory_points_threshold: 3000,
        },
    );
    snap.fw_wars.push((500_001, 500_004));
    snap.war_lifecycle_meta
        .insert(9701, WarLifecycleMeta::default());
    snap.killmail_details.insert(
        9901,
        KillmailDetailRecord {
            kill_id: 9901,
            killmail_time_unix: 1_700_000_500,
            damage_taken: 1500,
            victim_position: Some([1.0, 2.0, 3.0]),
            attackers: vec![],
        },
    );
    snap.station_office_rental_costs.insert(60003760, 100_000.0);
    snap.alliance_lifecycle_meta.insert(
        4001,
        AllianceLifecycleMeta {
            creator_id: 2001,
            creator_corporation_id: 3001,
            date_founded_unix: 1000,
            faction_id: None,
        },
    );
    snap.corp_hangar_divisions
        .insert(3001, HashMap::from([(1, "Main Hangar".into())]));
    snap.structure_esi_timer_meta
        .insert(1_020_000_000_001, StructureEsiTimerMeta::default());
    snap.corp_alliance_history.insert(
        3001,
        vec![CorpAllianceHistoryEntry {
            record_id: 1,
            alliance_id: Some(4001),
            start_date_unix: 1000,
            is_deleted: Some(false),
        }],
    );
    snap.corp_container_logs.insert(
        3001,
        vec![CorpContainerLogEntry {
            logged_at_unix: 1000,
            container_id: 5002,
            container_type_id: 17366,
            character_id: 2001,
            location_id: 60003760,
            location_flag: "CorpSAG1".into(),
            action: "add".into(),
            password_type: None,
            type_id: Some(34),
            quantity: Some(100),
            old_config_bitmask: None,
            new_config_bitmask: None,
        }],
    );
    snap.corp_medals.insert(
        3001,
        vec![CorpMedalEntry {
            medal_id: 1,
            title: "Star".into(),
            description: "Good".into(),
            creator_id: 2001,
            created_at_unix: 1000,
        }],
    );
    snap.corp_issued_medals.insert(
        3001,
        vec![CorpIssuedMedalEntry {
            medal_id: 1,
            character_id: 2001,
            reason: "Merit".into(),
            status: "public".into(),
            issuer_id: 2001,
            issued_at_unix: 1001,
        }],
    );
    snap.corp_member_limits.insert(3001, 50);
    snap.corp_titles
        .insert(3001, vec![CorpTitleEntry::default()]);
    snap.corp_member_titles.insert((3001, 2001), vec![1]);
    snap.corp_role_assignments
        .insert((3001, 2001), CorpMemberRoleAssignment::default());
    snap.corp_role_history.insert(
        3001,
        vec![CorpRoleHistoryEntry {
            character_id: 2001,
            changed_at_unix: 1000,
            issuer_id: 2001,
            role_type: "roles".into(),
            old_roles: vec![],
            new_roles: vec!["corpRoleDirector".into()],
        }],
    );
    snap.corp_shareholders.insert(
        3001,
        vec![CorpShareholderEntry {
            shareholder_id: 2001,
            shareholder_type: "character".into(),
            share_count: 1000,
        }],
    );
    snap.starbase_settings
        .insert(1_020_000_000_002, StarbaseDetailSettings::default());
    snap.corp_project_contributors.insert(
        9801,
        vec![CorpProjectContributorEntry {
            character_id: 2001,
            contributed_quantity: 10,
        }],
    );
    snap.char_research_agents.insert(
        2001,
        vec![CharacterResearchAgentRecord {
            agent_id: 3008416,
            skill_type_id: 11441,
            started_at_unix: 1000,
            points_per_day: 25.0,
            remainder_points: 10.0,
        }],
    );
    snap.char_medals.insert(
        2001,
        vec![CharacterMedalRecord {
            medal_id: 1,
            title: "Star".into(),
            description: "Good".into(),
            corporation_id: 3001,
            issuer_id: 2001,
            date_unix: 1001,
            reason: "Merit".into(),
            status: "public".into(),
            graphics: vec![],
        }],
    );
    snap.char_scoped_roles
        .insert(2001, CharacterScopedRolesRecord::default());
    snap.loyalty_store_offers.insert(
        1000035,
        vec![LoyaltyStoreOfferRecord {
            offer_id: 1,
            type_id: 17715,
            quantity: 1,
            lp_cost: 5000,
            isk_cost: 1_000_000,
            ak_cost: None,
            required_items: vec![],
        }],
    );
    snap.wallet_journal_timestamps.insert(9101, 1_700_000_000);
    snap.wallet_journal_meta.insert(
        9101,
        JournalMeta {
            ref_type_id: 10,
            first_party_id: 2002,
            second_party_id: 2001,
            context_id: 5001,
            context: JournalContext::Character,
            account_key: 1000,
            date_unix: 1_700_000_000,
        },
    );
    snap.char_wallet_transactions.insert(
        2001,
        vec![WalletTransactionRecord {
            transaction_id: 1,
            date_unix: 1_700_000_000,
            location_id: 60003760,
            type_id: 34,
            unit_price: 6.0,
            quantity: 100,
            client_id: 2002,
            is_buy: true,
            is_personal: true,
            journal_ref_id: 9101,
        }],
    );
    snap.corp_wallet_journals.insert(
        (3001, 1),
        vec![CorpWalletJournalRecord {
            id: 2,
            date_unix: 1_700_000_000,
            ref_type: "office_rental_fee".into(),
            description: "rent".into(),
            amount: -1000.0,
            balance: 99_000.0,
            first_party_id: Some(3001),
            second_party_id: None,
            reason: None,
        }],
    );
    snap.corp_wallet_transactions.insert(
        (3001, 1),
        vec![WalletTransactionRecord {
            transaction_id: 3,
            date_unix: 1_700_000_000,
            location_id: 60003760,
            type_id: 34,
            unit_price: 6.0,
            quantity: 50,
            client_id: 2002,
            is_buy: false,
            is_personal: false,
            journal_ref_id: 2,
        }],
    );
    snap.market_order_meta.insert(
        9001,
        MarketOrderMeta {
            issued_unix: 1_700_000_000,
            duration_days: 90,
            range: "region".into(),
            issued_by: Some(2001),
            wallet_division: None,
            is_corporation: false,
            state: Some("open".into()),
        },
    );
    snap.market_transactions.push(MarketTransactionRecord {
        transaction_id: 55,
        date_unix: 1_700_000_000,
        type_id: 34,
        quantity: 10,
        price: IskCents(600),
        location_id: 60003760,
        buyer_id: 2001,
        seller_id: 2002,
        buyer_account_key: 1000,
        seller_account_key: 1000,
        journal_ref_id: 9101,
    });
    snap.account_plex_vault.insert(1001, 500);
    snap.market_history.insert(
        (10000002, 34),
        vec![MarketHistoryDayRecord {
            date: "2023-11-14".into(),
            average: 6.0,
            highest: 6.5,
            lowest: 5.5,
            order_count: 20,
            volume: 100_000,
            open_price: Some(5.8),
            close_price: Some(6.2),
        }],
    );
    snap.planetary_colony_layouts
        .insert((2001, 40000001), PlanetaryColonyLayoutRecord::default());
    snap.corp_customs_offices.insert(
        3001,
        vec![CustomsOfficeRecord {
            office_id: 1_020_000_000_005,
            system_id: 30000142,
            reinforce_exit_start: 18,
            reinforce_exit_end: 20,
            allow_alliance_access: true,
            allow_access_with_standings: true,
            standing_level: None,
            corporation_tax_rate: Some(0.05),
            alliance_tax_rate: None,
            excellent_standing_tax_rate: None,
            good_standing_tax_rate: None,
            neutral_standing_tax_rate: None,
            bad_standing_tax_rate: None,
            terrible_standing_tax_rate: None,
        }],
    );
    snap.corp_vote_cases.insert(
        1,
        CorpVoteCaseRecord {
            case_id: 1,
            corporation_id: 3001,
            vote_type: 1,
            text: "Elect CEO".into(),
            description: "Vote".into(),
            parameter: 2001,
            start_ts: 1000,
            end_ts: 2000,
            executed: false,
            options: vec![(1, "Yes".into(), 10)],
        },
    );
    snap.corp_vote_ballots.insert(
        (1, 2001),
        CorpVoteBallotRecord {
            case_id: 1,
            voter_id: 2001,
            option_id: 1,
            weight: 10,
            voted_at_unix: 1500,
        },
    );
    snap.orbital_provision_types.insert(2001, 2233);
    snap.corp_lp_balances.insert((2001, 1000035), 2500);
    snap.char_lp_logs.insert(
        2001,
        vec![LpRewardLogEntry {
            log_id: 1,
            corp_id: 1000035,
            amount: 2500,
            reason: "mission".into(),
            timestamp_unix: 1_700_000_000,
        }],
    );
    snap.mailing_lists.insert(
        1,
        MailingListRecord {
            list_id: 1,
            name: "All".into(),
            owner_id: 2001,
            default_access: 1,
            default_role: 1,
            cost: 0,
            members: HashMap::from([(2001, 2)]),
            entity_access: HashMap::new(),
            welcome_mail: Some(("Welcome".into(), "Hi".into())),
            mail_ids: HashSet::from([9501]),
        },
    );
    snap.calendar_events.insert(
        1,
        CalendarEventRecord {
            event_id: 1,
            owner_id: 2001,
            creator_id: 2001,
            event_date_time: 1_700_000_000,
            duration: 60,
            importance: 1,
            title: "Op".into(),
            description: "Form up".into(),
            scope: "corp".into(),
            responses: HashMap::from([(2001, 1)]),
            is_deleted: false,
        },
    );
    let contact = SocialContactRecord {
        contact_id: 2002,
        contact_type: "character".into(),
        standing: 5.0,
        is_watched: true,
        is_blocked: false,
        label_ids: vec![1],
    };
    snap.char_contacts.insert(2001, vec![contact.clone()]);
    snap.corp_contacts.insert(3001, vec![contact.clone()]);
    snap.alliance_contacts.insert(4001, vec![contact]);
    snap.char_contact_labels.insert(2001, vec![(1, "Friends".into())]);
    snap.corp_contact_labels
        .insert(3001, vec![(1, "Allies".into(), 255)]);
    snap.alliance_contact_labels
        .insert(4001, vec![(1, "Blues".into())]);
    snap.char_notes.insert((2001, 2002), "Wingman".into());
    snap.chat_channel_acl
        .insert("player_1".into(), ChatChannelAclRecord::default());
    snap.employment_history
        .insert(2001, vec![(3001, 1000, None)]);
    snap.crimewatch_states.insert(
        2001,
        PersistedCrimeWatchState {
            character_id: 2001,
            weapon_timer_until_unix: 1500,
            pvp_timer_until_unix: 1900,
            suspect_flag_until_unix: 0,
            criminal_flag_until_unix: 2000,
            pending_concord_arrival: None,
        },
    );
    snap.crimewatch_aggressions.insert(
        (2001, 2002),
        PersistedAggressionRecord {
            aggressor_char_id: 2001,
            victim_char_id: 2002,
            system_id: 30000142,
            started_unix: 1000,
            expires_unix: 1800,
            is_legal: false,
            is_pod: false,
        },
    );
    snap.zarzakh_gate_locks.insert(2001, (30100001, 1800));
    snap.structure_docking_acl_denied
        .insert(1_020_000_000_001, HashSet::from([2002]));
}

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
