//! Deep branch-coverage and mutation-killing unit tests for `eve-db`.

use super::*;
use eve_sde::SdeRegistry;
use std::collections::HashMap;

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("evomu_{tag}_{nanos}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn inventory_move_repackage_stack_split_and_delete_error_and_overflow_branches() {
    let repo = WorldRepository::with_demo_seed();
    let (char_id, other_char, station_id) = (90_000_001_i64, 90_000_002_i64, 60_003_760_i64);
    let (locked_flag_id, remote_id, contract_locked_id) = (910_001_i64, 910_002, 910_003);
    let (cargo_ship_id, bpc_id, mutated_id, foreign_id) = (910_004_i64, 910_005, 910_006, 910_007);
    let (stack_a, stack_b, stack_singleton, stack_other_loc, max_stack, one_stack) =
        (910_008_i64, 910_009, 910_010, 910_011, 910_012, 910_013);

    repo.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
        let base_item = |id, tid, own, loc, flg, qty, sng| InventoryItemRecord {
            item_id: id,
            type_id: tid,
            owner_id: own,
            location_id: loc,
            flag: flg,
            quantity: qty,
            singleton: sng,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 500.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        };
        snap.items.insert(locked_flag_id, base_item(locked_flag_id, 34, char_id, station_id, InventoryFlag::Locked, 10, false));
        snap.items.insert(remote_id, base_item(remote_id, 34, char_id, 60_008_494, InventoryFlag::Hangar, 10, false));
        snap.items.insert(contract_locked_id, base_item(contract_locked_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(cargo_ship_id, base_item(cargo_ship_id, 587, char_id, station_id, InventoryFlag::Cargo, 1, true));
        let mut bpc = base_item(bpc_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
        bpc.is_blueprint_copy = true;
        snap.items.insert(bpc_id, bpc);
        let mut mutated = base_item(mutated_id, 587, char_id, station_id, InventoryFlag::Hangar, 1, true);
        mutated.source_mutaplasmid_id = Some(47_740);
        snap.items.insert(mutated_id, mutated);
        snap.items.insert(foreign_id, base_item(foreign_id, 34, other_char, station_id, InventoryFlag::Hangar, 20, false));

        snap.items.insert(stack_a, base_item(stack_a, 34, char_id, station_id, InventoryFlag::Hangar, 40, false));
        snap.item_custom_names.insert(stack_a, "Named Tritanium".into());
        snap.items.insert(stack_b, base_item(stack_b, 34, char_id, station_id, InventoryFlag::Hangar, 60, false));
        snap.items.insert(stack_singleton, base_item(stack_singleton, 34, char_id, station_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(stack_other_loc, base_item(stack_other_loc, 34, char_id, station_id, InventoryFlag::Cargo, 15, false));
        snap.items.insert(max_stack, base_item(max_stack, 35, char_id, station_id, InventoryFlag::Hangar, i32::MAX, false));
        snap.items.insert(one_stack, base_item(one_stack, 35, char_id, station_id, InventoryFlag::Hangar, 1, false));

        snap.contracts.insert(
            99_100,
            ContractRecord {
                contract_id: 99_100,
                status: "Outstanding".into(),
                item_ids: vec![contract_locked_id],
                ..Default::default()
            },
        );
    });

    // move_item_transactional error branches
    assert!(matches!(
        repo.move_item_transactional(char_id, locked_flag_id, station_id, InventoryFlag::Hangar, None, None),
        Err(DbError::ItemLocked(id)) if id == locked_flag_id
    ));
    assert!(matches!(
        repo.move_item_transactional(char_id, remote_id, station_id, InventoryFlag::Hangar, None, None),
        Err(DbError::InvalidDestinationLocation { character_id, location_id: 60_008_494 }) if character_id == char_id
    ));
    assert!(matches!(
        repo.move_item_transactional(char_id, contract_locked_id, station_id, InventoryFlag::Hangar, None, None),
        Err(DbError::ItemLocked(id)) if id == contract_locked_id
    ));

    // repackage_item branches
    assert!(matches!(repo.repackage_item(char_id, contract_locked_id), Err(DbError::ItemLocked(_))));
    assert!(matches!(repo.repackage_item(char_id, foreign_id), Err(DbError::ItemOwnershipViolation { .. })));
    assert!(matches!(repo.repackage_item(char_id, cargo_ship_id), Err(DbError::ValidationError(_))));
    assert!(matches!(repo.repackage_item(char_id, bpc_id), Err(DbError::ValidationError(_))));
    assert!(matches!(repo.repackage_item(char_id, mutated_id), Err(DbError::ValidationError(_))));

    // stack_items branches
    assert!(matches!(repo.stack_items(char_id, stack_a, contract_locked_id), Err(DbError::ItemLocked(_))));
    assert!(matches!(repo.stack_items(char_id, stack_a, stack_singleton), Err(DbError::ValidationError(_))));
    assert!(matches!(repo.stack_items(char_id, foreign_id, stack_a), Err(DbError::ItemOwnershipViolation { .. })));
    assert!(matches!(repo.stack_items(char_id, stack_singleton, stack_a), Err(DbError::ValidationError(_))));
    assert!(matches!(repo.stack_items(char_id, stack_other_loc, stack_a), Err(DbError::ValidationError(_))));
    assert!(matches!(repo.stack_items(char_id, max_stack, one_stack), Err(DbError::ValidationError(_))));

    // split_stack & delete_item branches
    assert!(matches!(repo.split_stack(char_id, stack_a, 0), Err(DbError::ValidationError(_))));
    assert!(matches!(repo.split_stack(char_id, contract_locked_id, 1), Err(DbError::ItemLocked(_))));
    assert!(matches!(repo.split_stack(char_id, foreign_id, 5), Err(DbError::ItemOwnershipViolation { .. })));
    assert!(matches!(repo.split_stack(char_id, stack_singleton, 1), Err(DbError::ValidationError(_))));
    assert!(matches!(repo.split_stack(char_id, stack_a, 40), Err(DbError::ValidationError(_))));
    let (orig, split_child) = repo.split_stack(char_id, stack_a, 15).unwrap();
    assert_eq!(orig.quantity, 25);
    assert_eq!(split_child.quantity, 15);
    assert_eq!(
        repo.with_snapshot(|s| s.item_custom_names.get(&split_child.item_id).cloned()),
        Some("Named Tritanium".into())
    );
    assert!(matches!(repo.delete_item(char_id, contract_locked_id), Err(DbError::ItemLocked(_))));
    repo.delete_item(char_id, split_child.item_id).unwrap();
}

#[test]
fn character_creation_lineage_errors_and_capsule_respawn_structure_and_station_fallbacks() {
    let repo = WorldRepository::with_demo_seed();
    let sde = SdeRegistry::with_canonical_seed();

    let first_sch = sde.schools.values().next().unwrap();
    let race_id = first_sch.race_id;
    let valid_sch = first_sch.school_id;
    let valid_bl = sde.bloodlines.values().find(|b| b.race_id == race_id).unwrap().bloodline_id;
    let valid_anc = sde.ancestries.values().find(|a| a.bloodline_id == valid_bl).unwrap().ancestry_id;

    // Wrong bloodline for race
    if let Some(other_bl) = sde.bloodlines.values().find(|b| b.race_id != race_id) {
        assert!(matches!(
            repo.create_character(1001, "Bad Line One", race_id, other_bl.bloodline_id, valid_anc, valid_sch, &sde),
            Err(DbError::ValidationError(_))
        ));
    }
    // Wrong ancestry for bloodline
    if let Some(other_anc) = sde.ancestries.values().find(|a| a.bloodline_id != valid_bl && a.bloodline_id != 0 && a.ancestry_id != 64) {
        assert!(matches!(
            repo.create_character(1001, "Bad Line Two", race_id, valid_bl, other_anc.ancestry_id, valid_sch, &sde),
            Err(DbError::ValidationError(_))
        ));
    }
    // create_character_with_system with unknown system falls back to school station
    let created = repo
        .create_character_with_system(1001, "Fallback Star", race_id, valid_bl, valid_anc, valid_sch, &sde, Some(999_999_888))
        .unwrap();
    let sch_station = sde.schools[&valid_sch].station_id;
    assert_eq!(created.station_id, Some(sch_station));

    // Respawn in structure with online named Clone Bay module (!= 35894)
    let mut custom_sde = sde.clone();
    let mut clone_bay_type = custom_sde.types.values().next().unwrap().clone();
    let custom_cb_tid = 998_101_u32;
    clone_bay_type.type_id = custom_cb_tid;
    clone_bay_type.name = "Standup Clone Bay I".into();
    custom_sde.types.insert(custom_cb_tid, clone_bay_type);

    let (struct_id, cb_mod_id, pod_ship_id) = (1_000_000_055_001_i64, 920_001_i64, 920_002_i64);
    let cid = created.character_id;
    repo.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.home_station_id = struct_id;
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                name: "Test Astrahus".into(),
                solar_system_id: 30_000_142,
                owner_corp_id: 98_000_001,
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                ..Default::default()
            },
        );
        snap.items.insert(
            cb_mod_id,
            InventoryItemRecord {
                item_id: cb_mod_id,
                type_id: custom_cb_tid,
                owner_id: 98_000_001,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
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
        snap.module_online_states.insert(cb_mod_id, true);
        snap.items.insert(
            pod_ship_id,
            InventoryItemRecord {
                item_id: pod_ship_id,
                type_id: 670,
                owner_id: cid,
                location_id: struct_id,
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
    });
    let old_ship = repo.with_snapshot(|s| s.characters[&cid].active_ship_id);
    let (respawned, _) = repo
        .respawn_character_at_home_station(cid, old_ship, &custom_sde)
        .unwrap();
    assert_eq!(respawned.structure_id, Some(struct_id));
    assert_eq!(respawned.station_id, None);
    assert_eq!(respawned.solar_system_id, 30_000_142);

    // Respawn via solar_systems[].stations lookup when top-level sde.stations lacks raw_home_id
    let custom_st_id = 60_999_123_i64;
    let custom_sys_id = 30_999_123_u32;
    let mut sys_only_sde = sde.clone();
    let mut custom_sys = sys_only_sde.solar_systems.values().next().unwrap().clone();
    let mut custom_st = sys_only_sde.stations.values().next().unwrap().clone();
    custom_st.station_id = custom_st_id;
    custom_st.solar_system_id = custom_sys_id;
    custom_sys.solar_system_id = custom_sys_id;
    custom_sys.constellation_id = 20_000_020;
    custom_sys.region_id = 10_000_002;
    custom_sys.stations = vec![custom_st];
    sys_only_sde.solar_systems.insert(custom_sys_id, custom_sys);

    repo.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.home_station_id = custom_st_id;
        }
    });
    let old_ship2 = repo.with_snapshot(|s| s.characters[&cid].active_ship_id);
    let (respawned2, _) = repo
        .respawn_character_at_home_station(cid, old_ship2, &sys_only_sde)
        .unwrap();
    assert_eq!(respawned2.station_id, Some(custom_st_id));
    assert_eq!(respawned2.solar_system_id, custom_sys_id);
}

