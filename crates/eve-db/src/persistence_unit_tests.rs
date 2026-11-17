//! Unit and mutation-killing tests for `persistence`.

use super::*;
use crate::clock::VirtualServerClock;
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn sample_character(character_id: i64, balance_cents: i64) -> CharacterRecord {
    CharacterRecord {
        activity_status: None,
        character_id,
        user_id: 1,
        name: format!("Pilot{character_id}"),
        corporation_id: 1_000_009,
        alliance_id: None,
        war_faction_id: None,
        station_id: Some(60_003_760),
        structure_id: None,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        active_ship_id: 100_000_001,
        balance: IskCents(balance_cents),
        plex_balance: 0,
        loyalty_points: HashMap::new(),
        standings: HashMap::new(),
        security_status: 0.0,
        clone_grade: 2,
        home_station_id: 60_003_760,
        race_id: 1,
        bloodline_id: 1,
        ancestry_id: 1,
        school_id: 17,
        neural: NeuralAttributes::default(),
        skill_points: 0.0,
        free_skill_points: 0.0,
        last_clone_jump_unix: 0,
        jump_fatigue_minutes: 0.0,
        jump_cooldown_until_unix: 0,
        jump_fatigue_until_unix: 0,
        description: String::new(),
        title: String::new(),
        gender: 1,
        create_date_unix: 1_700_000_000,
        fleet_id: None,
        autopilot_waypoints: Vec::new(),
        version: 1,
        system_epoch: 1,
    }
}

fn sample_market_order(order_id: i64, vol_rem: i32) -> MarketOrderRecord {
    MarketOrderRecord {
        order_id,
        owner_id: 90_000_001,
        station_id: 60_003_760,
        region_id: 10_000_002,
        type_id: 34,
        is_bid: false,
        price: IskCents(500),
        vol_entered: 100,
        vol_remaining: vol_rem,
        min_volume: 1,
        is_npc: false,
        escrow: IskCents(0),
    }
}

#[test]
fn wal_delta_from_diff_and_apply_to_hot_and_cold_paths() {
    let mut old = WorldStateSnapshot {
        next_entity_id: 200_000_000,
        wal_seq: 1,
        ..Default::default()
    };
    old.characters.insert(90_000_001, sample_character(90_000_001, 1_000));
    old.items.insert(
        101,
        InventoryItemRecord {
            item_id: 101,
            type_id: 34,
            quantity: 10,
            ..Default::default()
        },
    );
    old.market_orders.insert(501, sample_market_order(501, 50));
    old.wallet_journal.push(WalletJournalEntry {
        entry_id: 1,
        character_id: 90_000_001,
        entry_type: "Bounty".to_string(),
        amount: IskCents(1_000),
        balance_after: IskCents(1_000),
        reason: "init".to_string(),
    });
    old.contracts.insert(
        701,
        ContractRecord {
            contract_id: 701,
            status: "Outstanding".to_string(),
            ..Default::default()
        },
    );
    old.system_states.insert(
        30_000_142,
        SystemDynamicState {
            solar_system_id: 30_000_142,
            dbs_multiplier: 1.0,
            ..Default::default()
        },
    );
    old.committed_idempotency_keys
        .insert((90_000_001, "k1".to_string()));

    // Hot-path incremental mutations
    let mut new = old.clone();
    new.wal_seq = 5;
    new.next_entity_id = 200_000_010;
    new.characters
        .insert(90_000_001, sample_character(90_000_001, 2_500));
    new.items.remove(&101);
    new.items.insert(
        102,
        InventoryItemRecord {
            item_id: 102,
            type_id: 35,
            quantity: 20,
            ..Default::default()
        },
    );
    new.market_orders.remove(&501);
    new.market_orders.insert(502, sample_market_order(502, 80));
    new.wallet_journal.push(WalletJournalEntry {
        entry_id: 2,
        character_id: 90_000_001,
        entry_type: "Market".to_string(),
        amount: IskCents(1_500),
        balance_after: IskCents(2_500),
        reason: "sale".to_string(),
    });
    new.contracts.get_mut(&701).unwrap().status = "Completed".to_string();
    new.system_states.insert(
        30_000_144,
        SystemDynamicState {
            solar_system_id: 0, // from_diff must normalize to 30_000_144
            dbs_multiplier: 1.25,
            ..Default::default()
        },
    );
    new.committed_idempotency_keys
        .insert((90_000_001, "k2".to_string()));

    let delta = WorldStateWalDelta::from_diff(&old, &new);
    assert!(delta.full_snapshot.is_none());
    assert_eq!(delta.seq, 5);
    assert_eq!(delta.next_entity_id, 200_000_010);
    assert_eq!(delta.upserted_characters.len(), 1);
    assert_eq!(delta.removed_items, vec![101]);
    assert_eq!(delta.upserted_items.len(), 1);
    assert_eq!(delta.removed_market_orders, vec![501]);
    assert_eq!(delta.upserted_market_orders.len(), 1);
    assert_eq!(delta.appended_wallet_journal.len(), 1);
    assert_eq!(delta.upserted_contracts.len(), 1);
    assert_eq!(delta.upserted_system_states.len(), 1);
    assert_eq!(delta.upserted_system_states[0].solar_system_id, 30_000_144);
    assert_eq!(
        delta.added_idempotency_keys,
        vec![(90_000_001, "k2".to_string())]
    );

    let mut applied = old.clone();
    delta.apply_to(&mut applied);
    let mut expected_new = new.clone();
    expected_new
        .system_states
        .get_mut(&30_000_144)
        .unwrap()
        .solar_system_id = 30_000_144;
    assert_eq!(applied, expected_new);

    // Removing a character or modifying wallet_journal prefix must fall back to full_snapshot
    let mut removed_char_snap = old.clone();
    removed_char_snap.wal_seq = 9;
    removed_char_snap.characters.clear();
    let full_delta = WorldStateWalDelta::from_diff(&old, &removed_char_snap);
    assert!(full_delta.full_snapshot.is_some());
    let mut target = old.clone();
    full_delta.apply_to(&mut target);
    assert!(target.characters.is_empty());
    assert_eq!(target.wal_seq, 9);
}

#[test]
fn repository_metadata_clocks_write_depth_and_asteroids() {
    let r1 = WorldRepository::new_empty_with_snapshot_path(None).expect("in-memory repo 1");
    let r2 = WorldRepository::new_empty_with_snapshot_path(None).expect("in-memory repo 2");
    assert!(r2.instance_id() > r1.instance_id());
    assert!(r1.is_ephemeral_in_memory());
    assert_eq!(r1.sqlite_path(), None);

    let vclock = Arc::new(VirtualServerClock::with_fixed_base(1_750_000_000));
    r1.set_clock(vclock);
    assert_eq!(r1.now_unix_secs(), 1_750_000_000);

    assert_eq!(
        WorldRepository::wal_path_for(Path::new("/tmp/world.json")),
        std::path::PathBuf::from("/tmp/world.json.wal")
    );
    assert!(WorldRepository::is_sqlite_extension(Path::new("a.sqlite")));
    assert!(WorldRepository::is_sqlite_extension(Path::new("a.sqlite3")));
    assert!(WorldRepository::is_sqlite_extension(Path::new("a.db")));
    assert!(!WorldRepository::is_sqlite_extension(Path::new("a.json")));
    assert!(!WorldRepository::is_sqlite_extension(Path::new("noext")));

    assert!(!r1.is_write_locked_by_current_thread());
    r1.with_mut_snapshot(|_| {
        assert!(r1.is_write_locked_by_current_thread());
    });
    assert!(!r1.is_write_locked_by_current_thread());

    let res: Result<u32, String> = r1.try_with_mut_snapshot(|_| {
        assert!(r1.is_write_locked_by_current_thread());
        Ok(42)
    });
    assert_eq!(res, Ok(42));
    assert!(!r1.is_write_locked_by_current_thread());

    assert_eq!(r1.try_with_snapshot(|s| s.next_entity_id), Some(200_000_000));

    let ast = AsteroidRecord {
        asteroid_id: 777,
        solar_system_id: 30_000_142,
        belt_id: 40_000_001,
        type_id: 1230,
        quantity: 5_000,
        initial_quantity: 5_000,
        position: (1.0, 2.0, 3.0),
        radius: 150.0,
    };
    r1.spawn_asteroid(ast.clone());
    // Spawning duplicate asteroid_id replaces the existing record
    let mut ast_dup = ast.clone();
    ast_dup.quantity = 1;
    r1.spawn_asteroid(ast_dup.clone());
    assert_eq!(r1.get_asteroid(777), Some(ast_dup.clone()));
    assert_eq!(r1.get_asteroid(888), None);
    assert_eq!(r1.get_asteroids_in_system(30_000_142), vec![ast_dup]);
    assert!(r1.get_asteroids_in_system(30_000_144).is_empty());
}

#[test]
fn snapshot_wal_fault_injection_torn_tail_and_checkpoint_compaction() {
    let dir = std::env::temp_dir();
    let snap_path = dir.join(format!("evomu_unit_persistence_{}.json", std::process::id()));
    let wal_path = WorldRepository::wal_path_for(&snap_path);
    let sqlite_out = dir.join(format!(
        "evomu_unit_persistence_export_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(&wal_path);
    let _ = std::fs::remove_file(&sqlite_out);

    let repo =
        WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone())).expect("init json");
    assert!(!repo.is_ephemeral_in_memory());
    assert!(snap_path.exists());

    // TxFaultPlan FailNthStatement(1) and FailAtCommit roll back cleanly
    repo.set_fault_plan_for_test(crate::TxFaultPlan::FailNthStatement(1));
    let err1: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.next_entity_id = 200_000_111;
        Ok(())
    });
    assert!(err1.is_err());
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), 200_000_000);

    repo.set_fault_plan_for_test(crate::TxFaultPlan::FailAtCommit);
    let err2: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.next_entity_id = 200_000_222;
        Ok(())
    });
    assert!(err2.is_err());
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), 200_000_000);

    // Trigger WAL_CHECKPOINT_INTERVAL (64) to exercise compaction
    for i in 1..=WAL_CHECKPOINT_INTERVAL {
        repo.with_mut_snapshot(|snap| {
            snap.next_entity_id = 200_000_000 + i as i64;
        });
    }
    assert_eq!(repo.pending_wal_records.load(Ordering::SeqCst), 0);
    assert!(!wal_path.exists());

    // Append one more WAL delta, then append a torn trailing line and reload
    repo.with_mut_snapshot(|snap| {
        snap.next_entity_id = 200_009_999;
    });
    assert_eq!(repo.pending_wal_records.load(Ordering::SeqCst), 1);
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&wal_path)
            .unwrap();
        f.write_all(b"{\"seq\":999,\"truncated").unwrap();
    }
    repo.load_snapshot_from_file(&snap_path)
        .expect("reload healing torn tail");
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), 200_009_999);

    // Corrupt non-trailing WAL line must fail
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&wal_path).unwrap();
        f.write_all(b"{corrupt_line_1}\n{\"seq\":100,\"next_entity_id\":200010000}\n")
            .unwrap();
    }
    assert!(repo.load_snapshot_from_file(&snap_path).is_err());

    // Duplicate seq in WAL must fail
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&wal_path).unwrap();
        f.write_all(
            b"{\"seq\":70,\"next_entity_id\":200010000}\n{\"seq\":70,\"next_entity_id\":200010001}\n",
        )
        .unwrap();
    }
    assert!(repo.load_snapshot_from_file(&snap_path).is_err());

    // Clean WAL and test save_to_sqlite
    let _ = std::fs::remove_file(&wal_path);
    repo.pending_wal_records.store(0, Ordering::SeqCst);
    repo.save_to_sqlite(&sqlite_out).expect("save_to_sqlite");
    let sql_repo =
        WorldRepository::new_empty_with_snapshot_path(Some(sqlite_out.clone())).expect("open sqlite");
    assert_eq!(sql_repo.sqlite_path(), Some(sqlite_out.as_path()));
    assert!(!sql_repo.is_ephemeral_in_memory());
    assert_eq!(sql_repo.with_snapshot(|s| s.next_entity_id), 200_009_999);

    drop(sql_repo);
    drop(repo);
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(&wal_path);
    let _ = std::fs::remove_file(&sqlite_out);
}

