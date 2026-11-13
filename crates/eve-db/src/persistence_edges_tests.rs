use super::*;
use crate::types::{
    ContractRecord, SsoRefreshTokenRecord, SystemDynamicState, WalletJournalEntry,
};
use crate::VirtualServerClock;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn unique_tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "evomu_pers_edge_{tag}_{}_{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn wal_delta_from_diff_cold_fallback_triggers_and_seq_max() {
    let mut base = WorldStateSnapshot::default();
    base.wal_seq = 10;
    base.contracts.insert(1, ContractRecord::default());
    base.system_states.insert(30000142, SystemDynamicState::default());
    base.committed_idempotency_keys.insert((1, "k1".into()));
    base.wallet_journal.push(WalletJournalEntry {
        entry_id: 1,
        character_id: 1001,
        entry_type: "Test".into(),
        amount: IskCents(100),
        balance_after: IskCents(100),
        reason: "e1".into(),
    });
    base.wallet_journal.push(WalletJournalEntry {
        entry_id: 2,
        character_id: 1001,
        entry_type: "Test".into(),
        amount: IskCents(200),
        balance_after: IskCents(300),
        reason: "e2".into(),
    });

    // Removing a contract triggers full_snapshot fallback
    let mut no_contract = base.clone();
    no_contract.contracts.remove(&1);
    no_contract.wal_seq = 11;
    let d_contract = WorldStateWalDelta::from_diff(&base, &no_contract);
    assert!(d_contract.full_snapshot.is_some());

    // Removing a system_state triggers full_snapshot fallback
    let mut no_sys = base.clone();
    no_sys.system_states.remove(&30000142);
    no_sys.wal_seq = 11;
    let d_sys = WorldStateWalDelta::from_diff(&base, &no_sys);
    assert!(d_sys.full_snapshot.is_some());

    // Removing an idempotency key triggers full_snapshot fallback
    let mut no_idem = base.clone();
    no_idem.committed_idempotency_keys.remove(&(1, "k1".into()));
    no_idem.wal_seq = 11;
    let d_idem = WorldStateWalDelta::from_diff(&base, &no_idem);
    assert!(d_idem.full_snapshot.is_some());

    // Truncating wallet_journal triggers full_snapshot fallback
    let mut trunc_wallet = base.clone();
    trunc_wallet.wallet_journal.pop();
    trunc_wallet.wal_seq = 11;
    let d_trunc = WorldStateWalDelta::from_diff(&base, &trunc_wallet);
    assert!(d_trunc.full_snapshot.is_some());

    // Mutating existing prefix element in wallet_journal triggers full_snapshot fallback
    let mut mut_prefix = base.clone();
    mut_prefix.wallet_journal[0].amount.0 += 123;
    mut_prefix.wal_seq = 11;
    let d_prefix = WorldStateWalDelta::from_diff(&base, &mut_prefix);
    assert!(d_prefix.full_snapshot.is_some());

    // Cold field mutation triggers full_snapshot fallback
    let mut cold_mut = base.clone();
    cold_mut.next_corporation_id += 1;
    cold_mut.wal_seq = 11;
    let d_cold = WorldStateWalDelta::from_diff(&base, &cold_mut);
    assert!(d_cold.full_snapshot.is_some());

    // apply_to preserves max(wal_seq, delta.seq)
    let mut target = WorldStateSnapshot::default();
    target.wal_seq = 50;
    let mut small_next = base.clone();
    small_next.wal_seq = 20;
    let delta_smaller = WorldStateWalDelta::from_diff(&base, &small_next);
    delta_smaller.apply_to(&mut target);
    assert_eq!(target.wal_seq, 50);

    let mut large_next = base.clone();
    large_next.wal_seq = 75;
    let delta_larger = WorldStateWalDelta::from_diff(&base, &large_next);
    delta_larger.apply_to(&mut target);
    assert_eq!(target.wal_seq, 75);
}

