//! Unit and mutation-killing tests for `seed.rs` (`create_demo_snapshot`, `populate_demo_snapshot`, `with_demo_seed`).

use super::*;

#[test]
fn create_demo_snapshot_populates_accounts_characters_items_skills_markets_and_fw() {
    let snap = WorldRepository::create_demo_snapshot();
    assert_eq!(snap.next_entity_id, 200_000_000);
    assert_eq!(snap.wal_seq, 0);
    assert!(!snap.markets_seeded);

    // Accounts
    assert_eq!(snap.accounts.len(), 2);
    let demo_acc = &snap.accounts["demo"];
    assert_eq!(demo_acc.user_id, 1);
    assert_eq!(demo_acc.username, "demo");
    let demo_salt = demo_acc.password_token.split('$').nth(4).unwrap();
    assert_eq!(
        demo_acc.password_token,
        WorldRepository::hash_password_with_salt("demo", demo_salt, "demo-secret")
    );
    assert_eq!(demo_acc.role, service_roles::ROLE_PLAYER_DEFAULT);
    assert!(demo_acc.banned_until_unix.is_none());
    assert!(demo_acc.ban_reason.is_none());

    let other_acc = &snap.accounts["other"];
    assert_eq!(other_acc.user_id, 2);
    assert_eq!(other_acc.username, "other");
    let other_salt = other_acc.password_token.split('$').nth(4).unwrap();
    assert_eq!(
        other_acc.password_token,
        WorldRepository::hash_password_with_salt("other", other_salt, "other-secret")
    );

    // Characters & folded PLEX vault balances
    assert_eq!(snap.characters.len(), 2);
    let aura = &snap.characters[&90_000_001];
    assert_eq!(aura.user_id, 1);
    assert_eq!(aura.name, "Aura Capsuleer");
    assert_eq!(aura.corporation_id, 1_000_170);
    assert_eq!(aura.station_id, Some(60_003_760));
    assert_eq!(aura.solar_system_id, 30_000_142);
    assert_eq!(aura.constellation_id, 20_000_020);
    assert_eq!(aura.region_id, 10_000_002);
    assert_eq!(aura.active_ship_id, 100_000_001);
    assert_eq!(aura.balance, IskCents::from_isk(1_000_000_000));
    assert_eq!(aura.plex_balance, 0);
    assert_eq!(snap.account_plex_vault.get(&1), Some(&500));
    assert_eq!(aura.loyalty_points.get(&1_000_035), Some(&5_000));
    assert_eq!(aura.standings.get(&1_000_035), Some(&2.0));
    assert_eq!(aura.security_status, 0.5);
    assert_eq!(aura.clone_grade, 2);
    assert_eq!(aura.home_station_id, 60_003_760);
    assert_eq!(aura.race_id, 2);
    assert_eq!(aura.bloodline_id, 3);
    assert_eq!(aura.ancestry_id, 19);
    assert_eq!(aura.school_id, 14);
    assert_eq!(aura.skill_points, 15_000_000.0);
    assert_eq!(aura.free_skill_points, 50_000.0);
    assert_eq!(aura.create_date_unix, 1_704_067_200);
    assert_eq!(aura.version, 1);
    assert_eq!(aura.system_epoch, 1);

    let rival = &snap.characters[&90_000_002];
    assert_eq!(rival.user_id, 2);
    assert_eq!(rival.name, "Rival Pilot");
    assert_eq!(rival.corporation_id, 1_000_045);
    assert_eq!(rival.active_ship_id, 100_000_002);
    assert_eq!(rival.balance, IskCents::from_isk(50_000_000));
    assert_eq!(rival.plex_balance, 0);
    assert_eq!(snap.account_plex_vault.get(&2), Some(&100));
    assert_eq!(rival.clone_grade, 1);
    assert_eq!(rival.race_id, 1);
    assert_eq!(rival.bloodline_id, 1);
    assert_eq!(rival.ancestry_id, 11);
    assert_eq!(rival.school_id, 18);
    assert_eq!(rival.skill_points, 4_500_000.0);
    assert_eq!(rival.free_skill_points, 0.0);

    // Items (100_000_001..=100_000_012 => 12 items)
    assert_eq!(snap.items.len(), 12);
    assert_eq!(snap.items[&100_000_001].type_id, 587);
    assert_eq!(snap.items[&100_000_001].flag, InventoryFlag::Hangar);
    assert_eq!(snap.items[&100_000_005].type_id, 2889);
    assert_eq!(snap.items[&100_000_005].location_id, 100_000_001);
    assert_eq!(snap.items[&100_000_005].flag, InventoryFlag::HiSlot0);
    assert_eq!(snap.items[&100_000_006].location_id, 100_000_002);
    assert_eq!(snap.items[&100_000_008].type_id, 34);
    assert_eq!(snap.items[&100_000_008].quantity, 100_000);
    assert!(!snap.items[&100_000_008].singleton);

    // Market orders
    assert_eq!(snap.market_orders.len(), 2);
    assert_eq!(snap.market_orders[&700_000_001].type_id, 34);
    assert_eq!(snap.market_orders[&700_000_001].price, IskCents::from_isk(5));
    assert!(snap.market_orders[&700_000_001].is_npc);
    assert_eq!(snap.market_orders[&700_000_002].type_id, 438);
    assert_eq!(
        snap.market_orders[&700_000_002].price,
        IskCents::from_isk(550_000)
    );

    // System states & NPE & Skills & Corporations & FW
    assert_eq!(snap.system_states.len(), 5);
    assert_eq!(snap.system_states[&30_000_142].dbs_multiplier, 1.35);
    assert_eq!(snap.system_states[&30_000_142].military_index, 2);
    assert_eq!(snap.system_states[&30_000_142].strategic_index, 3);
    assert_eq!(snap.npe_progress[&90_000_001].node_graph_id, 473);
    let sde = {
        let _env_guard = crate::lock_test_env_read();
        eve_sde::SdeRegistry::with_canonical_seed()
    };
    let (aura_starter, _) =
        WorldRepository::starter_skills_for_race(snap.characters[&90_000_001].race_id, &sde);
    let (rival_starter, _) =
        WorldRepository::starter_skills_for_race(snap.characters[&90_000_002].race_id, &sde);
    assert!(snap.skills[&90_000_001].len() > aura_starter.len());
    assert!(snap.skills[&90_000_002].len() > rival_starter.len());
    assert!(aura_starter.contains_key(&3412));
    assert!(snap.skills[&90_000_001][&3412].level >= 1);
    assert_eq!(snap.skill_history[&90_000_001].len(), snap.skills[&90_000_001].len());
    assert_eq!(snap.skill_history[&90_000_002].len(), snap.skills[&90_000_002].len());
    assert_eq!(snap.skill_history[&90_000_001][0].event_type_id, 33);
    assert_eq!(snap.corporations[&98_000_001].ticker, "DEMO");
    assert_eq!(snap.corporations[&98_000_001].ceo_id, 90_000_001);
    assert_eq!(snap.fw_system_occupations.len(), 175);
}

