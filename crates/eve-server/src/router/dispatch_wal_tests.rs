use super::*;

#[tokio::test]
async fn test_b11r2_p1_01_second_with_mut_snapshot_refused_after_wal_error() {
    ensure_wal_error_hook_installed();
    let tmp_path = std::env::temp_dir().join(format!(
        "evomu_b11r2_p1_01_poison_{}.json",
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

    let recorded_err = DISPATCH_WAL_ERROR
        .scope(std::cell::RefCell::new(None), async {
            repo.with_mut_snapshot(|snap| {
                snap.service_state_blobs
                    .insert("first_tx".into(), "should_rollback".into());
            });
            assert!(
                current_dispatch_wal_error().is_some(),
                "first WAL failure must poison DISPATCH_WAL_ERROR scope"
            );
            // Even if the WAL path becomes writable again, the poisoned scope must refuse commits.
            let _ = std::fs::remove_dir_all(&wal_dir);
            repo.with_mut_snapshot(|snap| {
                snap.service_state_blobs
                    .insert("second_tx".into(), "must_not_persist".into());
            });
            take_dispatch_wal_error()
        })
        .await;

    assert!(recorded_err.is_some(), "dispatch scope must report WAL error");
    repo.with_snapshot(|snap| {
        assert!(
            !snap.service_state_blobs.contains_key("first_tx"),
            "first failed mutation must not be persisted"
        );
        assert!(
            !snap.service_state_blobs.contains_key("second_tx"),
            "second mutation in poisoned dispatch scope must not be persisted"
        );
    });

    drop(repo);
    let _ = std::fs::remove_file(&tmp_path);
    let _ = std::fs::remove_dir_all(&wal_dir);
}

#[tokio::test]
async fn test_wal_rollback_rehydrate_runtime_from_db_inside_poisoned_scope_is_read_only() {
    ensure_wal_error_hook_installed();
    let ctx = ServerContext::with_repository(std::sync::Arc::new(eve_db::WorldRepository::new_empty()));
    let recorded_err = DISPATCH_WAL_ERROR
        .scope(
            std::cell::RefCell::new(Some("synthetic WAL error".to_string())),
            async {
                assert!(current_dispatch_wal_error().is_some());
                ctx.rehydrate_runtime_from_db();
                take_dispatch_wal_error()
            },
        )
        .await;
    assert_eq!(recorded_err.as_deref(), Some("synthetic WAL error"));
}
