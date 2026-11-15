#[tokio::test]
async fn test_failed_wal_write_restores_before_image_without_poisoning_subsequent_txs() {
    let tmp_path = std::env::temp_dir().join(format!(
        "evomu_cow_wal_recover_{}.json",
        std::process::id()
    ));
    let wal_dir = tmp_path.with_extension("json.wal");
    let _ = std::fs::remove_file(&tmp_path);
    let _ = std::fs::remove_dir_all(&wal_dir);

    let repo = eve_db::WorldRepository::new_empty_with_snapshot_path(Some(tmp_path.clone()))
        .expect("new_empty_with_snapshot_path");
    std::fs::write(&tmp_path, b"{}").expect("seed tmp_path");
    // Block WAL append by placing a directory at `<tmp_path>.wal`.
    std::fs::create_dir_all(&wal_dir).expect("create_dir_all wal_dir");

    let first_err: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("first_tx".into(), "should_rollback".into());
        Ok(())
    });
    assert!(first_err.is_err(), "blocked WAL must fail the transaction");
    repo.with_snapshot(|snap| {
        assert!(
            !snap.service_state_blobs.contains_key("first_tx"),
            "failed WAL tx must restore pre-tx snapshot"
        );
    });

    // Once the WAL path becomes writable again, subsequent transactions commit cleanly without poison.
    let _ = std::fs::remove_dir_all(&wal_dir);
    repo.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("second_tx".into(), "committed_ok".into());
    });
    repo.with_snapshot(|snap| {
        assert_eq!(
            snap.service_state_blobs.get("second_tx").map(String::as_str),
            Some("committed_ok")
        );
    });

    drop(repo);
    let _ = std::fs::remove_file(&tmp_path);
    let _ = std::fs::remove_dir_all(&wal_dir);
}

fn issue_874_make_session(session_id: u64, user_id: u64, char_id: i64, corp_id: i64) -> eve_machonet::SessionState {
    let mut sess = eve_machonet::SessionState::new(session_id);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess
}

