//! Per-file integration-test fixture hygiene counters (`#1540`, `#1538` v2 schema).
//!
//! Tracks `crates/eve-server/tests/it/` against `crates/eve-testkit/ratchets/test_fixture_ratchet.json`
//! so new tests use [`crate::scenario::ScenarioBuilder`] and explicit RPC assertions rather than
//! hand-built `SessionState { ... }` literals or `let _ =` discarded RPC calls.

#![forbid(unsafe_code)]

use crate::arch_debt_ratchet::validate_per_file_counts;
use crate::client_harness::{is_ident_byte, strip_rust_comments_and_literals};
use crate::notify_ratchet::collect_rs_files_recursive;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Per-pattern v2 baseline record matching the `#1538` ratchet schema.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FixturePatternBaseline {
    pub rule: String,
    pub zero_required: bool,
    pub files: BTreeMap<String, usize>,
}

/// Top-level per-file ratchet document for `#1540` test hygiene counters.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TestFixtureRatchet {
    pub version: u32,
    pub owning_issue: String,
    pub description: String,
    pub patterns: BTreeMap<String, FixturePatternBaseline>,
}

/// Observed counts in one source string for both `#1540` fixture-hygiene patterns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TestFixtureCounts {
    pub hand_built_sessions: usize,
    pub discarded_rpc_results: usize,
}

fn manifest_dir() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")) }

pub fn default_ratchet_path() -> PathBuf { manifest_dir().join("ratchets/test_fixture_ratchet.json") }

pub fn default_it_dir() -> PathBuf { manifest_dir().join("../eve-server/tests/it") }

pub fn load_test_fixture_ratchet(path: &Path) -> Result<TestFixtureRatchet, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    let parsed: TestFixtureRatchet = serde_json::from_str(&raw).map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
    if parsed.version != 2 {
        return Err(format!("{}: expected version 2 ratchet, found {}", path.display(), parsed.version));
    }
    Ok(parsed)
}

#[inline]
fn skip_ascii_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() { i += 1; }
    i
}

#[inline]
fn ident_boundary(b: &[u8], start: usize, end: usize) -> bool {
    (start == 0 || !is_ident_byte(b[start - 1])) && (end >= b.len() || !is_ident_byte(b[end]))
}