#[test]
fn populate_demo_snapshot_fills_empty_and_preserves_existing_records() {
    let mut empty = WorldStateSnapshot::default();
    empty.next_entity_id = 100;
    WorldRepository::populate_demo_snapshot(&mut empty);
    assert_eq!(empty.next_entity_id, 200_000_000);
    assert_eq!(empty.accounts.len(), 2);
    assert_eq!(empty.characters.len(), 2);
    assert_eq!(empty.items.len(), 12);
    assert_eq!(empty.market_orders.len(), 2);
    assert_eq!(empty.corporations.len(), 1);
    assert_eq!(empty.system_states.len(), 5);
    assert_eq!(empty.npe_progress.len(), 1);

    // Modify an existing character and raise next_entity_id, then re-populate: existing values must not be overwritten
    empty
        .characters
        .get_mut(&90_000_001)
        .unwrap()
        .name = "Custom Name".to_string();
    empty.next_entity_id = 555_000_000;
    WorldRepository::populate_demo_snapshot(&mut empty);
    assert_eq!(empty.characters[&90_000_001].name, "Custom Name");
    assert_eq!(empty.next_entity_id, 555_000_000);
}

#[test]
fn with_demo_seed_initializes_working_sqlite_backed_repository() {
    let repo = WorldRepository::with_demo_seed();
    repo.with_snapshot(|snap| {
        assert_eq!(snap.accounts.len(), 2);
        assert_eq!(snap.characters.len(), 2);
        assert_eq!(snap.fw_system_occupations.len(), 175);
        assert_eq!(snap.corporations[&98_000_001].wallet_divisions.len(), 7);
    });
}

