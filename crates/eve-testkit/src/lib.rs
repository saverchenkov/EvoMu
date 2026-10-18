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
    if let Ok(pkg) = std::env::var("CARGO_PKG_NAME") {
        if !pkg.is_empty() {
            return pkg.replace('-', "_");
        }
    }
    "eve_workspace".to_string()
}

fn detect_test_name() -> String {
    // 1. In standard cargo test runners, thread name is set to the test name, e.g.
    // "test_foo" or "tests::test_foo" or "eve_sde::tests::test_real_sde...".
    if let Some(name) = std::thread::current().name() {
        if name != "main" && !name.starts_with("tokio-runtime-worker") {
            let leaf = name.rsplit("::").next().unwrap_or(name);
            return leaf.to_string();
        }
    }

    // 2. If thread name is generic (e.g. inside tokio worker), inspect backtrace.
    let bt = std::backtrace::Backtrace::capture();
    let bt_str = format!("{bt:?}");
    for line in bt_str.lines() {
        if let Some(idx) = line.find("test_") {
            let candidate = &line[idx..];
            let end = candidate
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(candidate.len());
            let sym = &candidate[..end];
            if !sym.is_empty() && sym != "test_name" && !sym.contains("testkit") {
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
    fn test_attest_noop_when_env_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("EVOMU_PARITY_ATTEST_DIR");

        let probe_dir = std::env::temp_dir().join(format!("testkit_noop_probe_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&probe_dir);
        let _ = std::fs::create_dir_all(&probe_dir);

        // Neither attest nor attest_named should perform any writes when env var is unset
        attest(&["sde:res:/staticdata/types.fsdbinary"]);
        attest_named("eve_testkit", "test_noop", &["sde:res:/staticdata/types.fsdbinary"]);

        let file_count = std::fs::read_dir(&probe_dir).unwrap().count();
        assert_eq!(file_count, 0, "No files must be written when EVOMU_PARITY_ATTEST_DIR is unset");

        // When pointing to empty string or non-existent path, must also be a safe no-op
        std::env::set_var("EVOMU_PARITY_ATTEST_DIR", "");
        attest(&["sde:res:/staticdata/types.fsdbinary"]);
        std::env::set_var("EVOMU_PARITY_ATTEST_DIR", probe_dir.join("nonexistent_subdir"));
        attest(&["sde:res:/staticdata/types.fsdbinary"]);

        let file_count_after = std::fs::read_dir(&probe_dir).unwrap().count();
        assert_eq!(file_count_after, 0, "No files must be created for invalid or empty dirs");

        std::env::remove_var("EVOMU_PARITY_ATTEST_DIR");
        let _ = std::fs::remove_dir_all(&probe_dir);
    }

    #[test]
    fn test_attest_writes_jsonl_when_env_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("testkit_attest_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::create_dir_all(&tmp);
        std::env::set_var("EVOMU_PARITY_ATTEST_DIR", &tmp);

        // 1. Directly test attest() with automatic crate and test name detection
        attest(&["sde:res:/staticdata/types.fsdbinary", "plan:T-1"]);

        // 2. Test explicit attest_named()
        attest_named("custom_crate", "custom_test", &["feat:test.attest_named"]);

        let files: Vec<_> = std::fs::read_dir(&tmp)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(files.len(), 2, "Expected 2 jsonl files (detected crate + custom_crate)");

        let detected_file = tmp.join(format!("{}.jsonl", detect_crate_name()));
        assert!(detected_file.exists(), "Detected crate file should exist");
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
}
