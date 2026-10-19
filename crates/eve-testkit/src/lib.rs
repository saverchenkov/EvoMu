//! `eve-testkit` — Test & Parity Acceptance Support Kit.
//!
//! Provides runtime attestation for data-driven tests against the EvoMu parity ledger
//! via `$EVOMU_PARITY_ATTEST_DIR`.

#![forbid(unsafe_code)]

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Attest that the calling test function covers the specified ledger row IDs.
///
/// If `$EVOMU_PARITY_ATTEST_DIR` is set and points to an existing directory,
/// appends a JSON line:
/// `{"test": "<test_fn_name>", "rows": ["row_1", "row_2", ...]}`
/// to `$EVOMU_PARITY_ATTEST_DIR/<crate_name>.jsonl`.
///
/// If `$EVOMU_PARITY_ATTEST_DIR` is unset, empty, or not a directory, this is a fast no-op.
pub fn attest(rows: &[&str]) {
    if rows.is_empty() {
        return;
    }
    let Some(dir) = std::env::var_os("EVOMU_PARITY_ATTEST_DIR") else {
        return;
    };
    if dir.is_empty() {
        return;
    }
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return;
    }

    let test_name = detect_test_name();
    let crate_name = detect_crate_name();
    write_attestation(&dir, &crate_name, &test_name, rows);
}

/// Explicit test attestation when caller provides crate and test name.
pub fn attest_named(crate_name: &str, test_name: &str, rows: &[&str]) {
    if rows.is_empty() {
        return;
    }
    let Some(dir) = std::env::var_os("EVOMU_PARITY_ATTEST_DIR") else {
        return;
    };
    if dir.is_empty() {
        return;
    }
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return;
    }
    write_attestation(&dir, crate_name, test_name, rows);
}

fn detect_crate_name() -> String {
    resolve_crate_name(std::env::var("CARGO_PKG_NAME").ok().as_deref())
}

fn resolve_crate_name(pkg_opt: Option<&str>) -> String {
    if let Some(pkg) = pkg_opt {
        if !pkg.is_empty() {
            return pkg.replace('-', "_");
        }
    }
    "eve_workspace".to_string()
}

fn detect_test_name() -> String {
    let bt = std::backtrace::Backtrace::force_capture();
    let bt_str = format!("{bt:?}");
    resolve_test_name(std::thread::current().name(), &bt_str)
}

fn resolve_test_name(thread_name: Option<&str>, bt_str: &str) -> String {
    if let Some(name) = thread_name {
        if !name.is_empty() && name != "main" && !name.starts_with("tokio-runtime-worker") {
            let leaf = name.rsplit("::").next().unwrap_or(name);
            return leaf.to_string();
        }
    }

    for line in bt_str.lines() {
        if let Some(idx) = line.find("test_") {
            let candidate = &line[idx..];
            let end = candidate
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(candidate.len());
            let sym = &candidate[..end];
            if sym.len() > 5 && sym != "test_name" && !line.contains("testkit") {
                return sym.to_string();
            }
        }
    }

    "unknown_test".to_string()
}