#[test]
fn accounts_courier_contract_failure_snapshot_path_and_try_read_branches_work() {
    // 1. Accounts malformed password tokens & PKCE verification
    let repo = WorldRepository::with_demo_seed();
    repo.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.values_mut().next() {
            acc.password_token = "argon2id$missing_second_dollar".into();
        }
    });
    assert!(matches!(repo.authenticate("test1", "pass"), Err(DbError::InvalidCredentials)));

    repo.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.values_mut().next() {
            acc.password_token = "sha256$v1$no_digest_separator".into();
        }
    });
    assert!(matches!(repo.authenticate("test1", "pass"), Err(DbError::InvalidCredentials)));

    repo.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.values_mut().next() {
            acc.password_token = "unknown_scheme$abc".into();
        }
    });
    assert!(matches!(repo.authenticate("test1", "pass"), Err(DbError::InvalidCredentials)));

    let now_u = 1_700_200_000_u64;
    let uid_pkce = repo.provision_player_account("pkce_tester", "pkce_secret");
    repo.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.user_id = uid_pkce;
        }
    });
    let verifier = "test_pkce_verifier_secret_987654321";
    let challenge = base64url_encode_no_pad(&accounts::sha256_bytes(verifier.as_bytes()));
    let pkce_code = repo
        .authorize_pkce(
            "pkce_tester",
            "pkce_secret",
            90_000_001,
            "evomu-cli",
            "http://127.0.0.1/callback",
            Vec::new(),
            &challenge,
            "S256",
            now_u,
        )
        .unwrap();
    let sso_pair = repo
        .exchange_pkce_code(&pkce_code, "evomu-cli", verifier, now_u)
        .unwrap();
    assert_eq!(
        repo.verify_sso_access_token(&sso_pair.access_token, now_u)
            .unwrap()
            .character_id,
        90_000_001
    );

    // 2. fail_courier_contract returns false on missing, and restores container items on InProgress
    repo.with_mut_snapshot(|snap| {
        assert!(!snap.fail_courier_contract(888_888, now_u));
        let (ship_id, wrap_id, freight_id) = (930_001_i64, 930_002, 930_003);
        let mk = |id, tid, loc, flg| InventoryItemRecord {
            item_id: id,
            type_id: tid,
            owner_id: 90_000_001,
            location_id: loc,
            flag: flg,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        };
        snap.items.insert(ship_id, mk(ship_id, 587, 60_003_760, InventoryFlag::Hangar));
        snap.items.insert(wrap_id, mk(wrap_id, 3468, ship_id, InventoryFlag::Cargo));
        snap.items.insert(freight_id, mk(freight_id, 34, wrap_id, InventoryFlag::None));
        snap.contracts.insert(
            77_001,
            ContractRecord {
                contract_id: 77_001,
                issuer_id: 90_000_001,
                issuer_corp_id: 98_000_001,
                acceptor_id: Some(90_000_002),
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                for_corp: true,
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                reward: IskCents(50_000),
                collateral: IskCents(100_000),
                item_ids: vec![freight_id],
                ..Default::default()
            },
        );
        assert!(snap.fail_courier_contract(77_001, now_u));
        assert!(!snap.items.contains_key(&wrap_id));
        let restored = &snap.items[&freight_id];
        assert_eq!(restored.location_id, ship_id);
        assert_eq!(restored.flag, InventoryFlag::Cargo);
        assert_eq!(snap.contracts[&77_001].status, "Failed");
    });

    // 3. Persistent snapshot create -> mutate -> reload -> WAL delta replay + cleanup
    let dir = temp_dir("db_deep_extra");
    let snap_path = dir.join("nested").join("world.json");
    std::fs::create_dir_all(snap_path.parent().unwrap()).unwrap();
    let persistent = WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone())).unwrap();
    assert!(snap_path.exists());
    persistent.with_mut_snapshot(|snap| {
        snap.next_entity_id += 7;
        assert!(persistent.is_write_locked_by_current_thread());
        assert!(persistent.try_with_snapshot(|_| ()).is_none());
    });
    assert!(!persistent.is_write_locked_by_current_thread());
    assert!(persistent.try_with_snapshot(|_| ()).is_some());
    drop(persistent);

    let reopened = WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone())).unwrap();
    reopened.load_snapshot_from_file(&snap_path).unwrap();
    drop(reopened);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ws_a1_service_domains_unit_behavior() {
    use crate::service_domains::*;
    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

    // 1. validate_service_state_blobs limits
    let mut ok_blobs = HashMap::new();
    ok_blobs.insert("k".to_string(), "v".to_string());
    assert!(validate_service_state_blobs(&ok_blobs).is_ok());

    let mut exact_keys = HashMap::new();
    for i in 0..MAX_SERVICE_STATE_BLOB_KEYS {
        exact_keys.insert(format!("k{i}"), "x".to_string());
    }
    assert!(validate_service_state_blobs(&exact_keys).is_ok());

    let mut exact_val = HashMap::new();
    exact_val.insert("k".to_string(), "a".repeat(MAX_SERVICE_STATE_BLOB_VALUE_BYTES));
    assert!(validate_service_state_blobs(&exact_val).is_ok());

    let mut exact_total = HashMap::new();
    for i in 0..16 {
        exact_total.insert(String::new(), "z".to_string());
        let _ = i;
    }
    exact_total.clear();
    for i in 0..16 {
        let k = format!("k_{i:02}");
        let v_len = (MAX_SERVICE_STATE_BLOB_TOTAL_BYTES / 16) - k.len();
        exact_total.insert(k, "z".repeat(v_len));
    }
    assert!(validate_service_state_blobs(&exact_total).is_ok());

    let mut too_many = HashMap::new();
    for i in 0..=MAX_SERVICE_STATE_BLOB_KEYS {
        too_many.insert(format!("k{i}"), "x".to_string());
    }
    let err_keys = validate_service_state_blobs(&too_many).unwrap_err();
    assert!(err_keys.contains("key count"));

    let mut big_val = HashMap::new();
    big_val.insert("k".to_string(), "a".repeat(MAX_SERVICE_STATE_BLOB_VALUE_BYTES + 1));
    let err_val = validate_service_state_blobs(&big_val).unwrap_err();
    assert!(err_val.contains("value length"));

    let mut big_total = HashMap::new();
    for i in 0..17 {
        big_total.insert(format!("key_{i:02}"), "z".repeat(4000));
    }
    let err_total = validate_service_state_blobs(&big_total).unwrap_err();
    assert!(err_total.contains("total size"));

    let mut snap = WorldStateSnapshot::default();
    snap.service_state_blobs = big_total;
    assert!(snap.validate_service_state_blobs().is_err());

    // 2. progression_seasons defaults & CharacterMilestoneState::accumulated_playtime_now
    let season_def = SeasonConfig::default();
    assert_eq!(season_def.season_id, 1);
    assert_eq!(season_def.max_active_challenges, 4);

    let proj_def = PlayerProjectState::default();
    assert_eq!(proj_def.rank, 1);
    assert_eq!(proj_def.max_bonus_samples, 10);
    assert_eq!(proj_def.project_id, "mcgill-polyseg");

    let proj_serde: PlayerProjectState = serde_json::from_str(
        r#"{
            "rank": 2, "experience": 10.0, "accuracy": 0.9, "score": 1.0,
            "tasks_submitted": 3, "analysis_kredits": 5, "tutorial_level": 1,
            "tutorial_complete": true, "current_task_index": 0, "bonus_samples_left": 2
        }"#,
    )
    .unwrap();
    assert_eq!(proj_serde.max_bonus_samples, 10);
    assert_eq!(proj_serde.project_id, "mcgill-polyseg");

    let m_inactive = CharacterMilestoneState {
        first_logon_unix: 100,
        last_activity_unix: 1_000,
        accumulated_playtime_secs: 500,
        active_session: false,
    };
    assert_eq!(m_inactive.accumulated_playtime_now(1_250), 500);

    let m_zero_act = CharacterMilestoneState {
        first_logon_unix: 100,
        last_activity_unix: 0,
        accumulated_playtime_secs: 500,
        active_session: true,
    };
    assert_eq!(m_zero_act.accumulated_playtime_now(1_250), 500);

    let m_active = CharacterMilestoneState {
        first_logon_unix: 100,
        last_activity_unix: 1_000,
        accumulated_playtime_secs: 500,
        active_session: true,
    };
    assert_eq!(m_active.accumulated_playtime_now(1_250), 750);
    assert_eq!(m_active.accumulated_playtime_now(900), 500);

    // 3. pvp_tourney_deployables: ticket_id_for_entry, TourneyMatch, mmju_spools, OutpostRaidSize
    let mut filament = PvpFilamentStore::default();
    let q_entry = PvpQueueCharacterEntry {
        item_id: 77_001,
        type_id: 54_731,
        location_id: 60_003_760,
        flag: 4,
        solar_system_id: 30_000_142,
        joined_unix: 1_700_000_000,
    };
    assert_eq!(filament.ticket_id_for_entry(90_000_001, &q_entry), 77_001);
    filament.queue_tickets.insert(90_000_001, 0);
    assert_eq!(filament.ticket_id_for_entry(90_000_001, &q_entry), 77_001);
    filament.queue_tickets.insert(90_000_001, 888_002);
    assert_eq!(filament.ticket_id_for_entry(90_000_001, &q_entry), 888_002);

    assert_eq!(TourneyMatchState::Setup.as_str(), "Setup");
    assert_eq!(TourneyMatchState::Banning.as_str(), "Banning");
    assert_eq!(TourneyMatchState::Countdown.as_str(), "Countdown");
    assert_eq!(TourneyMatchState::InProgress.as_str(), "InProgress");
    assert_eq!(TourneyMatchState::Completed.as_str(), "Completed");

    let tmatch = TourneyMatch::new(11, 22, 30_000_142, 100, 600, 120, 7);
    assert_eq!(tmatch.match_id, 11);
    assert_eq!(tmatch.tourney_id, 22);
    assert_eq!(tmatch.state, TourneyMatchState::Setup);

    let spool_entry_json = r#"{
        "ship_id": 303, "char_id": 9001, "unit_id": 202,
        "solar_system_id": 30000142, "started_unix": 10,
        "complete_at_unix": 20, "direction": [1.0, 0.0, 0.0]
    }"#;
    let dep_from_map: MobileDeployableRuntimeState = serde_json::from_str(&format!(
        r#"{{"mmju_spools": {{"101": {spool_entry_json}, "non_int": {spool_entry_json}}}}}"#
    ))
    .unwrap();
    assert!(dep_from_map.mmju_spools.contains_key(&101));
    assert!(dep_from_map.mmju_spools.contains_key(&202));

    let dep_from_seq: MobileDeployableRuntimeState = serde_json::from_str(&format!(
        r#"{{"mmju_spools": [{spool_entry_json}]}}"#
    ))
    .unwrap();
    assert!(dep_from_seq.mmju_spools.contains_key(&303));

    let err_spools = serde_json::from_str::<MobileDeployableRuntimeState>(r#"{"mmju_spools": 42}"#)
        .unwrap_err()
        .to_string();
    assert!(err_spools.contains("map or sequence of MmjuSpoolEntry"));

    assert_eq!(OutpostRaidSize::Small.slot_index(), 1);
    assert_eq!(OutpostRaidSize::Medium.slot_index(), 2);
    assert_eq!(OutpostRaidSize::Moderate.slot_index(), 3);
    assert_eq!(OutpostRaidSize::LargeAdv.slot_index(), 4);
    assert_eq!(OutpostRaidSize::Small.beacon_type_id(), 78_799);
    assert_eq!(OutpostRaidSize::Medium.beacon_type_id(), 78_805);
    assert_eq!(OutpostRaidSize::Moderate.beacon_type_id(), 97_658);
    assert_eq!(OutpostRaidSize::LargeAdv.beacon_type_id(), 78_814);
    assert_eq!(OutpostRaidSize::Small.base_capture_duration_secs(), 600);
    assert_eq!(OutpostRaidSize::Medium.base_capture_duration_secs(), 900);
    assert_eq!(OutpostRaidSize::Moderate.base_capture_duration_secs(), 900);
    assert_eq!(OutpostRaidSize::LargeAdv.base_capture_duration_secs(), 900);
    assert_eq!(OutpostRaidSize::Small.contribution_percent(), 0.25);
    assert_eq!(OutpostRaidSize::Medium.contribution_percent(), 0.5);
    assert_eq!(OutpostRaidSize::Moderate.contribution_percent(), 1.5);
    assert_eq!(OutpostRaidSize::LargeAdv.contribution_percent(), 1.0);
    assert_eq!(OutpostRaidSize::Small.contribution_subpoints(), 250);
    assert_eq!(OutpostRaidSize::Medium.contribution_subpoints(), 500);
    assert_eq!(OutpostRaidSize::Moderate.contribution_subpoints(), 1_500);
    assert_eq!(OutpostRaidSize::LargeAdv.contribution_subpoints(), 1_000);
    assert_eq!(OutpostRaidSize::Small.base_lp_reward(), 29_000);
    assert_eq!(OutpostRaidSize::Medium.base_lp_reward(), 41_500);
    assert_eq!(OutpostRaidSize::Moderate.base_lp_reward(), 52_500);
    assert_eq!(OutpostRaidSize::LargeAdv.base_lp_reward(), 52_500);
    assert_eq!(OutpostRaidSize::Small.respawn_delay_secs(), 900);
    assert_eq!(OutpostRaidSize::Medium.respawn_delay_secs(), 900);
    assert_eq!(OutpostRaidSize::Moderate.respawn_delay_secs(), 900);
    assert_eq!(OutpostRaidSize::LargeAdv.respawn_delay_secs(), 40 * 60);
    assert_eq!(OutpostRaidSize::Small.mordus_defender_type_id(), 79_545);
    assert_eq!(OutpostRaidSize::Medium.mordus_defender_type_id(), 79_549);
    assert_eq!(OutpostRaidSize::Moderate.mordus_defender_type_id(), 79_553);
    assert_eq!(OutpostRaidSize::LargeAdv.mordus_defender_type_id(), 79_557);
    assert_eq!(OutpostRaidSize::Small.pirate_defender_type_id(500_011), 79_515);
    assert_eq!(OutpostRaidSize::Medium.pirate_defender_type_id(500_011), 79_520);
    assert_eq!(OutpostRaidSize::Moderate.pirate_defender_type_id(500_011), 79_523);
    assert_eq!(OutpostRaidSize::LargeAdv.pirate_defender_type_id(500_011), 79_526);
    assert_eq!(OutpostRaidSize::Small.pirate_defender_type_id(500_010), 79_529);
    assert_eq!(OutpostRaidSize::Medium.pirate_defender_type_id(500_010), 79_533);
    assert_eq!(OutpostRaidSize::Moderate.pirate_defender_type_id(500_010), 79_537);
    assert_eq!(OutpostRaidSize::LargeAdv.pirate_defender_type_id(500_010), 79_541);
    assert!(OutpostRaidSize::Small.allows_ship_group(25));
    assert!(!OutpostRaidSize::Small.allows_ship_group(1305));
    assert!(!OutpostRaidSize::Small.allows_ship_group(26));
    assert!(!OutpostRaidSize::Small.allows_ship_group(419));
    assert!(!OutpostRaidSize::Small.allows_ship_group(963));
    assert!(!OutpostRaidSize::Small.allows_ship_group(27));
    assert!(OutpostRaidSize::Medium.allows_ship_group(25));
    assert!(OutpostRaidSize::Medium.allows_ship_group(1305));
    assert!(OutpostRaidSize::Medium.allows_ship_group(26));
    assert!(!OutpostRaidSize::Medium.allows_ship_group(419));
    assert!(!OutpostRaidSize::Medium.allows_ship_group(963));
    assert!(!OutpostRaidSize::Medium.allows_ship_group(27));
    assert!(OutpostRaidSize::Moderate.allows_ship_group(25));
    assert!(OutpostRaidSize::Moderate.allows_ship_group(1305));
    assert!(OutpostRaidSize::Moderate.allows_ship_group(26));
    assert!(OutpostRaidSize::Moderate.allows_ship_group(419));
    assert!(!OutpostRaidSize::Moderate.allows_ship_group(963));
    assert!(!OutpostRaidSize::Moderate.allows_ship_group(27));
    assert!(OutpostRaidSize::LargeAdv.allows_ship_group(25));
    assert!(OutpostRaidSize::LargeAdv.allows_ship_group(1305));
    assert!(OutpostRaidSize::LargeAdv.allows_ship_group(26));
    assert!(OutpostRaidSize::LargeAdv.allows_ship_group(419));
    assert!(OutpostRaidSize::LargeAdv.allows_ship_group(963));
    assert!(OutpostRaidSize::LargeAdv.allows_ship_group(27));
    assert!(!OutpostRaidSize::LargeAdv.allows_ship_group(28));

    // 4. pve_sites enums, serde defaults, Dungeon defaults, and PhasedAsteroidFieldState
    assert_eq!(AbyssalWeatherKind::Electrical.as_str(), "Electrical");
    assert_eq!(AbyssalWeatherKind::Dark.as_str(), "Dark");
    assert_eq!(AbyssalWeatherKind::Exotic.as_str(), "Exotic");
    assert_eq!(AbyssalWeatherKind::Firestorm.as_str(), "Firestorm");
    assert_eq!(AbyssalWeatherKind::Gamma.as_str(), "Gamma");

    assert_eq!(CrabBeaconPhase::Idle.as_str(), "idle");
    assert_eq!(CrabBeaconPhase::Linking.as_str(), "linking");
    assert_eq!(CrabBeaconPhase::Scanning.as_str(), "scanning");
    assert_eq!(CrabBeaconPhase::LootRestricted.as_str(), "loot_restricted");
    assert_eq!(CrabBeaconPhase::SelfDestructed.as_str(), "self_destructed");

    let wave_meta: IncursionSiteWaveMeta = serde_json::from_str("{}").unwrap();
    assert_eq!(wave_meta.current_wave, 1);
    assert_eq!(wave_meta.total_waves, 3);

    let site_entry: IncursionSiteEntry = serde_json::from_str(
        r#"{
            "site_id": "s1", "dungeon_id": 10, "dungeon_name": "VG",
            "solar_system_id": 30000142, "tier": "Vanguard",
            "min_pilots": 5, "max_optimal_pilots": 10,
            "base_isk": 1000, "base_concord_lp": 100,
            "influence_delta": 0.1, "locked_until_low_influence": false,
            "completed_count": 0, "boss_npc_ball_id": 99
        }"#,
    )
    .unwrap();
    assert_eq!(site_entry.boss_type_id, 2933);
    assert_eq!(DungeonRuntimeStore::default().next_instance_id, 950_001);
    assert_eq!(DungeonEditorStore::default().next_custom_room_id, 9_000_001);

    let mut field = PhasedAsteroidFieldState::new("ABC-123".into(), 30_000_142);
    assert!(!field.phased_in);
    assert!(!field.beacon_spawned);
    assert_eq!(field.mining_efficiency(), 0.10);
    field.advance_fracture(0.2);
    assert!(!field.beacon_spawned);
    field.advance_fracture(0.25);
    assert!(field.beacon_spawned);
    assert!(!field.phased_in);
    assert_eq!(field.mining_efficiency(), 0.10);
    field.advance_fracture(0.6);
    assert!(field.collapsed);
    assert_eq!(field.mining_efficiency(), 0.0);

    let mut field2 = PhasedAsteroidFieldState::new("DEF-456".into(), 30_000_142);
    assert!(!field2.connect_industrial_command_ship(501, 40));
    assert!(!field2.connect_industrial_command_ship(501, 40)); // duplicate ship ignored
    assert!(field2.connect_industrial_command_ship(502, 60)); // reaches 100 -> phased_in
    assert!(field2.phased_in);
    assert!(field2.connect_industrial_command_ship(502, 10)); // already phased

    // 5. quasar_and_social helpers & StructureDefenseSettings TTL boundaries
    assert!(!ListingTargetRecord::default().is_branded());
    assert!(ListingTargetRecord::Corporation(98_000_001).is_branded());
    assert!(ListingTargetRecord::Alliance(99_000_001).is_branded());
    assert!(ListingTargetRecord::Character(90_000_001).is_branded());

    let mut cmarket = CosmeticsMarketData::default();
    cmarket.record_tx(
        "ok_tx".into(),
        [1u8; 16],
        1_700_000_000,
        true,
        Some(1),
        None,
        CosmeticTransactionDetails::PurchasingSkin,
    );
    cmarket.record_tx(
        "fail_tx".into(),
        [2u8; 16],
        1_700_000_001,
        false,
        None,
        Some(2),
        CosmeticTransactionDetails::CreatingSkinListing,
    );
    assert_eq!(cmarket.transactions["ok_tx"].status, 3);
    assert_eq!(cmarket.transactions["fail_tx"].status, 2);

    let semver = SemVerSpec::default();
    assert_eq!((semver.major, semver.minor, semver.patch), (1, 0, 0));
    let dep_rec: DeploymentRecord = serde_json::from_str(
        r#"{"id":1,"name":"n","short_description":"d","content_group_id":2,"enabled":true,"current_content_count":0}"#,
    )
    .unwrap();
    assert!(dep_rec.no_region_limit);

    assert_eq!(CharacterEnergyState::default().current_energy, 100);
    assert_eq!(HorizonSiegePhase::Normal.as_str(), "Normal");
    assert_eq!(HorizonSiegePhase::Warning.as_str(), "Warning");
    assert_eq!(HorizonSiegePhase::Active.as_str(), "Active");
    assert_eq!(HorizonSiegePhase::Stabilizing.as_str(), "Stabilizing");

    let mut def = StructureDefenseSettings::default();
    assert_eq!(def.standing_threshold, 0.0);
    assert_eq!(AllianceSovConfig::default().current_prime_hour, 18);
    let sov_cfg: AllianceSovConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(sov_cfg.current_prime_hour, 18);
    assert_eq!(sov_cfg.new_prime_hour, 18);
    assert_eq!(BrowserLockdownPolicyState::default().default_home_page, "https://www.eveonline.com/");

    def.record_attacker(0, 1_000);
    def.record_attacker(-1, 1_000);
    assert!(def.known_attackers.is_empty());
    def.record_attacker(101, 100);
    def.record_attacker(102, 101);
    def.record_attacker(101, 200);
    let prune_now = 101 + STRUCTURE_ATTACKER_TTL_SECS; // cutoff == 101; 102 is at cutoff (must prune), 101 is at 200 (> cutoff)
    assert!(!def.is_active_attacker(102, prune_now));
    assert!(def.is_active_attacker(101, prune_now));
    def.prune_expired_attackers(prune_now);
    assert_eq!(def.known_attackers, vec![101]);
    let _ = (BTreeMap::<i64, i64>::new(), BTreeSet::<i64>::new(), HashSet::<i64>::new());
}