#[test]
fn noop_mut_snapshot_skips_wal_and_skill_tick_emits_incremental_delta() {
    let dir = std::env::temp_dir();
    let snap_path = dir.join(format!("evomu_idle_wal_{}.json", std::process::id()));
    let wal_path = WorldRepository::wal_path_for(&snap_path);
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(&wal_path);

    let repo =
        WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone())).expect("init json");
    repo.with_mut_snapshot(|snap| {
        snap.characters.insert(90_000_001, sample_character(90_000_001, 50_000));
        snap.skills.entry(90_000_001).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 3,
                skill_points: 8_000.0,
            },
        );
        snap.skill_queues.insert(
            90_000_001,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 4,
                start_sp: 8_000.0,
                end_sp: 45_255.0,
                duration_seconds: 1200.0,
                started_at_unix: Some(1_700_000_000),
            }],
        );
    });
    repo.save_snapshot_to_file(&snap_path).expect("checkpoint");
    assert!(!wal_path.exists());

    let seq_before = repo.with_snapshot(|s| s.wal_seq);
    let epoch_before = repo.state_epoch();

    // 1. No-op `with_mut_snapshot` and `try_with_mut_snapshot` must write 0 bytes to `.json.wal`
    // and keep `wal_seq`, `pending_wal_records`, and `state_epoch` completely unchanged.
    for _ in 0..10 {
        repo.with_mut_snapshot(|_snap| {});
        let ok_res: Result<(), String> = repo.try_with_mut_snapshot(|_snap| Ok(()));
        assert!(ok_res.is_ok());
    }
    assert!(
        !wal_path.exists(),
        "no-op mut snapshot calls must not create or append to .json.wal"
    );
    assert_eq!(repo.pending_wal_records.load(Ordering::SeqCst), 0);
    assert_eq!(repo.with_snapshot(|s| s.wal_seq), seq_before);
    assert_eq!(repo.state_epoch(), epoch_before);

    // 2. 1 Hz skill progression (`skills` + `skill_queues` + `characters` SP update) must emit a
    // compact incremental `.json.wal` delta without cloning or serializing `full_snapshot`.
    repo.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.skill_points += 0.5;
        }
        if let Some(sk) = snap.skills.get_mut(&90_000_001).and_then(|m| m.get_mut(&3300)) {
            sk.skill_points += 0.5;
        }
        if let Some(q) = snap.skill_queues.get_mut(&90_000_001).and_then(|v| v.first_mut()) {
            q.start_sp += 0.5;
            q.duration_seconds -= 1.0;
        }
    });
    assert!(wal_path.exists());
    let wal_text = std::fs::read_to_string(&wal_path).expect("read wal");
    assert!(
        !wal_text.contains("\"full_snapshot\""),
        "1 Hz skill tick WAL delta must be incremental without full_snapshot: {wal_text}"
    );
    let parsed_delta: WorldStateWalDelta =
        serde_json::from_str(wal_text.lines().next().unwrap()).expect("parse wal line");
    assert!(parsed_delta.full_snapshot.is_none());
    assert_eq!(parsed_delta.upserted_skills.len(), 1);
    assert_eq!(parsed_delta.upserted_skill_queues.len(), 1);

    // Simulate ungraceful crash and verify incremental skill WAL delta replays losslessly.
    std::mem::forget(repo);
    let reopened =
        WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone())).expect("reopen");
    reopened.with_snapshot(|snap| {
        assert!((snap.skills[&90_000_001][&3300].skill_points - 8_000.5).abs() < 1e-6);
        assert!((snap.skill_queues[&90_000_001][0].start_sp - 8_000.5).abs() < 1e-6);
    });

    drop(reopened);
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(&wal_path);
}

#[test]
fn persistence_auth_prune_wal_skip_and_missing_snapshot_fallback() {
    let pid = std::process::id();
    let db_path = std::env::temp_dir().join(format!("evomu_auth_prune_{pid}.sqlite"));
    let _ = std::fs::remove_file(&db_path);
    {
        let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
        {
            let mut rt = repo.auth_runtime.write().unwrap();
            rt.revoked_jtis.insert("jti-exp-1".into());
            rt.revoked_jti_expiries.insert("jti-exp-1".into(), 100);
            rt.revoked_jtis.insert("jti-keep-1".into());
            rt.revoked_jti_expiries.insert("jti-keep-1".into(), 5_000);
            rt.refresh_tokens.insert(
                "tok-exp".into(),
                SsoRefreshTokenRecord {
                    refresh_token: "tok-exp".into(),
                    user_id: 1,
                    character_id: 90_000_001,
                    client_id: "cli".into(),
                    scopes: vec!["s".into()],
                    expires_at_unix: 200,
                    revoked: false,
                    last_access_token_jti: "j".into(),
                    last_access_token_exp_unix: 200,
                },
            );
            rt.refresh_tokens.insert(
                "tok-keep".into(),
                SsoRefreshTokenRecord {
                    refresh_token: "tok-keep".into(),
                    user_id: 1,
                    character_id: 90_000_001,
                    client_id: "cli".into(),
                    scopes: vec!["s".into()],
                    expires_at_unix: 6_000,
                    revoked: false,
                    last_access_token_jti: "j2".into(),
                    last_access_token_exp_unix: 6_000,
                },
            );
        }
        let pruned = repo.prune_expired_auth_runtime(1_000);
        assert!(pruned >= 2);
        let rt = repo.auth_runtime.read().unwrap();
        assert!(rt.revoked_jtis.contains("jti-keep-1"));
        assert!(!rt.revoked_jtis.contains("jti-exp-1"));
        assert!(rt.refresh_tokens.contains_key("tok-keep"));
        assert!(!rt.refresh_tokens.contains_key("tok-exp"));
    }
    let _ = std::fs::remove_file(&db_path);

    // append_wal_delta_locked when snap_path does not exist falls back to save_snapshot_locked
    let snap_path = std::env::temp_dir().join(format!("evomu_missing_snap_fallback_{pid}.json"));
    let wal_path = WorldRepository::wal_path_for(&snap_path);
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(&wal_path);
    let repo = WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone())).unwrap();
    let _ = std::fs::remove_file(&snap_path);
    repo.with_mut_snapshot(|s| {
        s.next_entity_id = 200_000_123;
    });
    assert!(snap_path.exists());

    // load_snapshot_from_file skips WAL deltas with delta.seq <= snap.wal_seq
    {
        use std::io::Write as _;
        let mut f = std::fs::File::create(&wal_path).unwrap();
        f.write_all(b"{\"seq\":1,\"next_entity_id\":199999999}\n")
            .unwrap();
    }
    repo.load_snapshot_from_file(&snap_path).unwrap();
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), 200_000_123);

    drop(repo);
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(&wal_path);
}

#[test]
fn run_mut_snapshot_tx_clean_tracked_fastpath_and_persist_error_fp_rollback() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001;
    let orig_bal = repo.with_snapshot(|s| s.characters[&cid].balance);
    let seq0 = repo.with_snapshot(|s| s.wal_seq);
    let ep0 = repo.state_epoch();

    // 1. Clean no-op transaction returns early without opening a SQLite transaction or bumping wal_seq / state_epoch.
    repo.with_mut_snapshot(|_| {});
    assert_eq!(repo.with_snapshot(|s| s.wal_seq), seq0);
    assert_eq!(repo.state_epoch(), ep0);

    let cw_ep0 = repo.combat_state_epoch();
    repo.spawn_asteroid(AsteroidRecord {
        asteroid_id: 77_001,
        solar_system_id: 30_000_142,
        belt_id: 40_000_001,
        type_id: 1230,
        quantity: 100,
        initial_quantity: 100,
        position: (0.0, 0.0, 0.0),
        radius: 10.0,
    });
    assert!(repo.with_snapshot(|s| s.asteroids.contains_key(&77_001)));
    assert_eq!(repo.combat_state_epoch(), cw_ep0);
    repo.with_mut_snapshot(|_| {});

    let created = repo
        .add_or_stack_item(cid, 60_003_760, InventoryFlag::Hangar, 38, 10)
        .unwrap();
    assert_eq!(created.quantity, 10);
    let stacked = repo
        .add_or_stack_item(cid, 60_003_760, InventoryFlag::Hangar, 38, 5)
        .unwrap();
    assert_eq!(stacked.item_id, created.item_id);
    assert_eq!(stacked.quantity, 15);

    repo.with_mut_snapshot(|snap| {
        snap.crimewatch_states.insert(
            cid,
            PersistedCrimeWatchState {
                character_id: cid,
                weapon_timer_until_unix: 9_999,
                ..Default::default()
            },
        );
    });
    assert_eq!(repo.combat_state_epoch(), cw_ep0 + 1);

    // 2. Persist error mid-sync leaves `fp` and `published` untouched (`before == published`),
    //    so subsequent mutations to the same character still persist cleanly.
    repo.with_sqlite_conn(|conn| {
        conn.execute_batch(
            "CREATE TRIGGER fail_struct_ins BEFORE INSERT ON structures BEGIN SELECT RAISE(ABORT, 'inject_struct_abort'); END;",
        )
        .unwrap();
    });
    let st_ep0 = repo.state_epoch();
    let err_res: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().balance = IskCents(777_666_555);
        snap.structures.insert(
            999_888,
            UpwellStructureRecord {
                structure_id: 999_888,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "FailStruct".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        Ok(())
    });
    assert!(
        err_res
            .as_ref()
            .is_err_and(|e| e.contains("inject_struct_abort")),
        "expected inject_struct_abort, got {err_res:?}"
    );
    assert_eq!(repo.state_epoch(), st_ep0);
    assert_eq!(repo.with_snapshot(|s| s.characters[&cid].balance), orig_bal);
    assert!(!repo.with_snapshot(|s| s.structures.contains_key(&999_888)));

    repo.with_sqlite_conn(|conn| {
        conn.execute_batch("DROP TRIGGER fail_struct_ins;").unwrap();
    });
    repo.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().balance = IskCents(777_666_555);
    });
    let sql_bal: i64 = repo.with_sqlite_conn(|conn| {
        conn.query_row(
            "SELECT balance_cents FROM characters WHERE character_id = ?1",
            rusqlite::params![cid],
            |r| r.get(0),
        )
        .unwrap()
    });
    assert_eq!(sql_bal, 777_666_555);
}

