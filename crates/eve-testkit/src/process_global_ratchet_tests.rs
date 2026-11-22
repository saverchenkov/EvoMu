//! Unit tests for `process_global_ratchet` (`#1583` Track 1).

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "eve_process_global_ratchet_{prefix}_{}_{}",
            std::process::id(),
            seq
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp workspace dir");
        Self(dir)
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct EnvRestoreGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvRestoreGuard {
    fn capture(keys: &[&'static str]) -> Self {
        Self(keys.iter().map(|&k| (k, std::env::var_os(k))).collect())
    }
}

impl Drop for EnvRestoreGuard {
    fn drop(&mut self) {
        for (k, v) in self.0.drain(..) {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}

fn write_test_file(root: &Path, rel: &str, contents: &str) {
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).expect("create parent dir");
    }
    std::fs::write(full, contents).expect("write test file");
}

fn clean_empty_ratchet() -> ProcessGlobalRatchet {
    let mut groups = BTreeMap::new();
    for g in ProcessGlobalGroup::ALL {
        groups.insert(
            g,
            GroupPolicy {
                zero_required: false,
                max_count: 10,
                rationale_required: g == ProcessGlobalGroup::GProcessWideAllowlist,
                description: format!("test group {g}"),
            },
        );
    }
    ProcessGlobalRatchet {
        version: 1,
        owning_issue: 1583,
        description: "test".to_string(),
        excluded_crates: vec!["eve-testkit".to_string()],
        groups,
        declarations: Vec::new(),
    }
}

#[test]
fn live_workspace_scan_matches_process_global_ratchet_cleanly() {
    let ratchet =
        ProcessGlobalRatchet::load_default().expect("load default process_global_ratchet.json");
    let summary = ratchet
        .verify_default()
        .expect("live workspace scan must satisfy process_global_ratchet.json");

    assert_eq!(
        summary.report.scanned_crates,
        vec![
            "eve-db".to_string(),
            "eve-destiny".to_string(),
            "eve-dogma".to_string(),
            "eve-machonet".to_string(),
            "eve-proto".to_string(),
            "eve-sde".to_string(),
            "eve-server".to_string(),
        ]
    );
    assert!(
        !summary
            .report
            .findings
            .iter()
            .any(|f| f.rel_file.contains("r6_t1_p1_security_tests_combat")),
        "#[cfg(test)] external module files must be skipped"
    );
    let group_sum: usize = summary.per_group_counts.values().sum();
    assert_eq!(group_sum, summary.report.findings.len());
}

#[test]
fn scanner_ignores_comments_strings_cfg_test_and_const_statics() {
    let src = r##"
        // static COMMENTED_OUT: OnceLock<u64> = OnceLock::new();
        /*
            static BLOCK_COMMENT: Mutex<u64> = Mutex::new(0);
            tokio::task_local! { static IN_BLOCK_COMMENT: u64; }
        */
        const RAW: &str = r#"thread_local! { static IN_STRING: Cell<u64> = const { Cell::new(0) }; }"#;
        static CONST_SCALAR: u64 = 42;
        static CONST_TABLE: &[u32] = &[1, 2, 3];
        static CONST_BYTES: [u8; 4] = [1, 2, 3, 4];
        static CONST_PAIRS: &[(&'static str, u16)] = &[("a", 1)];
        static LIFE_USER: &'static str = "ok";
        fn uses_fn_once<F: FnOnce()>(_f: F) {}
        static MUT_COUNTER: AtomicU64 = AtomicU64::new(0);
        static MUT_GLOBAL_CELL: OnceLock<Mutex<u64>> = OnceLock::new();
        static ONCE_FLAG: std::sync::Once = std::sync::Once::new();
        static mut RAW_MUT: u32 = 0;

        thread_local! {
            static TL_ONE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
            static TL_TWO: std::cell::RefCell<Option<u64>> = const { std::cell::RefCell::new(None) };
        }

        tokio::task_local! {
            static TASK_SCOPE: bool;
        }

        #[cfg(test)]
        static TEST_ONLY_HOOK: OnceLock<Mutex<u64>> = OnceLock::new();

        #[cfg(test)]
        mod tests {
            static IN_MOD_TEST: AtomicU64 = AtomicU64::new(0);
            thread_local! { static TL_TEST: Cell<u64> = const { Cell::new(0) }; }
        }
    "##;

    let findings = scan_rust_source("eve-server", "sample.rs", src);
    let got: Vec<(ProcessGlobalKind, &str)> = findings
        .iter()
        .map(|f| (f.kind, f.symbol.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            (ProcessGlobalKind::StaticSync, "MUT_COUNTER"),
            (ProcessGlobalKind::StaticSync, "MUT_GLOBAL_CELL"),
            (ProcessGlobalKind::StaticSync, "ONCE_FLAG"),
            (ProcessGlobalKind::StaticMut, "RAW_MUT"),
            (ProcessGlobalKind::ThreadLocal, "TL_ONE"),
            (ProcessGlobalKind::ThreadLocal, "TL_TWO"),
            (ProcessGlobalKind::TaskLocal, "TASK_SCOPE"),
        ]
    );
}

#[test]
fn scanner_flags_wrapper_structs_type_aliases_renamed_imports_and_unlisted_sync_primitives() {
    let cases = [
        (
            "WRAPPER_GLOBAL",
            "struct GlobalCounter { inner: std::sync::atomic::AtomicU64 }\nimpl GlobalCounter { const fn new() -> Self { Self { inner: std::sync::atomic::AtomicU64::new(0) } } }\nstatic WRAPPER_GLOBAL: GlobalCounter = GlobalCounter::new();\n",
        ),
        (
            "ALIAS_GLOBAL",
            "type GlobalMap = OnceLock<Mutex<Vec<u8>>>;\nstatic ALIAS_GLOBAL: GlobalMap = OnceLock::new();\n",
        ),
        (
            "RENAMED_IMPORT_GLOBAL",
            "use std::sync::OnceLock as Lock;\nstatic RENAMED_IMPORT_GLOBAL: Lock<u64> = Lock::new();\n",
        ),
        (
            "TOKIO_SEMAPHORE",
            "static TOKIO_SEMAPHORE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);\n",
        ),
        (
            "TOKIO_NOTIFY",
            "static TOKIO_NOTIFY: tokio::sync::Notify = tokio::sync::Notify::const_new();\n",
        ),
        (
            "STD_CONDVAR",
            "static STD_CONDVAR: std::sync::Condvar = std::sync::Condvar::new();\n",
        ),
        (
            "DASHMAP_GLOBAL",
            "static DASHMAP_GLOBAL: once_cell::sync::Lazy<dashmap::DashMap<u64, u64>> = once_cell::sync::Lazy::new(dashmap::DashMap::new);\n",
        ),
        (
            "MUTABLE_ARRAY_ELEM",
            "static MUTABLE_ARRAY_ELEM: [std::sync::atomic::AtomicU64; 2] = [std::sync::atomic::AtomicU64::new(0), std::sync::atomic::AtomicU64::new(1)];\n",
        ),
        (
            "MUTABLE_TUPLE_ELEM",
            "static MUTABLE_TUPLE_ELEM: (u32, std::sync::Mutex<u32>) = (1, std::sync::Mutex::new(2));\n",
        ),
    ];

    let ratchet = clean_empty_ratchet();
    for (expected_sym, src) in cases {
        let tmp = TempDirGuard::new(expected_sym);
        write_test_file(&tmp.0, "crates/eve-server/src/sneak.rs", src);
        let err = ratchet
            .verify_workspace(&tmp.0)
            .expect_err("non-immutable static must fail the ratchet");
        assert!(
            err.contains("eve-server") && err.contains("sneak.rs") && err.contains(expected_sym),
            "expected failure naming {expected_sym}, got: {err}"
        );
    }
}

#[test]
fn scanner_skips_cfg_test_external_modules_and_test_paths() {
    let tmp = TempDirGuard::new("skip_cfg_test_mod");
    write_test_file(
        &tmp.0,
        "crates/eve-server/src/lib.rs",
        "#[cfg(test)]\nmod security_helpers_combat;\n",
    );
    write_test_file(
        &tmp.0,
        "crates/eve-server/src/security_helpers_combat.rs",
        "static TEST_SDE: OnceLock<u64> = OnceLock::new();\n",
    );
    write_test_file(
        &tmp.0,
        "crates/eve-testkit/src/lib.rs",
        "static TESTKIT_OK: OnceLock<u64> = OnceLock::new();\n",
    );

    let excluded: BTreeSet<String> = ["eve-testkit".to_string()].into_iter().collect();
    let report = scan_workspace_process_globals(&tmp.0, &excluded).expect("scan temp workspace");
    assert!(report.findings.is_empty(), "got {:#?}", report.findings);
}

#[test]
fn scanner_resolves_path_attr_against_parent_dir_in_both_orders_for_non_tests_targets() {
    let tmp = TempDirGuard::new("path_attr_both_orders");
    write_test_file(
        &tmp.0,
        "crates/eve-server/src/router/entry.rs",
        "#[cfg(test)]\n#[path = \"sub/helper_alpha.rs\"]\nmod helper_alpha;\n\n#[path = \"../helper_beta.rs\"]\n#[cfg(test)]\nmod helper_beta;\n",
    );
    write_test_file(
        &tmp.0,
        "crates/eve-server/src/router/sub/helper_alpha.rs",
        "static TEST_ALPHA: OnceLock<u64> = OnceLock::new();\n",
    );
    write_test_file(
        &tmp.0,
        "crates/eve-server/src/helper_beta.rs",
        "static TEST_BETA: OnceLock<u64> = OnceLock::new();\n",
    );
    // An ordinary non-cfg(test) sibling module must still be scanned.
    write_test_file(
        &tmp.0,
        "crates/eve-server/src/router/real_prod.rs",
        "static PROD_LEAK: OnceLock<u64> = OnceLock::new();\n",
    );

    let excluded: BTreeSet<String> = ["eve-testkit".to_string()].into_iter().collect();
    let report = scan_workspace_process_globals(&tmp.0, &excluded).expect("scan temp workspace");
    let got: Vec<&str> = report.findings.iter().map(|f| f.symbol.as_str()).collect();
    assert_eq!(got, vec!["PROD_LEAK"]);
}

#[test]
fn scanner_depth_aware_cfg_test_handles_struct_fields_variants_match_arms_and_array_semicolons() {
    let src = r#"
        struct Config {
            #[cfg(test)]
            pub test_map: std::collections::HashMap<i64, u64>,
            #[cfg(test)]
            pub tail_without_comma: [u8; 4]
        }

        static AFTER_STRUCT_FIELDS: OnceLock<u64> = OnceLock::new();

        enum Event {
            #[cfg(test)]
            TestVariant { payload: [u8; 8], count: usize },
            #[cfg(test)]
            TestTuple(Vec<u8>),
            LiveVariant,
        }

        fn dispatch(code: u32) -> u32 {
            match code {
                #[cfg(test)]
                1 if code < 10 => {
                    static INSIDE_ARM: OnceLock<u64> = OnceLock::new();
                    99
                }
                #[cfg(test)]
                2 => 0,
                _ => code,
            }
        }

        static AFTER_ENUM_AND_MATCH: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);

        #[cfg(test)]
        static TEST_BYTES: [u8; 4] = [1, 2, 3, 4];

        #[cfg(test)]
        fn helper_with_array_sig(buf: [u8; 4]) -> [u8; 4] {
            static HIDDEN_IN_TEST_FN: OnceLock<u64> = OnceLock::new();
            buf
        }
    "#;

    let findings = scan_rust_source("eve-server", "cfg_edges.rs", src);
    let syms: Vec<&str> = findings.iter().map(|f| f.symbol.as_str()).collect();
    assert_eq!(syms, vec!["AFTER_STRUCT_FIELDS", "AFTER_ENUM_AND_MATCH"]);
}

#[test]
fn unlisted_static_oncelock_thread_local_and_task_local_fail_naming_declaration() {
    let cases = [
        (
            "eve-db",
            "store.rs",
            "static mut LEAKY_RAW: u64 = 0;\n",
            "static_mut",
            "LEAKY_RAW",
        ),
        (
            "eve-proto",
            "codec.rs",
            "static NEW_CACHE: OnceLock<Vec<u8>> = OnceLock::new();\n",
            "static_sync",
            "NEW_CACHE",
        ),
        (
            "eve-sde",
            "loader.rs",
            "thread_local! { static BAD_TL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }\n",
            "thread_local",
            "BAD_TL",
        ),
        (
            "eve-server",
            "router/scope.rs",
            "tokio::task_local! { static BAD_TASK_SCOPE: u64; }\n",
            "task_local",
            "BAD_TASK_SCOPE",
        ),
    ];

    let ratchet = clean_empty_ratchet();
    for (crate_name, rel_file, code, expected_kind, expected_sym) in cases {
        let tmp = TempDirGuard::new(expected_sym);
        write_test_file(
            &tmp.0,
            &format!("crates/{crate_name}/src/{rel_file}"),
            code,
        );
        let err = ratchet
            .verify_workspace(&tmp.0)
            .expect_err("unlisted process-global declaration must fail");
        assert!(
            err.contains(crate_name)
                && err.contains(rel_file)
                && err.contains(expected_kind)
                && err.contains(expected_sym),
            "error must name crate, file, kind, and symbol; got: {err}"
        );
    }
}

#[test]
fn classification_count_regression_kind_change_zero_required_and_allowlist_checks() {
    let mut ratchet = clean_empty_ratchet();
    ratchet.declarations.push(ProcessGlobalBaselineEntry {
        crate_name: "eve-db".to_string(),
        rel_file: "metrics.rs".to_string(),
        symbol: "METRICS_COUNT".to_string(),
        kind: ProcessGlobalKind::StaticSync,
        group: ProcessGlobalGroup::GProcessWideAllowlist,
        max_count: 1,
        rationale: Some("Process-wide metric counter.".to_string()),
    });
    ratchet.declarations.push(ProcessGlobalBaselineEntry {
        crate_name: "eve-server".to_string(),
        rel_file: "state.rs".to_string(),
        symbol: "MAP".to_string(),
        kind: ProcessGlobalKind::StaticSync,
        group: ProcessGlobalGroup::AGameState,
        max_count: 1,
        rationale: None,
    });

    let tmp = TempDirGuard::new("regression");
    write_test_file(
        &tmp.0,
        "crates/eve-server/src/state.rs",
        "fn a() { static MAP: OnceLock<u64> = OnceLock::new(); }\nfn b() { static MAP: OnceLock<u64> = OnceLock::new(); }\n",
    );
    let err = ratchet
        .verify_workspace(&tmp.0)
        .expect_err("count exceeding max_count must fail");
    assert!(
        err.contains("state.rs") && err.contains("MAP") && err.contains("A_game_state"),
        "unexpected err: {err}"
    );

    // Removing all declarations satisfies falling ratchet even before tightening JSON.
    write_test_file(&tmp.0, "crates/eve-server/src/state.rs", "");
    let ok_reduced = ratchet
        .verify_workspace(&tmp.0)
        .expect("reducing declarations below baseline must pass");
    assert_eq!(ok_reduced.per_group_counts[&ProcessGlobalGroup::AGameState], 0);

    // Kind mismatch fails loudly.
    write_test_file(
        &tmp.0,
        "crates/eve-server/src/state.rs",
        "thread_local! { static MAP: Cell<u64> = const { Cell::new(0) }; }\n",
    );
    let kind_err = ratchet
        .verify_workspace(&tmp.0)
        .expect_err("kind change must fail");
    assert!(
        kind_err.contains("kind changed") && kind_err.contains("MAP"),
        "unexpected kind_err: {kind_err}"
    );

    // Zero-required group enforcement rejects non-zero policy and remaining entries.
    ratchet
        .groups
        .get_mut(&ProcessGlobalGroup::AGameState)
        .unwrap()
        .zero_required = true;
    let zero_err = ratchet
        .validate()
        .expect_err("zero_required=true with baseline entry must fail");
    assert!(
        zero_err.contains("A_game_state") && zero_err.contains("zero_required"),
        "unexpected zero_err: {zero_err}"
    );

    // Clearing A_game_state entry & setting max_count=0 succeeds when zero_required=true.
    ratchet.declarations.retain(|d| d.symbol != "MAP");
    let policy_a = ratchet
        .groups
        .get_mut(&ProcessGlobalGroup::AGameState)
        .unwrap();
    policy_a.max_count = 0;
    assert!(ratchet.validate().is_ok());

    // Blank Group G rationale fails loudly.
    ratchet.declarations[0].rationale = Some("   ".to_string());
    let rat_err = ratchet
        .validate()
        .expect_err("Group G entry with blank rationale must fail");
    assert!(rat_err.contains("METRICS_COUNT") && rat_err.contains("rationale"));
}

#[test]
fn two_repository_and_two_context_isolation_helpers_work_side_by_side() {
    let pair = TwoRepositoryPair::with_demo_seed_isolated();
    pair.assert_distinct_instances();

    let right_before_epoch = pair.right_repo().state_epoch();
    let right_before_commits = pair.right_repo().tx_commit_count();
    let shared_char_id = 90_000_001_i64;
    let right_before_sec = pair
        .right_repo()
        .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status));

    pair.left_repo()
        .tx("two_context_left_isolation", |tx| {
            let ch = tx
                .snap
                .characters
                .get_mut(&shared_char_id)
                .expect("demo seed character 90_000_001 must exist");
            ch.security_status = 4.25;
            Ok::<(), eve_db::TxError>(())
        })
        .expect("commit left tx");

    assert!(pair.left_repo().state_epoch() > right_before_epoch);
    assert_eq!(pair.right_repo().state_epoch(), right_before_epoch);
    assert_eq!(pair.right_repo().tx_commit_count(), right_before_commits);
    assert_ne!(right_before_sec, Some(4.25));
    assert_eq!(
        pair.left_repo()
            .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status)),
        Some(4.25)
    );
    assert_eq!(
        pair.right_repo()
            .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status)),
        right_before_sec
    );

    let server_pair = TwoContextPair::from_factory(|| {
        eve_server::router::TestContext::with_sde_and_repository(
            Arc::new(SdeRegistry::with_canonical_seed()),
            Arc::new(WorldRepository::with_demo_seed_in_memory()),
        )
    });
    assert_ne!(
        server_pair.left().instance_id,
        server_pair.right().instance_id
    );
    assert_ne!(
        server_pair.left().db.instance_id(),
        server_pair.right().db.instance_id()
    );
}