#[test]
fn ws_a1_migrate_legacy_service_state_and_idempotency() {
    use crate::service_domains::*;
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    // Phase 1: corrupt JSON / non-numeric keys cleanly removed without panicking
    let mut bad_snap = WorldStateSnapshot::default();
    bad_snap.owner_notes.insert(-8_300_000_000_000, OwnerNoteRecord {
        note_id: -8_300_000_000_000, character_id: 0, item_id: 0, label: "x".into(), note: "{bad".into(), timestamp_unix: 0,
    });
    bad_snap.owner_notes.insert(-10_000_055, OwnerNoteRecord {
        note_id: -10_000_055, character_id: 0, item_id: 0, label: "x".into(), note: "{bad".into(), timestamp_unix: 0,
    });
    bad_snap.owner_notes.insert(-8_650_000_000_000, OwnerNoteRecord {
        note_id: -8_650_000_000_000, character_id: 0, item_id: 0, label: "x".into(), note: "{bad".into(), timestamp_unix: 0,
    });
    bad_snap.owner_notes.insert(-990_050, OwnerNoteRecord {
        note_id: -990_050, character_id: 0, item_id: 0, label: "x".into(), note: "{bad".into(), timestamp_unix: 0,
    });
    bad_snap.service_state_blobs.insert("abyssal_active_clouds".into(), "{bad".into());
    bad_snap.service_state_blobs.insert("observatory_site:notanid".into(), "{}".into());
    bad_snap.service_state_blobs.insert("custom:opaque_keep".into(), "keep_me".into());
    migrate_legacy_service_state(&mut bad_snap);
    assert_eq!(bad_snap.service_state_blobs.get("custom:opaque_keep").map(String::as_str), Some("keep_me"));
    assert!(bad_snap.owner_notes.is_empty());

    let mut bad_pf_glob = WorldStateSnapshot::default();
    bad_pf_glob.service_state_blobs.insert("pvp_filament:global".into(), "{bad".into());
    migrate_legacy_service_state(&mut bad_pf_glob);
    assert!(bad_pf_glob.service_state_blobs.is_empty());

    // Phase 2: comprehensive valid migration covering sentinels, every single-key blob, and every prefix
    let mut snap = WorldStateSnapshot::default();
    let sample_room = AbyssalRoomRecord {
        room_index: 0, dungeon_id: 1, dungeon_name: "R0".into(),
        npc_ball_ids: vec![], cache_item_id: 10, cleared: true, gate_unlocked_at_sec: None,
    };
    let sample_pocket = AbyssalPocketSession {
        pocket_id: 55, character_id: 90_000_001, ship_id: 91_000_001,
        origin_solar_system_id: 30_000_142, abyssal_solar_system_id: 32_000_001,
        origin_position: [0.0; 3], grid_offset: [0.0; 3], filament_type_id: 47_762,
        tier: 1, weather: AbyssalWeatherKind::Electrical, weather_penalty_pct: 0.0,
        weather_bonus_pct: 0.5, npc_difficulty_mult: 1.0, current_room: 0,
        rooms: [sample_room.clone(), sample_room.clone(), sample_room],
        started_at_sec: 100, collapse_at_sec: 1300, completed: false,
        collapsed_destroyed: false, trace_ball_id: 700, fleet_ship_ids: vec![],
        ship_room: HashMap::new(), max_ships: 1,
    };
    let sample_trace = AbyssalTraceRecord {
        trace_ball_id: 700, char_id: 90_000_001, filament_type_id: 47_762,
        origin_solar_system_id: 30_000_142, position: [0.0; 3], created_unix: 100,
        expires_at_sec: 200, max_ships: 1, entered_char_ids: vec![90_000_001],
        pocket_id: Some(55), fleet_id: None,
    };
    let sample_void = VoidSpaceSessionRecord {
        char_id: 90_000_001, ship_id: 91_000_001, origin_solar_system_id: 30_000_142,
        origin_position: [0.0; 3], target_id: 800, entered_unix: 10, expires_at_sec: 20,
        nebula_graphic_id: Some(21506),
    };
    let sample_beacon = CrabBeaconRecord {
        beacon_id: 44, type_id: 60_244, deployer_char_id: 90_000_001,
        solar_system_id: 30_000_142, position: [0.0; 3], deployed_at_sec: 10,
        phase: CrabBeaconPhase::Idle, linked_char_id: None, linked_ship_id: None,
        link_started_at_sec: 0, link_completes_at_sec: 0, scan_completes_at_sec: 0,
        self_destruct_at_sec: 100, waves_spawned: 0, wave_npc_ball_ids: vec![],
        open_container_id: None,
    };
    let pve_combined = serde_json::json!({
        "initialized": true,
        "crab_beacons": { "44": sample_beacon },
        "abyssal_pockets": { "55": sample_pocket },
        "abyssal_traces": { "700": sample_trace },
        "void_space_sessions": { "90000001": sample_void },
        "next_seq": 99
    })
    .to_string();
    snap.owner_notes.insert(-8_300_000_000_000, OwnerNoteRecord {
        note_id: -8_300_000_000_000, character_id: 0, item_id: 0, label: "pve".into(), note: pve_combined, timestamp_unix: 0,
    });
    snap.owner_notes.insert(-10_000_055, OwnerNoteRecord {
        note_id: -10_000_055, character_id: 0, item_id: 0, label: "t".into(),
        note: serde_json::to_string(&TourneyStore::default()).unwrap(), timestamp_unix: 0,
    });
    snap.owner_notes.insert(-8_650_000_000_000, OwnerNoteRecord {
        note_id: -8_650_000_000_000, character_id: 0, item_id: 0, label: "d".into(),
        note: serde_json::to_string(&MobileDeployableRuntimeState::default()).unwrap(), timestamp_unix: 0,
    });
    // Populate sentinel PVP_FILAMENT_NOTE_ID so it migrates and gets removed cleanly; also test pvp_filament_store below
    let mut pf_store = PvpFilamentStore::default();
    pf_store.next_match_id = 42;
    pf_store.queue.insert(
        90_000_001,
        PvpQueueCharacterEntry {
            item_id: 777,
            type_id: 587,
            location_id: 60_003_760,
            flag: 4,
            solar_system_id: 30_000_142,
            joined_unix: 100,
        },
    );
    snap.owner_notes.insert(-990_050, OwnerNoteRecord {
        note_id: -990_050, character_id: 0, item_id: 0, label: "pf".into(),
        note: serde_json::to_string(&pf_store).unwrap(), timestamp_unix: 0,
    });

    let mut pf_blob_snap = WorldStateSnapshot::default();
    pf_blob_snap.service_state_blobs.insert("pvp_filament_store".into(), serde_json::to_string(&pf_store).unwrap());
    migrate_legacy_service_state(&mut pf_blob_snap);
    assert_eq!(pf_blob_snap.pvp_filament_state.next_match_id, 42);

    let mut b_map = HashMap::new();
    b_map.insert("abyssal_active_clouds", "{\"1\":2}".to_string());
    b_map.insert("abyssal_boundary_ticks", "{\"1\":3}".to_string());
    b_map.insert("deathzone_active_ships", "[10]".to_string());
    b_map.insert("dungeon_runtime_store", serde_json::to_string(&DungeonRuntimeStore::default()).unwrap());
    b_map.insert("dungeon_editor_store", serde_json::to_string(&DungeonEditorStore::default()).unwrap());
    b_map.insert("aoe_superweapon_runtime", serde_json::to_string(&PersistedSuperweaponRuntime::default()).unwrap());
    b_map.insert("insurgency_complex_ops", serde_json::to_string(&InsurgencyComplexOpsState::default()).unwrap());
    let field_map = HashMap::from([(1u32, PhasedAsteroidFieldState::new("S1".into(), 30_000_142))]);
    b_map.insert("phased_asteroid_fields", serde_json::to_string(&field_map).unwrap());
    b_map.insert("phased_field_last_tick_unix", "12345".into());
    b_map.insert("moon_fractured_fields", "{}".into());
    b_map.insert("mining_ledger_waste", "{}".into());
    b_map.insert("metenox_last_tick_unix", "23456".into());
    b_map.insert("client_node_graphs:seq", "77".into());
    b_map.insert("login_campaign_enrollments", "{\"1\":2}".into());
    b_map.insert("daily_login_campaign_schedule", serde_json::to_string(&ScheduledDailyCampaign {
        campaign_id: 5, start_unix: 1, end_unix: None, is_rookie: Some(false),
    }).unwrap());
    b_map.insert("seasonal_login_schedule", serde_json::to_string(&SeasonalCampaignSchedule {
        campaign_id: 6, start_unix: 1, end_unix: 9, turnover_secs: 86400,
    }).unwrap());
    b_map.insert("project_discovery", serde_json::to_string(&ProjectDiscoveryStore::default()).unwrap());
    b_map.insert("project_discovery_votes", "{}".into());
    b_map.insert("season_manager", serde_json::to_string(&HashMap::from([(90_000_001i64, CharacterSeasonState::default())])).unwrap());
    b_map.insert("payment_tokens", "[]".into());
    b_map.insert("payment_receipts", "[]".into());
    b_map.insert("payment_schedules", "[]".into());
    b_map.insert("plex_tx_logs", "{}".into());
    b_map.insert("nes_store_catalog", serde_json::to_string(&StoreCatalog::default()).unwrap());
    b_map.insert("nes_store_offer_tags", "{}".into());
    b_map.insert("plex_invoices", "[]".into());
    b_map.insert("quasar_cosmetics_market", serde_json::to_string(&CosmeticsMarketData::default()).unwrap());
    b_map.insert("content_orchestrator_state", serde_json::to_string(&OrchestratorState::default()).unwrap());
    b_map.insert("store_failed_fulfillments", "[]".into());
    b_map.insert("store_targeted_offers", "[]".into());
    b_map.insert("quasar_acl_registry", "{}".into());
    b_map.insert("b34_lp_assetholding_ops", "[\"op1\"]".into());
    b_map.insert("fleet_saved_setups", "{}".into());
    b_map.insert("fleet_stored_formations", "{}".into());
    b_map.insert("fleet_adverts", "{\"1\":{\"fleet_id\":1,\"leader_id\":9,\"fleet_name\":\"fn\",\"description\":\"d\",\"solar_system_id\":30000142,\"invite_scope\":0,\"min_standing\":0.0,\"join_needs_approval\":false,\"hide_info\":false,\"use_fleet_location\":false,\"created_unix\":1}}".into());
    b_map.insert("fleet_join_requests", "{}".into());
    b_map.insert("fleet_watchlist", "{}".into());
    b_map.insert("fleet_options", "[{\"1\":true},{\"1\":false}]".into());
    b_map.insert("corp_recruitment_ads", "[]".into());
    b_map.insert("corp_allow_war", "{\"98000001\":true}".into());
    b_map.insert("hypernet_subs", "{}".into());
    b_map.insert("ingamereport_tickets", "[]".into());
    b_map.insert("lp_store_offers", "[]".into());
    b_map.insert("agentMgr:disabled_missions", "[101,102]".into());
    b_map.insert("pending_probe_scans", "[]".into());
    b_map.insert("cluster_misc_state", serde_json::to_string(&PersistedClusterMiscState::default()).unwrap());
    b_map.insert("clone_vat_bay_state", serde_json::to_string(&CloneVatBayState::default()).unwrap());
    b_map.insert("load_service_state", serde_json::to_string(&LoadServiceState::default()).unwrap());
    b_map.insert("active_sessions", "[10]".into());
    b_map.insert("session_registry", "{}".into());
    b_map.insert("cluster_node_table", "{\"node\":\"1\"}".into());
    b_map.insert("cluster_admin_broadcasts", "[]".into());
    b_map.insert("cluster_service_states", "{\"svc\":true}".into());
    b_map.insert("cluster_attacks", "[]".into());
    b_map.insert("external_queue_events", "[]".into());
    b_map.insert("cluster_downtime_state", serde_json::to_string(&PersistedDowntimeState::default()).unwrap());
    b_map.insert("macho_client_code_hash", "{\"h\":\"1\"}".into());
    b_map.insert("macho_global_config", "{}".into());
    b_map.insert("user_sessions", "[]".into());
    b_map.insert("gm_audit_log", "[]".into());
    b_map.insert("alert_bean_counts", "{\"err\":1}".into());
    b_map.insert("alert_log_modes", "{\"m\":\"info\"}".into());
    b_map.insert("alert_client_traces", "{}".into());
    b_map.insert("alert_proxy_traces", "{}".into());
    b_map.insert("alert_group_bean_bytes", "{}".into());
    b_map.insert("browser_lockdown_policy", r#"{"lockdown":true,"default_home_page":"home","flagged_sites":["https://old.eve",{"site_url":"https://new.eve","site_flag":1}]}"#.into());
    b_map.insert("localization:qa_wrap_users", "{\"1\":true}".into());
    b_map.insert("client_stat_logger:strings", "[\"s1\"]".into());
    b_map.insert("blacklisted_planets", "[40000001]".into());
    b_map.insert("module_reload_until_unix", "{\"1\":99}".into());
    b_map.insert("warp_vector_cooldown_until", "{\"1\":88}".into());
    b_map.insert("jumpCloneSvc:ship_clone_offers", "{\"1\":[2,3]}".into());
    b_map.insert("account_resculpt_tokens", "{\"user1\":2}".into());
    b_map.insert("server_runtime_state", "{\"tick\":1}".into());

    // Prefixed keys
    let obs = ObservatorySiteState {
        site_id: 1, solar_system_id: 30_000_142, site_pos: [0.0; 3], gate_pos: [0.0; 3],
        gate_id: 2, world_ark_id: 3, defense_wave_ids: vec![], dungeon_instance_id: 4,
        pocket_entrants: BTreeSet::new(), created_unix: 100,
    };
    b_map.insert("observatory_site:1", serde_json::to_string(&obs).unwrap());
    b_map.insert("observatory_respawn_until:30000142", "555".into());
    b_map.insert("fob_combatants:1", "[9001]".into());
    b_map.insert("fob_faction:1", "500011".into());
    b_map.insert("fob_respawn_until:30000142", "666".into());
    b_map.insert("fob_defense_triggered:1", "1".into());
    b_map.insert("ice_heist_respawn_until:30000142", "777".into());
    b_map.insert("ess_reserve_pulses:30000142", serde_json::to_string(&EssReservePulseState::default()).unwrap());
    b_map.insert("void_space_content:1", "888".into());
    b_map.insert("air_npe_runner:90000001", serde_json::to_string(&AirNpeRunnerState::default()).unwrap());
    let cng = ActiveClientNodeGraph {
        instance_id: 1, node_graph_id: 2, category_id: 3, operation_id: 4, blackboard: BTreeMap::new(),
    };
    b_map.insert("client_node_graphs:90000001", serde_json::to_string(&vec![cng]).unwrap());
    b_map.insert("node_graph_bb:90000001:2", "{\"k\":\"v\"}".into());
    let exp_sys = ExpertSystemRecord {
        type_id: 10, inserted_at_ft: 1, expires_at_ft: 2, skills_granted: BTreeMap::new(),
    };
    b_map.insert("expert_systems:90000001", serde_json::to_string(&vec![exp_sys]).unwrap());
    b_map.insert("character_certificates:90000001", serde_json::to_string(&CharacterCertificateState::default()).unwrap());
    b_map.insert("char_certs:90000002", "{\"10\":5}".into());
    b_map.insert("season_state:season_configuration", serde_json::to_string(&SeasonConfig::default()).unwrap());
    b_map.insert("season_state:seasons", serde_json::to_string(&BTreeMap::from([(1i64, SeasonConfig::default())])).unwrap());
    b_map.insert("season_state:challenge_definitions", "[]".into());
    b_map.insert("season_state:loot_tables", "{}".into());
    b_map.insert("season_state:announced_initialized", "1".into());
    b_map.insert("season_state:announced_states", "{\"1\":\"active\"}".into());
    b_map.insert("season_manager:90000001", serde_json::to_string(&CharacterSeasonState::default()).unwrap());
    b_map.insert("active_season:90000001", "1".into());
    b_map.insert("hacking_wins:90000001", "9".into());
    b_map.insert("milestone_playtime:90000001", serde_json::to_string(&CharacterMilestoneState::default()).unwrap());
    b_map.insert("auto_injected_rewards:90000001", "[34]".into());
    b_map.insert("login_reward_expiry:90000001", "9999".into());
    b_map.insert("project_discovery:90000001", serde_json::to_string(&PlayerProjectState::default()).unwrap());
    b_map.insert("activity_mgr:90000001", serde_json::to_string(&CharacterActivityState::default()).unwrap());
    b_map.insert("fast_checkout:1", serde_json::to_string(&UserFastCheckoutState::default()).unwrap());
    b_map.insert("contract_deposit:777", "50000".into());
    b_map.insert("delayed_rewards:90000001", "{}".into());
    b_map.insert("mission_taxed_journal:555", "1".into());
    b_map.insert("agentMgr:disabled_missions:90000001", "[202]".into());
    b_map.insert("agentMgr:replay_ts:90000001:3000001", "1234".into());
    b_map.insert("signal_tracker_sub:30000142", "[90000001]".into());
    b_map.insert("wh_local_witnesses:31000001", "[90000001]".into());
    b_map.insert("structure_defense:1000000001", serde_json::to_string(&StructureDefenseSettings::default()).unwrap());
    b_map.insert("alliance_sov_cfg:99000001", serde_json::to_string(&AllianceSovConfig::default()).unwrap());
    b_map.insert("portrait:90000001", "abc.jpg".into());
    b_map.insert("paperdoll_state:90000001", "1".into());
    b_map.insert("char_system_visits:90000001", r#"{"30000142":{"visits":2,"last_visited_ticks":10},"30000144":7}"#.into());
    b_map.insert("auto_reloaded_event:90000001", "[[10],34,1.5]".into());
    b_map.insert("mct_slots:1", "[90000002]".into());
    b_map.insert("account_resculpt_tokens:u2", "4".into());
    b_map.insert("resculpt_pending:90000001", "true".into());
    b_map.insert("char_cohorts:90000001", "[\"c1\"]".into());
    b_map.insert("user_cohorts:1", "[\"u1\"]".into());
    b_map.insert("character_lock:90000001", "locked".into());
    b_map.insert("omega_downgraded:1", "1".into());
    b_map.insert("character_medals:90000001", "[]".into());
    b_map.insert("char_medals:90000001", "[]".into());
    b_map.insert("unmigrated:opaque", "stay".into());

    for (k, v) in b_map {
        snap.service_state_blobs.insert(k.to_string(), v);
    }

    migrate_legacy_service_state(&mut snap);

    assert!(snap.owner_notes.is_empty());
    assert_eq!(snap.service_state_blobs.len(), 1);
    assert_eq!(snap.service_state_blobs.get("unmigrated:opaque").map(String::as_str), Some("stay"));
    assert!(snap.pve_special_sites.initialized);
    assert_eq!(snap.pve_special_sites.next_seq, 99);
    assert!(snap.crab_beacons.beacons.contains_key(&44));
    assert!(snap.abyssal_state.pockets.contains_key(&55));
    assert_eq!(snap.abyssal_state.next_seq, 99);
    assert_eq!(snap.pvp_filament_state.next_match_id, 42);
    assert!(snap.quasar_commerce.lp_asset_ops_completed.contains("op1"));
    assert!(snap.fleet_and_social_ops.agent_global_disabled_missions.contains(&101));
    assert_eq!(snap.fleet_and_social_ops.agent_replay_ts.get(&(90_000_001, 3_000_001)), Some(&1234));
    assert_eq!(snap.cluster_and_runtime.browser_lockdown_policy.as_ref().unwrap().flagged_sites.len(), 2);
    assert_eq!(snap.cluster_and_runtime.char_system_visits[&90_000_001][&30_000_144].visits, 7);
    assert!(snap.cluster_and_runtime.resculpt_pending.contains(&90_000_001));
    assert!(snap.cluster_and_runtime.omega_downgraded.contains(&1));
    assert!(snap.seasons_and_campaigns.announced_season_init);
    assert!(snap.node_graph_state.certificates.contains_key(&90_000_002));

    // Idempotency: re-running with conflicting pvp_filament_store / incursions_state / abyssal_active_clouds does not clobber existing state
    snap.service_state_blobs.insert(
        "pvp_filament_store".into(),
        serde_json::to_string(&PvpFilamentStore { next_match_id: 999, ..Default::default() }).unwrap(),
    );
    snap.service_state_blobs.insert("incursions_state".into(), "{\"initialized\":true,\"next_seq\":999}".into());
    snap.service_state_blobs.insert("abyssal_active_clouds".into(), "{\"99\":99}".into());
    migrate_legacy_service_state(&mut snap);
    assert_eq!(snap.pvp_filament_state.next_match_id, 42);
    assert_eq!(snap.pve_special_sites.next_seq, 99);
    assert_eq!(snap.abyssal_state.active_clouds.get(&1), Some(&2));
}

#[test]
fn characters_snapshot_persistence_accounts_and_gateway_deep_branches() {
    use crate::accounts::{base64url_encode_no_pad, hmac_sha256};
    use crate::types::FromWalError;
    use std::collections::HashSet;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let repo = WorldRepository::with_demo_seed();
    let sde = SdeRegistry::with_canonical_seed();

    // 1. Character creation empty starter_skills + solar_system missing fallback
    let first_sch = sde.schools.values().next().unwrap();
    let race_id = first_sch.race_id;
    let valid_sch = first_sch.school_id;
    let valid_bl = sde.bloodlines.values().find(|b| b.race_id == race_id).unwrap().bloodline_id;
    let valid_anc = sde.ancestries.values().find(|a| a.bloodline_id == valid_bl).unwrap().ancestry_id;
    let mut custom_sde = sde.clone();
    if let Some(r) = custom_sde.races.get_mut(&race_id) {
        r.starter_skills.clear();
    }
    let no_station_sys = custom_sde
        .solar_systems
        .values()
        .find(|sys| sys.stations.is_empty())
        .map(|sys| sys.solar_system_id);
    if let Some(empty_sys) = no_station_sys {
        let ch = repo
            .create_character_with_system(1001, "Empty Skill", race_id, valid_bl, valid_anc, valid_sch, &custom_sde, Some(empty_sys))
            .unwrap();
        assert_eq!(ch.free_skill_points, 0.0);
    } else {
        custom_sde.solar_systems.clear();
        let ch = repo
            .create_character_with_system(1001, "Empty Skill", race_id, valid_bl, valid_anc, valid_sch, &custom_sde, None)
            .unwrap();
        assert_eq!(ch.free_skill_points, 0.0);
    }

    // 2. undock_character, dock_character, dock_character_if_in_space, transition_character_solar_system_if_at
    let cid = 90_000_001_i64;
    let cid_uid = repo.with_snapshot(|s| s.characters[&cid].user_id);
    let wrong_ship_id = 955_001_i64;
    let owned_other_station_ship = 955_002_i64;
    let cid_ship = repo.with_snapshot(|s| s.characters[&cid].active_ship_id);
    let struct_id = 1_000_000_099_001_i64;
    repo.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            ch.home_station_id = 60_999_999; // unknown station to exercise school/station fallback on respawn
        }
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                name: "Test Fort".into(),
                solar_system_id: 30_000_142,
                owner_corp_id: 98_000_001,
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        let mk_ship = |id, owner, loc| InventoryItemRecord {
            item_id: id, type_id: 587, owner_id: owner, location_id: loc,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1,
            dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
            insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
        };
        snap.items.insert(wrong_ship_id, mk_ship(wrong_ship_id, 90_000_002, 60_003_760));
        snap.items.insert(owned_other_station_ship, mk_ship(owned_other_station_ship, cid, 60_008_494));
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.active_ship_id = wrong_ship_id;
        }
    });
    assert!(matches!(repo.undock_character(cid_uid, cid), Err(DbError::ItemOwnershipViolation { .. })));

    repo.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().active_ship_id = owned_other_station_ship;
    });
    assert!(matches!(repo.undock_character(cid_uid, cid), Err(DbError::ShipNotAtDockedStation { .. })));

    repo.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().active_ship_id = cid_ship;
        if let Some(sh) = snap.items.get_mut(&cid_ship) {
            sh.location_id = 60_003_760;
            sh.flag = InventoryFlag::Hangar;
        }
    });
    assert!(matches!(
        repo.dock_character(cid, 1_000_999_888, true),
        Err(DbError::InvalidDestinationLocation { .. })
    ));
    assert!(repo.dock_character(cid, 60_003_760, false).is_ok());
    assert!(matches!(
        repo.dock_character_if_in_space(cid, 30_000_142, 60_003_760, false),
        Err(DbError::ValidationError(_))
    ));
    assert!(matches!(
        repo.transition_character_solar_system_if_at(cid, 30_000_142, 30_000_144, &sde),
        Err(DbError::ValidationError(_))
    ));

    // Undock -> transition_if_at mismatch -> unknown dest -> valid transition -> dock_if_in_space
    let (_, undocked, _) = repo.undock_character(cid_uid, cid).unwrap();
    assert_eq!(undocked.station_id, None);
    assert!(matches!(
        repo.transition_character_solar_system_if_at(cid, 30_002_187, 30_000_144, &sde),
        Err(DbError::ValidationError(_))
    ));
    let mut empty_sde = sde.clone();
    empty_sde.solar_systems.clear();
    empty_sde.schools.clear();
    empty_sde.stations.clear();
    let jumped = repo
        .transition_character_solar_system_if_at(cid, 30_000_142, 30_000_144, &empty_sde)
        .unwrap();
    assert_eq!(jumped.solar_system_id, 30_000_144);
    assert_eq!(repo.with_snapshot(|s| s.items[&cid_ship].location_id), 30_000_144);

    assert!(matches!(
        repo.dock_character_if_in_space(cid, 30_000_144, 1_000_000_999_999, true),
        Err(DbError::InvalidDestinationLocation { .. })
    ));
    // Structure in 30_000_142 fails solar-system match check for character in 30_000_144
    assert!(matches!(
        repo.dock_character_if_in_space(cid, 30_000_144, struct_id, true),
        Err(DbError::InvalidDestinationLocation { .. })
    ));
    repo.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().solar_system_id = 30_000_144;
    });
    let docked_st = repo
        .dock_character_if_in_space(cid, 30_000_144, struct_id, true)
        .unwrap();
    assert_eq!(docked_st.structure_id, Some(struct_id));

    // Respawn fallback when home_station_id is missing and SDE has only stations fallback
    let mut st_fallback_sde = sde.clone();
    st_fallback_sde.schools.clear();
    let _ = repo.respawn_character_at_home_station(cid, cid_ship, &st_fallback_sde);
    let _ = repo.respawn_character_at_home_station(cid, cid_ship, &empty_sde);

    // 3. snapshot_ext: effective_corp_roles_at, is_item_consumable_by, resolve_expired_contract, strip_ship_cosmetics
    let mut snap = WorldStateSnapshot::default();
    let test_corp = 98_123_001_i64;
    let ceo_id = 90_123_001_i64;
    let mem_id = 90_123_002_i64;
    // Unknown corp still folds corp_role_assignments
    snap.corp_role_assignments.insert(
        (test_corp, mem_id),
        CorpMemberRoleAssignment {
            character_id: mem_id, roles: 1, grantable_roles: 0,
            roles_at_hq: 2, grantable_roles_at_hq: 0,
            roles_at_base: 4, grantable_roles_at_base: 0,
            roles_at_other: 8, grantable_roles_at_other: 0,
        },
    );
    assert_eq!(snap.effective_corp_roles_at(test_corp, mem_id, None, 0, 100), (false, 1 | 2 | 4 | 8));

    snap.corporations.insert(
        test_corp,
        CorporationRecord {
            corporation_id: test_corp, name: "C".into(), ticker: "C".into(),
            ceo_id, alliance_id: None, tax_rate: 0.1,
            master_wallet: IskCents(0), members: HashSet::from([ceo_id, mem_id]),
            member_roles: HashMap::from([(mem_id, crate::corp_roles::ROLE_ACCOUNTANT)]),
            hq_station_id: Some(60_003_760),
            ..Default::default()
        },
    );
    assert_eq!(snap.effective_corp_roles_at(test_corp, ceo_id, None, 0, 100), (true, u64::MAX));

    let mut gov = CorpGovState::default();
    gov.member_bases.insert(mem_id, 60_008_494);
    gov.pending_role_grants.insert(mem_id, (crate::corp_roles::ROLE_DIRECTOR, 200));
    gov.pending_scoped_role_grants.insert(mem_id, (16, 32, 64, 200));
    gov.pending_title_role_grants.insert(mem_id, (128, 200));
    gov.titles.insert(1, ("Title1".into(), 128 | 256));
    gov.member_titles.insert(mem_id, 1);
    snap.corp_governance.insert(test_corp, gov);
    snap.corp_member_titles.insert((test_corp, mem_id), vec![1]);
    snap.corp_titles.insert(
        test_corp,
        vec![CorpTitleEntry {
            title_id: 1, name: "T1".into(),
            roles: vec!["Trader".into()], grantable_roles: vec![],
            roles_at_hq: vec!["Diplomat".into()], grantable_roles_at_hq: vec![],
            roles_at_base: vec!["Auditor".into()], grantable_roles_at_base: vec![],
            roles_at_other: vec!["Security Officer".into()], grantable_roles_at_other: vec![],
        }],
    );
    // Container chain resolving to HQ, base, other, and missing parent break
    let box_id = 777_001_i64;
    snap.items.insert(box_id, InventoryItemRecord {
        item_id: box_id, type_id: 17366, owner_id: mem_id, location_id: 60_003_760,
        flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1,
        dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
        insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
    });
    let (_, r_hq_pending) = snap.effective_corp_roles_at(test_corp, mem_id, Some(box_id), 0, 100);
    assert_eq!(r_hq_pending & crate::corp_roles::ROLE_DIRECTOR, 0);
    assert_eq!(r_hq_pending & crate::corp_roles::ROLE_TRADER, crate::corp_roles::ROLE_TRADER);
    let (_, r_base_matured) = snap.effective_corp_roles_at(test_corp, mem_id, Some(60_008_494), 0, 300);
    assert_ne!(r_base_matured & crate::corp_roles::ROLE_AUDITOR, 0);
    let (_, r_other_matured) = snap.effective_corp_roles_at(test_corp, mem_id, Some(60_003_761), 0, 300);
    assert_ne!(r_other_matured & crate::corp_roles::ROLE_SECURITY_OFFICER, 0);

    // is_ship_colocated_with_character missing ship/char -> false; consumable_ignoring_contents + ACL check
    assert!(!snap.is_ship_colocated_with_character(mem_id, 999_999, Some(box_id)));
    let sub_item = InventoryItemRecord {
        item_id: 777_002, type_id: 34, owner_id: mem_id, location_id: box_id,
        flag: InventoryFlag::None, quantity: 10, singleton: false, version: 1,
        dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
        insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
    };
    snap.items.insert(777_002, sub_item);
    assert!(!snap.is_item_consumable_by(&snap.items[&box_id].clone(), mem_id, Some(60_003_760), None));
    assert!(snap.is_item_consumable_by_ignoring_contents(&snap.items[&box_id].clone(), mem_id, Some(60_003_760), None));

    // resolve_expired_contract: corp bidder wins -> CorpDeliveries + extra_ids + strip_ship_cosmetics
    let mut csnap = WorldRepository::create_demo_snapshot();
    csnap.quasar_state_blobs.insert(
        "quasar_cosmetics_skinr".into(),
        r#"{"applied_thirdparty_skins":{"960001":{"skin":1}}}"#.into(),
    );
    csnap.quasar_state_blobs.insert(
        "quasar_cosmetics_live".into(),
        r#"{"ship_logos":{"960001":{"logo":1}}}"#.into(),
    );
    csnap.strip_ship_cosmetics(&[-1]); // empty negative skip path
    csnap.items.insert(960_001, InventoryItemRecord {
        item_id: 960_001, type_id: 587, owner_id: 90_000_001, location_id: 88_001,
        flag: InventoryFlag::Locked, quantity: 1, singleton: true, version: 1,
        dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
        insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
    });
    csnap.contracts.insert(88_001, ContractRecord {
        contract_id: 88_001, issuer_id: 90_000_001, issuer_corp_id: 98_000_001,
        acceptor_id: None, contract_type: "Auction".into(), status: "Outstanding".into(),
        for_corp: false, start_station_id: 60_003_760, end_station_id: 60_003_760,
        expiration_unix: 100, current_bidder_id: Some(98_000_001), current_bid: IskCents(50_000),
        item_ids: vec![], ..Default::default()
    });
    csnap.contracts.insert(88_002, ContractRecord {
        contract_id: 88_002, issuer_id: 90_000_001, issuer_corp_id: 0,
        acceptor_id: None, contract_type: "ItemExchange".into(), status: "Finished".into(),
        for_corp: false, start_station_id: 60_003_760, end_station_id: 60_003_760,
        expiration_unix: 100, ..Default::default()
    });
    assert_eq!(csnap.resolve_expired_contract(88_001, 200), Some("Completed"));
    assert_eq!(csnap.items[&960_001].flag, InventoryFlag::CorpDeliveries);
    assert_eq!(csnap.items[&960_001].owner_id, 98_000_001);
    assert!(csnap.resolve_expired_contract(88_002, 200).is_none());
    assert!(!csnap.fail_courier_contract(88_002, 200));

    // types: CorpWalletDivision / FromWalError panic check
    let mut corp = CorporationRecord::default();
    corp.wallet_divisions.push(CorpWalletDivisionRecord { division_id: 1, name: "M".into(), balance: IskCents(500) });
    corp.ensure_wallet_divisions();
    assert_eq!(corp.master_wallet, IskCents(500));
    assert_eq!(corp.debit_division(1, IskCents(-1)), None);
    assert_eq!(corp.debit_division(1, IskCents(600)), None);
    assert!(std::panic::catch_unwind(|| {
        let _ = <() as FromWalError>::from_wal_error("boom".into());
    }).is_err());

    // 4. persistence epochs, save_to_sqlite, prune_expired_auth_runtime + accounts + gateway_tokens local dev JWT
    assert!(repo.state_epoch() >= 1);
    assert!(repo.rollback_epoch() >= 1);
    assert!(repo.combat_state_epoch() >= 1);
    assert!(repo.sqlite_path().is_none());
    let save_dir = temp_dir("save_to_sqlite");
    assert!(repo.save_to_sqlite(save_dir.join("saved.sqlite")).is_ok());
    let _ = std::fs::remove_dir_all(&save_dir);
    assert_eq!(repo.prune_expired_auth_runtime(1_700_000_000), 0);

    // accounts: acquire_online_lock_with_computer_hash conflict & switch char stats + gateway_tokens dev JWT branches
    let uid = repo.with_snapshot(|s| s.accounts.values().find(|a| a.user_id > 0).unwrap().user_id);
    repo.acquire_online_lock_with_computer_hash(uid, 5001, Some(90_000_001), Some("pc1".into()), false, 1_800_000_000).unwrap();
    repo.set_online_session_peer_ip(uid, 5001, "127.0.0.1:40000");
    assert!(matches!(
        repo.acquire_online_lock_with_computer_hash(uid, 5002, Some(90_000_001), Some("pc2".into()), false, 1_800_000_001),
        Err(DbError::AlreadyLoggedIn { .. })
    ));
    let displaced = repo
        .acquire_online_lock_with_computer_hash(uid, 5002, Some(90_000_002), Some("pc2".into()), true, 1_800_000_002)
        .unwrap();
    assert!(displaced.is_some());
    repo.set_online_session_peer_ip(uid, 5002, "127.0.0.1:40001");
    assert_eq!(
        repo.with_snapshot(|s| s.character_login_stats.get(&90_000_001).map(|st| st.last_logout_unix)),
        Some(1_800_000_002)
    );
    // Displaced session id retry rejected
    assert!(matches!(
        repo.acquire_online_lock_with_computer_hash(uid, 5001, None, None, false, 1_800_000_003),
        Err(DbError::ValidationError(_))
    ));

    let sign_dev_jwt = |hdr: &str, pay: &str| -> String {
        let h = base64url_encode_no_pad(hdr.as_bytes());
        let p = base64url_encode_no_pad(pay.as_bytes());
        let sig = hmac_sha256(b"dev", format!("{h}.{p}").as_bytes());
        format!("{h}.{p}.{}", base64url_encode_no_pad(&sig))
    };
    let good_tok = sign_dev_jwt(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        &format!(r#"{{"sub":"USER:EVE:{uid}","exp":1900000000,"nbf":1800000000,"iat":1800000000,"jti":"jti-ok"}}"#),
    );
    // IPv4 localhost session matches IPv6 ::1 peer IP
    let verified = repo.verify_local_dev_client_jwt(&good_tok, IpAddr::V6(Ipv6Addr::LOCALHOST), 1_800_000_010).unwrap();
    assert_eq!(verified.user_id, uid);
    // Peer IP mismatch -> InvalidCredentials
    assert!(repo.verify_local_dev_client_jwt(&good_tok, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 1_800_000_010).is_err());
    // crit header / wrong alg / malformed / bad sig / forbidden claim / wrong sub / uid mismatch
    assert!(repo.verify_local_dev_client_jwt("bad.tok", IpAddr::V4(Ipv4Addr::LOCALHOST), 1_800_000_010).is_err());
    let crit_tok = sign_dev_jwt(r#"{"alg":"HS256","crit":["exp"]}"#, r#"{"sub":"USER:EVE:1"}"#);
    assert!(repo.verify_local_dev_client_jwt(&crit_tok, IpAddr::V4(Ipv4Addr::LOCALHOST), 1_800_000_010).is_err());
    let bad_sig = format!("{good_tok}x");
    assert!(repo.verify_local_dev_client_jwt(&bad_sig, IpAddr::V4(Ipv4Addr::LOCALHOST), 1_800_000_010).is_err());
    let priv_tok = sign_dev_jwt(r#"{"alg":"HS256"}"#, r#"{"sub":"USER:EVE:1","role":"admin"}"#);
    assert!(repo.verify_local_dev_client_jwt(&priv_tok, IpAddr::V4(Ipv4Addr::LOCALHOST), 1_800_000_010).is_err());
    let bad_sub = sign_dev_jwt(r#"{"alg":"HS256"}"#, r#"{"sub":"BAD:1"}"#);
    assert!(repo.verify_local_dev_client_jwt(&bad_sub, IpAddr::V4(Ipv4Addr::LOCALHOST), 1_800_000_010).is_err());
    let bad_uid = sign_dev_jwt(r#"{"alg":"HS256"}"#, &format!(r#"{{"sub":"USER:EVE:{uid}","user_id":999999}}"#));
    assert!(repo.verify_local_dev_client_jwt(&bad_uid, IpAddr::V4(Ipv4Addr::LOCALHOST), 1_800_000_010).is_err());
    let rev_sso = SsoTokenClaims {
        sub: format!("USER:EVE:{uid}"),
        name: "Demo".into(),
        owner: "Demo".into(),
        user_id: uid,
        character_id: 90_000_001,
        scp: vec![],
        iss: "https://login.eveonline.com".into(),
        aud: vec!["EVE Online".into()],
        iat: 1_800_000_000,
        exp: 1_900_000_000,
        jti: "jti-rev".into(),
    };
    let rev_sso_jwt = sign_dev_jwt(r#"{"alg":"HS256"}"#, &serde_json::to_string(&rev_sso).unwrap());
    assert!(repo.revoke_sso_refresh_token(&rev_sso_jwt));
    let rev_tok = sign_dev_jwt(
        r#"{"alg":"HS256"}"#,
        &format!(r#"{{"sub":"USER:EVE:{uid}","jti":"jti-rev"}}"#),
    );
    assert!(repo.verify_local_dev_client_jwt(&rev_tok, IpAddr::V4(Ipv4Addr::LOCALHOST), 1_800_000_010).is_err());
    let _ = crate::gateway_tokens::normalize_peer_ip(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, 1)));
}

#[test]
fn eve_db_pre_harden_coverage_and_edge_cases() {
    let repo = WorldRepository::with_demo_seed();
    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let station_id = 60_003_760_i64;
    let struct_id = 1_000_000_777_001_i64;

    // 1. inventory: idempotency eviction, stack_items fitted/non-empty rejection,
    //    resolve_corp_root_location non-corp parent break, matches_location_constraint without flag,
    //    transfer_inventory_item non-corp ownership violation.
    repo.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
        snap.committed_idempotency_keys.clear();
        for i in 0..4_096 {
            snap.committed_idempotency_keys
                .insert((char_id, format!("old_key_{i}")));
        }
        snap.items.insert(
            980_001,
            InventoryItemRecord {
                item_id: 980_001,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            980_002,
            InventoryItemRecord {
                item_id: 980_002,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            980_003,
            InventoryItemRecord {
                item_id: 980_003,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            980_004,
            InventoryItemRecord {
                item_id: 980_004,
                type_id: 23,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            980_005,
            InventoryItemRecord {
                item_id: 980_005,
                type_id: 35,
                owner_id: char_id,
                location_id: 980_004,
                flag: InventoryFlag::None,
                quantity: 4,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    repo.move_item_transactional(
        char_id,
        980_001,
        station_id,
        InventoryFlag::Hangar,
        None,
        Some("overflow_k"),
    )
    .unwrap();
    repo.with_snapshot(|s| {
        assert_eq!(s.committed_idempotency_keys.len(), 1);
        assert!(s
            .committed_idempotency_keys
            .contains(&(char_id, "overflow_k".to_string())));
    });

    assert!(matches!(
        repo.stack_items(char_id, 980_003, 980_002),
        Err(DbError::ValidationError(_))
    ));
    assert!(matches!(
        repo.stack_items(char_id, 980_003, 980_004),
        Err(DbError::ValidationError(_))
    ));

    let mut direct_no_flag = ConsumeExactOptions::for_character_stack(char_id);
    direct_no_flag.required_location_id = Some(station_id);
    direct_no_flag.required_flag = None;
    let mut nested_no_flag = direct_no_flag;
    nested_no_flag.allow_nested_in_location = true;

    repo.with_mut_snapshot(|snap| {
        snap.consume_exact_item_quantity(980_003, 2, direct_no_flag)
            .unwrap();
        snap.consume_exact_item_quantity(980_005, 2, nested_no_flag)
            .unwrap();

        // Corp item inside personal (non-corp) parent container triggers non-corp break in resolve_corp_root_location.
        snap.items.insert(
            980_006,
            InventoryItemRecord {
                item_id: 980_006,
                type_id: 34,
                owner_id: corp_id,
                location_id: 980_004,
                flag: InventoryFlag::CorpSAG1,
                quantity: 3,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        assert!(snap
            .validate_corp_take_access(other_char, corp_id, 980_006)
            .is_err());

        // Non-corp ownership mismatch when expected_owner_id is None.
        let spec = TransferItemSpec::move_whole(
            Some(other_char),
            None,
            None,
            station_id,
            InventoryFlag::Hangar,
        );
        assert!(matches!(
            snap.transfer_inventory_item(980_001, spec),
            Err(DbError::ItemOwnershipViolation { .. })
        ));

        // 2. snapshot_ext: missing character co-location, ACL denial in is_item_consumable_by,
        //    and resolve_expired_contract updating c.item_ids directly.
        assert!(!snap.is_ship_colocated_with_character(999_777_666, 980_004, None));

        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                solar_system_id: 30_000_142,
                owner_corp_id: corp_id,
                state: "ShieldVulnerable".into(),
                name: "DeniedCitadel".into(),
                ..Default::default()
            },
        );
        snap.structure_docking_acl_denied
            .insert(struct_id, [char_id].into_iter().collect());
        let st_item = InventoryItemRecord {
            item_id: 980_007,
            type_id: 34,
            owner_id: char_id,
            location_id: struct_id,
            flag: InventoryFlag::Hangar,
            quantity: 5,
            singleton: false,
            version: 1,
            ..Default::default()
        };
        assert!(!snap.is_item_consumable_by(&st_item, char_id, Some(struct_id), None));

        snap.items.insert(
            980_008,
            InventoryItemRecord {
                item_id: 980_008,
                type_id: 587,
                owner_id: char_id,
                location_id: 980_099,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            980_099,
            ContractRecord {
                contract_id: 980_099,
                issuer_id: char_id,
                issuer_corp_id: corp_id,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                for_corp: false,
                start_station_id: station_id,
                end_station_id: station_id,
                expiration_unix: 100,
                current_bidder_id: Some(other_char),
                current_bid: IskCents(1_000),
                item_ids: vec![980_008],
                ..Default::default()
            },
        );
        assert_eq!(snap.resolve_expired_contract(980_099, 200), Some("Completed"));
        assert_eq!(snap.items[&980_008].owner_id, other_char);
        assert_eq!(snap.items[&980_008].location_id, station_id);
    });

    // 3. service_domains::migrate_legacy_service_state with initialized == false + non-empty incursions.
    let mut legacy_snap = WorldStateSnapshot::default();
    legacy_snap.service_state_blobs.insert(
        "incursions_state".into(),
        serde_json::json!({
            "initialized": false,
            "incursions": {
                "20000001": {
                    "tale_id": 1,
                    "constellation_id": 20000001,
                    "constellation_name": "Kimotoro",
                    "staging_solar_system_id": 30000142,
                    "vanguard_solar_system_ids": [],
                    "assault_solar_system_ids": [],
                    "hq_solar_system_id": 30000144,
                    "influence": 0.5,
                    "state": "active",
                    "mothership_spawned": false,
                    "mothership_defeated": false,
                    "sites": []
                }
            }
        })
        .to_string(),
    );
    legacy_snap.migrate_legacy_service_state();
    assert!(legacy_snap.pve_special_sites.incursions.contains_key(&20_000_001));
}
