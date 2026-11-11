//! Isolated integration test for `WAL_ERROR_HOOK` and `WAL_POISON_CHECK_HOOK`
//! in `eve_db::persistence` (runs in its own test binary so global `OnceLock` hooks
//! cannot affect other `eve-db` unit tests).

use eve_db::persistence::{
    record_last_wal_error, set_wal_error_hook, set_wal_poison_check_hook, WorldRepository,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[test]
fn wal_error_hook_and_poison_hook_roundtrip() {
    // 1. Before any WAL_ERROR_HOOK is installed, record_last_wal_error returns false.
    assert!(!record_last_wal_error("pre-hook-error"));

    // 2. Install hooks: WAL_ERROR_HOOK captures error and returns true (suppressing panic in with_mut_snapshot);
    //    WAL_POISON_CHECK_HOOK returns Some(...) only when `poison_active` is true.
    let poison_active = Arc::new(AtomicBool::new(false));
    let poison_flag = Arc::clone(&poison_active);
    set_wal_poison_check_hook(Arc::new(move || {
        poison_flag
            .load(Ordering::SeqCst)
            .then(|| "poisoned-wal-state".to_string())
    }));

    let captured = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let captured_ref = Arc::clone(&captured);
    set_wal_error_hook(Arc::new(move |err| {
        captured_ref.lock().unwrap().push(err.to_string());
        true
    }));

    let repo = WorldRepository::new_empty_with_snapshot_path(None).unwrap();

    // Activate poison hook: try_with_mut_snapshot returns Err(poison_err) and rolls back snapshot
    poison_active.store(true, Ordering::SeqCst);
    let res: Result<(), String> = repo.try_with_mut_snapshot(|s| {
        s.next_entity_id = 200_999_999;
        Ok(())
    });
    assert_eq!(res, Err("poisoned-wal-state".to_string()));
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), 200_000_000);

    // with_mut_snapshot under poison hook hits record_last_wal_error -> hook returns true (no panic)
    repo.with_mut_snapshot(|s| {
        s.next_entity_id = 200_888_888;
    });
    assert_eq!(repo.with_snapshot(|s| s.next_entity_id), 200_000_000);
    assert_eq!(captured.lock().unwrap().as_slice(), ["poisoned-wal-state"]);

    poison_active.store(false, Ordering::SeqCst);
}