#[test]
fn clock_and_open_sqlite_and_auth_runtime_edges() {
    let dir = unique_tmp_dir("sqlite");
    let nested_db = dir.join("nested").join("sub").join("world.sqlite3");

    let repo = WorldRepository::open_sqlite(&nested_db).unwrap();
    assert!(nested_db.exists());
    assert_eq!(repo.sqlite_path(), Some(nested_db.as_path()));

    // clock() and set_clock()
    let _orig_clock = repo.clock();
    repo.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_700_000_500)));
    assert_eq!(repo.now_unix_secs(), 1_700_000_500);

    // Populate AuthRuntimeState & prune_expired_auth_runtime + save_to_sqlite
    {
        let mut st = repo.auth_runtime.write().unwrap();
        st.revoked_jtis.insert("jti-expired".into());
        st.revoked_jti_expiries.insert("jti-expired".into(), 1_700_000_100);
        st.revoked_jtis.insert("jti-active".into());
        st.revoked_jti_expiries.insert("jti-active".into(), 1_700_000_900);
        st.kicked_users.insert(10, (1_700_000_100, "expired".into()));
        st.kicked_users.insert(20, (1_700_000_900, "active".into()));
        st.refresh_tokens.insert(
            "tok-expired".into(),
            SsoRefreshTokenRecord {
                refresh_token: "tok-expired".into(),
                user_id: 1,
                character_id: 1001,
                client_id: "c1".into(),
                scopes: vec![],
                expires_at_unix: 1_700_000_100,
                revoked: false,
                last_access_token_jti: "".into(),
                last_access_token_exp_unix: 0,
            },
        );
        st.refresh_tokens.insert(
            "tok-active".into(),
            SsoRefreshTokenRecord {
                refresh_token: "tok-active".into(),
                user_id: 2,
                character_id: 1002,
                client_id: "c2".into(),
                scopes: vec![],
                expires_at_unix: 1_700_000_900,
                revoked: false,
                last_access_token_jti: "".into(),
                last_access_token_exp_unix: 0,
            },
        );
    }
    let exported_db = dir.join("exported.sqlite3");
    repo.save_to_sqlite(&exported_db).unwrap();
    repo.prune_expired_auth_runtime(1_700_000_500);
    {
        let st = repo.auth_runtime.read().unwrap();
        assert!(!st.revoked_jtis.contains("jti-expired"));
        assert!(st.revoked_jtis.contains("jti-active"));
        assert!(!st.refresh_tokens.contains_key("tok-expired"));
        assert!(st.refresh_tokens.contains_key("tok-active"));
    }
    drop(repo);

    // Reopen exported db and verify AuthRuntimeState persisted
    let repo2 = WorldRepository::open_sqlite(&exported_db).unwrap();
    {
        let st = repo2.auth_runtime.read().unwrap();
        assert!(st.revoked_jtis.contains("jti-active"));
        assert!(st.kicked_users.contains_key(&20));
    }
    drop(repo2);

    // open_sqlite errors on directory & corrupt file
    assert!(WorldRepository::open_sqlite(&dir).is_err());
    let corrupt_db = dir.join("corrupt.sqlite");
    std::fs::write(&corrupt_db, b"not a sqlite db at all").unwrap();
    assert!(WorldRepository::open_sqlite(&corrupt_db).is_err());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn snapshot_path_extensions_errors_wal_tail_and_drop_checkpoint() {
    let dir = unique_tmp_dir("wal_drop");

    // .sqlite3 and .db extensions work via new_empty_with_snapshot_path
    let sqlite3_path = dir.join("auto.sqlite3");
    let r_s3 = WorldRepository::new_empty_with_snapshot_path(Some(sqlite3_path.clone())).unwrap();
    assert!(sqlite3_path.exists());
    drop(r_s3);

    let db_path = dir.join("auto.db");
    let r_db = WorldRepository::new_empty_with_snapshot_path(Some(db_path.clone())).unwrap();
    assert!(db_path.exists());
    drop(r_db);

    // Corrupt existing JSON fails loudly
    let bad_json = dir.join("bad_snap.json");
    std::fs::write(&bad_json, b"{not valid json").unwrap();
    assert!(WorldRepository::new_empty_with_snapshot_path(Some(bad_json)).is_err());

    // Uncreatable JSON path (parent is an existing regular file) fails
    let file_parent = dir.join("blocking_file");
    std::fs::write(&file_parent, b"blocker").unwrap();
    let impossible_snap = file_parent.join("snap.json");
    assert!(WorldRepository::new_empty_with_snapshot_path(Some(impossible_snap)).is_err());

    // WAL + Drop auto-checkpoint when pending_wal_records > 0
    let snap_path = dir.join("world.json");
    let wal_path = WorldRepository::wal_path_for(&snap_path);
    {
        let repo = WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone())).unwrap();
        repo.with_mut_snapshot(|s| {
            s.next_entity_id += 1;
        });
        assert!(wal_path.exists());
        // Simulate trailing partial/corrupt line in WAL without newline
        std::fs::OpenOptions::new()
            .append(true)
            .open(&wal_path)
            .unwrap()
            .write_all(b"{\"partial_corrupt_tail\":true")
            .unwrap();
        repo.with_mut_snapshot(|s| {
            s.next_entity_id += 1;
        });
        assert!(repo.pending_wal_records.load(Ordering::Relaxed) > 0);
        // Drop triggers checkpoint because pending_wal_records > 0
    }
    // Reloading after Drop has updated snapshot
    let repo_reloaded = WorldRepository::new_empty_with_snapshot_path(None).unwrap();
    repo_reloaded.load_snapshot_from_file(&snap_path).unwrap();
    assert!(repo_reloaded.with_snapshot(|s| s.wal_seq) >= 2);
    drop(repo_reloaded);

    // append_wal_delta_locked recreates snapshot if deleted from disk
    let repo2 = WorldRepository::new_empty_with_snapshot_path(Some(snap_path.clone())).unwrap();
    std::fs::remove_file(&snap_path).unwrap();
    repo2.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
    });
    assert!(snap_path.exists());
    drop(repo2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_snapshot_from_file_wal_sorting_blank_lines_and_errors() {
    let dir = unique_tmp_dir("load_wal");
    let repo = WorldRepository::new_empty_with_snapshot_path(None).unwrap();

    let missing = dir.join("does_not_exist.json");
    assert!(repo.load_snapshot_from_file(&missing).is_err());

    let bad = dir.join("corrupt.json");
    std::fs::write(&bad, b"[]").unwrap();
    assert!(repo.load_snapshot_from_file(&bad).is_err());

    // Valid snapshot at wal_seq = 5, plus WAL with blank line, stale seq<=5, out-of-order seq 7 then seq 6
    let snap_path = dir.join("snap.json");
    let mut base = WorldStateSnapshot::default();
    base.wal_seq = 5;
    base.next_entity_id = 200_000_100;
    std::fs::write(&snap_path, serde_json::to_vec(&base).unwrap()).unwrap();

    let mut s6 = base.clone();
    s6.wal_seq = 6;
    s6.next_entity_id = 200_000_200;
    let d6 = WorldStateWalDelta::from_diff(&base, &s6);

    let mut s7 = s6.clone();
    s7.wal_seq = 7;
    s7.next_entity_id = 200_000_300;
    let d7 = WorldStateWalDelta::from_diff(&s6, &s7);

    let mut s_stale = base.clone();
    s_stale.wal_seq = 5;
    let d_stale = WorldStateWalDelta::from_diff(&base, &s_stale); // seq <= 5 skipped

    let wal_path = WorldRepository::wal_path_for(&snap_path);
    let wal_content = format!(
        "\n{}\n{}\n{}\n",
        serde_json::to_string(&d7).unwrap(),
        serde_json::to_string(&d_stale).unwrap(),
        serde_json::to_string(&d6).unwrap(),
    );
    std::fs::write(&wal_path, wal_content).unwrap();

    repo.load_snapshot_from_file(&snap_path).unwrap();
    assert_eq!(repo.with_snapshot(|s| s.wal_seq), 7);
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), 200_000_300);

    // seq == 0 delta in WAL returns Err
    let mut s0 = base.clone();
    s0.wal_seq = 0;
    let d0 = WorldStateWalDelta::from_diff(&base, &s0);
    std::fs::write(&wal_path, format!("{}\n", serde_json::to_string(&d0).unwrap())).unwrap();
    assert!(repo.load_snapshot_from_file(&snap_path).is_err());

    // Corrupt non-trailing WAL line returns Err
    std::fs::write(
        &wal_path,
        format!("not_json\n{}\n", serde_json::to_string(&d6).unwrap()),
    )
    .unwrap();
    assert!(repo.load_snapshot_from_file(&snap_path).is_err());

    // Torn trailing line is healed & replayed
    std::fs::write(
        &wal_path,
        format!("{}\n{{\"torn\":1", serde_json::to_string(&d6).unwrap()),
    )
    .unwrap();
    repo.load_snapshot_from_file(&snap_path).unwrap();
    assert_eq!(repo.with_snapshot(|s| s.wal_seq), 6);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn try_with_snapshot_and_try_with_mut_snapshot_rollback_edges() {
    let dir = unique_tmp_dir("try_mut");
    let db_path = dir.join("repo.sqlite");
    let repo = WorldRepository::open_sqlite(&db_path).unwrap();

    // Readers read `published` without blocking on the writer and see pre-transaction state inside
    // an active write closure; `try_with_snapshot` returns None only during `published.write()`.
    let pre_id = repo.with_snapshot(|s| s.next_entity_id);
    repo.with_mut_snapshot(|s| {
        s.next_entity_id += 5;
        assert_eq!(repo.try_with_snapshot(|r| r.next_entity_id), Some(pre_id));
    });
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), pre_id + 5);
    {
        let _p = repo.published.write().unwrap();
        assert!(repo.try_with_snapshot(|s| s.next_entity_id).is_none());
    }
    assert!(repo.try_with_snapshot(|s| s.next_entity_id).is_some());

    let initial_next_id = repo.with_snapshot(|s| s.next_entity_id);

    // Closure returning Err rolls back in-memory mutation
    let closure_err: Result<(), String> = repo.try_with_mut_snapshot(|s| {
        s.next_entity_id += 500;
        Err("intentional failure".to_string())
    });
    assert!(closure_err.is_err());
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), initial_next_id);

    // Persistence failure via TxFaultPlan on JSON-backed repo rolls back in-memory state
    let snap_path = dir.join("fault.json");
    let json_repo = WorldRepository::new_empty_with_snapshot_path(Some(snap_path)).unwrap();
    let json_init = json_repo.with_snapshot(|s| s.next_entity_id);
    json_repo.set_fault_plan_for_test(crate::TxFaultPlan::FailNthStatement(1));
    let wal_err: Result<(), String> = json_repo.try_with_mut_snapshot(|s| {
        s.next_entity_id += 777;
        Ok(())
    });
    assert!(wal_err.is_err());
    assert_eq!(json_repo.with_snapshot(|s| s.next_entity_id), json_init);

    // FailAtCommit also rolls back cleanly
    json_repo.set_fault_plan_for_test(crate::TxFaultPlan::FailAtCommit);
    let sync_err: Result<(), String> = json_repo.try_with_mut_snapshot(|s| {
        s.next_entity_id += 888;
        Ok(())
    });
    assert!(sync_err.is_err());
    assert_eq!(json_repo.with_snapshot(|s| s.next_entity_id), json_init);

    // SQLite failure during try_with_mut_snapshot (drop a core table and dirty that domain) rolls back
    repo.with_sqlite_conn(|conn| {
        conn.execute("DROP TABLE d6_corp_crimewatch_kv", []).unwrap();
    });
    let sqlite_err: Result<(), String> = repo.try_with_mut_snapshot(|s| {
        s.next_entity_id += 999;
        s.zarzakh_gate_locks.insert(42, (1, 999));
        Ok(())
    });
    assert!(sqlite_err.is_err());
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), initial_next_id);
    assert!(!repo.with_snapshot(|s| s.zarzakh_gate_locks.contains_key(&42)));

    let _ = std::fs::remove_dir_all(&dir);
}