#[test]
fn two_repository_pair_stays_isolated_in_memory_under_env_overrides() {
    let _env_write_lock = eve_db::lock_test_env_write();
    let _restore = EnvRestoreGuard::capture(&[
        "EVOMU_DB_PATH",
        "EVOMU_SNAPSHOT_PATH",
        "EVOMU_DISABLE_DEMO_SEED",
    ]);
    let tmp = TempDirGuard::new("env_isolation");
    let db_path = tmp.0.join("shared_db.sqlite");
    let snap_path = tmp.0.join("shared_snap.json");
    std::env::set_var("EVOMU_DB_PATH", &db_path);
    std::env::set_var("EVOMU_SNAPSHOT_PATH", &snap_path);
    std::env::set_var("EVOMU_DISABLE_DEMO_SEED", "1");

    let empty_pair = TwoRepositoryPair::new_empty_isolated();
    empty_pair.assert_distinct_instances();
    empty_pair
        .left_repo()
        .provision_player_account("only_left", "pw-left");
    assert_eq!(
        empty_pair
            .left_repo()
            .with_snapshot(|s| s.accounts.contains_key("only_left")),
        true
    );
    assert_eq!(
        empty_pair
            .right_repo()
            .with_snapshot(|s| s.accounts.contains_key("only_left")),
        false
    );
    assert!(!db_path.exists(), "EVOMU_DB_PATH must remain untouched");
    assert!(!snap_path.exists(), "EVOMU_SNAPSHOT_PATH must remain untouched");

    // Pre-create an empty DB at EVOMU_DB_PATH and confirm new_empty_isolated still ignores it.
    let seeded_disk = WorldRepository::open_sqlite(&db_path).expect("seed disk sqlite");
    seeded_disk.provision_player_account("disk_leak", "pw-disk");
    drop(seeded_disk);

    let clean_empty = TwoRepositoryPair::new_empty_isolated();
    assert!(
        clean_empty
            .left_repo()
            .with_snapshot(|s| !s.accounts.contains_key("disk_leak")),
        "new_empty_isolated must not load EVOMU_DB_PATH"
    );

    let demo_pair = TwoRepositoryPair::with_demo_seed_isolated();
    demo_pair.assert_distinct_instances();
    demo_pair
        .left_repo()
        .provision_player_account("demo_left_only", "pw");
    assert!(demo_pair
        .right_repo()
        .with_snapshot(|s| !s.accounts.contains_key("demo_left_only")));
    assert!(demo_pair
        .right_repo()
        .with_snapshot(|s| s.characters.contains_key(&90_000_001)));
    assert!(!snap_path.exists());
}
