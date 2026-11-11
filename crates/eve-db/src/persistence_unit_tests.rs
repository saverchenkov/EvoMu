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

    // Fault mode 1 (partial write_all) and fault mode 2 (sync_data failure) roll back cleanly
    repo.wal_fault_mode.store(1, Ordering::SeqCst);
    let err1: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.next_entity_id = 200_000_111;
        Ok(())
    });
    assert!(err1.is_err());
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), 200_000_000);

    repo.wal_fault_mode.store(2, Ordering::SeqCst);
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

    // 1. Clean tracked transaction (`enable_dirty_tracking` with no dirty hints or meta change)
    // returns early without opening a SQLite transaction or bumping wal_seq / state_epoch.
    repo.with_mut_snapshot(|snap| {
        snap.enable_dirty_tracking();
    });
    assert_eq!(repo.with_snapshot(|s| s.wal_seq), seq0);
    assert_eq!(repo.state_epoch(), ep0);

    // 1b. spawn_asteroid sets `dirty_hints.asteroids_dirty`, and the next `with_mut_snapshot` clears `dirty_hints`;
    // `add_or_stack_item` marks both newly inserted and merged item stacks in `dirty_hints.touched_items`.
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
    assert!(repo.with_snapshot(|s| s.dirty_hints.asteroids_dirty));
    assert_eq!(repo.combat_state_epoch(), cw_ep0);
    repo.with_mut_snapshot(|_| {});
    assert!(!repo.with_snapshot(|s| s.dirty_hints.asteroids_dirty));

    let created = repo
        .add_or_stack_item(cid, 60_003_760, InventoryFlag::Hangar, 38, 10)
        .unwrap();
    assert_eq!(created.quantity, 10);
    assert!(repo.with_snapshot(|s| s.dirty_hints.touched_items.contains(&created.item_id)));
    let stacked = repo
        .add_or_stack_item(cid, 60_003_760, InventoryFlag::Hangar, 38, 5)
        .unwrap();
    assert_eq!(stacked.item_id, created.item_id);
    assert_eq!(stacked.quantity, 15);
    assert!(repo.with_snapshot(|s| s.dirty_hints.touched_items.contains(&stacked.item_id)));

    repo.with_mut_snapshot(|snap| {
        snap.crimewatch_states.insert(
            cid,
            PersistedCrimeWatchState {
                character_id: cid,
                weapon_timer_until_unix: 9_999,
                ..Default::default()
            },
        );
        snap.mark_crimewatch_dirty(false);
    });
    assert_eq!(repo.combat_state_epoch(), cw_ep0 + 1);

    // 2. Persist error mid-sync after mutating fp.characters in-place reloads clean `fp` from SQLite
    // and rolls back `snap` so subsequent mutations to the same character still persist cleanly.
    {
        let conn = repo.sqlite_conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TRIGGER fail_struct_ins BEFORE INSERT ON structures BEGIN SELECT RAISE(ABORT, 'inject_struct_abort'); END;",
        )
        .unwrap();
    }
    let rb0 = repo.rollback_epoch();
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
    assert_eq!(repo.rollback_epoch(), rb0 + 1);
    assert_eq!(repo.with_snapshot(|s| s.characters[&cid].balance), orig_bal);
    assert!(!repo.with_snapshot(|s| s.structures.contains_key(&999_888)));

    // Drop the fault trigger and apply the exact same character balance mutation (`777_666_555`).
    // Because `*fp = clean_fp` restored `fp.characters[&cid]` to `orig_bal`'s fingerprint,
    // `sync_dirty_to_sqlite` detects the change and commits it to SQLite.
    {
        let conn = repo.sqlite_conn.lock().unwrap();
        conn.execute_batch("DROP TRIGGER fail_struct_ins;").unwrap();
    }
    repo.with_mut_snapshot(|snap| {
        snap.enable_dirty_tracking();
        snap.characters.get_mut(&cid).unwrap().balance = IskCents(777_666_555);
        snap.mark_character_dirty(cid);
    });
    let sql_bal: i64 = {
        let conn = repo.sqlite_conn.lock().unwrap();
        conn.query_row(
            "SELECT balance_cents FROM characters WHERE character_id = ?1",
            rusqlite::params![cid],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(sql_bal, 777_666_555);
}