#[test]
fn w5a_p2_0a_service_state_blob_write_time_enforcement_and_non_fatal_sync() {
    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("evomu_w5a_p2_0a_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");

    let violations_before = crate::service_domains::service_state_blob_limit_violations_total();

    // 1. Write-time enforcement via set_service_state_blob rejects oversized value and total byte overflow
    //    without mutating service_state_blobs, and increments counter (#902 / #832).
    repo.with_mut_snapshot(|snap| {
        let oversized_val = "x".repeat(crate::service_domains::MAX_SERVICE_STATE_BLOB_VALUE_BYTES + 1);
        assert!(snap
            .set_service_state_blob("char_admin:oversized".into(), oversized_val)
            .is_err());
        assert!(!snap.service_state_blobs.contains_key("char_admin:oversized"));

        snap.set_service_state_blob("char_admin:valid".into(), "{\"ok\":true}".into())
            .expect("within bounds succeeds");
    });
    assert!(
        crate::service_domains::service_state_blob_limit_violations_total() >= violations_before + 1
    );

    // 2. Even if raw service_state_blobs.insert bypasses set_service_state_blob with an oversized value,
    //    sync_service_domains logs and increments the counter WITHOUT aborting unrelated SQLite writes (#902 / #832).
    let violations_mid = crate::service_domains::service_state_blob_limit_violations_total();
    repo.with_mut_snapshot(|snap| {
        snap.next_entity_id = 200_555_001;
        snap.service_state_blobs.insert(
            "raw_oversized_bypass".into(),
            "y".repeat(crate::service_domains::MAX_SERVICE_STATE_BLOB_VALUE_BYTES + 1),
        );
    });
    assert!(
        crate::service_domains::service_state_blob_limit_violations_total() > violations_mid,
        "sync_service_domains must record limit violation on raw bypass (#902)"
    );
    drop(repo);

    let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen_sqlite");
    assert_eq!(reopened.with_snapshot(|s| s.next_entity_id), 200_555_001);
    assert_eq!(
        reopened.with_snapshot(|s| s.service_state_blobs.get("char_admin:valid").cloned()),
        Some("{\"ok\":true}".into())
    );
    assert!(
        !reopened.with_snapshot(|s| s.service_state_blobs.contains_key("raw_oversized_bypass")),
        "oversized service_state_blobs must be skipped during SQLite sync and not persisted to domain_rows (#902)"
    );
    drop(reopened);
    let _ = std::fs::remove_file(&db_path);
}

#[test]
fn w5a_p2_0b_non_panicking_with_mut_snapshot_and_rollback_reload_fallback() {
    let dir = std::env::temp_dir();
    let snap_path = dir.join(format!("evomu_w5a_p2_0b_{}.json", std::process::id()));
    let wal_path = WorldRepository::wal_path_for(&snap_path);
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(&wal_path);

    let repo =
        WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone())).expect("init json");
    let init_id = repo.with_snapshot(|s| s.next_entity_id);
    let failures_before = crate::persist_failures_total();
    let st_before = repo.state_epoch();

    // 1. tx fault injection returns Err cleanly, restores working to `before`, leaves `state_epoch`
    //    unchanged, and increments `persist_failures_total()`.
    repo.set_fault_plan_for_test(crate::TxFaultPlan::FailAtCommit);
    let err_res: Result<(), crate::TxError> = repo.tx("fault_at_commit", |wtx| {
        wtx.snap.next_entity_id = 200_999_111;
        Ok(())
    });
    assert!(err_res.is_err());
    assert_eq!(
        repo.with_snapshot(|s| s.next_entity_id),
        init_id,
        "failed tx must keep pre-transaction published snapshot"
    );
    assert_eq!(repo.state_epoch(), st_before);
    assert!(crate::persist_failures_total() > failures_before);

    // 2. with_mut_snapshot panics on persistence failure (while still restoring `before`), and
    //    try_with_mut_snapshot returns Err cleanly on user error without incrementing persist failures.
    let err_res: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.next_entity_id = 200_888_777;
        Err("abort transaction cleanly".into())
    });
    assert!(err_res.is_err());
    assert_eq!(repo.state_epoch(), st_before);
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), init_id);
    assert!(repo.quarantined_rows().expect("quarantine_rows").is_empty());

    drop(repo);
    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(&wal_path);
}

#[test]
fn w5a_p2_0c_closure_panic_catch_unwind_rolls_back_snapshot_and_bumps_counter() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001;
    let orig_bal = repo.with_snapshot(|s| s.characters[&cid].balance);
    let orig_next_id = repo.with_snapshot(|s| s.next_entity_id);
    let panics_before = crate::tx_panics_total();
    let st_before = repo.state_epoch();

    // 1. Panic inside try_with_mut_snapshot is caught by WorldRepository::tx, restores `before`,
    //    increments tx_panics_total(), and returns Err (#904 / #833).
    let res: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.next_entity_id = 999_111_222;
        snap.characters.get_mut(&cid).unwrap().balance = IskCents(123);
        panic!("simulated try_with_mut_snapshot panic");
    });
    let err_msg = res.expect_err("panic in try_with_mut_snapshot must return Err");
    assert!(err_msg.contains("simulated try_with_mut_snapshot panic"));
    assert_eq!(crate::tx_panics_total(), panics_before + 1);
    assert_eq!(repo.state_epoch(), st_before);
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), orig_next_id);
    assert_eq!(repo.with_snapshot(|s| s.characters[&cid].balance), orig_bal);

    // 2. Panic inside with_mut_snapshot restores `before` and releases all locks cleanly
    //    before propagating the panic (#904 / #833).
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        repo.with_mut_snapshot(|snap| {
            snap.next_entity_id = 888_222_333;
            snap.characters.get_mut(&cid).unwrap().balance = IskCents(456);
            panic!("simulated with_mut_snapshot panic");
        });
    }));
    assert!(caught.is_err());
    assert_eq!(crate::tx_panics_total(), panics_before + 2);
    assert_eq!(repo.state_epoch(), st_before);
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), orig_next_id);
    assert_eq!(repo.with_snapshot(|s| s.characters[&cid].balance), orig_bal);
    assert!(!repo.published.is_poisoned(), "published RwLock must not be poisoned");
    assert!(!repo.writer.is_poisoned(), "writer Mutex must not be poisoned");
}

#[test]
fn w5a_p2_0d_fnv_serializer_i128_u128_map_key_errors_and_json_fallback() {
    use crate::sqlite_store::checked_json::{canonical_fp, FnvHasher, FnvSerializer};
    use serde::ser::SerializeMap;

    #[derive(Serialize)]
    struct WideInts {
        signed: i128,
        unsigned: u128,
    }

    let h1 = canonical_fp(&WideInts {
        signed: -1,
        unsigned: 1,
    });
    let h2 = canonical_fp(&WideInts {
        signed: -2,
        unsigned: 1,
    });
    let h3 = canonical_fp(&WideInts {
        signed: -1,
        unsigned: u128::MAX,
    });
    assert_ne!(h1, 0);
    assert_ne!(h1, h2, "i128 mutation must change canonical_fp (#905 / #855)");
    assert_ne!(h1, h3, "u128 mutation must change canonical_fp (#905 / #855)");
    assert_ne!(canonical_fp(&1i128), canonical_fp(&1u128));
    assert_ne!(canonical_fp(&1i128), canonical_fp(&1i64));
    assert_ne!(canonical_fp(&i128::MIN), canonical_fp(&i128::MAX));

    // Map key error propagation (#905 / #855): a map key whose Serialize implementation returns Err
    // must propagate the error out of FnvMapSer::serialize_key instead of hashing key_hash = 0.
    struct FailingKey;
    impl Serialize for FailingKey {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("failing map key"))
        }
    }
    struct MapWithFailingKey;
    impl Serialize for MapWithFailingKey {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut m = serializer.serialize_map(Some(1))?;
            m.serialize_entry(&FailingKey, &123i64)?;
            m.end()
        }
    }
    let mut hasher = FnvHasher::new();
    let ser_res = MapWithFailingKey.serialize(FnvSerializer(&mut hasher));
    assert!(
        ser_res.is_err(),
        "FnvMapSer::serialize_key must propagate map key serialization errors (#905)"
    );
}

