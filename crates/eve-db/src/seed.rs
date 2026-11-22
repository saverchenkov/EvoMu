//! Pre-seeded canonical demonstration world fixtures for offline testing.

use crate::accounts::service_roles;
use crate::persistence::WorldRepository;
use crate::sqlite_store;
use crate::types::*;
use std::collections::{HashMap, HashSet};

/// Computes SDE starter skills using the shared cached `SdeRegistry` (`SdeRegistry::open_shared`)
/// while honoring `EVOMU_SDE_PATH` under the test-env read lock and avoiding any extra cache static.
fn demo_starter_skills_for_default_sde(
    race_ids: &[u32],
) -> Option<HashMap<u32, HashMap<TypeId, TrainedSkillRecord>>> {
    let path = {
        let _env_guard = crate::lock_test_env_read();
        eve_sde::SdeRegistry::default_db_path()
    };
    let sde = eve_sde::SdeRegistry::open_shared(&path).ok()?;
    Some(
        race_ids
            .iter()
            .copied()
            .map(|race_id| (race_id, WorldRepository::starter_skills_for_race(race_id, &sde).0))
            .collect(),
    )
}

fn merge_seeded_skills_with_starter(
    starter: Option<&HashMap<TypeId, TrainedSkillRecord>>,
    seeded_overrides: &HashMap<TypeId, TrainedSkillRecord>,
) -> HashMap<TypeId, TrainedSkillRecord> {
    let mut merged = starter.cloned().unwrap_or_default();
    for (skill_id, seeded) in seeded_overrides {
        merged
            .entry(*skill_id)
            .and_modify(|existing| {
                // Preserve higher seeded specializations without regressing starter skills.
                if (seeded.level, seeded.skill_points) > (existing.level, existing.skill_points) {
                    *existing = seeded.clone();
                }
            })
            .or_insert_with(|| seeded.clone());
    }
    merged
}

fn char_creation_skill_history(
    skills: &HashMap<TypeId, TrainedSkillRecord>,
) -> Vec<SkillHistoryEntry> {
    let mut sorted: Vec<_> = skills.values().cloned().collect();
    sorted.sort_by_key(|s| s.type_id);
    sorted
        .into_iter()
        .map(|s| SkillHistoryEntry {
            log_date_ft: 133_485_408_000_000_000, // 2024-01-01T00:00:00Z in BlueOS FILETIME ticks
            event_type_id: 33,                   // SkillEventCharCreation (EVE_Skills.h:24)
            skill_type_id: s.type_id,
            skill_level: s.level,
            absolute_points: s.skill_points,
        })
        .collect()
}

impl WorldRepository {
    pub fn create_demo_snapshot() -> WorldStateSnapshot {
        Self::create_demo_snapshot_with_sde(None)
    }

