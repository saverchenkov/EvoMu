//! Smoke tests for side-by-side [`crate::common::TwoServerContexts`] isolation (`#1583` Track 1).

use crate::common::TwoServerContexts;
use eve_sde::SdeRegistry;
use std::sync::Arc;

#[test]
fn two_server_contexts_isolate_repositories_and_sdes() {
    let contexts = TwoServerContexts::new();
    let shared_char_id = 90_000_001_i64;

    let right_sec_before = contexts
        .right()
        .db
        .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status));
    let right_epoch_before = contexts.right().db.state_epoch();

    contexts
        .left()
        .db
        .tx("two_context_left_isolation", |tx| {
            let ch = tx
                .snap
                .characters
                .get_mut(&shared_char_id)
                .expect("demo seed character 90_000_001 must exist");
            ch.security_status = 4.25;
            Ok::<(), eve_db::TxError>(())
        })
        .expect("left context tx commit");

    assert_ne!(right_sec_before, Some(4.25));

    assert_eq!(
        contexts
            .left()
            .db
            .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status)),
        Some(4.25)
    );
    assert_eq!(
        contexts
            .right()
            .db
            .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status)),
        right_sec_before
    );
    assert_eq!(contexts.right().db.state_epoch(), right_epoch_before);
    assert!(contexts.left().db.state_epoch() > right_epoch_before);
    contexts.assert_teardown_invariants();

    let empty = TwoServerContexts::new_empty();
    assert_ne!(empty.left().instance_id, empty.right().instance_id);
    assert_ne!(
        empty.left().db.instance_id(),
        empty.right().db.instance_id()
    );

    let custom = TwoServerContexts::with_sdes(
        Arc::new(SdeRegistry::default()),
        Arc::new(SdeRegistry::default()),
    );
    assert!(!Arc::ptr_eq(&custom.left().sde, &custom.right().sde));
    assert!(!Arc::ptr_eq(&custom.left().db, &custom.right().db));
}

#[test]
fn two_server_contexts_stay_in_memory_under_env_path_overrides() {
    struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (k, v) in self.0.drain(..) {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    struct TempDirGuard(std::path::PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let _lock = eve_db::lock_test_env_write();
    let keys = [
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_DISABLE_DEMO_SEED",
        "EVOMU_SDE_PATH",
    ];
    let _restore = EnvRestore(keys.iter().map(|&k| (k, std::env::var_os(k))).collect());

    let tmp_dir = std::env::temp_dir().join(format!("eve_it_two_ctx_env_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let _tmp_guard = TempDirGuard(tmp_dir.clone());
    let db_path = tmp_dir.join("shared.sqlite");
    let snap_path = tmp_dir.join("shared_snap.json");
    let missing_sde_path = tmp_dir.join("missing_sde.sqlite");
    std::env::set_var("EVOMU_DB_PATH", &db_path);
    std::env::set_var("EVOMU_SNAPSHOT_PATH", &snap_path);
    std::env::set_var("EVOMU_DISABLE_DEMO_SEED", "1");

    let empty = TwoServerContexts::new_empty();
    empty.left().db.provision_player_account("left_only", "pw");
    assert!(empty.left().db.with_snapshot(|s| s.accounts.contains_key("left_only")));
    assert!(empty.right().db.with_snapshot(|s| !s.accounts.contains_key("left_only")));
    assert!(!db_path.exists());

    let seeded = TwoServerContexts::new();
    seeded.left().db.provision_player_account("seeded_left", "pw");
    assert!(seeded.left().db.with_snapshot(|s| s.characters.contains_key(&90_000_001)));
    assert!(seeded.right().db.with_snapshot(|s| !s.accounts.contains_key("seeded_left")));
    assert!(!snap_path.exists());

    // Poison EVOMU_SDE_PATH and prove TwoServerContexts::with_sdes ignores it and seeds each
    // context's repository exclusively from its caller-supplied SdeRegistry.
    std::env::set_var("EVOMU_SDE_PATH", &missing_sde_path);
    let make_race2_sde = |skill_id: u32, level: u8| {
        let mut sde = SdeRegistry::default();
        sde.races.insert(
            2,
            eve_sde::RaceInfo {
                race_id: 2,
                name: "Minmatar".to_string(),
                corvette_type_id: None,
                starter_skills: std::collections::HashMap::from([(skill_id, level)]),
                description: String::new(),
            },
        );
        Arc::new(sde)
    };
    let custom = TwoServerContexts::with_sdes(
        make_race2_sde(999_001, 2),
        make_race2_sde(999_002, 4),
    );
    assert!(!Arc::ptr_eq(&custom.left().sde, &custom.right().sde));
    assert!(!Arc::ptr_eq(&custom.left().db, &custom.right().db));
    assert_eq!(
        custom
            .left()
            .db
            .with_snapshot(|s| s.skills[&90_000_001].get(&999_001).map(|sk| sk.level)),
        Some(2)
    );
    assert!(!custom
        .left()
        .db
        .with_snapshot(|s| s.skills[&90_000_001].contains_key(&999_002)));
    assert_eq!(
        custom
            .right()
            .db
            .with_snapshot(|s| s.skills[&90_000_001].get(&999_002).map(|sk| sk.level)),
        Some(4)
    );
    assert!(!custom
        .right()
        .db
        .with_snapshot(|s| s.skills[&90_000_001].contains_key(&999_001)));
}