#[test]
fn demo_snapshot_starter_items_system_states_corp_roles_and_partial_populate() {
    let snap = WorldRepository::create_demo_snapshot();

    // Verify every seeded starter item 100_000_003..=100_000_012
    let expected_items = [
        (100_000_003i64, 438u32, 1i32, true),
        (100_000_004, 1236, 2, false),
        (100_000_007, 691, 1, true),
        (100_000_008, 34, 100_000, false),
        (100_000_009, 35, 50_000, false),
        (100_000_010, 36, 20_000, false),
        (100_000_011, 37, 5_000, false),
        (100_000_012, 1230, 10_000, false),
    ];
    for (id, tid, qty, single) in expected_items {
        let item = &snap.items[&id];
        assert_eq!(item.type_id, tid);
        assert_eq!(item.quantity, qty);
        assert_eq!(item.singleton, single);
        assert_eq!(item.owner_id, 90_000_001);
        assert_eq!(item.location_id, 60_003_760);
        assert_eq!(item.flag, InventoryFlag::Hangar);
    }

    // All 5 seeded systems
    for sys_id in [30_000_142u32, 30_000_144, 30_002_813, 30_002_904, 31_000_005] {
        let st = &snap.system_states[&sys_id];
        assert_eq!(st.solar_system_id, sys_id);
        assert_eq!(st.dbs_multiplier, 1.35);
        assert_eq!(st.industrial_index, 2);
        assert_eq!(st.strategic_index, 3);
    }

    // Demo corporation wallet divisions & member roles
    let corp = &snap.corporations[&98_000_001];
    assert_eq!(corp.master_wallet, IskCents::from_isk(100_000_000));
    assert_eq!(corp.wallet_divisions[0].balance, IskCents::from_isk(100_000_000));
    assert_eq!(corp.member_roles.get(&90_000_001), Some(&u64::MAX));
    assert_eq!(corp.member_roles.get(&90_000_002), Some(&0));
    assert!(corp.members.contains(&90_000_001));
    assert!(corp.members.contains(&90_000_002));

    // Starter skill history sorted ascending by skill_type_id
    let hist = &snap.skill_history[&90_000_001];
    assert!(hist.windows(2).all(|w| w[0].skill_type_id <= w[1].skill_type_id));
    assert_eq!(hist[0].log_date_ft, 133_485_408_000_000_000);

    // Partial populate_demo_snapshot preserves pre-existing entries across all maps while filling missing ones
    let mut partial = WorldStateSnapshot::default();
    partial.account_plex_vault.insert(1, 999);
    partial.items.insert(
        100_000_008,
        InventoryItemRecord {
            item_id: 100_000_008,
            type_id: 34,
            quantity: 7,
            ..Default::default()
        },
    );
    partial.market_orders.insert(
        700_000_001,
        MarketOrderRecord {
            order_id: 700_000_001,
            owner_id: 1,
            station_id: 60_003_760,
            region_id: 10_000_002,
            type_id: 34,
            is_bid: false,
            price: IskCents::from_isk(99),
            vol_entered: 1,
            vol_remaining: 1,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents::ZERO,
        },
    );
    WorldRepository::populate_demo_snapshot(&mut partial);
    assert_eq!(partial.account_plex_vault[&1], 999);
    assert_eq!(partial.account_plex_vault[&2], 100);
    assert_eq!(partial.items[&100_000_008].quantity, 7);
    assert_eq!(partial.items.len(), 12);
    assert_eq!(partial.market_orders[&700_000_001].price, IskCents::from_isk(99));
    assert_eq!(partial.market_orders.len(), 2);
}

