use super::*;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("{prefix}_{}_{}", std::process::id(), n));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn test_deep_attest_concurrent_threads_and_jsonl_integrity() {
    let _guard = crate::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prev_dir = std::env::var_os("EVOMU_PARITY_ATTEST_DIR");
    let prev_pkg = std::env::var_os("CARGO_PKG_NAME");

    let tmp_dir = unique_temp_dir("evomu_testkit_concurrent");
    std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &tmp_dir);
    std::env::set_var("CARGO_PKG_NAME", "eve-testkit-concurrent");

    let handles: Vec<_> = (0..8)
        .map(|tid| {
            std::thread::Builder::new()
                .name(format!("worker_crate::submod::test_thread_case_{tid}"))
                .spawn(move || {
                    for iter in 0..5 {
                        let row_id = format!("row-{tid}-{iter}");
                        attest(&["row-shared", row_id.as_str()]);
                        attest_named("custom-sub-pkg", "explicit_case", &[row_id.as_str()]);
                    }
                })
                .expect("spawn thread")
        })
        .collect();

    for h in handles {
        h.join().expect("thread join");
    }

    match prev_dir {
        Some(v) => std::env::set_var("EVOMU_PARITY_ATTEST_DIR", v),
        None => std::env::remove_var("EVOMU_PARITY_ATTEST_DIR"),
    }
    match prev_pkg {
        Some(v) => std::env::set_var("CARGO_PKG_NAME", v),
        None => std::env::remove_var("CARGO_PKG_NAME"),
    }

    let auto_file = tmp_dir.join("eve_testkit_concurrent.jsonl");
    let auto_text = fs::read_to_string(&auto_file).expect("read auto jsonl");
    let auto_lines: Vec<&str> = auto_text.lines().collect();
    assert_eq!(auto_lines.len(), 40);
    for line in auto_lines {
        let val: serde_json::Value = serde_json::from_str(line).expect("valid json line");
        let test_field = val["test"].as_str().expect("string test name");
        assert!(test_field.starts_with("test_thread_case_"));
        assert!(!test_field.contains("::"));
        let rows = val["rows"].as_array().expect("rows array");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].as_str(), Some("row-shared"));
    }

    let named_file = tmp_dir.join("custom-sub-pkg.jsonl");
    let named_text = fs::read_to_string(&named_file).expect("read named jsonl");
    assert_eq!(named_text.lines().count(), 40);

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_deep_attest_regular_file_and_whitespace_dir_edge_cases() {
    let _guard = crate::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prev_dir = std::env::var_os("EVOMU_PARITY_ATTEST_DIR");

    // 1. Pointing EVOMU_PARITY_ATTEST_DIR to a regular file must be a safe no-op
    let base_dir = unique_temp_dir("evomu_testkit_edges");
    let regular_file = base_dir.join("not_a_directory.txt");
    fs::write(&regular_file, b"keep-me-intact").expect("write regular file");
    std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &regular_file);
    attest(&["row-1"]);
    attest_named("pkg", "fn_name", &["row-2"]);
    assert_eq!(
        fs::read(&regular_file).expect("read regular file"),
        b"keep-me-intact"
    );

    // 2. Directory with spaces + special characters in test name and row IDs
    let spaced_dir = base_dir.join("nested spaced dir");
    fs::create_dir_all(&spaced_dir).expect("create spaced dir");
    std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &spaced_dir);
    attest_named(
        "eve_sde",
        "test_unicode_and_quotes\"line\nbreak",
        &["row\"escaped", "row-ümlaut-🚀", ""],
    );

    match prev_dir {
        Some(v) => std::env::set_var("EVOMU_PARITY_ATTEST_DIR", v),
        None => std::env::remove_var("EVOMU_PARITY_ATTEST_DIR"),
    }

    let out_file = spaced_dir.join("eve_sde.jsonl");
    let content = fs::read_to_string(&out_file).expect("read escaped jsonl");
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 1);
    let parsed: serde_json::Value = serde_json::from_str(lines[0]).expect("parse json");
    assert_eq!(
        parsed["test"].as_str(),
        Some("test_unicode_and_quotes\"line\nbreak")
    );
    assert_eq!(parsed["rows"][0].as_str(), Some("row\"escaped"));
    assert_eq!(parsed["rows"][1].as_str(), Some("row-ümlaut-🚀"));
    assert_eq!(parsed["rows"][2].as_str(), Some(""));

    let _ = fs::remove_dir_all(&base_dir);
}