/// Counts hand-built `SessionState { ... }` struct literals in stripped Rust bytes.
pub fn count_hand_built_sessions(src: &str) -> usize {
    let clean = strip_rust_comments_and_literals(src);
    let (b, needle, mut count, mut i) = (clean.as_bytes(), b"SessionState", 0usize, 0usize);
    while i + needle.len() <= b.len() {
        if &b[i..i + needle.len()] == needle && ident_boundary(b, i, i + needle.len()) {
            let j = skip_ascii_ws(b, i + needle.len());
            if j < b.len() && b[j] == b'{' { count += 1; }
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

fn stmt_has_rpc_call(stmt: &[u8]) -> bool {
    const PLAIN_RPCS: &[&[u8]] = &[b"dispatch_rpc", b"call_rpc", b"rpc", b"rpc_res", b"call_exchange"];
    let (n, mut i) = (stmt.len(), 0usize);
    while i < n {
        if !is_ident_byte(stmt[i]) { i += 1; continue; }
        let start = i;
        while i < n && is_ident_byte(stmt[i]) { i += 1; }
        let ident = &stmt[start..i];
        let j = skip_ascii_ws(stmt, i);
        if j < n && stmt[j] == b'(' {
            if PLAIN_RPCS.contains(&ident) { return true; }
            if ident == b"call" {
                let mut p = start;
                while p > 0 && stmt[p - 1].is_ascii_whitespace() { p -= 1; }
                if p > 0 && stmt[p - 1] == b'.' { return true; }
            }
        }
    }
    false
}

/// Counts `let _ = <expr>;` statements whose initializer invokes an RPC call in stripped Rust source.
pub fn count_discarded_rpc_results(src: &str) -> usize {
    let clean = strip_rust_comments_and_literals(src);
    let (b, n, mut count, mut i) = (clean.as_bytes(), clean.len(), 0usize, 0usize);
    while i + 3 <= n {
        if &b[i..i + 3] == b"let" && ident_boundary(b, i, i + 3) {
            let j = skip_ascii_ws(b, i + 3);
            if j < n && b[j] == b'_' && (j + 1 >= n || !is_ident_byte(b[j + 1])) {
                let j = skip_ascii_ws(b, j + 1);
                if j < n && b[j] == b'=' && (j + 1 >= n || (b[j + 1] != b'=' && b[j + 1] != b'>')) {
                    let (mut k, mut pd, mut bd, mut cd) = (j + 1, 0usize, 0usize, 0usize);
                    while k < n {
                        match b[k] {
                            b'(' => pd += 1,
                            b')' => pd = pd.saturating_sub(1),
                            b'[' => bd += 1,
                            b']' => bd = bd.saturating_sub(1),
                            b'{' => cd += 1,
                            b'}' if cd == 0 => break,
                            b'}' => cd -= 1,
                            b';' if pd == 0 && bd == 0 && cd == 0 => break,
                            _ => {}
                        }
                        k += 1;
                    }
                    if stmt_has_rpc_call(&b[j + 1..k]) { count += 1; }
                    i = k.saturating_add(1);
                    continue;
                }
            }
        }
        i += 1;
    }
    count
}

/// Counts both `#1540` fixture-hygiene patterns in one source string.
pub fn scan_test_fixture_source(src: &str) -> TestFixtureCounts {
    TestFixtureCounts { hand_built_sessions: count_hand_built_sessions(src), discarded_rpc_results: count_discarded_rpc_results(src) }
}

/// Scans every `.rs` file under `it_dir` and returns per-file counts keyed relative to repo root.
pub fn scan_it_fixture_counts(it_dir: &Path) -> Result<(BTreeMap<String, usize>, BTreeMap<String, usize>), String> {
    let mut files = Vec::new();
    collect_rs_files_recursive(it_dir, it_dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let (mut sessions, mut discards) = (BTreeMap::new(), BTreeMap::new());
    for (rel, abs) in files {
        let raw = std::fs::read_to_string(&abs).map_err(|e| format!("failed reading {}: {e}", abs.display()))?;
        let key = format!("crates/eve-server/tests/it/{rel}");
        let counts = scan_test_fixture_source(&raw);
        if counts.hand_built_sessions > 0 { sessions.insert(key.clone(), counts.hand_built_sessions); }
        if counts.discarded_rpc_results > 0 { discards.insert(key, counts.discarded_rpc_results); }
    }
    Ok((sessions, discards))
}

/// Validates live `crates/eve-server/tests/it/` counts against `test_fixture_ratchet.json`.
pub fn check_test_fixture_ratchet(ratchet_path: &Path, it_dir: &Path) -> Result<(), String> {
    let ratchet = load_test_fixture_ratchet(ratchet_path)?;
    let (sessions_obs, discards_obs) = scan_it_fixture_counts(it_dir)?;
    let sess_pat = ratchet.patterns.get("hand_built_sessions").ok_or("missing `hand_built_sessions` pattern in test_fixture_ratchet.json")?;
    let disc_pat = ratchet.patterns.get("discarded_rpc_results").ok_or("missing `discarded_rpc_results` pattern in test_fixture_ratchet.json")?;
    let mut errs = Vec::new();
    if let Err(msg) = validate_per_file_counts("hand_built_sessions", sess_pat.zero_required, &sess_pat.files, &sessions_obs) {
        errs.push(msg);
    }
    if let Err(msg) = validate_per_file_counts("discarded_rpc_results", disc_pat.zero_required, &disc_pat.files, &discards_obs) {
        errs.push(msg);
    }
    if errs.is_empty() { Ok(()) } else { Err(errs.join("\n")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_detects_hand_built_sessions_and_discarded_rpcs_but_ignores_clean_patterns() {
        let clean_src = r#"
            // SessionState { comment }
            // let _ = ctx.dispatch_rpc(&mut s, "svc", None, "M", &[], &kw, None).await;
            let text = "SessionState { literal } let _ = dispatch_rpc();";
            let mut sess = SessionState::new(100);
            let rep = harness.call_ok("account", None, "GetCashBalance", &[], &kw).await;
            let _unused_local = 42;
            let _ = std::fs::remove_file("/tmp/none");
        "#;
        assert_eq!(scan_test_fixture_source(clean_src), TestFixtureCounts::default());

        let dirty_src = r#"
            let s1 = SessionState { session_id: 1, ..Default::default() };
            let s2 = SessionState { session_id: 2, ..Default::default() };
            let _ = ctx.dispatch_rpc(&mut sess, "a", None, "B", &[], &kw, None).await;
            let _ = ctx.dispatch_rpc(&mut sess, "a", None, "C", &[], &kw, None).await.expect("C");
            let _ = harness.call("a", None, "D", &[], &kw).await;
            let _ = call_rpc(&ctx, &mut sess, "a", "E", &[]).await;
        "#;
        assert_eq!(scan_test_fixture_source(dirty_src), TestFixtureCounts { hand_built_sessions: 2, discarded_rpc_results: 4 });
    }

    #[test]
    fn validate_per_file_counts_rejects_new_or_increased_debt_for_test_fixtures() {
        let baseline = BTreeMap::from([("crates/eve-server/tests/it/a.rs".to_string(), 1usize)]);
        let worse = BTreeMap::from([("crates/eve-server/tests/it/a.rs".to_string(), 2usize), ("crates/eve-server/tests/it/new.rs".to_string(), 1usize)]);
        let err = validate_per_file_counts("hand_built_sessions", false, &baseline, &worse).expect_err("regressions must fail");
        assert!(err.contains("a.rs") && err.contains("new.rs"), "unexpected msg: {err}");
        let reduced = BTreeMap::from([("crates/eve-server/tests/it/a.rs".to_string(), 1usize)]);
        validate_per_file_counts("hand_built_sessions", false, &baseline, &reduced).expect("matching baseline passes");
    }

    #[test]
    fn live_tests_it_meets_test_fixture_ratchet_baselines() {
        let (ratchet_path, it_dir) = (default_ratchet_path(), default_it_dir());
        if !it_dir.is_dir() { return; }
        check_test_fixture_ratchet(&ratchet_path, &it_dir).unwrap_or_else(|e| panic!("test_fixture_ratchet regression:\n{e}"));
    }
}