#[test]
fn w5a_p2_0e_fail_closed_loader_quarantine_unreadable_finite_floats_and_serde_defaults() {
    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("evomu_w5a_p2_0e_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);

    let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
    repo.with_mut_snapshot(|snap| {
        snap.char_notes.insert((90_000_001, 1), "good note".into());
    });

    // 1. Non-finite float check (#906 / #856): NaN or Infinity in a float field or JSON-serialized
    //    struct is rejected before serde_json can silently write `null`, rolling back cleanly.
    let nan_res: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.incursions.insert(
            20_000_001,
            IncursionRecord {
                constellation_id: 20_000_001,
                staging_solar_system_id: 30_000_142,
                infested_solar_systems: vec![30_000_142],
                faction_id: 500_019,
                has_boss: true,
                influence: f64::NAN,
                state: "Mobilizing".into(),
                incursion_type: "Incursion".into(),
            },
        );
        Ok(())
    });
    assert!(
        nan_res.is_err(),
        "NaN float inside JSON-serialized record must be rejected before writing null (#906)"
    );
    assert!(repo.with_snapshot(|s| s.incursions.is_empty()));

    // 2. Inject corrupt JSON into `domain_rows` (`notes_lp.char_lp_logs`) and legacy
    //    `social_store` (`char_contact_labels`), then verify default `open_sqlite` fails closed
    //    naming the domain/table and primary key (#906 / #856 / #916).
    repo.with_sqlite_conn(|conn| {
        conn.execute_batch(crate::sqlite_store::social_store::LEGACY_D6_SOCIAL_SQL)
            .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO domain_rows (domain, key, value) VALUES (?1, '90000088', '{corrupt')",
            [crate::sqlite_store::domain_rows::DOMAIN_CHAR_LP_LOGS],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO char_contact_labels (character_id, labels_json) VALUES (90000077, '{corrupt')",
            [],
        )
        .unwrap();
    });
    drop(repo);

    let fail_closed_err = match WorldRepository::open_sqlite(&db_path) {
        Ok(_) => panic!("open_sqlite must fail closed on corrupt row by default (#906)"),
        Err(e) => e,
    };
    assert!(
        fail_closed_err.contains("char_lp_logs") || fail_closed_err.contains("char_contact_labels"),
        "expected table name in error, got: {fail_closed_err}"
    );

    // 3. Opening with `open_sqlite_with_options(&db_path, true)` (`--quarantine-unreadable`)
    //    moves both corrupt rows into `quarantine_rows`, deletes them from the source tables,
    //    and preserves healthy rows (`char_notes`) (#906 / #856).
    let quarantined_repo =
        WorldRepository::open_sqlite_with_options(&db_path, true).expect("quarantine open");
    assert_eq!(
        quarantined_repo.with_snapshot(|s| s.char_notes.get(&(90_000_001, 1)).cloned()),
        Some("good note".into())
    );
    let q_rows = quarantined_repo.quarantined_rows().expect("quarantined_rows");
    assert_eq!(q_rows.len(), 2);
    assert!(q_rows
        .iter()
        .any(|r| r.source_table == "domain_rows" && r.key == "90000088"));
    assert!(q_rows
        .iter()
        .any(|r| r.source_table == "char_contact_labels" && r.key == "90000077"));
    drop(quarantined_repo);

    // Subsequent default `open_sqlite` now succeeds cleanly because corrupt rows were moved to `quarantine_rows`!
    let clean_reopen = WorldRepository::open_sqlite(&db_path).expect("clean reopen after quarantine");
    assert_eq!(clean_reopen.quarantined_rows().unwrap().len(), 2);
    drop(clean_reopen);

    // 4. Container-level `#[serde(default)]` (#906 / #856): partial JSON `{}` deserializes cleanly
    //    into persisted domain records without failing when new fields are added.
    let _ch: CharacterRecord = serde_json::from_str("{}").expect("CharacterRecord serde(default)");
    let _it: InventoryItemRecord =
        serde_json::from_str("{}").expect("InventoryItemRecord serde(default)");
    let _ct: ContractRecord = serde_json::from_str("{}").expect("ContractRecord serde(default)");
    let _cp: CorporationRecord =
        serde_json::from_str("{}").expect("CorporationRecord serde(default)");
    let _sp6: crate::Sp6PersistedState =
        serde_json::from_str("{}").expect("Sp6PersistedState serde(default)");

    // 5. CLI flag parser helper (#906)
    assert!(crate::parse_cli_quarantine_unreadable(&[
        "eve-server",
        "--quarantine-unreadable"
    ]));
    assert!(!crate::parse_cli_quarantine_unreadable(&["eve-server"]));

    // 6. Startup migration flush error propagation (#878 / #906): if the startup migration flush
    //    in `load_snapshot_from_sqlite` fails, the error is propagated via `?` instead of ignored.
    let mig_conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::sqlite_store::apply_migrations(&mig_conn).unwrap();
    mig_conn
        .execute_batch(
            "CREATE TRIGGER fail_startup_flush BEFORE INSERT ON domain_rows BEGIN SELECT RAISE(ABORT, 'startup_flush_abort'); END;",
        )
        .unwrap();
    let flush_err = crate::sqlite_store::load_snapshot_from_sqlite(&mig_conn)
        .expect_err("startup migration flush failure must propagate (#878)");
    assert!(flush_err.contains("startup_flush_abort"));

    let _ = std::fs::remove_file(&db_path);
}

#[test]
fn issue_874_tx_error_hooks_poison_short_circuit_and_next_entity_id_monotonicity() {
    thread_local! {
        static RECORDED_TX_ERRORS: std::cell::RefCell<Vec<String>> =
            const { std::cell::RefCell::new(Vec::new()) };
        static POISON_MSG: std::cell::RefCell<Option<String>> =
            const { std::cell::RefCell::new(None) };
    }
    fn test_record_hook(err: &TxError) {
        RECORDED_TX_ERRORS.with(|v| v.borrow_mut().push(err.to_string()));
    }
    fn test_poison_hook() -> Option<String> {
        POISON_MSG.with(|m| m.borrow().clone())
    }
    struct HookResetGuard;
    impl Drop for HookResetGuard {
        fn drop(&mut self) {
            crate::set_tx_error_hook(None);
            crate::set_tx_poison_check_hook(None);
        }
    }

    let _guard = HookResetGuard;
    crate::set_tx_error_hook(Some(test_record_hook));
    crate::set_tx_poison_check_hook(Some(test_poison_hook));
    RECORDED_TX_ERRORS.with(|v| v.borrow_mut().clear());
    POISON_MSG.with(|m| *m.borrow_mut() = None);

    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("evomu_issue_874_hooks_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");

    // 1. User business errors inside `try_with_mut_snapshot` must NOT trigger `record_tx_error`.
    let user_err: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.char_notes.insert((90_000_001, 2), "rolled back user err".into());
        Err("business validation rejected".to_string())
    });
    assert_eq!(user_err.unwrap_err(), "business validation rejected");
    assert!(
        RECORDED_TX_ERRORS.with(|v| v.borrow().is_empty()),
        "user closure errors must not record a TxError poison (#874)"
    );
    assert!(!repo.with_snapshot(|s| s.char_notes.contains_key(&(90_000_001, 2))));

    // 2. Persist failure inside `try_with_mut_snapshot` triggers `record_tx_error` and never reuses allocated entity IDs.
    let pre_fail_id = repo.with_snapshot(|s| s.next_entity_id);
    repo.with_sqlite_conn(|conn| {
        conn.execute_batch(
            "CREATE TRIGGER abort_notes_ins BEFORE INSERT ON domain_rows BEGIN SELECT RAISE(ABORT, 'abort_note_persist'); END;",
        )
        .unwrap();
    });
    let mut allocated_during_fail = 0i64;
    let persist_err: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        allocated_during_fail = snap.next_entity_id;
        snap.char_notes.insert((90_000_001, 3), "must rollback".into());
        Ok(())
    });
    assert!(persist_err.is_err());
    assert_eq!(
        RECORDED_TX_ERRORS.with(|v| v.borrow().len()),
        1,
        "persist failure in try_with_mut_snapshot must notify tx error hook (#874)"
    );
    assert!(!repo.with_snapshot(|s| s.char_notes.contains_key(&(90_000_001, 3))));
    let post_fail_id = repo.entity_ids.current();
    assert!(
        post_fail_id >= allocated_during_fail && post_fail_id > pre_fail_id,
        "entity ID allocator must never reuse IDs from rolled-back try_with_mut_snapshot (pre={pre_fail_id}, alloc={allocated_during_fail}, post={post_fail_id})"
    );

    // 3. When poison hook returns `Some(...)`, subsequent writes abort before running their closure.
    repo.with_sqlite_conn(|conn| {
        conn.execute_batch("DROP TRIGGER abort_notes_ins;").unwrap();
    });
    POISON_MSG.with(|m| *m.borrow_mut() = Some("abort_note_persist".into()));
    let mut closure_ran = false;
    let poisoned_res: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        closure_ran = true;
        snap.char_notes.insert((90_000_001, 4), "must not run".into());
        Ok(())
    });
    assert!(!closure_ran, "poisoned tx must short-circuit before running closure");
    assert!(poisoned_res.unwrap_err().contains("abort_note_persist"));
    assert!(!repo.with_snapshot(|s| s.char_notes.contains_key(&(90_000_001, 4))));

    drop(repo);
    let _ = std::fs::remove_file(&db_path);
}

#[test]
fn issue_857_rollback_is_failsafe_across_all_abort_modes_and_partial_loads() {
    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("evomu_issue_857_rollback_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");

    let cid = 90_000_001_i64;
    let base_item_id = 200_500_001_i64;
    repo.with_mut_snapshot(|snap| {
        snap.items.insert(
            base_item_id,
            InventoryItemRecord {
                item_id: base_item_id,
                type_id: 34,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                ..Default::default()
            },
        );
        snap.owner_notes.insert(
            1,
            OwnerNoteRecord {
                note_id: 1,
                character_id: cid,
                item_id: base_item_id,
                label: "Base".into(),
                note: "KeepMe".into(),
                timestamp_unix: 1_700_000_000,
            },
        );
        snap.char_notes.insert((cid, 1), "BaseCharNote".into());
    });

    let before_published = repo.read_state();

    // 1. Validation error (NaN float) after allocating an entity ID must keep `published` pointer-identical.
    let val_err: Result<(), crate::TxError> = repo.tx("nan_validation_abort", |wtx| {
        let burned = wtx.alloc_entity_id()?;
        wtx.snap.items.insert(
            burned,
            InventoryItemRecord {
                item_id: burned,
                type_id: 34,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                insured_payout_isk: f64::NAN,
                ..Default::default()
            },
        );
        wtx.snap.owner_notes.remove(&1);
        Ok(())
    });
    assert!(matches!(val_err, Err(crate::TxError::NonFinite { .. })));
    assert!(
        std::sync::Arc::ptr_eq(&repo.read_state(), &before_published),
        "validation-rejected tx must leave published snapshot pointer-identical (#857)"
    );

    // 2. Closure panic, user Err, FailNthStatement, and FailAtCommit must all leave `published` untouched.
    let panic_res: Result<(), crate::TxError> = repo.tx("panic_abort", |wtx| {
        let _ = wtx.alloc_entity_id()?;
        wtx.snap.items.clear();
        panic!("simulated panic in tx");
    });
    assert!(matches!(panic_res, Err(crate::TxError::Panicked { .. })));
    assert!(std::sync::Arc::ptr_eq(&repo.read_state(), &before_published));

    repo.set_fault_plan_for_test(crate::TxFaultPlan::FailAtCommit);
    let commit_err: Result<(), crate::TxError> = repo.tx("commit_fault_abort", |wtx| {
        let _ = wtx.alloc_entity_id()?;
        wtx.snap.items.get_mut(&base_item_id).unwrap().quantity = 999;
        wtx.snap.char_notes.insert((cid, 2), "AbortedNote".into());
        Ok(())
    });
    assert!(commit_err.is_err());
    assert!(std::sync::Arc::ptr_eq(&repo.read_state(), &before_published));

    // Next committed write must NOT delete baseline rows from SQLite.
    repo.with_mut_snapshot(|snap| {
        snap.char_notes.insert((cid, 3), "CommittedAfterRollback".into());
    });
    let reloaded = repo.reload_snapshot_from_sqlite().expect("reload_snapshot_from_sqlite");
    assert_eq!(reloaded.items.get(&base_item_id).map(|i| i.quantity), Some(100));
    assert_eq!(reloaded.owner_notes.get(&1).map(|n| n.note.as_str()), Some("KeepMe"));
    assert_eq!(reloaded.char_notes.get(&(cid, 1)).map(String::as_str), Some("BaseCharNote"));
    assert_eq!(
        reloaded.char_notes.get(&(cid, 3)).map(String::as_str),
        Some("CommittedAfterRollback")
    );

    drop(repo);
    let _ = std::fs::remove_file(&db_path);
}