#[test]
fn test_deep_resolve_crate_and_test_name_edge_cases() {
    assert_eq!(resolve_crate_name(Some("a-b-c-d")), "a_b_c_d");
    assert_eq!(resolve_crate_name(Some("no_hyphens")), "no_hyphens");
    assert_eq!(resolve_crate_name(Some("-")), "_");
    assert_eq!(resolve_crate_name(Some("")), "eve_workspace");
    assert_eq!(resolve_crate_name(None), "eve_workspace");

    // Thread name priority & tokio / main / empty fallbacks
    assert_eq!(
        resolve_test_name(Some("crate::mod::inner::test_alpha"), ""),
        "test_alpha"
    );
    assert_eq!(
        resolve_test_name(Some("custom_leaf_without_prefix"), "at mod::test_ignored"),
        "custom_leaf_without_prefix"
    );
    assert_eq!(
        resolve_test_name(Some("tokio-runtime-worker"), "at mod::test_real_case+0x10"),
        "test_real_case"
    );
    assert_eq!(
        resolve_test_name(Some("tokio-runtime-worker-0"), "at mod::test_worker_n"),
        "test_worker_n"
    );
    assert_eq!(
        resolve_test_name(Some("main"), "at testkit::helper_test_ignored\nat app::test_kept"),
        "test_kept"
    );
    // Exact boundary length checks on sym.len() > 5 and non-alnum delimiters
    assert_eq!(
        resolve_test_name(
            Some(""),
            "at test_\nat test_a"
        ),
        "test_a"
    );
    assert_eq!(
        resolve_test_name(
            Some(""),
            "at test_name\nat mod::test_with_digits_123::{{closure}}"
        ),
        "test_with_digits_123"
    );
    assert_eq!(resolve_test_name(None, "no test symbols here"), "unknown_test");
}

#[test]
fn test_deep_detect_crate_name_and_exact_jsonl_bytes_under_env_lock() {
    let _guard = crate::tests::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prev_pkg = std::env::var_os("CARGO_PKG_NAME");
    let prev_dir = std::env::var_os("EVOMU_PARITY_ATTEST_DIR");

    let tmp_dir = unique_temp_dir("evomu_testkit_exact_bytes");
    std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &tmp_dir);

    // Non-existent nested path is a safe no-op and does not create directories
    let missing_nested = tmp_dir.join("does_not_exist_yet");
    std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &missing_nested);
    attest(&["r1"]);
    attest_named("pkg", "t1", &["r1"]);
    assert!(!missing_nested.exists());

    // Valid directory: verify exact byte-for-byte JSONL lines, trailing newline, and append semantics
    std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &tmp_dir);
    std::env::set_var("CARGO_PKG_NAME", "eve-custom-hyphen-pkg");
    assert_eq!(detect_crate_name(), "eve_custom_hyphen_pkg");

    attest_named("hyphen-explicit", "first_fn", &["a", "b"]);
    attest_named("hyphen-explicit", "second_fn", &["c"]);
    let raw_named = fs::read_to_string(tmp_dir.join("hyphen-explicit.jsonl")).expect("read named");
    assert_eq!(
        raw_named,
        "{\"test\":\"first_fn\",\"rows\":[\"a\",\"b\"]}\n{\"test\":\"second_fn\",\"rows\":[\"c\"]}\n"
    );

    // When thread name is "main" or worker-like, detect_test_name falls back cleanly and writes valid JSON
    attest(&["row_auto"]);
    let raw_auto =
        fs::read_to_string(tmp_dir.join("eve_custom_hyphen_pkg.jsonl")).expect("read auto");
    assert!(raw_auto.ends_with('\n'));
    let parsed: serde_json::Value =
        serde_json::from_str(raw_auto.trim_end_matches('\n')).expect("valid json");
    assert!(!parsed["test"].as_str().unwrap_or("").is_empty());
    assert_eq!(parsed["rows"], serde_json::json!(["row_auto"]));

    std::env::set_var("CARGO_PKG_NAME", "");
    assert_eq!(detect_crate_name(), "eve_workspace");
    std::env::remove_var("CARGO_PKG_NAME");
    assert_eq!(detect_crate_name(), "eve_workspace");

    match prev_pkg {
        Some(v) => std::env::set_var("CARGO_PKG_NAME", v),
        None => std::env::remove_var("CARGO_PKG_NAME"),
    }
    match prev_dir {
        Some(v) => std::env::set_var("EVOMU_PARITY_ATTEST_DIR", v),
        None => std::env::remove_var("EVOMU_PARITY_ATTEST_DIR"),
    }
    let _ = fs::remove_dir_all(&tmp_dir);
}