#[test]
fn seeded_demo_pilots_receive_full_sde_starter_skills_matching_character_creation() {
    let sde = {
        let _env_guard = crate::lock_test_env_read();
        eve_sde::SdeRegistry::with_canonical_seed()
    };

    let default_snap = WorldRepository::create_demo_snapshot();
    let explicit_snap = WorldRepository::create_demo_snapshot_with_sde(Some(&sde));

    let repo = WorldRepository::with_demo_seed();
    let aura = &default_snap.characters[&90_000_001];
    let created_minmatar = repo
        .create_character(
            2,
            "StarterParityPilot",
            aura.race_id,
            aura.bloodline_id,
            aura.ancestry_id,
            aura.school_id,
            &sde,
        )
        .expect("create Minmatar character via canonical helper");
    let created_skills = repo.with_snapshot(|s| s.skills[&created_minmatar.character_id].clone());

    assert_eq!(default_snap.skills[&90_000_001], explicit_snap.skills[&90_000_001]);
    assert_eq!(default_snap.skills[&90_000_002], explicit_snap.skills[&90_000_002]);
    assert_eq!(default_snap.characters[&90_000_001].skill_points, 15_000_000.0);
    assert_eq!(default_snap.characters[&90_000_002].skill_points, 4_500_000.0);

    // Every skill trained by character creation is present on seeded pilot 90_000_001 at >= starter level.
    let aura_skills = &default_snap.skills[&90_000_001];
    for (&skill_id, created) in &created_skills {
        let seeded = aura_skills
            .get(&skill_id)
            .unwrap_or_else(|| panic!("seeded pilot 90_000_001 missing starter skill {skill_id}"));
        assert!(
            seeded.level >= created.level,
            "seeded pilot 90_000_001 skill {skill_id} regressed below starter level {} (got {})",
            created.level,
            seeded.level
        );
        assert!(
            seeded.skill_points >= created.skill_points,
            "seeded pilot 90_000_001 skill {skill_id} SP {} < starter {}",
            seeded.skill_points,
            created.skill_points
        );
    }

    // Astrometrics (3412) is granted by Minmatar starter skills so Core Probe Launcher I (17938) fits,
    // Hull Upgrades (3394) stays at starter level 3 instead of regressing to 2, and seeded overrides stay intact.
    assert_eq!(aura_skills[&3412].level, 3);
    assert_eq!(aura_skills[&3412].skill_points, 24_000.0);
    assert_eq!(aura_skills[&3394].level, 3);
    assert_eq!(aura_skills[&3394].skill_points, 16_000.0);
    assert_eq!(aura_skills[&3302].level, 5);
    assert_eq!(aura_skills[&11084].level, 1);
    assert_eq!(aura_skills[&3446].level, 4);
    assert_eq!(aura_skills[&3452].level, 4);
    assert_eq!(aura_skills[&16622].level, 4);
    assert_eq!(aura_skills[&3363].level, 1);
    assert_eq!(aura_skills[&3436].level, 5);

    // Rival pilot 90_000_002 (Caldari) also receives full racial starter skills merged with demo overrides.
    let rival_race_id = default_snap.characters[&90_000_002].race_id;
    let (rival_starter, _) = WorldRepository::starter_skills_for_race(rival_race_id, &sde);
    let rival_skills = &default_snap.skills[&90_000_002];
    for (&skill_id, starter) in &rival_starter {
        let seeded = rival_skills
            .get(&skill_id)
            .unwrap_or_else(|| panic!("seeded pilot 90_000_002 missing starter skill {skill_id}"));
        assert!(seeded.level >= starter.level);
        assert!(seeded.skill_points >= starter.skill_points);
    }
    assert_eq!(rival_skills[&3412].level, 3);

    // Skill history stays synchronized and sorted for both seeded pilots.
    for char_id in [90_000_001_i64, 90_000_002_i64] {
        let hist = &default_snap.skill_history[&char_id];
        assert_eq!(hist.len(), default_snap.skills[&char_id].len());
        assert!(hist.windows(2).all(|w| w[0].skill_type_id < w[1].skill_type_id));
    }

    // Explicit populate_demo_snapshot_with_sde matches create_demo_snapshot_with_sde on empty snapshot.
    let mut populated = WorldStateSnapshot::default();
    WorldRepository::populate_demo_snapshot_with_sde(&mut populated, &sde);
    assert_eq!(populated.skills[&90_000_001], explicit_snap.skills[&90_000_001]);
    assert_eq!(populated.skills[&90_000_002], explicit_snap.skills[&90_000_002]);
}