#[tokio::test]
async fn test_issue_874_ignored_try_with_mut_snapshot_failure_poisons_rpc_without_poisoning_next_rpc() {
    let db_path = std::env::temp_dir().join(format!(
        "evomu_874_multiwrite_poison_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db_path);

    let seed_ctx = crate::router::ServerContext::with_repository(std::sync::Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    ));
    let char_id = 90_000_001i64;
    let mut ch = seed_ctx.db.get_character(char_id).expect("demo char");
    let uid = ch.user_id;
    let corp_id = if ch.corporation_id > 0 { ch.corporation_id } else { 98_000_001 };
    ch.corporation_id = corp_id;
    ch.balance = eve_db::IskCents::from_isk(5_000_000);
    let mut corp = seed_ctx
        .db
        .with_snapshot(|s| s.corporations.get(&corp_id).cloned())
        .unwrap_or_else(|| {
            let mut c = eve_db::CorporationRecord::default();
            c.corporation_id = corp_id;
            c.name = "Issue874 Corp".into();
            c.ticker = "I874".into();
            c
        });
    corp.ceo_id = char_id;
    seed_ctx.db.with_mut_snapshot(|s| {
        s.characters.insert(char_id, ch);
        s.corporations.insert(corp_id, corp);
    });
    seed_ctx.db.save_to_sqlite(&db_path).expect("save_to_sqlite");

    let ctx = crate::router::ServerContext::from_sqlite(&db_path).expect("from_sqlite");
    ctx.db
        .acquire_online_lock(uid, 95_874, Some(char_id), true, ctx.now_unix())
        .expect("online lock");
    let mut sess = issue_874_make_session(95_874, uid, char_id, corp_id);

    // Abort the character/corp ISK debit inside `corpRecProxy.CreateRecruitmentAd`, which uses
    // `let _ = ctx.db.try_with_mut_snapshot(...)` before writing the recruitment ad.
    let ads_before = ctx
        .db
        .with_snapshot(|s| s.fleet_and_social_ops.corp_recruitment_ads.len());
    let gov_ads_before = ctx.db.with_snapshot(|s| {
        s.corp_governance
            .get(&corp_id)
            .map_or(0, |g| g.recruitment_ads.len())
    });
    ctx.db.with_sqlite_conn(|conn| {
        conn.execute_batch(
            "CREATE TRIGGER abort_874_char_fee BEFORE UPDATE ON characters BEGIN SELECT RAISE(ABORT, 'abort_874_corp_ad_fee'); END;",
        )
        .unwrap();
    });

    let err = ctx
        .dispatch_rpc(
            &mut sess,
            "corpRecProxy",
            None,
            "CreateRecruitmentAd",
            &[
                eve_proto::PyRep::String("PoisTest Ad".into()),
                eve_proto::PyRep::String("Must roll back".into()),
                eve_proto::PyRep::Integer(7),
                eve_proto::PyRep::Integer(30),
            ],
            &std::collections::HashMap::new(),
            None,
        )
        .await
        .expect_err("ignored try_with_mut_snapshot failure must poison RPC and fail dispatch");
    assert_eq!(err.code, "PersistenceFailure");
    assert!(err.message.contains("abort_874_corp_ad_fee"));
    assert_eq!(
        ctx.db.get_character(char_id).unwrap().balance,
        eve_db::IskCents::from_isk(5_000_000),
        "character balance must be rolled back"
    );
    assert_eq!(
        ctx.db
            .with_snapshot(|s| s.fleet_and_social_ops.corp_recruitment_ads.len()),
        ads_before,
        "second write in same RPC must be poisoned and not commit a free recruitment ad"
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| {
            s.corp_governance
                .get(&corp_id)
                .map_or(0, |g| g.recruitment_ads.len())
        }),
        gov_ads_before,
        "corp_governance must not record an ad after poisoned try_with_mut_snapshot"
    );

    // Clear the fault and verify subsequent independent RPCs on the same context/session succeed.
    ctx.db.with_sqlite_conn(|conn| {
        conn.execute_batch("DROP TRIGGER abort_874_char_fee;")
            .unwrap();
    });
    let ok_res = ctx
        .dispatch_rpc(
            &mut sess,
            "corpRecProxy",
            None,
            "CreateRecruitmentAd",
            &[
                eve_proto::PyRep::String("Clean Ad".into()),
                eve_proto::PyRep::String("Should succeed".into()),
                eve_proto::PyRep::Integer(7),
                eve_proto::PyRep::Integer(30),
            ],
            &std::collections::HashMap::new(),
            None,
        )
        .await;
    assert!(ok_res.is_ok(), "next independent RPC must not remain poisoned: {ok_res:?}");

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_issue_874_staged_notifications_discarded_for_caller_and_bystander_on_failure() {
    let wal_tmp = std::env::temp_dir().join(format!(
        "evomu_874_notify_stage_{}.json",
        std::process::id()
    ));
    let wal_dir = wal_tmp.with_extension("json.wal");
    let _ = std::fs::remove_file(&wal_tmp);
    let _ = std::fs::remove_dir_all(&wal_dir);

    let seed = eve_db::WorldRepository::with_demo_seed();
    let char_a = 90_000_001i64;
    let char_b = 90_000_874i64;
    let ch_a = seed.get_character(char_a).expect("demo char");
    let uid_a = ch_a.user_id;
    let uid_b = uid_a + 10_874;
    let mut ch_b = ch_a.clone();
    ch_b.character_id = char_b;
    ch_b.user_id = uid_b;
    ch_b.name = "Bystander874".into();
    let acct_a = seed
        .with_snapshot(|s| s.accounts.values().find(|a| a.user_id == uid_a).cloned())
        .expect("acct_a");
    let mut acct_b = acct_a.clone();
    acct_b.user_id = uid_b;
    acct_b.username = "bystander874".into();

    let repo = eve_db::WorldRepository::new_empty_with_snapshot_path(Some(wal_tmp.clone()))
        .expect("wal repo");
    std::fs::write(&wal_tmp, b"{}").expect("seed wal");
    let ctx = crate::router::ServerContext::with_repository(std::sync::Arc::new(repo));
    ctx.db.with_mut_snapshot(|s| {
        s.accounts.insert(acct_a.username.clone(), acct_a);
        s.accounts.insert(acct_b.username.clone(), acct_b);
        s.characters.insert(char_a, ch_a);
        s.characters.insert(char_b, ch_b);
    });
    ctx.db
        .acquire_online_lock(uid_a, 96_001, Some(char_a), true, ctx.now_unix())
        .expect("lock a");
    ctx.db
        .acquire_online_lock(uid_b, 96_002, Some(char_b), true, ctx.now_unix())
        .expect("lock b");

    // Pre-existing notifications before the failed dispatch scope must be preserved.
    ctx.notify(
        crate::router::NotificationTarget::Character(char_b),
        "PreExistingBystander",
        "charid",
        vec![],
    );
    let mut notify_rx = ctx.notify_tx.subscribe();

    // Block WAL writes so `try_with_mut_snapshot` fails while caller & non-caller notifications are emitted.
    let _ = std::fs::remove_file(&wal_dir);
    std::fs::create_dir_all(&wal_dir).expect("block wal");

    super::ensure_dispatch_tx_hooks_installed();
    let fail_outcome: Result<(), eve_proto::BridgeRpcError> = super::maybe_with_dispatch_tx_error(
        true,
        super::maybe_with_staged_dispatch_notifications(
            true,
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            crate::router::cache_notifications::with_active_dispatch_char(Some(char_a), async {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_a),
                    "ShouldDropCaller",
                    "charid",
                    vec![],
                );
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_b),
                    "ShouldDropBystander",
                    "charid",
                    vec![],
                );
                crate::router::notify_registry::emit_typed_char_with_db(
                    &ctx.db,
                    Some(&*ctx.pending_notifications),
                    Some(&ctx.notify_tx),
                    char_b,
                    crate::router::notify_registry::OnContactLoggedOn { char_id: char_a },
                );
                let _ = ctx.db.try_with_mut_snapshot(|s| -> Result<(), String> {
                    s.char_notes.insert((char_a, 874), "rolled back".into());
                    Ok(())
                });
                if let Some((code, msg)) = super::DISPATCH_TX_ERROR
                    .try_with(|c| c.borrow_mut().take())
                    .ok()
                    .flatten()
                {
                    return Err(eve_proto::BridgeRpcError {
                        code,
                        message: msg,
                        client_error: None,
                    });
                }
                Ok(())
            }),
        ),
    )
    .await;
    assert_eq!(fail_outcome.unwrap_err().code, "PersistenceFailure");
    assert!(
        notify_rx.try_recv().is_err(),
        "failed RPC must not broadcast staged notifications on notify_tx"
    );
    {
        let queues = ctx.pending_notifications.read().unwrap();
        assert!(!queues.contains_key(&char_a));
        let b_events: Vec<&str> = queues[&char_b].iter().map(|n| n.event.as_str()).collect();
        assert_eq!(b_events, vec!["PreExistingBystander"]);
    }

    // Unblock WAL writes and confirm a succeeding dispatch scope flushes caller + bystander notifications.
    let _ = std::fs::remove_dir_all(&wal_dir);
    let ok_outcome: Result<(), eve_proto::BridgeRpcError> = super::maybe_with_dispatch_tx_error(
        true,
        super::maybe_with_staged_dispatch_notifications(
            true,
            &*ctx.pending_notifications,
            &ctx.notify_tx,
            crate::router::cache_notifications::with_active_dispatch_char(Some(char_a), async {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_b),
                    "CommittedBystander",
                    "charid",
                    vec![],
                );
                ctx.db.with_mut_snapshot(|s| {
                    s.char_notes.insert((char_a, 875), "committed".into());
                });
                if super::dispatch_check_tx_poison().is_none() {
                    crate::router::cache_notifications::flush_staged_dispatch_notifications(
                        &*ctx.pending_notifications,
                        &ctx.notify_tx,
                    );
                }
                Ok(())
            }),
        ),
    )
    .await;
    assert!(ok_outcome.is_ok());
    let (rx_cid, rx_notif) = notify_rx.try_recv().expect("live notify_tx on commit");
    assert_eq!(rx_cid, char_b);
    assert_eq!(rx_notif.event, "CommittedBystander");
    let b_events: Vec<String> = ctx.pending_notifications.read().unwrap()[&char_b]
        .iter()
        .map(|n| n.event.clone())
        .collect();
    assert_eq!(b_events, vec!["PreExistingBystander", "CommittedBystander"]);

    drop(ctx);
    let _ = std::fs::remove_file(&wal_tmp);
    let _ = std::fs::remove_dir_all(&wal_dir);
}