fn write_attestation(dir: &Path, crate_name: &str, test_name: &str, rows: &[&str]) {
    #[derive(serde::Serialize)]
    struct Record<'a> {
        test: &'a str,
        rows: &'a [&'a str],
    }

    let record = Record {
        test: test_name,
        rows,
    };
    let json_line = match serde_json::to_string(&record) {
        Ok(mut s) => {
            s.push('\n');
            s
        }
        Err(_) => return,
    };

    let filename = format!("{crate_name}.jsonl");
    let target = dir.join(filename);

    let _guard = WRITE_LOCK.lock().unwrap();
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(target) {
        let _ = f.write_all(json_line.as_bytes());
        let _ = f.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    // parity: plan:T-1
    fn test_attest_noop_when_env_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("EVOMU_PARITY_ATTEST_DIR");

        let probe_dir =
            std::env::temp_dir().join(format!("testkit_noop_probe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&probe_dir);
        let _ = std::fs::create_dir_all(&probe_dir);

        // Set EVOMU_PARITY_ATTEST_DIR to probe_dir first to verify empty rows slice is a no-op
        std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &probe_dir);
        attest(&[]);
        attest_named("eve_testkit", "test_noop", &[]);
        assert_eq!(
            std::fs::read_dir(&probe_dir).unwrap().count(),
            0,
            "Empty rows slice must not create any JSONL file"
        );

        // When EVOMU_PARITY_ATTEST_DIR is unset, empty, or points to a file/nonexistent path, no-op
        std::env::remove_var("EVOMU_PARITY_ATTEST_DIR");
        attest(&["sde:res:/staticdata/types.fsdbinary"]);
        attest_named(
            "eve_testkit",
            "test_noop",
            &["sde:res:/staticdata/types.fsdbinary"],
        );

        std::env::set_var("EVOMU_PARITY_ATTEST_DIR", "");
        attest(&["sde:res:/staticdata/types.fsdbinary"]);
        attest_named(
            "eve_testkit",
            "test_noop",
            &["sde:res:/staticdata/types.fsdbinary"],
        );

        let non_dir = probe_dir.join("nonexistent_subdir");
        std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &non_dir);
        attest(&["sde:res:/staticdata/types.fsdbinary"]);
        attest_named(
            "eve_testkit",
            "test_noop",
            &["sde:res:/staticdata/types.fsdbinary"],
        );
        assert!(!non_dir.exists());
        assert_eq!(
            std::fs::read_dir(&probe_dir).unwrap().count(),
            0,
            "No files must be created for unset, empty, or non-directory paths"
        );

        std::env::remove_var("EVOMU_PARITY_ATTEST_DIR");
        let _ = std::fs::remove_dir_all(&probe_dir);
    }

    #[test]
    // parity: plan:T-1
    fn test_attest_writes_jsonl_when_env_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("testkit_attest_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::create_dir_all(&tmp);
        std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &tmp);

        // 1. Directly test attest() with automatic crate ("eve_testkit") and test name detection
        attest(&["sde:res:/staticdata/types.fsdbinary", "plan:T-1"]);

        // 2. Test explicit attest_named()
        attest_named("custom_crate", "custom_test", &["feat:test.attest_named"]);

        let files: Vec<_> = std::fs::read_dir(&tmp)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(
            files.len(),
            2,
            "Expected 2 jsonl files (eve_testkit + custom_crate)"
        );

        let detected_file = tmp.join("eve_testkit.jsonl");
        assert!(detected_file.exists(), "eve_testkit.jsonl must exist");
        let detected_content = std::fs::read_to_string(&detected_file).unwrap();
        assert!(
            detected_content.contains("\"test\":\"test_attest_writes_jsonl_when_env_set\""),
            "Expected detected test name in record: {detected_content}"
        );
        assert!(detected_content.contains("sde:res:/staticdata/types.fsdbinary"));
        assert!(detected_content.contains("plan:T-1"));

        let custom_file = tmp.join("custom_crate.jsonl");
        assert!(custom_file.exists(), "Custom crate file should exist");
        let custom_content = std::fs::read_to_string(&custom_file).unwrap();
        assert!(custom_content.contains("\"test\":\"custom_test\""));
        assert!(custom_content.contains("feat:test.attest_named"));

        std::env::remove_var("EVOMU_PARITY_ATTEST_DIR");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_resolve_crate_and_test_name_edge_cases() {
        assert_eq!(resolve_crate_name(Some("eve-sde")), "eve_sde");
        assert_eq!(resolve_crate_name(Some("")), "eve_workspace");
        assert_eq!(resolve_crate_name(None), "eve_workspace");
        assert_eq!(detect_crate_name(), "eve_testkit");

        assert_eq!(
            resolve_test_name(Some("eve_sde::tests::test_real_sde"), ""),
            "test_real_sde"
        );
        assert_eq!(
            resolve_test_name(
                Some("main"),
                "0: test_name\n1: eve_testkit::test_helper\n2: test_\n3: my_mod::test_tokio_worker_case::{{closure}}"
            ),
            "test_tokio_worker_case"
        );
        assert_eq!(
            resolve_test_name(
                Some("tokio-runtime-worker"),
                "frame 0: crate::tests::test_async_rpc_handler()"
            ),
            "test_async_rpc_handler"
        );
        assert_eq!(
            resolve_test_name(Some(""), "frame 0: no_matching_symbol"),
            "unknown_test"
        );
        assert_eq!(resolve_test_name(None, ""), "unknown_test");
    }
}