struct SeedEnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl SeedEnvRestore {
    fn capture(keys: &[&'static str]) -> Self {
        Self(keys.iter().map(|&k| (k, std::env::var_os(k))).collect())
    }
}

impl Drop for SeedEnvRestore {
    fn drop(&mut self) {
        for (k, v) in self.0.drain(..) {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

struct SeedTempDirGuard(std::path::PathBuf);

impl SeedTempDirGuard {
    fn new(prefix: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("{prefix}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for SeedTempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn with_demo_seed_in_memory_ignores_env_db_and_snapshot_paths() {
    let _lock = crate::lock_test_env_write();
    let _restore = SeedEnvRestore::capture(&[
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_DISABLE_DEMO_SEED",
    ]);

    let tmp_guard = SeedTempDirGuard::new("eve_seed_mem_env");
    let db_path = tmp_guard.0.join("shared.sqlite");
    let snap_path = tmp_guard.0.join("shared_snap.json");
    std::env::set_var("EVOMU_DB_PATH", &db_path);
    std::env::set_var("EVOMU_SNAPSHOT_PATH", &snap_path);
    std::env::set_var("EVOMU_DISABLE_DEMO_SEED", "1");

    let left = WorldRepository::with_demo_seed_in_memory();
    let right = WorldRepository::with_demo_seed_in_memory();
    assert_ne!(left.instance_id(), right.instance_id());
    assert!(!db_path.exists());
    assert!(!snap_path.exists());
    assert_eq!(left.with_snapshot(|s| s.characters.len()), 2);
}

#[test]
fn with_demo_seed_in_memory_for_sde_ignores_evomu_sde_path_and_seeds_caller_sde() {
    let _lock = crate::lock_test_env_write();
    let _restore = SeedEnvRestore::capture(&[
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_DISABLE_DEMO_SEED",
        "EVOMU_SDE_PATH",
    ]);

    let tmp_guard = SeedTempDirGuard::new("eve_seed_sde_iso");
    let db_path = tmp_guard.0.join("shared.sqlite");
    let snap_path = tmp_guard.0.join("shared_snap.json");
    let missing_sde_path = tmp_guard.0.join("nonexistent_sde.sqlite");
    std::env::set_var("EVOMU_DB_PATH", &db_path);
    std::env::set_var("EVOMU_SNAPSHOT_PATH", &snap_path);
    std::env::set_var("EVOMU_DISABLE_DEMO_SEED", "1");
    std::env::set_var("EVOMU_SDE_PATH", &missing_sde_path);

    let make_race2_sde = |skill_id: eve_sde::TypeId, level: u8| {
        let mut sde = eve_sde::SdeRegistry::default();
        sde.races.insert(
            2,
            eve_sde::RaceInfo {
                race_id: 2,
                name: "Minmatar".to_string(),
                corvette_type_id: None,
                starter_skills: HashMap::from([(skill_id, level)]),
                description: String::new(),
            },
        );
        sde
    };
    let left_sde = make_race2_sde(999_001, 2);
    let right_sde = make_race2_sde(999_002, 4);

    let left = WorldRepository::with_demo_seed_in_memory_for_sde(&left_sde);
    let right = WorldRepository::with_demo_seed_in_memory_for_sde(&right_sde);
    assert_ne!(left.instance_id(), right.instance_id());
    assert!(!db_path.exists());
    assert!(!snap_path.exists());
    assert_eq!(
        left.with_snapshot(|s| s.skills[&90_000_001].get(&999_001).map(|sk| sk.level)),
        Some(2)
    );
    assert!(!left.with_snapshot(|s| s.skills[&90_000_001].contains_key(&999_002)));
    assert_eq!(
        right.with_snapshot(|s| s.skills[&90_000_001].get(&999_002).map(|sk| sk.level)),
        Some(4)
    );
    assert!(!right.with_snapshot(|s| s.skills[&90_000_001].contains_key(&999_001)));
}

#[test]
fn seeded_demo_pilots_satisfy_transitive_sde_item_requirements_and_fallback() {
    let sde = {
        let _env_guard = crate::lock_test_env_read();
        eve_sde::SdeRegistry::with_canonical_seed()
    };

    let default_snap = WorldRepository::create_demo_snapshot();
    let explicit_snap = WorldRepository::create_demo_snapshot_with_sde(Some(&sde));
    let mut populated = WorldStateSnapshot::default();
    WorldRepository::populate_demo_snapshot_with_sde(&mut populated, &sde);

    // Every seeded demo pilot must satisfy all transitive SDE skill prerequisites for every owned
    // ship and module across default-SDE, explicit-SDE, and populate_with_sde entry points.
    for snap in [&default_snap, &explicit_snap, &populated] {
        for char_id in [90_000_001_i64, 90_000_002_i64] {
            let owned_type_ids: Vec<TypeId> = snap
                .items
                .values()
                .filter(|item| item.owner_id == char_id)
                .map(|item| item.type_id)
                .collect();
            let required = WorldRepository::collect_transitive_required_skills_for_types(
                &sde,
                owned_type_ids,
            );
            assert!(
                required.get(&3312) == Some(&3),
                "expected 2889 -> 11084 -> 3312 level 3 transitive requirement for pilot {char_id}"
            );
            assert_eq!(required.get(&3327), Some(&1));
            assert_eq!(required.get(&3329), Some(&1));

            let skills = &snap.skills[&char_id];
            assert_eq!(skills[&3312].level, 3);
            assert_eq!(skills[&3312].skill_points, 16_000.0);

            for (skill_id, req_lvl) in required {
                let trained = skills.get(&skill_id).unwrap_or_else(|| {
                    panic!("pilot {char_id} missing derived skill {skill_id} (req L{req_lvl})")
                });
                assert!(
                    trained.level >= req_lvl,
                    "pilot {char_id} skill {skill_id} L{} < required L{req_lvl}",
                    trained.level
                );
                let min_expected = WorldRepository::trained_skill_record(skill_id, req_lvl, &sde);
                assert!(trained.skill_points >= min_expected.skill_points);
            }

            let hist = &snap.skill_history[&char_id];
            assert_eq!(hist.len(), skills.len());
            let hist_by_skill: HashMap<TypeId, &SkillHistoryEntry> =
                hist.iter().map(|h| (h.skill_type_id, h)).collect();
            for (&skill_id, rec) in skills {
                let entry = hist_by_skill[&skill_id];
                assert_eq!(entry.skill_level, rec.level);
                assert_eq!(entry.absolute_points, rec.skill_points);
            }
        }
    }

    // Mutation-killing dynamic derivation proof: clone SDE and add a cyclic + zero-level transitive
    // chain on 2889 -> 99_901 (L2) -> 99_902 (L4, rank 3) -> back-edge 99_901 (L1) + ignored L0 99_903.
    let mut custom_sde = sde.clone();
    let mut syn_skill_1 = sde.types[&3312].clone();
    syn_skill_1.type_id = 99_901;
    syn_skill_1.default_attributes = HashMap::from([(275, 1.0), (182, 99_902.0), (277, 4.0)]);
    let mut syn_skill_2 = sde.types[&3312].clone();
    syn_skill_2.type_id = 99_902;
    syn_skill_2.default_attributes = HashMap::from([
        (275, 3.0),
        (182, 99_901.0),
        (277, 1.0),
        (183, 99_903.0),
        (278, 0.0),
    ]);
    custom_sde.types.insert(99_901, syn_skill_1);
    custom_sde.types.insert(99_902, syn_skill_2);
    let turret = custom_sde.types.get_mut(&2889).unwrap();
    turret.default_attributes.insert(1285, 99_901.0);
    turret.default_attributes.insert(1286, 2.0);

    let custom_snap = WorldRepository::create_demo_snapshot_with_sde(Some(&custom_sde));
    for char_id in [90_000_001_i64, 90_000_002_i64] {
        let skills = &custom_snap.skills[&char_id];
        assert_eq!(skills[&99_901].level, 2);
        assert_eq!(skills[&99_901].skill_points, 1_415.0);
        assert_eq!(skills[&99_902].level, 4);
        assert_eq!(skills[&99_902].skill_points, 135_765.0);
        assert!(!skills.contains_key(&99_903));
        assert_eq!(custom_snap.skill_history[&char_id].len(), skills.len());
    }

    // Offline / missing-SDE fallback still satisfies canonical seeded ship/module prerequisites.
    let offline_snap = crate::without_default_sde(WorldRepository::create_demo_snapshot);
    for char_id in [90_000_001_i64, 90_000_002_i64] {
        let skills = &offline_snap.skills[&char_id];
        assert_eq!(skills[&3312].level, 3);
        assert_eq!(skills[&3312].skill_points, 16_000.0);
        assert_eq!(skills[&3392].level, 1);
        assert_eq!(skills[&3392].skill_points, 250.0);
        assert_eq!(skills[&3302].level, 5);
        assert_eq!(skills[&11084].level, 1);
        assert_eq!(skills[&3327].level, 4);
        assert_eq!(skills[&3329].level, 4);
        assert_eq!(skills[&3450].level, 4);
        assert_eq!(offline_snap.skill_history[&char_id].len(), skills.len());
    }
}
