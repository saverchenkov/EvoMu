//! Comprehensive unit and mutation-killing tests for `eve-testkit` attestation helpers,
//! crate-name normalization, thread/backtrace test-name resolution, and JSONL file appends.

use super::*;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

static TESTKIT_COV_SEQ: AtomicU64 = AtomicU64::new(1);

fn fresh_temp_dir(tag: &str) -> PathBuf {
    let seq = TESTKIT_COV_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("evomu_tk_cov_{tag}_{}_{seq}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

struct EnvRestoreGuard {
    prev_attest_dir: Option<std::ffi::OsString>,
    prev_pkg_name: Option<std::ffi::OsString>,
}

impl EnvRestoreGuard {
    fn capture() -> Self {
        Self {
            prev_attest_dir: std::env::var_os("EVOMU_PARITY_ATTEST_DIR"),
            prev_pkg_name: std::env::var_os("CARGO_PKG_NAME"),
        }
    }
}

impl Drop for EnvRestoreGuard {
    fn drop(&mut self) {
        match &self.prev_attest_dir {
            Some(v) => std::env::set_var("EVOMU_PARITY_ATTEST_DIR", v),
            None => std::env::remove_var("EVOMU_PARITY_ATTEST_DIR"),
        }
        match &self.prev_pkg_name {
            Some(v) => std::env::set_var("CARGO_PKG_NAME", v),
            None => std::env::remove_var("CARGO_PKG_NAME"),
        }
    }
}

#[test]
fn test_resolve_crate_name_all_branches_and_env_roundtrip() {
    assert_eq!(resolve_crate_name(None), "eve_workspace");
    assert_eq!(resolve_crate_name(Some("")), "eve_workspace");
    assert_eq!(resolve_crate_name(Some("x")), "x");
    assert_eq!(resolve_crate_name(Some("-")), "_");
    assert_eq!(resolve_crate_name(Some("--")), "__");
    assert_eq!(resolve_crate_name(Some("eve-testkit")), "eve_testkit");
    assert_eq!(
        resolve_crate_name(Some("multi-segment-crate-01")),
        "multi_segment_crate_01"
    );

    let _lock = crate::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _restore = EnvRestoreGuard::capture();

    std::env::set_var("CARGO_PKG_NAME", "eve-db-custom");
    assert_eq!(detect_crate_name(), "eve_db_custom");

    std::env::set_var("CARGO_PKG_NAME", "already_underscored");
    assert_eq!(detect_crate_name(), "already_underscored");

    std::env::set_var("CARGO_PKG_NAME", "");
    assert_eq!(detect_crate_name(), "eve_workspace");

    std::env::remove_var("CARGO_PKG_NAME");
    assert_eq!(detect_crate_name(), "eve_workspace");
}

#[test]
fn test_resolve_test_name_thread_filters_and_backtrace_boundaries() {
    // 1. Thread-name acceptance vs rejection boundaries.
    assert_eq!(resolve_test_name(None, ""), "unknown_test");
    assert_eq!(resolve_test_name(Some(""), ""), "unknown_test");
    assert_eq!(resolve_test_name(Some("main"), ""), "unknown_test");
    assert_eq!(
        resolve_test_name(Some("tokio-runtime-worker"), ""),
        "unknown_test"
    );
    assert_eq!(
        resolve_test_name(Some("tokio-runtime-worker-12"), ""),
        "unknown_test"
    );
    // Prefix shorter than "tokio-runtime-worker" is NOT filtered out.
    assert_eq!(
        resolve_test_name(Some("tokio-runtime"), "0: suite::test_fallback"),
        "tokio-runtime"
    );
    // "main_worker" is NOT equal to "main", so it is accepted directly.
    assert_eq!(
        resolve_test_name(Some("main_worker"), "0: suite::test_fallback"),
        "main_worker"
    );
    // rsplit("::") extracts only the final leaf segment (never the first segment).
    assert_eq!(
        resolve_test_name(Some("root_mod::mid_mod::leaf_case"), "0: suite::test_other"),
        "leaf_case"
    );
    assert_eq!(
        resolve_test_name(Some("standalone_leaf"), "0: suite::test_other"),
        "standalone_leaf"
    );
    // Trailing "::" yields an empty leaf string from rsplit("::").next().
    assert_eq!(resolve_test_name(Some("mod_only::"), ""), "");

    // 2. Backtrace symbol scanner boundary & filter checks.
    // Exact length 5 ("test_") must be rejected (`sym.len() > 5`, not `>= 5`).
    assert_eq!(resolve_test_name(None, "0: mod::test_"), "unknown_test");
    assert_eq!(resolve_test_name(None, "0: mod::test_()"), "unknown_test");
    // Exact length 6 ("test_a", "test_0", "test__") must be accepted.
    assert_eq!(resolve_test_name(None, "0: mod::test_a()"), "test_a");
    assert_eq!(resolve_test_name(None, "0: mod::test_0"), "test_0");
    assert_eq!(resolve_test_name(None, "0: mod::test__+0x4"), "test__");

    // Candidate reaching end-of-line with no trailing delimiter (`unwrap_or(candidate.len())`).
    assert_eq!(
        resolve_test_name(None, "frame 3: crate::it::test_Alpha_99_EOF"),
        "test_Alpha_99_EOF"
    );

    // Multi-line skip chain exercising every rejection predicate before matching a valid frame:
    // - line 0: no "test_" substring
    // - line 1: "test_" length == 5
    // - line 2: exact "test_name" rejected
    // - line 3: line contains "testkit" in path even though symbol is valid
    // - line 4: valid symbol "test_name_accepted" (prefix "test_name" but longer) -> accepted!
    let multi_bt = "\
0: std::rt::lang_start_internal
1: my_crate::test_::trampoline
2: helper::test_name()
3: at crates/eve-testkit/src/lib.rs:90 in suite::test_skipped_due_to_path()
4: eve_db::tests::test_name_accepted::{{closure}}+0x2f
5: eve_db::tests::test_never_reached()";
    assert_eq!(
        resolve_test_name(Some("main"), multi_bt),
        "test_name_accepted"
    );

    // Delimiters stop identifier scan cleanly (`:`, `(`, `<`, `$`, `-`, whitespace).
    assert_eq!(
        resolve_test_name(None, "0: mod::test_stop_at_hyphen-suffix"),
        "test_stop_at_hyphen"
    );
    assert_eq!(
        resolve_test_name(None, "0: mod::test_stop_at_generic<T>()"),
        "test_stop_at_generic"
    );
}

#[test]
fn test_detect_test_name_across_unnamed_main_worker_and_named_threads() {
    let _lock = crate::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _restore = EnvRestoreGuard::capture();

    let dir = fresh_temp_dir("threads");
    std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &dir);
    std::env::set_var("CARGO_PKG_NAME", "eve-tk-threads");

    // 1. Unnamed thread (`std::thread::current().name()` is `None`) exercises Backtrace capture path.
    let unnamed_detected = std::thread::spawn(|| {
        assert!(std::thread::current().name().is_none());
        let name = detect_test_name();
        attest(&["plan:T-1"]);
        name
    })
    .join()
    .expect("join unnamed thread");
    assert_eq!(unnamed_detected, "unknown_test");

    // 2. Thread explicitly named "main" also falls through to Backtrace capture path.
    let main_detected = std::thread::Builder::new()
        .name("main".to_string())
        .spawn(|| {
            let name = detect_test_name();
            attest(&["plan:T-2"]);
            name
        })
        .expect("spawn main thread")
        .join()
        .expect("join main thread");
    assert_eq!(main_detected, "unknown_test");

    // 3. Thread named "tokio-runtime-worker-3" also falls through cleanly.
    let worker_detected = std::thread::Builder::new()
        .name("tokio-runtime-worker-3".to_string())
        .spawn(detect_test_name)
        .expect("spawn worker thread")
        .join()
        .expect("join worker thread");
    assert_eq!(worker_detected, "unknown_test");

    // 4. Named module-qualified test thread resolves to leaf function name.
    let named_detected = std::thread::Builder::new()
        .name("eve_db::gateway::test_leaf_detection".to_string())
        .spawn(|| {
            let name = detect_test_name();
            attest(&["plan:T-3"]);
            name
        })
        .expect("spawn named thread")
        .join()
        .expect("join named thread");
    assert_eq!(named_detected, "test_leaf_detection");

    let jsonl = fs::read_to_string(dir.join("eve_tk_threads.jsonl")).expect("read thread jsonl");
    let records: Vec<serde_json::Value> = jsonl
        .lines()
        .map(|l| serde_json::from_str(l).expect("valid json line"))
        .collect();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["test"], "unknown_test");
    assert_eq!(records[0]["rows"], serde_json::json!(["plan:T-1"]));
    assert_eq!(records[1]["test"], "unknown_test");
    assert_eq!(records[1]["rows"], serde_json::json!(["plan:T-2"]));
    assert_eq!(records[2]["test"], "test_leaf_detection");
    assert_eq!(records[2]["rows"], serde_json::json!(["plan:T-3"]));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_attest_and_write_attestation_guards_preserve_existing_files() {
    let _lock = crate::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _restore = EnvRestoreGuard::capture();

    let dir = fresh_temp_dir("guards");
    std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &dir);
    std::env::set_var("CARGO_PKG_NAME", "eve-tk-guards");

    // Seed one initial record, then verify empty slices do not append blank records.
    attest_named("eve_tk_guards", "seed_test", &["row:seed"]);
    let target = dir.join("eve_tk_guards.jsonl");
    let before = fs::read_to_string(&target).expect("read seeded jsonl");

    attest(&[]);
    attest_named("eve_tk_guards", "seed_test", &[]);
    let after_empty = fs::read_to_string(&target).expect("read after empty rows");
    assert_eq!(after_empty, before);

    // Direct `write_attestation` with explicit multi-row payload appends exact JSON line + '\n'.
    write_attestation(&dir, "eve_tk_guards", "direct_write", &["r:1", "r:2", "r:3"]);
    let after_direct = fs::read_to_string(&target).expect("read after direct write");
    let lines: Vec<&str> = after_direct.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        lines[1],
        "{\"test\":\"direct_write\",\"rows\":[\"r:1\",\"r:2\",\"r:3\"]}"
    );

    let _ = fs::remove_dir_all(&dir);
}