#[test]
fn issue_826_with_mut_snapshot_cowmap_and_domain_rows_zero_cold_clones_and_zero_fingerprinting() {
    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("evomu_issue_826_zero_fp_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");

    let cid = 90_000_001_i64;
    let item_id = 200_600_001_i64;
    let order_id = 300_600_001_i64;

    // Seed initial rows across all 5 hot CowMaps, all 6 domain_rows CowMaps, and a cold table.
    repo.with_mut_snapshot(|snap| {
        snap.characters.insert(
            cid,
            CharacterRecord {
                character_id: cid,
                user_id: 1,
                name: "Pilot826".into(),
                balance: IskCents(500_000),
                version: 1,
                ..Default::default()
            },
        );
        let mut sk = HashMap::new();
        sk.insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 3,
                skill_points: 8_000.0,
            },
        );
        snap.skills.insert(cid, sk);
        snap.skill_queues.insert(
            cid,
            vec![SkillQueueEntry {
                type_id: 3300,
                target_level: 4,
                start_sp: 8_000.0,
                end_sp: 45_255.0,
                duration_seconds: 1_200.0,
                started_at_unix: Some(1_700_000_000),
            }],
        );
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 34,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                ..Default::default()
            },
        );
        snap.market_orders.insert(
            order_id,
            MarketOrderRecord {
                order_id,
                owner_id: cid,
                station_id: 60_003_760,
                region_id: 10_000_002,
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
        snap.wallet_journal_meta
            .insert(1, crate::ledger::JournalMeta::new(10, cid, cid, 1_700_000_000));
        snap.market_transactions.insert(
            1,
            crate::ledger::MarketTransactionRecord {
                transaction_id: 1,
                date_unix: 1_700_000_000,
                type_id: 34,
                quantity: 10,
                price: IskCents(600),
                location_id: 60_003_760,
                buyer_id: cid,
                seller_id: cid,
                buyer_account_key: 1000,
                seller_account_key: 1000,
                journal_ref_id: 1,
            },
        );
        snap.account_plex_vault.insert(1, 100);
        snap.owner_notes.insert(
            1,
            OwnerNoteRecord {
                note_id: 1,
                character_id: cid,
                item_id,
                label: "L1".into(),
                note: "N1".into(),
                timestamp_unix: 1_700_000_000,
            },
        );
        snap.corp_lp_balances.insert((98_000_001, 1_000_035), 250);
        snap.char_lp_logs.insert(
            cid,
            vec![crate::snapshot_ext::LpRewardLogEntry {
                log_id: 1,
                corp_id: 1_000_035,
                amount: 250,
                reason: "mission".into(),
                timestamp_unix: 1_700_000_000,
            }],
        );
        snap.char_notes.insert((cid, 1), "SeededColdNote".into());
    });

    super::reset_thread_cold_domain_syncs_for_test();

    // Mutate all 5 hot CowMap domains and all 6 domain_rows CowMap domains via `with_mut_snapshot`.
    repo.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().balance = IskCents(750_000);
        let sk_map = snap.skills.get_mut(&cid).unwrap();
        sk_map.get_mut(&3300).unwrap().level = 4;
        sk_map.insert(
            3301,
            TrainedSkillRecord {
                type_id: 3301,
                level: 2,
                skill_points: 1_415.0,
            },
        );
        snap.skill_queues.get_mut(&cid).unwrap()[0].target_level = 5;
        snap.items.get_mut(&item_id).unwrap().quantity = 75;
        snap.market_orders.get_mut(&order_id).unwrap().vol_remaining = 60;
        snap.wallet_journal_meta.insert(
            2,
            crate::ledger::JournalMeta::new(10, cid, cid, 1_700_000_010),
        );
        snap.market_transactions.get_mut(&1).unwrap().quantity = 25;
        snap.account_plex_vault.insert(1, 250);
        snap.owner_notes.get_mut(&1).unwrap().note = "N1-updated".into();
        snap.corp_lp_balances.insert((98_000_001, 1_000_035), 500);
        snap.char_lp_logs.get_mut(&cid).unwrap()[0].amount = 500;
        snap.char_notes.insert((cid, 1), "UpdatedSocialNote".into());
    });

    assert_eq!(
        super::thread_cold_domain_syncs_for_test(),
        0,
        "CowMap and domain_rows mutations must trigger zero cold domain syncs (#826)"
    );

    let reloaded = repo.reload_snapshot_from_sqlite().expect("reload_snapshot_from_sqlite");
    assert_eq!(reloaded.characters[&cid].balance, IskCents(750_000));
    assert_eq!(reloaded.skills[&cid][&3300].level, 4);
    assert_eq!(reloaded.skills[&cid][&3301].level, 2);
    assert_eq!(reloaded.skill_queues[&cid][0].target_level, 5);
    assert_eq!(reloaded.items[&item_id].quantity, 75);
    assert_eq!(reloaded.market_orders[&order_id].vol_remaining, 60);
    assert!(reloaded.wallet_journal_meta.contains_key(&2));
    assert_eq!(reloaded.market_transactions[&1].quantity, 25);
    assert_eq!(reloaded.account_plex_vault.get(&1), Some(&250));
    assert_eq!(reloaded.owner_notes[&1].note, "N1-updated");
    assert_eq!(reloaded.corp_lp_balances.get(&(98_000_001, 1_000_035)), Some(&500));
    assert_eq!(reloaded.char_lp_logs[&cid][0].amount, 500);
    assert_eq!(reloaded.char_notes.get(&(cid, 1)), Some(&"UpdatedSocialNote".into()));

    // Mutating a cold domain syncs cold domains exactly once.
    repo.with_mut_snapshot(|snap| {
        snap.character_last_jump_unix.insert(cid, 1_700_000_050);
    });
    assert_eq!(super::thread_cold_domain_syncs_for_test(), 1);

    drop(repo);
    let _ = std::fs::remove_file(&db_path);
}

#[test]
fn issue_1181_is_entity_id_referenced_exact_lookup_and_collision_skip() {
    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("evomu_issue_1181_entity_ref_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");

    let cid = 90_000_010_i64;
    let mail_id_old = 100_000_501_i64;
    let mail_id_new = 100_000_502_i64;
    let wj_id_old = 100_000_601_i64;
    let wj_id_new = 100_000_602_i64;
    let lp_id_old = 100_000_701_i64;
    let lp_id_new = 100_000_702_i64;
    let npc_id_old = 100_000_801_i64;
    let npc_id_new = 100_000_802_i64;
    let ekv_id = 100_000_901_i64;

    repo.tx("seed_all_five_stores", |wtx| -> Result<(), TxError> {
        wtx.snap.characters.insert(cid, sample_character(cid, 10_000));
        wtx.snap.mails.push(MailRecord {
            message_id: mail_id_old,
            sender_id: cid,
            recipient_id: cid,
            subject: "S".into(),
            body: "B".into(),
        });
        wtx.snap.wallet_journal.push(WalletJournalEntry {
            entry_id: wj_id_old,
            character_id: cid,
            entry_type: "Bounty".into(),
            amount: IskCents(100),
            balance_after: IskCents(100),
            reason: "Test".into(),
        });
        wtx.snap.char_lp_logs.insert(
            cid,
            vec![crate::snapshot_ext::LpRewardLogEntry {
                log_id: lp_id_old,
                corp_id: 1_000_035,
                amount: 50,
                reason: "Mission".into(),
                timestamp_unix: 1_700_000_000,
            }],
        );
        wtx.snap.active_missions.insert(
            (cid, 1),
            ActiveMissionRecord {
                mission_id: 10,
                character_id: cid,
                agent_id: 1,
                solar_system_id: 30_000_142,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 1,
                spawned_npc_ball_ids: vec![npc_id_old],
                bookmark_id: None,
            },
        );
        wtx.snap.entity_kv.insert(
            crate::service_domains::entity_kv::EntityKvKey {
                ns: crate::service_domains::entity_kv::Ns::ContractDeposit,
                entity_id: ekv_id,
                sub_key: "state".into(),
            },
            crate::service_domains::entity_kv::EntityKvRow {
                value_json: "\"ok\"".into(),
                updated_unix: 1_700_000_000,
            },
        );
        Ok(())
    })
    .expect("seed_all_five_stores");

    repo.with_snapshot(|snap| {
        assert!(snap.is_entity_id_referenced(mail_id_old));
        assert!(snap.is_entity_id_referenced(wj_id_old));
        assert!(snap.is_entity_id_referenced(lp_id_old));
        assert!(snap.is_entity_id_referenced(npc_id_old));
        assert!(snap.is_entity_id_referenced(ekv_id));
        assert!(!snap.is_entity_id_referenced(mail_id_new));
        assert!(!snap.is_entity_id_referenced(wj_id_new));
        assert!(!snap.is_entity_id_referenced(lp_id_new));
        assert!(!snap.is_entity_id_referenced(npc_id_new));
        assert!(!snap.is_entity_id_referenced(ekv_id + 1));
    });

    // Mutate via IndexMut / get_mut / remove and verify index reconciliation works both
    // mid-transaction and after commit, and verify alloc_entity_id skips colliding IDs.
    repo.tx("mutate_and_alloc", |wtx| -> Result<(), TxError> {
        wtx.snap.mails[0].message_id = mail_id_new;
        wtx.snap.wallet_journal[0].entry_id = wj_id_new;
        wtx.snap.char_lp_logs.get_mut(&cid).unwrap()[0].log_id = lp_id_new;
        wtx.snap.active_missions.get_mut(&(cid, 1)).unwrap().spawned_npc_ball_ids = vec![npc_id_new];
        wtx.snap.entity_kv.remove(&crate::service_domains::entity_kv::EntityKvKey {
            ns: crate::service_domains::entity_kv::Ns::ContractDeposit,
            entity_id: ekv_id,
            sub_key: "state".into(),
        });

        assert!(!wtx.snap.is_entity_id_referenced(mail_id_old));
        assert!(wtx.snap.is_entity_id_referenced(mail_id_new));
        assert!(!wtx.snap.is_entity_id_referenced(wj_id_old));
        assert!(wtx.snap.is_entity_id_referenced(wj_id_new));
        assert!(!wtx.snap.is_entity_id_referenced(lp_id_old));
        assert!(wtx.snap.is_entity_id_referenced(lp_id_new));
        assert!(!wtx.snap.is_entity_id_referenced(npc_id_old));
        assert!(wtx.snap.is_entity_id_referenced(npc_id_new));
        assert!(!wtx.snap.is_entity_id_referenced(ekv_id));

        // Seed next candidate collision in mails, verify alloc_entity_id skips over it.
        let peek_next = wtx.snap.next_entity_id + 1;
        wtx.snap.mails.push(MailRecord {
            message_id: peek_next,
            sender_id: cid,
            recipient_id: cid,
            subject: "Collide".into(),
            body: "Collide".into(),
        });
        let allocated = wtx.alloc_entity_id()?;
        assert!(
            allocated > peek_next,
            "alloc_entity_id ({allocated}) must skip colliding mail message_id ({peek_next})"
        );
        Ok(())
    })
    .expect("mutate_and_alloc");

    repo.with_snapshot(|snap| {
        assert!(!snap.is_entity_id_referenced(mail_id_old));
        assert!(snap.is_entity_id_referenced(mail_id_new));
        assert!(!snap.is_entity_id_referenced(wj_id_old));
        assert!(snap.is_entity_id_referenced(wj_id_new));
        assert!(!snap.is_entity_id_referenced(lp_id_old));
        assert!(snap.is_entity_id_referenced(lp_id_new));
        assert!(!snap.is_entity_id_referenced(npc_id_old));
        assert!(snap.is_entity_id_referenced(npc_id_new));
        assert!(!snap.is_entity_id_referenced(ekv_id));
    });

    drop(repo);
    let _ = std::fs::remove_file(&db_path);
}

