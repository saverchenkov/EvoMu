//! Pre-seeded canonical demonstration world fixtures for offline testing.

use crate::accounts::service_roles;
use crate::persistence::WorldRepository;
use crate::sqlite_store;
use crate::types::*;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

impl WorldRepository {
    pub fn with_demo_seed() -> Self {
        let mut accounts = HashMap::new();
        accounts.insert(
            "demo".to_string(),
            AccountRecord {
                user_id: 1,
                username: "demo".to_string(),
                password_token: Self::hash_password_token("demo", "demo-secret"),
                role: service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        accounts.insert(
            "other".to_string(),
            AccountRecord {
                user_id: 2,
                username: "other".to_string(),
                password_token: Self::hash_password_token("other", "other-secret"),
                role: service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );

        let mut characters = HashMap::new();
        characters.insert(
            90_000_001,
            CharacterRecord {
                character_id: 90_000_001,
                user_id: 1,
                name: "Aura Capsuleer".to_string(),
                corporation_id: 1_000_170, // Republic Military School (school 14)
                alliance_id: None,
                war_faction_id: None,
                station_id: Some(60_003_760), // Jita 4-4
                structure_id: None,
                solar_system_id: 30_000_142, // Jita
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: 100_000_001,
                balance: IskCents::from_isk(1_000_000_000),
                plex_balance: 500,
                loyalty_points: HashMap::from([(1_000_035, 5_000)]),
                standings: HashMap::from([(1_000_035, 2.0)]),
                security_status: 0.5,
                clone_grade: 2, // Omega
                home_station_id: 60_003_760,
                race_id: 2,
                bloodline_id: 3,
                ancestry_id: 19,
                school_id: 14, // Republic Military School (Minmatar)
                neural: NeuralAttributes::default(),
                skill_points: 15_000_000.0,
                free_skill_points: 50_000.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 1704067200,
                version: 1,
                system_epoch: 1,
            },
        );
        characters.insert(
            90_000_002,
            CharacterRecord {
                character_id: 90_000_002,
                user_id: 2,
                name: "Rival Pilot".to_string(),
                corporation_id: 1_000_045, // Science and Trade Institute (school 18)
                alliance_id: None,
                war_faction_id: None,
                station_id: Some(60_003_760),
                structure_id: None,
                solar_system_id: 30_000_142,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: 100_000_002,
                balance: IskCents::from_isk(50_000_000),
                plex_balance: 100,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 1, // Alpha
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 11,
                school_id: 18, // Science and Trade Institute (Caldari)
                neural: NeuralAttributes::default(),
                skill_points: 4_500_000.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 1704067200,
                version: 1,
                system_epoch: 1,
            },
        );

        let mut items = HashMap::new();
        items.insert(
            100_000_001,
            InventoryItemRecord {
                item_id: 100_000_001,
                type_id: 587, // Rifter
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        items.insert(
            100_000_002,
            InventoryItemRecord {
                item_id: 100_000_002,
                type_id: 587,
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Fitted 200mm AutoCannon II on active ships 100_000_001 and 100_000_002
        items.insert(
            100_000_005,
            InventoryItemRecord {
                item_id: 100_000_005,
                type_id: 2889, // 200mm AutoCannon II
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        items.insert(
            100_000_006,
            InventoryItemRecord {
                item_id: 100_000_006,
                type_id: 2889, // 200mm AutoCannon II
                owner_id: 90_000_002,
                location_id: 100_000_002,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );

        // Seed only starter frigate modules, basic minerals/ore, and an unresearched Rifter BPO (IDs 100_000_003..=100_000_012)
        let starter_items = [
            (100_000_003, 438, 1, true),        // 1MN Afterburner II
            (100_000_004, 1236, 2, true),       // Overdrive Injector System II
            (100_000_007, 691, 1, true),        // Rifter Blueprint (unresearched BPO ME0/TE0)
            (100_000_008, 34, 100_000, false),  // Tritanium
            (100_000_009, 35, 50_000, false),   // Pyerite
            (100_000_010, 36, 20_000, false),   // Mexallon
            (100_000_011, 37, 5_000, false),    // Isogen
            (100_000_012, 1230, 10_000, false), // Veldspar
        ];
        for (id, tid, qty, single) in starter_items {
            items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: 90_000_001,
                    location_id: 60_003_760,
                    flag: InventoryFlag::Hangar,
                    quantity: qty,
                    singleton: single,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }

        // Seed NPC & synthetic market orders in Jita 4-4 (60_003_760)
        let mut market_orders = HashMap::new();
        market_orders.insert(
            700_000_001,
            MarketOrderRecord {
                order_id: 700_000_001,
                owner_id: 1_000_035, // Caldari Navy NPC
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34, // Tritanium
                is_bid: false,
                price: IskCents::from_isk(5),
                vol_entered: 1_000_000,
                vol_remaining: 1_000_000,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        market_orders.insert(
            700_000_002,
            MarketOrderRecord {
                order_id: 700_000_002,
                owner_id: 1_000_035,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 438, // 1MN Afterburner II
                is_bid: false,
                price: IskCents::from_isk(550_000),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );

        let mut system_states = HashMap::new();
        for sys_id in [30_000_142, 30_000_144, 30_002_813, 30_002_904, 31_000_005] {
            system_states.insert(
                sys_id,
                SystemDynamicState {
                    solar_system_id: sys_id,
                    dbs_multiplier: 1.35,
                    ess_main_bank: IskCents(0),
                    ess_reserve_bank: IskCents(0),
                    ess_linked_character_id: None,
                    ess_link_started_tick: None,
                    sov_alliance_id: None,
                    military_index: 2,
                    industrial_index: 2,
                    strategic_index: 3,
                    fw_contested_pct: 0.0,
                    insurgency_corruption_stage: 2,
                    insurgency_suppression_stage: 1,
                    wormhole_remaining_mass_kg: 0.0,
                    wormhole_destination_system_id: None,
                    signature_position: None,
                    fw_complex_position: None,
                    fw_capture_seconds_elapsed: 0,
                    fw_capture_duration_seconds: 0,
                    last_fw_capture_tick: 0,
                    last_sov_claim_tick: 0,
                },
            );
        }

        let mut npe_progress = HashMap::new();
        npe_progress.insert(
            90_000_001,
            NpeProgressRecord {
                character_id: 90_000_001,
                node_graph_id: 473,
                current_node_id: 1,
                completed: false,
                career_goal_progress: HashMap::new(),
                claimed_goals: HashSet::new(),
            },
        );

        let mut skills = HashMap::new();
        let starter_skill_map = HashMap::from([
            (
                3300,
                TrainedSkillRecord {
                    type_id: 3300, // Gunnery
                    level: 4,
                    skill_points: 45_255.0,
                },
            ),
            (
                3302,
                TrainedSkillRecord {
                    type_id: 3302, // Small Projectile Turret
                    level: 5,
                    skill_points: 256_000.0,
                },
            ),
            (
                11084,
                TrainedSkillRecord {
                    type_id: 11084, // Small Autocannon Specialization
                    level: 1,
                    skill_points: 750.0,
                },
            ),
            (
                3327,
                TrainedSkillRecord {
                    type_id: 3327, // Spaceship Command
                    level: 4,
                    skill_points: 45_255.0,
                },
            ),
            (
                3329,
                TrainedSkillRecord {
                    type_id: 3329, // Minmatar Frigate
                    level: 4,
                    skill_points: 90_510.0,
                },
            ),
            (
                3380,
                TrainedSkillRecord {
                    type_id: 3380, // Industry
                    level: 5,
                    skill_points: 256_000.0,
                },
            ),
            (
                3394,
                TrainedSkillRecord {
                    type_id: 3394, // Hull Upgrades
                    level: 2,
                    skill_points: 2_829.0,
                },
            ),
            (
                3443,
                TrainedSkillRecord {
                    type_id: 3443, // Trade
                    level: 4,
                    skill_points: 45_255.0,
                },
            ),
            (
                3446,
                TrainedSkillRecord {
                    type_id: 3446, // Broker Relations
                    level: 4,
                    skill_points: 90_510.0,
                },
            ),
            (
                3449,
                TrainedSkillRecord {
                    type_id: 3449, // Navigation
                    level: 4,
                    skill_points: 45_255.0,
                },
            ),
            (
                3450,
                TrainedSkillRecord {
                    type_id: 3450, // Afterburner
                    level: 4,
                    skill_points: 45_255.0,
                },
            ),
            (
                3452,
                TrainedSkillRecord {
                    type_id: 3452, // Acceleration Control
                    level: 4,
                    skill_points: 181_020.0,
                },
            ),
            (
                16622,
                TrainedSkillRecord {
                    type_id: 16622, // Accounting
                    level: 4,
                    skill_points: 135_765.0,
                },
            ),
        ]);
        skills.insert(90_000_001, starter_skill_map.clone());
        skills.insert(90_000_002, starter_skill_map);

        let snapshot_path = std::env::var("EVOMU_SNAPSHOT_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from);

        let initial_snap = WorldStateSnapshot {
            next_entity_id: 200_000_000,
            wal_seq: 0,
            markets_seeded: false,
            accounts,
            characters,
            skills,
            skill_queues: HashMap::new(),
            jump_clones: HashMap::new(),
            active_implants: HashMap::new(),
            items,
            saved_fittings: HashMap::new(),
            market_orders,
            wallet_journal: Vec::new(),
            contracts: HashMap::new(),
            industry_jobs: HashMap::new(),
            planetary_colonies: HashMap::new(),
            active_missions: HashMap::new(),
            mission_tally: HashMap::new(),
            standing_transactions: HashMap::new(),
            npe_progress,
            bookmarks: HashMap::new(),
            mails: Vec::new(),
            fleets: HashMap::new(),
            corporations: HashMap::new(),
            alliances: HashMap::new(),
            wars: HashMap::new(),
            corp_projects: HashMap::new(),
            killmails: HashMap::new(),
            kill_rights: HashMap::new(),
            structures: HashMap::new(),
            system_states,
            committed_idempotency_keys: HashSet::new(),
            licensed_skins: HashMap::new(),
            applied_ship_skins: HashMap::new(),
            chat_channels: HashMap::new(),
            cosmic_signatures: HashMap::new(),
            read_mails: HashSet::new(),
            deleted_mails: HashSet::new(),
            mail_labels: HashMap::new(),
            mail_assigned_labels: HashMap::new(),
            bookmark_folders: HashMap::new(),
            wh_polarizations: HashMap::new(),
            mining_ledger: Vec::new(),
            module_online_states: HashMap::new(),
            weapon_banks: HashMap::new(),
            asteroids: HashMap::new(),
            moon_extractions: HashMap::new(),
            auto_moon_miners: HashMap::new(),
            observer_mining_ledger: HashMap::new(),
            owner_notes: HashMap::new(),
            paused_skill_queues: HashSet::new(),
        };
        let mut conn = rusqlite::Connection::open_in_memory()
            .expect("open in-memory SQLite connection for WorldRepository");
        sqlite_store::apply_migrations(&conn).expect("apply SQLite migrations for WorldRepository");
        let fp = sqlite_store::replace_sqlite_from_snapshot(&mut conn, &initial_snap)
            .expect("initialize SQLite from demo snapshot");
        let jwt_secret =
            sqlite_store::load_or_init_jwt_secret(&conn, Self::generate_random_secret_32)
                .expect("initialize per-deployment JWT signing secret");

        let mut repo = Self {
            inner: RwLock::new(initial_snap),
            sqlite_conn: std::sync::Mutex::new(conn),
            sqlite_fp: std::sync::Mutex::new(fp),
            sqlite_path: None,
            auth_runtime: RwLock::new(AuthRuntimeState::new(jwt_secret)),
            clock: std::sync::RwLock::new(std::sync::Arc::new(
                crate::clock::VirtualServerClock::new(),
            )),
            wal_lock: std::sync::Mutex::new(()),
            snapshot_path: None,
            pending_wal_records: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            wal_fault_mode: std::sync::atomic::AtomicU8::new(0),
        };

        if let Some(ref p) = snapshot_path {
            if p.exists() {
                repo.load_snapshot_from_file(p)
                    .expect("Failed to load persisted WorldRepository snapshot");
            } else {
                repo.save_snapshot_to_file(p)
                    .expect("Failed to initialize persisted WorldRepository snapshot");
            }
            repo.snapshot_path = snapshot_path;
        }
        repo
    }
}
