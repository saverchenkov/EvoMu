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