#[test]
fn issue_1181_cow_id_vec_wal_delta_and_sqlite_sync_append_truncate_rewrite() {
    let dir = std::env::temp_dir();
    let db_path = dir.join(format!("evomu_issue_1181_wal_sync_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
    let cid = 90_000_020_i64;

    let snap_base = repo.with_snapshot(|s| s.clone());

    // 1a. AppendOnly wallet_journal -> incremental WAL delta (`full_snapshot.is_none()`)
    repo.tx("append_wallet_journal", |wtx| -> Result<(), TxError> {
        for i in 1..=3_i64 {
            wtx.snap.wallet_journal.push(WalletJournalEntry {
                entry_id: 200_000 + i,
                character_id: cid,
                entry_type: "Bounty".into(),
                amount: IskCents(100 * i),
                balance_after: IskCents(100 * i),
                reason: format!("r{i}"),
            });
        }
        Ok(())
    })
    .expect("append_wallet_journal");

    let snap_wj = repo.with_snapshot(|s| s.clone());
    assert_eq!(
        snap_base.wallet_journal.classify_diff(&snap_wj.wallet_journal),
        crate::CowVecDiff::AppendOnly { old_len: 0 }
    );
    let delta_wj_append = WorldStateWalDelta::from_diff(&snap_base, &snap_wj);
    assert_eq!(delta_wj_append.appended_wallet_journal.len(), 3);
    assert!(delta_wj_append.full_snapshot.is_none());

    let mut target_wj = snap_base.clone();
    delta_wj_append.apply_to(&mut target_wj);
    assert_eq!(target_wj.wallet_journal, snap_wj.wallet_journal);

    // 1b. AppendOnly mails -> `CowVecDiff::AppendOnly` + SQLite incremental append
    repo.tx("append_mails", |wtx| -> Result<(), TxError> {
        for i in 1..=3_i64 {
            wtx.snap.mails.push(MailRecord {
                message_id: 300_000 + i,
                sender_id: cid,
                recipient_id: cid,
                subject: format!("s{i}"),
                body: format!("b{i}"),
            });
        }
        Ok(())
    })
    .expect("append_mails");

    let snap_1 = repo.with_snapshot(|s| s.clone());
    assert_eq!(
        snap_wj.mails.classify_diff(&snap_1.mails),
        crate::CowVecDiff::AppendOnly { old_len: 0 }
    );

    let reloaded_1 = repo.reload_snapshot_from_sqlite().expect("reload_1");
    assert_eq!(reloaded_1.wallet_journal, snap_1.wallet_journal);
    assert_eq!(reloaded_1.mails, snap_1.mails);

    // 2. Rewritten (IndexMut edit at index 0 keeping length equal)
    repo.tx("rewrite_prefix", |wtx| -> Result<(), TxError> {
        wtx.snap.wallet_journal[0].reason = "r1-edited".into();
        wtx.snap.mails[0].body = "b1-edited".into();
        Ok(())
    })
    .expect("rewrite_prefix");

    let snap_2 = repo.with_snapshot(|s| s.clone());
    assert_eq!(
        snap_1.wallet_journal.classify_diff(&snap_2.wallet_journal),
        crate::CowVecDiff::Rewritten
    );
    assert_eq!(
        snap_1.mails.classify_diff(&snap_2.mails),
        crate::CowVecDiff::Rewritten
    );
    let delta_rewrite = WorldStateWalDelta::from_diff(&snap_1, &snap_2);
    assert!(delta_rewrite.full_snapshot.is_some());

    let mut target_2 = snap_1.clone();
    delta_rewrite.apply_to(&mut target_2);
    assert_eq!(target_2.wallet_journal, snap_2.wallet_journal);
    assert_eq!(target_2.mails, snap_2.mails);

    let reloaded_2 = repo.reload_snapshot_from_sqlite().expect("reload_2");
    assert_eq!(reloaded_2.wallet_journal[0].reason, "r1-edited");
    assert_eq!(reloaded_2.mails[0].body, "b1-edited");

    // 3. Rewritten via truncate
    repo.tx("truncate_tail", |wtx| -> Result<(), TxError> {
        wtx.snap.wallet_journal.truncate(1);
        wtx.snap.mails.truncate(2);
        Ok(())
    })
    .expect("truncate_tail");

    let snap_3 = repo.with_snapshot(|s| s.clone());
    assert_eq!(
        snap_2.wallet_journal.classify_diff(&snap_3.wallet_journal),
        crate::CowVecDiff::Rewritten
    );
    assert_eq!(
        snap_2.mails.classify_diff(&snap_3.mails),
        crate::CowVecDiff::Rewritten
    );
    let delta_trunc = WorldStateWalDelta::from_diff(&snap_2, &snap_3);
    assert!(delta_trunc.full_snapshot.is_some());

    let reloaded_3 = repo.reload_snapshot_from_sqlite().expect("reload_3");
    assert_eq!(reloaded_3.wallet_journal.len(), 1);
    assert_eq!(reloaded_3.mails.len(), 2);
    assert!(!reloaded_3.is_entity_id_referenced(200_002));
    assert!(!reloaded_3.is_entity_id_referenced(300_003));

    drop(repo);
    let _ = std::fs::remove_file(&db_path);
}

#[test]
fn issue_1181_1m_wallet_journal_and_mails_tx_commit_and_alloc_entity_id_are_ologn_and_zero_clone() {
    let n_cold = 1_000_000usize;
    let cid = 90_000_030_i64;

    let mut conn = rusqlite::Connection::open_in_memory().expect("open_in_memory");
    crate::sqlite_store::apply_migrations(&conn).expect("migrations");

    let mut snap = WorldStateSnapshot::default();
    let wj_vec: Vec<WalletJournalEntry> = (0..n_cold)
        .map(|i| WalletJournalEntry {
            entry_id: 500_000_000 + i as i64,
            character_id: cid,
            entry_type: "Bounty".into(),
            amount: IskCents(100),
            balance_after: IskCents(100),
            reason: "j".into(),
        })
        .collect();
    let mail_vec: Vec<MailRecord> = (0..n_cold)
        .map(|i| MailRecord {
            message_id: 600_000_000 + i as i64,
            sender_id: cid,
            recipient_id: cid,
            subject: "s".into(),
            body: "b".into(),
        })
        .collect();
    snap.wallet_journal = CowIdVec::from(wj_vec);
    snap.mails = CowIdVec::from(mail_vec);
    snap.wallet_journal_timestamps = (0..n_cold)
        .map(|i| (500_000_000 + i as i64, 1_700_000_000_u64))
        .collect();
    snap.wallet_journal_meta = (0..n_cold)
        .map(|i| {
            (
                500_000_000 + i as i64,
                crate::ledger::JournalMeta::new(10, cid, cid, 1_700_000_000),
            )
        })
        .collect();

    // Also populate CowMap, CowCell, CowLpLogs, CowActiveMissions, and cold stores.
    let item_id = 200_900_001_i64;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id: 34,
            owner_id: cid,
            location_id: 60_003_760,
            flag: InventoryFlag::Hangar,
            quantity: 10,
            ..Default::default()
        },
    );
    snap.char_lp_logs.insert(
        cid,
        vec![crate::snapshot_ext::LpRewardLogEntry {
            log_id: 700_000_001,
            corp_id: 1_000_035,
            amount: 100,
            reason: "m".into(),
            timestamp_unix: 1_700_000_000,
        }],
    );
    snap.active_missions.insert(
        (cid, 1),
        ActiveMissionRecord {
            mission_id: 1,
            character_id: cid,
            agent_id: 1,
            solar_system_id: 30_000_142,
            state: "Accepted".into(),
            current_room_id: 1,
            npcs_remaining: 1,
            spawned_npc_ball_ids: vec![800_000_001],
            bookmark_id: None,
        },
    );
    snap.entity_kv.insert(
        crate::service_domains::entity_kv::EntityKvKey {
            ns: crate::service_domains::entity_kv::Ns::ContractDeposit,
            entity_id: 850_000_001,
            sub_key: "k".into(),
        },
        crate::service_domains::entity_kv::EntityKvRow {
            value_json: "\"v\"".into(),
            updated_unix: 1_700_000_000,
        },
    );

    {
        let tx = conn.transaction().expect("seed item + 1M sql tx");
        let item = snap.items.get(&item_id).unwrap();
        tx.execute(
            "INSERT INTO items (item_id, type_id, owner_id, location_id, flag, quantity, singleton, version, record_json)
             VALUES (?1, 34, ?2, 60003760, 4, 10, 0, 1, ?3)",
            rusqlite::params![item_id, cid, serde_json::to_string(item).unwrap()],
        )
        .expect("seed item");
        {
            let mut st_wj = tx
                .prepare_cached(
                    "INSERT INTO wallet_journal (seq_idx, entry_id, character_id, entry_type, amount_cents, balance_after_cents, reason)
                     VALUES (?1, ?2, ?3, 'Bounty', 100, 100, 'j')",
                )
                .expect("prepare wallet_journal");
            for (idx, e) in snap.wallet_journal.iter().enumerate() {
                st_wj
                    .execute(rusqlite::params![idx as i64, e.entry_id, cid])
                    .expect("insert wallet_journal");
            }
        }
        {
            let mut st_mail = tx
                .prepare_cached(
                    "INSERT INTO mail (seq_idx, message_id, sender_id, recipient_id, subject, body)
                     VALUES (?1, ?2, ?3, ?3, 's', 'b')",
                )
                .expect("prepare mail");
            for (idx, m) in snap.mails.iter().enumerate() {
                st_mail
                    .execute(rusqlite::params![idx as i64, m.message_id, cid])
                    .expect("insert mail");
            }
        }
        tx.commit().expect("seed 1M commit");
    }

    // Indexed SQLite entity ID checks on 2M persisted rows must stay O(log N) (< 5ms).
    let t_sqlite_exists = std::time::Instant::now();
    assert!(
        crate::sqlite_store::sqlite_entity_id_exists(&conn, 500_500_000)
            .expect("sqlite_entity_id_exists wj")
    );
    assert!(
        crate::sqlite_store::sqlite_entity_id_exists(&conn, 600_500_000)
            .expect("sqlite_entity_id_exists mail")
    );
    assert!(
        !crate::sqlite_store::sqlite_entity_id_exists(&conn, 999_999_999)
            .expect("sqlite_entity_id_exists miss")
    );
    let sqlite_exists_dur = t_sqlite_exists.elapsed();
    assert!(
        sqlite_exists_dur < std::time::Duration::from_millis(5),
        "indexed sqlite_entity_id_exists across 2M rows took {sqlite_exists_dur:?} (expected < 5ms)"
    );

    let jwt_secret =
        crate::sqlite_store::load_or_init_jwt_secret(&conn, WorldRepository::generate_random_secret_32)
            .expect("jwt");
    let repo = WorldRepository::from_parts(
        snap,
        conn,
        900_000_000,
        None,
        crate::types::AuthRuntimeState::new(jwt_secret),
    );

    let before_snap = repo.with_snapshot(|s| s.clone());
    let cloned_snap = before_snap.clone();
    assert!(before_snap.wallet_journal.ptr_eq(&cloned_snap.wallet_journal));
    assert!(before_snap
        .wallet_journal_timestamps
        .ptr_eq(&cloned_snap.wallet_journal_timestamps));
    assert!(before_snap
        .wallet_journal_meta
        .ptr_eq(&cloned_snap.wallet_journal_meta));
    assert!(before_snap.mails.ptr_eq(&cloned_snap.mails));
    assert!(before_snap.char_lp_logs.ptr_eq(&cloned_snap.char_lp_logs));
    assert!(before_snap.active_missions.ptr_eq(&cloned_snap.active_missions));
    assert!(before_snap.structures.ptr_eq(&cloned_snap.structures));
    assert!(before_snap.contracts.ptr_eq(&cloned_snap.contracts));
    assert!(before_snap.system_states.ptr_eq(&cloned_snap.system_states));
    assert!(before_snap.corp_governance.ptr_eq(&cloned_snap.corp_governance));
    assert!(before_snap.fleet_and_social_ops.ptr_eq(&cloned_snap.fleet_and_social_ops));
    assert!(before_snap.committed_idempotency_keys.ptr_eq(&cloned_snap.committed_idempotency_keys));
    assert!(before_snap.entity_kv.ptr_eq(&cloned_snap.entity_kv));
    assert!(before_snap.sp6_lifecycle.ptr_eq(&cloned_snap.sp6_lifecycle));
    assert!(before_snap.pve_special_sites.ptr_eq(&cloned_snap.pve_special_sites));
    assert!(before_snap.abyssal_state.ptr_eq(&cloned_snap.abyssal_state));
    assert!(before_snap.dungeon_runtime.ptr_eq(&cloned_snap.dungeon_runtime));
    assert!(before_snap.dungeon_editor.ptr_eq(&cloned_snap.dungeon_editor));
    assert!(before_snap.module_online_states.ptr_eq(&cloned_snap.module_online_states));
    assert!(before_snap.module_active_states.ptr_eq(&cloned_snap.module_active_states));
    assert!(before_snap.module_active_charges.ptr_eq(&cloned_snap.module_active_charges));
    assert!(before_snap.paused_skill_queues.ptr_eq(&cloned_snap.paused_skill_queues));

    crate::cow::reset_thread_cow_full_touch_for_test();
    super::reset_thread_cold_domain_syncs_for_test();

    // Commit 5 transactions: each mutates 1 item and allocates 100 entity IDs.
    let t0 = std::time::Instant::now();
    for round in 0..5_i32 {
        repo.tx("hot_1_item_plus_100_allocs", |wtx| -> Result<(), TxError> {
            wtx.snap.items.get_mut(&item_id).unwrap().quantity += 1;
            let mut prev = 0_i64;
            for _ in 0..100 {
                let id = wtx.alloc_entity_id()?;
                assert!(id > prev);
                prev = id;
            }
            let _ = round;
            Ok(())
        })
        .expect("hot_1_item_plus_100_allocs");
    }
    let avg_tx_dur = t0.elapsed() / 5;
    assert!(
        avg_tx_dur < std::time::Duration::from_millis(50),
        "1M wallet_journal + 1M mails single-item tx + 100 allocs took {avg_tx_dur:?} (expected << 50ms)"
    );
    assert_eq!(
        crate::cow::thread_cow_full_touch_for_test(),
        0,
        "1M-cold tx commit must perform 0 full-table touches"
    );
    assert_eq!(
        super::thread_cold_domain_syncs_for_test(),
        0,
        "hot item tx with 1M wallet_journal/mails must perform 0 cold domain syncs"
    );

    let after_hot_snap = repo.with_snapshot(|s| s.clone());
    assert!(before_snap.wallet_journal.ptr_eq(&after_hot_snap.wallet_journal));
    assert!(before_snap
        .wallet_journal_timestamps
        .ptr_eq(&after_hot_snap.wallet_journal_timestamps));
    assert!(before_snap
        .wallet_journal_meta
        .ptr_eq(&after_hot_snap.wallet_journal_meta));
    assert!(before_snap.mails.ptr_eq(&after_hot_snap.mails));
    assert!(before_snap.char_lp_logs.ptr_eq(&after_hot_snap.char_lp_logs));
    assert!(before_snap.active_missions.ptr_eq(&after_hot_snap.active_missions));
    assert!(before_snap.contracts.ptr_eq(&after_hot_snap.contracts));
    assert!(before_snap.corp_governance.ptr_eq(&after_hot_snap.corp_governance));
    assert!(before_snap.fleet_and_social_ops.ptr_eq(&after_hot_snap.fleet_and_social_ops));
    assert!(before_snap.entity_kv.ptr_eq(&after_hot_snap.entity_kv));
    assert!(!before_snap.items.ptr_eq(&after_hot_snap.items));

    // Appending 1 journal entry via `WorldStateSnapshot::append_journal_entry` (updating
    // `wallet_journal`, `wallet_journal_meta`, and `wallet_journal_timestamps`) + 1 mail entry on
    // 1M SQLite-backed sequences/maps is O(log N), performs 0 cold domain syncs, and 0 full-table
    // touches (`CowMap::diff` for `wallet_journal_timestamps` / `wallet_journal_meta`).
    crate::cow::reset_thread_cow_full_touch_for_test();
    super::reset_thread_cold_domain_syncs_for_test();

    let mut appended_journal_id = 0_i64;
    let t_append = std::time::Instant::now();
    repo.tx("append_1_journal_and_1_mail", |wtx| -> Result<(), TxError> {
        let m_id = wtx.alloc_entity_id()?;
        let j_id = wtx.snap.append_journal_entry(crate::ledger::JournalEntryInput {
            owner_id: cid,
            entry_type: "Bounty",
            amount: IskCents(500),
            balance_after: IskCents(600),
            reason: "append-fast".into(),
            meta: crate::ledger::JournalMeta::new(10, cid, cid, 1_700_000_123),
        });
        assert_ne!(j_id, m_id);
        appended_journal_id = j_id;
        wtx.snap.mails.push(MailRecord {
            message_id: m_id,
            sender_id: cid,
            recipient_id: cid,
            subject: "Fast".into(),
            body: "Fast".into(),
        });
        Ok(())
    })
    .expect("append_1_journal_and_1_mail");
    let append_dur = t_append.elapsed();
    assert!(
        append_dur < std::time::Duration::from_millis(50),
        "appending 1 journal + 1 mail to 1M sequences/maps took {append_dur:?} (expected << 50ms)"
    );
    assert_eq!(
        super::thread_cold_domain_syncs_for_test(),
        0,
        "append_1_journal_and_1_mail must perform 0 cold domain syncs"
    );
    assert_eq!(
        crate::cow::thread_cow_full_touch_for_test(),
        0,
        "append_journal_entry must diff wallet_journal_timestamps/meta with 0 full-table touches"
    );
    let read_after = repo.read_state();
    assert_eq!(read_after.wallet_journal.len(), n_cold + 1);
    assert_eq!(read_after.wallet_journal_timestamps.len(), n_cold + 1);
    assert_eq!(read_after.wallet_journal_meta.len(), n_cold + 1);
    assert_eq!(
        read_after.wallet_journal_timestamps.get(&appended_journal_id),
        Some(&1_700_000_123)
    );
    assert_eq!(read_after.mails.len(), n_cold + 1);
    let (sql_wj_cnt, sql_mail_cnt, ts_val, meta_exists): (i64, i64, String, bool) = repo
        .with_sqlite_conn(|c| {
            let wj: i64 = c
                .query_row("SELECT COUNT(*) FROM wallet_journal", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            let m: i64 = c
                .query_row("SELECT COUNT(*) FROM mail", [], |r| r.get(0))
                .map_err(|e| e.to_string())?;
            let ts: String = c
                .query_row(
                    "SELECT value FROM domain_rows WHERE domain = ?1 AND key = ?2",
                    rusqlite::params![
                        crate::sqlite_store::domain_rows::DOMAIN_WALLET_AND_MARKET,
                        format!("wallet_journal_timestamps/{appended_journal_id}"),
                    ],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())?;
            let meta_row: Option<String> = c
                .query_row(
                    "SELECT value FROM domain_rows WHERE domain = ?1 AND key = ?2",
                    rusqlite::params![
                        crate::sqlite_store::domain_rows::DOMAIN_WALLET_JOURNAL_META,
                        appended_journal_id.to_string(),
                    ],
                    |r| r.get(0),
                )
                .ok();
            Ok::<(i64, i64, String, bool), String>((wj, m, ts, meta_row.is_some()))
        })
        .expect("sqlite count");
    assert_eq!(sql_wj_cnt, (n_cold + 1) as i64);
    assert_eq!(sql_mail_cnt, (n_cold + 1) as i64);
    assert_eq!(ts_val, "1700000123");
    assert!(meta_exists);

    // Updating + deleting an existing entry in 1M `wallet_journal_timestamps` also runs in
    // O(k log N) with 0 full-table touches, 0 cold domain syncs, and exact `domain_rows` SQLite sync.
    crate::cow::reset_thread_cow_full_touch_for_test();
    super::reset_thread_cold_domain_syncs_for_test();
    repo.tx("update_1m_wallet_ts", |wtx| -> Result<(), TxError> {
        wtx.snap
            .wallet_journal_timestamps
            .insert(appended_journal_id, 1_700_000_999);
        Ok(())
    })
    .expect("update_1m_wallet_ts");
    assert_eq!(crate::cow::thread_cow_full_touch_for_test(), 0);
    assert_eq!(super::thread_cold_domain_syncs_for_test(), 0);
    let updated_ts: String = repo
        .with_sqlite_conn(|c| {
            c.query_row(
                "SELECT value FROM domain_rows WHERE domain = ?1 AND key = ?2",
                rusqlite::params![
                    crate::sqlite_store::domain_rows::DOMAIN_WALLET_AND_MARKET,
                    format!("wallet_journal_timestamps/{appended_journal_id}"),
                ],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())
        })
        .expect("sqlite verify ts update");
    assert_eq!(updated_ts, "1700000999");

    repo.tx("remove_1m_wallet_ts", |wtx| -> Result<(), TxError> {
        wtx.snap
            .wallet_journal_timestamps
            .remove(&appended_journal_id);
        Ok(())
    })
    .expect("remove_1m_wallet_ts");
    assert_eq!(crate::cow::thread_cow_full_touch_for_test(), 0);
    assert_eq!(super::thread_cold_domain_syncs_for_test(), 0);
    let removed_exists: bool = repo
        .with_sqlite_conn(|c| {
            let row: Option<String> = c
                .query_row(
                    "SELECT value FROM domain_rows WHERE domain = ?1 AND key = ?2",
                    rusqlite::params![
                        crate::sqlite_store::domain_rows::DOMAIN_WALLET_AND_MARKET,
                        format!("wallet_journal_timestamps/{appended_journal_id}"),
                    ],
                    |r| r.get(0),
                )
                .ok();
            Ok::<bool, String>(row.is_some())
        })
        .expect("sqlite verify ts delete");
    assert!(!removed_exists);

    let after_append_snap = repo.with_snapshot(|s| s.clone());
    assert!(after_hot_snap.contracts.ptr_eq(&after_append_snap.contracts));
    assert!(after_hot_snap.corp_governance.ptr_eq(&after_append_snap.corp_governance));
}

#[test]
fn issue_1181_incremental_cold_tables_tx_zero_clone_and_zero_full_touch() {
    let mut conn = rusqlite::Connection::open_in_memory().expect("open_in_memory");
    crate::sqlite_store::apply_migrations(&conn).expect("migrations");

    let mut snap = WorldStateSnapshot::default();
    let n_seed = 200_i64;
    for i in 1..=n_seed {
        snap.structures.insert(
            1_020_000_000_000 + i,
            crate::types::UpwellStructureRecord {
                structure_id: 1_020_000_000_000 + i,
                type_id: 35832,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: format!("Struct-{i}"),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.mail_timestamps.insert(600_000_000 + i, 1_700_000_000 + i as u64);
        snap.item_custom_names
            .insert(200_000_000 + i, format!("Ship-{i}"));
        snap.entity_kv.insert(
            crate::service_domains::entity_kv::EntityKvKey::new(
                crate::service_domains::entity_kv::Ns::ContractDeposit,
                850_000_000 + i,
                "k",
            ),
            crate::service_domains::entity_kv::EntityKvRow {
                value_json: format!("{i}"),
                updated_unix: 1_700_000_000 + i as u64,
            },
        );
    }

    crate::sqlite_store::sync::replace_sqlite_from_snapshot(&mut conn, &snap)
        .expect("initial sync");

    let jwt_secret =
        crate::sqlite_store::load_or_init_jwt_secret(&conn, WorldRepository::generate_random_secret_32)
            .expect("jwt");
    let repo = WorldRepository::from_parts(
        snap,
        conn,
        900_000_000,
        None,
        crate::types::AuthRuntimeState::new(jwt_secret),
    );

    // 1. Mutate 1 structure
    crate::cow::reset_thread_cow_full_touch_for_test();
    super::reset_thread_cold_domain_syncs_for_test();
    repo.tx("single_structure", |wtx| -> Result<(), TxError> {
        wtx.snap
            .structures
            .get_mut(&(1_020_000_000_001))
            .unwrap()
            .fuel_blocks = 999;
        Ok(())
    })
    .expect("single_structure");
    assert_eq!(super::thread_cold_domain_syncs_for_test(), 0);
    assert_eq!(crate::cow::thread_cow_full_touch_for_test(), 0);

    // 2. Mutate 1 mail_timestamp
    crate::cow::reset_thread_cow_full_touch_for_test();
    super::reset_thread_cold_domain_syncs_for_test();
    repo.tx("single_mail_timestamp", |wtx| -> Result<(), TxError> {
        wtx.snap.mail_timestamps.insert(600_000_999, 1_800_000_000);
        Ok(())
    })
    .expect("single_mail_timestamp");
    assert_eq!(super::thread_cold_domain_syncs_for_test(), 0);
    assert_eq!(crate::cow::thread_cow_full_touch_for_test(), 0);

    // 3. Mutate 1 item_custom_name
    crate::cow::reset_thread_cow_full_touch_for_test();
    super::reset_thread_cold_domain_syncs_for_test();
    repo.tx("single_item_custom_name", |wtx| -> Result<(), TxError> {
        wtx.snap
            .item_custom_names
            .insert(200_000_001, "Renamed".into());
        Ok(())
    })
    .expect("single_item_custom_name");
    assert_eq!(super::thread_cold_domain_syncs_for_test(), 0);
    assert_eq!(crate::cow::thread_cow_full_touch_for_test(), 0);

    // 4. Insert + remove 1 entity_kv row
    crate::cow::reset_thread_cow_full_touch_for_test();
    super::reset_thread_cold_domain_syncs_for_test();
    repo.tx("single_entity_kv", |wtx| -> Result<(), TxError> {
        wtx.snap.entity_kv.insert(
            crate::service_domains::entity_kv::EntityKvKey::new(
                crate::service_domains::entity_kv::Ns::ContractDeposit,
                850_000_001,
                "k",
            ),
            crate::service_domains::entity_kv::EntityKvRow {
                value_json: "9999".into(),
                updated_unix: 1_800_000_001,
            },
        );
        wtx.snap.entity_kv.remove(
            &crate::service_domains::entity_kv::EntityKvKey::new(
                crate::service_domains::entity_kv::Ns::ContractDeposit,
                850_000_002,
                "k",
            ),
        );
        Ok(())
    })
    .expect("single_entity_kv");
    assert_eq!(super::thread_cold_domain_syncs_for_test(), 0);
    assert_eq!(crate::cow::thread_cow_full_touch_for_test(), 0);

    let expected_snap = repo.read_state();
    let loaded_snap = repo
        .with_sqlite_conn(|c| crate::sqlite_store::load_snapshot_from_sqlite_with_options(c, false))
        .expect("reload sqlite");
    assert_eq!(loaded_snap.structures, expected_snap.structures);
    assert_eq!(loaded_snap.mail_timestamps, expected_snap.mail_timestamps);
    assert_eq!(loaded_snap.item_custom_names, expected_snap.item_custom_names);
    assert_eq!(loaded_snap.entity_kv, expected_snap.entity_kv);
}

#[test]
fn issue_1181_snapshot_alloc_entity_id_reconciles_dirty_lp_logs_and_active_missions() {
    let mut snap = WorldStateSnapshot::default();
    snap.next_entity_id = 200_000_000;
    let cid = 90_000_031_i64;
    snap.char_lp_logs.insert(
        cid,
        vec![crate::snapshot_ext::LpRewardLogEntry {
            log_id: 200_000_000,
            corp_id: 1_000_035,
            amount: 50,
            reason: "seed".into(),
            timestamp_unix: 1_700_000_000,
        }],
    );
    snap.active_missions.insert(
        (cid, 1),
        ActiveMissionRecord {
            mission_id: 1,
            character_id: cid,
            agent_id: 1,
            solar_system_id: 30_000_142,
            state: "Accepted".into(),
            current_room_id: 1,
            npcs_remaining: 1,
            spawned_npc_ball_ids: vec![],
            bookmark_id: None,
        },
    );

    // Dirty both indexes without explicit reconciliation.
    for (_k, vec) in snap.char_lp_logs.iter_mut() {
        vec.push(crate::snapshot_ext::LpRewardLogEntry {
            log_id: 200_000_001,
            corp_id: 1_000_035,
            amount: 75,
            reason: "dirty-iter-mut".into(),
            timestamp_unix: 1_700_000_001,
        });
    }
    for m in snap.active_missions.values_mut() {
        m.spawned_npc_ball_ids.push(200_000_002);
    }

    let id = snap.alloc_entity_id();
    assert_eq!(id, 200_000_003);
    assert_eq!(snap.next_entity_id, 200_000_003);

    // Also verify WorldTx::alloc_entity_id reconciles dirty char_lp_logs.
    let conn = rusqlite::Connection::open_in_memory().expect("mem");
    crate::sqlite_store::apply_migrations(&conn).expect("migrations");
    let jwt_secret =
        crate::sqlite_store::load_or_init_jwt_secret(&conn, WorldRepository::generate_random_secret_32)
            .expect("jwt");
    let repo = WorldRepository::from_parts(
        snap,
        conn,
        200_000_003,
        None,
        crate::types::AuthRuntimeState::new(jwt_secret),
    );
    repo.tx("tx_alloc_dirty_lp_logs", |wtx| -> Result<(), TxError> {
        for (_k, vec) in wtx.snap.char_lp_logs.iter_mut() {
            vec.push(crate::snapshot_ext::LpRewardLogEntry {
                log_id: 200_000_004,
                corp_id: 1_000_035,
                amount: 99,
                reason: "dirty-tx".into(),
                timestamp_unix: 1_700_000_002,
            });
        }
        let next_id = wtx.alloc_entity_id()?;
        assert_eq!(next_id, 200_000_005);
        Ok(())
    })
    .expect("tx_alloc_dirty_lp_logs");
}

#[test]
fn issue_1181_entity_kv_cow_map_and_wrapped_fields_serde_roundtrip() {
    let mut snap = WorldStateSnapshot::default();
    snap.entity_kv.insert(
        crate::service_domains::entity_kv::EntityKvKey::new(
            crate::service_domains::entity_kv::Ns::ContractDeposit,
            501,
            "escrow",
        ),
        crate::service_domains::entity_kv::EntityKvRow {
            value_json: "12345".into(),
            updated_unix: 1_700_000_010,
        },
    );
    snap.entity_kv.insert(
        crate::service_domains::entity_kv::EntityKvKey::new(
            crate::service_domains::entity_kv::Ns::XmppHostname,
            0,
            "",
        ),
        crate::service_domains::entity_kv::EntityKvRow {
            value_json: "\"chat.evomu.local\"".into(),
            updated_unix: 1_700_000_020,
        },
    );
    snap.paused_skill_queues.insert(90_000_001);
    snap.module_online_states.insert(200_000_010, true);
    snap.module_active_states.insert(200_000_011);
    snap.module_active_charges.insert(200_000_012, 1234);

    let cloned = snap.clone();
    assert!(snap.entity_kv.ptr_eq(&cloned.entity_kv));
    assert!(snap.paused_skill_queues.ptr_eq(&cloned.paused_skill_queues));
    assert!(snap.module_online_states.ptr_eq(&cloned.module_online_states));
    assert!(snap.module_active_states.ptr_eq(&cloned.module_active_states));
    assert!(snap.module_active_charges.ptr_eq(&cloned.module_active_charges));

    let json = serde_json::to_string(&snap).expect("serialize snapshot");
    let expected_k1 = crate::service_domains::entity_kv::EntityKvKey::new(
        crate::service_domains::entity_kv::Ns::ContractDeposit,
        501,
        "escrow",
    )
    .to_map_key_string();
    let expected_k2 = crate::service_domains::entity_kv::EntityKvKey::new(
        crate::service_domains::entity_kv::Ns::XmppHostname,
        0,
        "",
    )
    .to_map_key_string();
    assert!(json.contains(&format!("\"{expected_k1}\"")));
    assert!(json.contains(&format!("\"{expected_k2}\"")));
    let decoded: WorldStateSnapshot = serde_json::from_str(&json).expect("deserialize snapshot");
    assert_eq!(decoded, snap);
}
