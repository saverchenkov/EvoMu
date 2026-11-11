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
        "oversized service_state_blobs must be skipped during SQLite sync and not persisted to world_kv_blobs (#902)"
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
    let rb_before = repo.rollback_epoch();

    // 1. with_mut_snapshot does NOT panic on WAL write failure outside dispatch_rpc;
    //    instead it rolls back in-memory state, bumps rollback_epoch, and increments persist_failures_total (#903 / #833).
    repo.wal_fault_mode.store(1, Ordering::SeqCst);
    let ret = repo.with_mut_snapshot(|snap| {
        snap.next_entity_id = 200_999_111;
        42_u32
    });
    assert_eq!(ret, 42);
    assert_eq!(
        repo.with_snapshot(|s| s.next_entity_id),
        init_id,
        "with_mut_snapshot must roll back in-memory state when persistence fails (#903)"
    );
    assert_eq!(repo.rollback_epoch(), rb_before + 1);
    assert!(crate::persist_failures_total() > failures_before);

    // 2. When incremental rollback_dirty_from_sqlite fails (e.g. a row in accounts has a corrupt column
    //    or table is altered so rollback_dirty_from_sqlite errors), rollback_or_reload_from_sqlite falls
    //    back to full reload from SQLite without panicking (#903 / #833).
    {
        let conn = repo.sqlite_conn.lock().unwrap();
        conn.execute(
            "INSERT INTO accounts (username, user_id, password_token, role) VALUES ('u1', 1, 'tok', 0)",
            [],
        )
        .unwrap();
        repo.sqlite_fp
            .lock()
            .unwrap()
            .accounts
            .insert("missing_in_sqlite".into(), 123_456);
    }
    let rb_mid = repo.rollback_epoch();
    let err_res: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.next_entity_id = 200_888_777;
        Err("abort transaction to trigger rollback fallback".into())
    });
    assert!(err_res.is_err());
    assert_eq!(repo.rollback_epoch(), rb_mid + 1);
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), init_id);
    assert!(repo.with_snapshot(|s| s.accounts.contains_key("u1")));
    assert!(!repo.quarantine_unreadable);
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
    let rb_before = repo.rollback_epoch();

    // 1. Panic inside try_with_mut_snapshot is caught by run_mut_snapshot_tx, rolls back partial
    //    in-memory mutations from SQLite, increments tx_panics_total(), and returns Err (#904 / #833).
    let res: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.next_entity_id = 999_111_222;
        snap.characters.get_mut(&cid).unwrap().balance = IskCents(123);
        panic!("simulated try_with_mut_snapshot panic");
    });
    let err_msg = res.expect_err("panic in try_with_mut_snapshot must return Err");
    assert!(err_msg.contains("Write closure panicked: simulated try_with_mut_snapshot panic"));
    assert_eq!(crate::tx_panics_total(), panics_before + 1);
    assert_eq!(repo.rollback_epoch(), rb_before + 1);
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), orig_next_id);
    assert_eq!(repo.with_snapshot(|s| s.characters[&cid].balance), orig_bal);

    // 2. Panic inside with_mut_snapshot rolls back in-memory mutations from SQLite and releases all
    //    locks cleanly before propagating the panic (#904 / #833).
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        repo.with_mut_snapshot(|snap| {
            snap.next_entity_id = 888_222_333;
            snap.characters.get_mut(&cid).unwrap().balance = IskCents(456);
            panic!("simulated with_mut_snapshot panic");
        });
    }));
    assert!(caught.is_err());
    assert_eq!(crate::tx_panics_total(), panics_before + 2);
    assert_eq!(repo.rollback_epoch(), rb_before + 2);
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), orig_next_id);
    assert_eq!(repo.with_snapshot(|s| s.characters[&cid].balance), orig_bal);
    assert!(!repo.inner.is_poisoned(), "inner RwLock must not be poisoned");
    assert!(
        !repo.sqlite_conn.is_poisoned(),
        "sqlite_conn Mutex must not be poisoned"
    );
}

#[test]
fn w5a_p2_0d_fnv_serializer_i128_u128_map_key_errors_and_json_fallback() {
    use crate::sqlite_store::fingerprints::{canonical_fp, FnvHasher, FnvSerializer};
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

    // 2. Inject corrupt JSON into char_lp_logs and social_store (`char_contact_labels`), then verify
    //    default `open_sqlite` fails closed naming the table and primary key (#906 / #856).
    {
        let conn = repo.sqlite_conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO char_lp_logs (character_id, logs_json) VALUES (90000088, '{corrupt')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO char_contact_labels (character_id, labels_json) VALUES (90000077, '{corrupt')",
            [],
        )
        .unwrap();
    }
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
        .any(|r| r.source_table == "char_lp_logs" && r.key == "90000088"));
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
            "CREATE TRIGGER fail_startup_flush BEFORE INSERT ON d6_corp_crimewatch_kv BEGIN SELECT RAISE(ABORT, 'startup_flush_abort'); END;",
        )
        .unwrap();
    let flush_err = crate::sqlite_store::load_snapshot_from_sqlite(&mig_conn)
        .expect_err("startup migration flush failure must propagate (#878)");
    assert!(flush_err.contains("startup_flush_abort"));

    let _ = std::fs::remove_file(&db_path);
}
