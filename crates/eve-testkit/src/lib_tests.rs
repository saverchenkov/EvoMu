//! Extended unit and mutation-killing tests for `eve-testkit`.

use super::*;

#[test]
fn test_attest_and_attest_named_regular_file_and_multi_append() {
    let _guard = super::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let base_dir =
        std::env::temp_dir().join(format!("testkit_ext_probe_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base_dir);
    std::fs::create_dir_all(&base_dir).unwrap();

    // 1. Pointing EVOMU_PARITY_ATTEST_DIR at an existing regular file must be a no-op.
    let regular_file = base_dir.join("not_a_directory.txt");
    std::fs::write(&regular_file, b"placeholder").unwrap();
    std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &regular_file);
    attest(&["plan:T-1"]);
    attest_named("eve_testkit", "test_regular_file", &["plan:T-1"]);
    assert_eq!(
        std::fs::read_to_string(&regular_file).unwrap(),
        "placeholder"
    );

    // 2. Pointing EVOMU_PARITY_ATTEST_DIR at a valid directory appends multiple JSONL records.
    let attest_dir = base_dir.join("out");
    std::fs::create_dir_all(&attest_dir).unwrap();
    std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &attest_dir);

    attest_named("multi_crate", "test_alpha", &["row:1", "row:2"]);
    attest_named("multi_crate", "test_beta", &["row:3"]);

    let jsonl_path = attest_dir.join("multi_crate.jsonl");
    let raw = std::fs::read_to_string(&jsonl_path).unwrap();
    assert!(raw.ends_with('\n'), "JSONL output must end with newline");
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(lines.len(), 2, "Expected 2 appended JSONL lines");

    let rec0: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(rec0["test"], "test_alpha");
    assert_eq!(rec0["rows"], serde_json::json!(["row:1", "row:2"]));

    let rec1: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(rec1["test"], "test_beta");
    assert_eq!(rec1["rows"], serde_json::json!(["row:3"]));

    // 3. Direct write_attestation helper check on unwritable / missing directory is a safe no-op.
    let missing_dir = base_dir.join("does_not_exist");
    write_attestation(&missing_dir, "ghost_crate", "test_ghost", &["row:ghost"]);
    assert!(!missing_dir.exists());

    std::env::remove_var("EVOMU_PARITY_ATTEST_DIR");
    let _ = std::fs::remove_dir_all(&base_dir);
}

#[test]
fn test_resolve_test_name_and_crate_name_boundary_mutations() {
    // Crate name normalization boundaries
    assert_eq!(resolve_crate_name(Some("a-b-c")), "a_b_c");
    assert_eq!(resolve_crate_name(Some("plain")), "plain");
    assert_eq!(resolve_crate_name(Some("")), "eve_workspace");
    assert_eq!(resolve_crate_name(None), "eve_workspace");
    assert_eq!(detect_crate_name(), "eve_testkit");

    // Thread name leaf extraction vs fallback to backtrace
    assert_eq!(resolve_test_name(Some("single_leaf_name"), ""), "single_leaf_name");
    assert_eq!(
        resolve_test_name(Some("a::b::c::leaf_fn"), "0: test_ignored"),
        "leaf_fn"
    );
    assert_eq!(
        resolve_test_name(Some(""), "0: my_crate::test_from_empty_thread()"),
        "test_from_empty_thread"
    );
    assert_eq!(
        resolve_test_name(
            Some("tokio-runtime-worker-2"),
            "0: my_crate::test_from_numbered_worker"
        ),
        "test_from_numbered_worker"
    );

    // Boundary length: "test_" is 5 chars (rejected), "test_x" is 6 chars (accepted: len > 5)
    assert_eq!(resolve_test_name(Some("main"), "0: test_()"), "unknown_test");
    assert_eq!(resolve_test_name(Some("main"), "0: test_x()"), "test_x");
    assert_eq!(resolve_test_name(Some("main"), "0: test_9"), "test_9");

    // Reject "test_name" and lines containing "testkit" even when length > 5
    assert_eq!(
        resolve_test_name(Some("main"), "0: fn test_name()"),
        "unknown_test"
    );
    assert_eq!(
        resolve_test_name(Some("main"), "0: eve_testkit::test_valid_looking_sym()"),
        "unknown_test"
    );

    // Direct detect_test_name() returns this test function's name
    assert_eq!(
        detect_test_name(),
        "test_resolve_test_name_and_crate_name_boundary_mutations"
    );
}
