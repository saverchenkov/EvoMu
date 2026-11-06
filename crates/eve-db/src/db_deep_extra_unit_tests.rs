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