#[test]
fn test_issue_874_rehydrate_preserves_live_runtime_maps_on_checkpoint_sql_error() {
    let db_path = std::env::temp_dir().join(format!(
        "evomu_874_rehydrate_err_{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&db_path);

    let seed_ctx = crate::router::ServerContext::with_repository(std::sync::Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    ));
    seed_ctx.db.save_to_sqlite(&db_path).expect("save_to_sqlite");
    let ctx = crate::router::ServerContext::from_sqlite(&db_path).expect("from_sqlite");

    // Seed live runtime state across checkpointed and crash-lost maps.
    ctx.scanned_signatures
        .write()
        .unwrap()
        .insert((90_000_001, 30_000_142, 874));
    ctx.tactical_state
        .write()
        .unwrap()
        .system_sim_time_s
        .insert((0, 30_000_142), 123.5);
    ctx.location_change_invuln
        .lock()
        .unwrap()
        .insert(98_874, 1_800_000_060);

    // Replace `system_checkpoint` with an incompatible view so `load_checkpoints_into_ctx` errors.
    ctx.db.with_sqlite_conn(|conn| {
        conn.execute_batch(
            "DROP TABLE IF EXISTS system_checkpoint; CREATE VIEW system_checkpoint AS SELECT 1 AS broken_col;",
        )
        .unwrap();
    });

    ctx.rehydrate_runtime_from_db();

    assert!(
        ctx.scanned_signatures
            .read()
            .unwrap()
            .contains(&(90_000_001, 30_000_142, 874)),
        "checkpoint read error must not wipe scanned_signatures (#874)"
    );
    assert_eq!(
        ctx.tactical_state
            .read()
            .unwrap()
            .system_sim_time_s
            .get(&(0, 30_000_142))
            .copied(),
        Some(123.5),
        "checkpoint read error must not reset tactical_state (#874)"
    );
    assert!(
        ctx.location_change_invuln
            .lock()
            .unwrap()
            .contains_key(&98_874),
        "checkpoint read error must not clear live invuln map via fallback default apply (#874)"
    );

    drop(ctx);
    let _ = std::fs::remove_file(&db_path);
}
