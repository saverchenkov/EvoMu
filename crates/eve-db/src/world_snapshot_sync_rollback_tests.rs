use crate::types::*;
use crate::IskCents;
use std::collections::{HashMap, HashSet};

pub(crate) fn populate_world_part1(snap: &mut WorldStateSnapshot) {
    snap.next_entity_id = 10_000_900;
    snap.wal_seq = 42;
    snap.markets_seeded = true;
    snap.next_alliance_id = 99_000_002;
    snap.next_corporation_id = 98_000_002;
    snap.next_notification_id = 777;
    snap.next_report_id = 888;
    snap.service_state_blobs.insert("svc".into(), "blob_v1".into());
    snap.quasar_state_blobs.insert("qsvc".into(), "qblob_v1".into());

    snap.accounts.insert(
        "alice".into(),
        AccountRecord {
            user_id: 1001,
            username: "alice".into(),
            password_token: "$argon2id$stub".into(),
            role: 2,
            banned_until_unix: None,
            ban_reason: None,
        },
    );
    snap.characters.insert(
        2001,
        CharacterRecord {
            activity_status: Some("Docked".into()),
            character_id: 2001,
            user_id: 1001,
            name: "Alice Pilot".into(),
            corporation_id: 3001,
            alliance_id: Some(4001),
            war_faction_id: Some(500_001),
            station_id: Some(60003760),
            structure_id: None,
            solar_system_id: 30000142,
            constellation_id: 20000020,
            region_id: 10000002,
            active_ship_id: 5001,
            balance: IskCents(1_500_000),
            plex_balance: 500,
            loyalty_points: HashMap::from([(1000035, 250)]),
            standings: HashMap::from([(1000035, 1.5)]),
            security_status: 0.5,
            clone_grade: 2,
            home_station_id: 60003760,
            race_id: 1,
            bloodline_id: 1,
            ancestry_id: 1,
            school_id: 11,
            neural: NeuralAttributes::default(),
            skill_points: 50_000.0,
            free_skill_points: 10_000.0,
            last_clone_jump_unix: 100,
            jump_fatigue_minutes: 0.0,
            jump_cooldown_until_unix: 0,
            jump_fatigue_until_unix: 0,
            description: "pilot".into(),
            title: "cdr".into(),
            gender: 1,
            create_date_unix: 1_600_000_000,
            fleet_id: Some(9601),
            autopilot_waypoints: vec![30000144],
            version: 1,
            system_epoch: 1,
        },
    );
    snap.skills.entry(2001).or_default().insert(
        3300,
        TrainedSkillRecord {
            type_id: 3300,
            level: 2,
            skill_points: 1000.0,
        },
    );
    snap.skill_queues.insert(
        2001,
        vec![SkillQueueEntry {
            type_id: 3300,
            target_level: 3,
            start_sp: 1000.0,
            end_sp: 8000.0,
            duration_seconds: 1200.0,
            started_at_unix: Some(1_700_000_000),
        }],
    );
    snap.jump_clones.insert(
        2001,
        vec![JumpCloneRecord {
            jump_clone_id: 7001,
            character_id: 2001,
            location_id: 60003760,
            implants: vec![9941],
        }],
    );
    snap.active_implants.insert(2001, vec![9941]);
    snap.items.insert(
        5001,
        InventoryItemRecord {
            item_id: 5001,
            type_id: 606,
            owner_id: 2001,
            location_id: 60003760,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..InventoryItemRecord::default()
        },
    );
    snap.saved_fittings.insert(
        8001,
        SavedFittingRecord {
            fitting_id: 8001,
            owner_id: 2001,
            name: "Scout Fit".into(),
            ship_type_id: 606,
            description: "fast".into(),
            items: vec![(3170, InventoryFlag::HiSlot0, 1)],
        },
    );
    snap.market_orders.insert(
        9001,
        MarketOrderRecord {
            order_id: 9001,
            owner_id: 2001,
            station_id: 60003760,
            region_id: 10000002,
            type_id: 34,
            is_bid: false,
            price: IskCents(600),
            vol_entered: 100,
            vol_remaining: 100,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents(0),
        },
    );
    snap.wallet_journal.push(WalletJournalEntry {
        entry_id: 9101,
        character_id: 2001,
        entry_type: "player_donation".into(),
        amount: IskCents(5000),
        balance_after: IskCents(1_500_000),
        reason: "gift".into(),
    });
    snap.contracts.insert(
        9201,
        ContractRecord {
            contract_id: 9201,
            issuer_id: 2001,
            acceptor_id: Some(2002),
            contract_type: "Courier".into(),
            status: "InProgress".into(),
            start_station_id: 60003760,
            end_station_id: 60008494,
            reward: IskCents(10_000),
            collateral: IskCents(50_000),
            item_ids: vec![5002],
            expiration_unix: 1_900_000_000,
            title: "Haul".into(),
            ..ContractRecord::default()
        },
    );
    snap.industry_jobs.insert(
        9301,
        IndustryJobRecord {
            job_id: 9301,
            installer_id: 2001,
            facility_id: 60003760,
            blueprint_item_id: 5001,
            blueprint_type_id: 687,
            activity_id: 1,
            runs: 2,
            product_type_id: 587,
            status: "Active".into(),
            duration_seconds: 3600,
            start_unix: 1000,
            blueprint_location_id: 60003760,
            output_location_id: 60003760,
            cost: Some(1200.5),
            licensed_runs: Some(10),
            probability: Some(0.5),
            completed_unix: None,
            completed_character_id: None,
        },
    );
    snap.planetary_colonies.insert(
        (2001, 40000001),
        PlanetaryColonyRecord {
            planet_id: 40000001,
            owner_id: 2001,
            solar_system_id: 30000142,
            command_center_level: 3,
            pins_count: 2,
            extracted_commodities: HashMap::from([(2389, 250)]),
            last_extracted_unix: 1_700_000_100,
        },
    );
    snap.active_missions.insert(
        (2001, 101),
        ActiveMissionRecord {
            mission_id: 101,
            character_id: 2001,
            agent_id: 3008416,
            solar_system_id: 30000142,
            state: "Accepted".into(),
            current_room_id: 1,
            npcs_remaining: 4,
            spawned_npc_ball_ids: vec![70001],
            bookmark_id: Some(9401),
        },
    );
    snap.mission_tally.insert(2001, 5);
    snap.standing_transactions.insert(
        2001,
        vec![StandingTransactionRecord {
            event_id: 1,
            from_id: 1000035,
            to_id: 2001,
            modification: 0.25,
            event_type_id: 10,
        }],
    );
    snap.npe_progress.insert(
        2001,
        NpeProgressRecord {
            character_id: 2001,
            node_graph_id: 1,
            current_node_id: 2,
            completed: false,
            career_goal_progress: HashMap::from([(1, 100)]),
            claimed_goals: HashSet::from([1]),
        },
    );
    snap.bookmarks.insert(
        9401,
        BookmarkRecord {
            bookmark_id: 9401,
            owner_id: 2001,
            label: "Safe".into(),
            solar_system_id: 30000142,
            x: 1.0,
            y: 2.0,
            z: 3.0,
            item_id: Some(60003760),
            memo: "safe spot".into(),
            folder_id: Some(9450),
            is_shared: true,
        },
    );
    snap.mails.push(MailRecord {
        message_id: 9501,
        sender_id: 2001,
        recipient_id: 2002,
        subject: "Hi".into(),
        body: "Hello".into(),
    });
    snap.fleets.insert(
        9601,
        FleetRecord {
            fleet_id: 9601,
            boss_id: 2001,
            members: vec![2001],
            wings: vec![9610],
            squads: vec![9620],
            free_move: true,
            is_registered: true,
            motd: "Welcome".into(),
            ..FleetRecord::default()
        },
    );
    snap.corporations.insert(
        3001,
        CorporationRecord {
            corporation_id: 3001,
            name: "Test Corp".into(),
            ticker: "TSTC".into(),
            ceo_id: 2001,
            alliance_id: Some(4001),
            tax_rate: 0.05,
            master_wallet: IskCents(10_000_000),
            members: HashSet::from([2001]),
            member_roles: HashMap::from([(2001, corp_roles::ROLE_DIRECTOR)]),
            wallet_divisions: CorporationRecord::default_wallet_divisions(IskCents(10_000_000)),
            ..CorporationRecord::default()
        },
    );
    snap.alliances.insert(
        4001,
        AllianceRecord {
            alliance_id: 4001,
            name: "Test Alliance".into(),
            short_name: "TSTA".into(),
            executor_corp_id: 3001,
            member_corp_ids: HashSet::from([3001]),
        },
    );
    snap.wars.insert(
        9701,
        WarRecord {
            war_id: 9701,
            aggressor_corp_id: 3001,
            defender_corp_id: 3002,
            mutual: true,
            started_unix: 1000,
            active: true,
        },
    );
    snap.corp_projects.insert(
        9801,
        CorpProjectRecord {
            project_id: 9801,
            corporation_id: 3001,
            name: "Mine".into(),
            target_quantity: 100,
            current_quantity: 10,
            isk_per_unit: IskCents(500),
            escrow_remaining: IskCents(45_000),
        },
    );
    snap.killmails.insert(
        9901,
        KillmailRecord {
            kill_id: 9901,
            victim_character_id: 2002,
            attacker_character_id: 2001,
            solar_system_id: 30000142,
            ship_type_id: 606,
            total_value_isk: 125_000.0,
            killed_unix: 1_700_000_500,
            victim_corp_id: 3002,
            victim_alliance_id: None,
            attacker_corp_id: 3001,
            attacker_alliance_id: Some(4001),
        },
    );
    snap.kill_rights.insert(
        10101,
        KillRightRecord {
            kill_right_id: 10101,
            owner_id: 2001,
            target_id: 2002,
            activation_fee: IskCents(100_000),
            is_active_suspect: true,
        },
    );
    snap.structures.insert(
        1_020_000_000_001,
        UpwellStructureRecord {
            structure_id: 1_020_000_000_001,
            type_id: 35832,
            owner_corp_id: 3001,
            solar_system_id: 30000142,
            name: "Fort".into(),
            state: "ShieldVulnerable".into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            ..UpwellStructureRecord::default()
        },
    );
    snap.system_states.insert(
        30000142,
        SystemDynamicState {
            solar_system_id: 30000142,
            dbs_multiplier: 1.0,
            sov_alliance_id: Some(4001),
            military_index: 2,
            ..SystemDynamicState::default()
        },
    );
    snap.committed_idempotency_keys.insert((2001, "idem_1".into()));
    snap.licensed_skins.insert(2001, vec![12345]);
    snap.applied_ship_skins.insert(5001, 12345);
    snap.chat_channels.insert(
        "player_1".into(),
        ChatChannelRecord {
            channel_id: "player_1".into(),
            name: "Fleet".into(),
            motd: "Fly safe".into(),
            owner_id: Some(2001),
            members: HashSet::from([2001]),
            is_system_local: false,
            solar_system_id: None,
            is_private: true,
            operators: HashSet::from([2001]),
            password: Some("pw".into()),
            is_delayed_local: false,
        },
    );
    snap.cosmic_signatures.insert(
        30000142,
        vec![CosmicSignatureRecord {
            signature_id: "ABC-123".into(),
            solar_system_id: 30000142,
            sig_group: "Wormhole".into(),
            type_name: "C1".into(),
            position: [1.0, 2.0, 3.0],
            signal_strength: 0.5,
            wormhole_type_id: Some(30574),
            destination_system_id: Some(31000005),
            created_unix: 100,
            lifetime_seconds: 86400,
            max_mass_kg: 500_000.0,
            remaining_mass_kg: 500_000.0,
            max_jump_mass_kg: 20_000.0,
            collapsed: false,
        }],
    );
    snap.cosmic_anomalies.insert(
        30000142,
        vec![CosmicAnomalyRecord {
            anomaly_id: "ANM-1".into(),
            solar_system_id: 30000142,
            name: "Hideaway".into(),
            anomaly_type: "Combat".into(),
            position: [2.0, 3.0, 4.0],
            signal_strength: 1.0,
            ore_types: vec![1230],
            remaining_volume: 1000.0,
            created_unix: 100,
            lifetime_seconds: 3600,
            cleared: false,
        }],
    );
    snap.read_mails.insert((2001, 9501));
    snap.deleted_mails.insert((2002, 9501));
    snap.trashed_mails.insert((2001, 9502));
    snap.mail_labels.insert(
        2001,
        vec![MailLabelRecord {
            label_id: 1,
            name: "Urgent".into(),
            color: "#ff0000".into(),
        }],
    );
    snap.mail_assigned_labels.insert((2001, 9501), vec![1]);
    snap.mail_timestamps.insert(9501, 1_700_000_000);
    snap.bookmark_folders.insert(
        9450,
        BookmarkFolderRecord {
            folder_id: 9450,
            owner_id: 2001,
            folder_name: "Tacticals".into(),
            is_shared: true,
            corporation_id: Some(3001),
            ..BookmarkFolderRecord::default()
        },
    );
    snap.bookmark_subfolders.insert(9451, 9450);
    snap.bookmark_subfolder_parents.insert(9451, 9450);
    snap.bookmark_expiries.insert(9401, 2_000_000_000);
    snap.bookmark_folder_acls.insert(9450, vec![1, 2]);
    snap.access_groups.insert(1, vec![2001, 2002]);
    snap.bookmark_folder_known_by.insert((2001, 9450), true);
    snap.wh_polarizations.insert((9402, 2001), 3);
    snap.mining_ledger.push(MiningLedgerRecord {
        character_id: 2001,
        solar_system_id: 30000142,
        type_id: 1230,
        quantity: 1000,
        date: unix_secs_to_date_string(1_700_000_000),
        timestamp_unix: 1_700_000_000,
    });
    snap.module_online_states.insert(5100, true);
    snap.module_active_states.insert(5100);
    snap.module_active_charges.insert(5100, 178);
    snap.weapon_banks
        .entry(5001)
        .or_default()
        .insert(0, vec![5100]);
    snap.asteroids.insert(
        71001,
        AsteroidRecord {
            asteroid_id: 71001,
            solar_system_id: 30000142,
            belt_id: 40000002,
            type_id: 1230,
            quantity: 5000,
            initial_quantity: 5000,
            position: (1.0, 2.0, 3.0),
            radius: 150.0,
        },
    );
    snap.moon_extractions.insert(
        1_020_000_000_001,
        MoonExtractionRecord {
            structure_id: 1_020_000_000_001,
            moon_id: 40000003,
            extraction_start_unix: 100,
            chunk_arrival_unix: 200,
            natural_decay_unix: 300,
            status: "Started".into(),
            auto_fracture: true,
            event_id: 1,
            moon_ores: vec![(45490, 1.0)],
            spawned_asteroid_ids: vec![71001],
        },
    );
    snap.auto_moon_miners.insert(
        1_020_000_000_001,
        AutoMoonMinerRecord {
            structure_id: 1_020_000_000_001,
            fuel_units_remaining: 100,
            magmatic_gas_remaining: 50,
            efficiency: 0.9,
            cycle_duration_seconds: 3600,
            last_cycle_unix: 100,
            ore_type_id: 45490,
            hourly_yield: 200,
            status: "Active".into(),
        },
    );
    snap.observer_mining_ledger.insert(
        1_020_000_000_001,
        vec![ObserverMiningRecord {
            observer_id: 1_020_000_000_001,
            character_id: 2001,
            recorded_corporation_id: 3001,
            type_id: 45490,
            quantity: 300,
            last_updated_date: "2023-11-14".into(),
            timestamp_unix: 1_700_000_000,
        }],
    );
    snap.owner_notes.insert(
        1,
        OwnerNoteRecord {
            note_id: 1,
            character_id: 2001,
            item_id: 5001,
            label: "TODO".into(),
            note: "Refuel".into(),
            timestamp_unix: 1_700_000_000,
        },
    );
}