    pub fn create_demo_snapshot_with_sde(
        sde: Option<&eve_sde::SdeRegistry>,
    ) -> WorldStateSnapshot {
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

        let mut characters = CowMap::new();
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
                activity_status: None,
            free_skill_points: 50_000.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 1704067200,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
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
                activity_status: None,
            free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 1704067200,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 1,
            },
        );

        let mut items = CowMap::new();
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
            (100_000_004, 1236, 2, false),      // Overdrive Injector System II
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
        let mut market_orders = CowMap::new();
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
                    ess_cycle_started_unix: 0,
                    ess_main_bank_contributors: HashMap::new(),
                    sov_alliance_id: None,
                    military_index: 2,
                    industrial_index: 2,
                    strategic_index: 3,
                    fw_contested_pct: 0.0,
                    insurgency_corruption_stage: 0,
                    insurgency_suppression_stage: 0,
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

        let seeded_skill_overrides = HashMap::from([
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
                3363,
                TrainedSkillRecord {
                    type_id: 3363, // Corporation Management
                    level: 1,
                    skill_points: 250.0,
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
            (
                3436,
                TrainedSkillRecord {
                    type_id: 3436, // Drones (R-DRONE-NOSKILL-5)
                    level: 5,
                    skill_points: 256_000.0,
                },
            ),
        ]);
        let demo_pilot_races: Vec<(i64, u32)> = [90_000_001_i64, 90_000_002_i64]
            .into_iter()
            .filter_map(|char_id| characters.get(&char_id).map(|ch| (char_id, ch.race_id)))
            .collect();
        let race_ids: Vec<u32> = demo_pilot_races.iter().map(|(_, race_id)| *race_id).collect();
        let starter_by_race = match sde {
            Some(sde) => Some(
                race_ids
                    .iter()
                    .copied()
                    .map(|race_id| (race_id, Self::starter_skills_for_race(race_id, sde).0))
                    .collect(),
            ),
            None => demo_starter_skills_for_default_sde(&race_ids),
        };

        let mut skills = CowMap::new();
        let mut skill_history = HashMap::new();
        for (char_id, race_id) in demo_pilot_races {
            let starter = starter_by_race.as_ref().and_then(|m| m.get(&race_id));
            let char_skills = merge_seeded_skills_with_starter(starter, &seeded_skill_overrides);
            skill_history.insert(char_id, char_creation_skill_history(&char_skills));
            skills.insert(char_id, char_skills);
        }

        let mut corporations = HashMap::new();
        corporations.insert(
            98_000_001,
            CorporationRecord {
                corporation_id: 98_000_001,
                name: "Demo Corporation".to_string(),
                ticker: "DEMO".to_string(),
                ceo_id: 90_000_001,
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(100_000_000),
                wallet_divisions: CorporationRecord::default_wallet_divisions(IskCents::from_isk(100_000_000)),
                members: [90_000_001, 90_000_002].into_iter().collect(),
                member_roles: HashMap::from([(90_000_001, u64::MAX), (90_000_002, 0)]),
                ..Default::default()
            },
        );

        let initial_snap = WorldStateSnapshot {
            next_entity_id: 200_000_000,
            wal_seq: 0,
            markets_seeded: false,
            accounts: accounts.into(),
            characters,
            skills,
            skill_queues: CowMap::new(),
            jump_clones: Default::default(),
            active_implants: Default::default(),
            items,
            saved_fittings: Default::default(),
            market_orders,
            wallet_journal: Default::default(),
            contracts: Default::default(),
            industry_jobs: Default::default(),
            planetary_colonies: Default::default(),
            active_missions: Default::default(),
            mission_tally: Default::default(),
            standing_transactions: Default::default(),
            npe_progress: npe_progress.into(),
            bookmarks: Default::default(),
            mails: Default::default(),
            fleets: Default::default(),
            corporations: corporations.into(),
            alliances: Default::default(),
            wars: Default::default(),
            corp_projects: Default::default(),
            killmails: Default::default(),
            kill_rights: Default::default(),
            structures: Default::default(),
            system_states: system_states.into(),
            committed_idempotency_keys: Default::default(),
            licensed_skins: Default::default(),
            applied_ship_skins: Default::default(),
            chat_channels: Default::default(),
            cosmic_signatures: Default::default(),
            read_mails: Default::default(),
            deleted_mails: Default::default(),
            mail_labels: Default::default(),
            mail_assigned_labels: Default::default(),
            bookmark_folders: Default::default(),
            wh_polarizations: Default::default(),
            mining_ledger: Default::default(),
            module_online_states: Default::default(),
            module_active_states: Default::default(),
            module_active_charges: Default::default(),
            ship_fit_inputs: Default::default(),
            weapon_banks: Default::default(),
            asteroids: Default::default(),
            moon_extractions: Default::default(),
            auto_moon_miners: Default::default(),
            observer_mining_ledger: Default::default(),
            owner_notes: crate::cow::CowMap::new(),
            paused_skill_queues: Default::default(),
            cosmic_anomalies: Default::default(),
            sov_prospecting_levels: Default::default(),
            last_mission_decline_times: Default::default(),
            character_last_jump_unix: Default::default(),
            character_cspa_charges: Default::default(),
            mail_timestamps: CowMap::new(),
            character_login_stats: Default::default(),
            item_custom_names: CowMap::new(),
            item_positions: Default::default(),
            structure_vulnerability_windows: Default::default(),
            skill_history: skill_history.into(),
            character_doll_info: Default::default(),
            fw_system_occupations: Default::default(),
            ..Default::default()
        };
        let mut initial_snap = initial_snap;
        initial_snap.fold_legacy_plex_balances();
        initial_snap.backfill_fw_system_occupations();
        initial_snap
    }

    pub fn populate_demo_snapshot(snap: &mut WorldStateSnapshot) {
        Self::populate_demo_snapshot_from(snap, Self::create_demo_snapshot());
    }

    pub fn populate_demo_snapshot_with_sde(
        snap: &mut WorldStateSnapshot,
        sde: &eve_sde::SdeRegistry,
    ) {
        Self::populate_demo_snapshot_from(snap, Self::create_demo_snapshot_with_sde(Some(sde)));
    }

    fn populate_demo_snapshot_from(snap: &mut WorldStateSnapshot, demo: WorldStateSnapshot) {
        for (k, v) in demo.accounts.into_inner() {
            snap.accounts.entry(k).or_insert(v);
        }
        for (k, v) in demo.characters {
            snap.characters.entry(k).or_insert(v);
        }
        for (k, v) in demo.skills {
            snap.skills.entry(k).or_insert(v);
        }
        for (k, v) in demo.skill_history.into_inner() {
            snap.skill_history.entry(k).or_insert(v);
        }
        for (k, v) in demo.items {
            snap.items.entry(k).or_insert(v);
        }
        for (k, v) in demo.market_orders {
            snap.market_orders.entry(k).or_insert(v);
        }
        for (k, v) in demo.planetary_colonies.into_inner() {
            snap.planetary_colonies.entry(k).or_insert(v);
        }
        for (k, v) in demo.corporations.into_inner() {
            snap.corporations.entry(k).or_insert(v);
        }
        for (k, v) in demo.system_states.into_inner() {
            snap.system_states.entry(k).or_insert(v);
        }
        for (k, v) in demo.npe_progress.into_inner() {
            snap.npe_progress.entry(k).or_insert(v);
        }
        for (k, v) in demo.account_plex_vault {
            snap.account_plex_vault.entry(k).or_insert(v);
        }
        for (k, v) in demo.fw_system_occupations.into_inner() {
            snap.fw_system_occupations.entry(k).or_insert(v);
        }
        if snap.next_entity_id < demo.next_entity_id {
            snap.next_entity_id = demo.next_entity_id;
        }
    }

    fn build_demo_repo_in_memory(initial_snap: WorldStateSnapshot) -> Self {
        let mut conn = rusqlite::Connection::open_in_memory()
            .expect("open in-memory SQLite connection for WorldRepository");
        sqlite_store::apply_migrations(&conn).expect("apply SQLite migrations for WorldRepository");
        sqlite_store::replace_sqlite_from_snapshot(&mut conn, &initial_snap)
            .expect("initialize SQLite from demo snapshot");
        let jwt_secret =
            sqlite_store::load_or_init_jwt_secret(&conn, Self::generate_random_secret_32)
                .expect("initialize per-deployment JWT signing secret");
        let entity_block = initial_snap.next_entity_id.max(200_000_000);

        Self::from_parts(
            initial_snap,
            conn,
            entity_block,
            None,
            AuthRuntimeState::new(jwt_secret),
        )
    }

    /// Creates an ephemeral in-memory demo-seeded `WorldRepository` seeded from `sde` without
    /// reading `EVOMU_DB_PATH`, `EVOMU_SNAPSHOT_PATH`, `EVOMU_DISABLE_DEMO_SEED`,
    /// `EVOMU_SDE_PATH`, `CARGO_MANIFEST_DIR`, or the process-global `SDE_CACHE` (`#1583` BF-01).
    pub fn with_demo_seed_in_memory_for_sde(sde: &eve_sde::SdeRegistry) -> Self {
        Self::build_demo_repo_in_memory(Self::create_demo_snapshot_with_sde(Some(sde)))
    }

    /// Creates an ephemeral in-memory demo-seeded `WorldRepository` without consulting
    /// `EVOMU_DB_PATH`, `EVOMU_SNAPSHOT_PATH`, or `EVOMU_DISABLE_DEMO_SEED` (`#1583` PG-01).
    ///
    /// Note: this still calls [`Self::create_demo_snapshot`], which reads `EVOMU_SDE_PATH` /
    /// `SdeRegistry::default_db_path()` and `SdeRegistry::open_shared()` (`SDE_CACHE`) to populate
    /// racial starter skills (`#1582`); callers requiring full SDE/env isolation must use
    /// [`Self::with_demo_seed_in_memory_for_sde`].
    pub fn with_demo_seed_in_memory() -> Self {
        Self::build_demo_repo_in_memory(Self::create_demo_snapshot())
    }

    pub fn with_demo_seed() -> Self {
        let repo = Self::build_demo_repo_in_memory(Self::create_demo_snapshot());

        #[cfg(any(test, feature = "legacy-json-snapshot"))]
        {
            let mut repo = repo;
            let snapshot_path = {
                let _env_guard = crate::lock_test_env_read();
                std::env::var("EVOMU_SNAPSHOT_PATH")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .map(std::path::PathBuf::from)
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
            return repo;
        }
        #[cfg(not(any(test, feature = "legacy-json-snapshot")))]
        repo
    }
}

#[cfg(test)]
#[path = "seed_unit_tests.rs"]
mod seed_unit_tests;

