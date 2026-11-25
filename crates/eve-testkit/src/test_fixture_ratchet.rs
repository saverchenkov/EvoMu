//! Per-file test fixture hygiene counters (`#1540`, `#1538` v2 schema, `#1496`).
//!
//! Tracks `crates/eve-server/tests/it/` for hand-built sessions / discarded RPC results and
//! `crates/eve-server/tests/`, test modules, and inline `#[cfg(test)]` blocks under
//! `crates/eve-server/src/` (excluding `mission_fixture.rs`) for direct mission store access
//! against `crates/eve-testkit/ratchets/test_fixture_ratchet.json` so new tests use
//! [`crate::scenario::ScenarioBuilder`], explicit RPC assertions, and `mission_fixture`.

#![forbid(unsafe_code)]

use crate::arch_debt_ratchet::validate_per_file_counts;
use crate::client_harness::{is_ident_byte, strip_rust_comments_and_literals};
use crate::notify_ratchet::{collect_rs_files_recursive, is_test_only_rel_path};
use crate::process_global_ratchet::cfg_test_item_ranges;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Per-pattern v2 baseline record matching the `#1538` ratchet schema.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FixturePatternBaseline {
    pub rule: String,
    pub zero_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owning_issue: Option<String>,
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

pub fn default_server_tests_dir() -> PathBuf { manifest_dir().join("../eve-server/tests") }

pub fn default_server_src_dir() -> PathBuf { manifest_dir().join("../eve-server/src") }

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
fn skip_ascii_ws_rev(b: &[u8], mut i: usize) -> usize {
    while i > 0 && b[i - 1].is_ascii_whitespace() { i -= 1; }
    i
}

#[inline]
fn ident_boundary(b: &[u8], start: usize, end: usize) -> bool {
    (start == 0 || !is_ident_byte(b[start - 1])) && (end >= b.len() || !is_ident_byte(b[end]))
}

/// Returns `true` iff `b[..pos]` ends with `->` followed by optional whitespace and path segments.
fn preceded_by_return_arrow(b: &[u8], pos: usize) -> bool {
    let mut k = pos;
    loop {
        k = skip_ascii_ws_rev(b, k);
        if k >= 2 && &b[k - 2..k] == b"::" {
            k = skip_ascii_ws_rev(b, k - 2);
            let ident_end = k;
            while k > 0 && is_ident_byte(b[k - 1]) { k -= 1; }
            if k < ident_end { continue; }
        }
        break;
    }
    k = skip_ascii_ws_rev(b, k);
    k >= 2 && &b[k - 2..k] == b"->"
}

/// Counts hand-built `SessionState { ... }` struct literals in stripped Rust bytes,
/// excluding function/closure return type annotations (`-> SessionState {` or `-> path::SessionState {`).
pub fn count_hand_built_sessions(src: &str) -> usize {
    let clean = strip_rust_comments_and_literals(src);
    let (b, needle, mut count, mut i) = (clean.as_bytes(), b"SessionState", 0usize, 0usize);
    while i + needle.len() <= b.len() {
        if &b[i..i + needle.len()] == needle && ident_boundary(b, i, i + needle.len()) {
            let j = skip_ascii_ws(b, i + needle.len());
            if j < b.len() && b[j] == b'{' && !preceded_by_return_arrow(b, i) { count += 1; }
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

fn stmt_has_rpc_call(stmt: &[u8]) -> bool {
    const PLAIN_RPCS: &[&[u8]] = &[
        b"dispatch_rpc",
        b"call_rpc",
        b"rpc",
        b"rpc_res",
        b"call_exchange",
        b"call_wire",
        b"wire_session_call_ok",
    ];
    let (n, mut i) = (stmt.len(), 0usize);
    while i < n {
        if !is_ident_byte(stmt[i]) { i += 1; continue; }
        let start = i;
        while i < n && is_ident_byte(stmt[i]) { i += 1; }
        let ident = &stmt[start..i];
        let j = skip_ascii_ws(stmt, i);
        if j < n && stmt[j] == b'(' {
            if PLAIN_RPCS.contains(&ident) { return true; }
            if ident == b"call" || ident == b"call_ok" {
                let mut p = start;
                while p > 0 && stmt[p - 1].is_ascii_whitespace() { p -= 1; }
                if p > 0 && stmt[p - 1] == b'.' { return true; }
            }
        }
    }
    false
}

/// Counts `let _ = <expr>;` and `let _name = <expr>;` statements whose initializer invokes an RPC call in stripped Rust source.
pub fn count_discarded_rpc_results(src: &str) -> usize {
    let clean = strip_rust_comments_and_literals(src);
    let (b, n, mut count, mut i) = (clean.as_bytes(), clean.len(), 0usize, 0usize);
    while i + 3 <= n {
        if &b[i..i + 3] == b"let" && ident_boundary(b, i, i + 3) {
            let j = skip_ascii_ws(b, i + 3);
            if j < n && b[j] == b'_' {
                let mut name_end = j + 1;
                while name_end < n && is_ident_byte(b[name_end]) { name_end += 1; }
                let j = skip_ascii_ws(b, name_end);
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

const DIRECT_MISSION_STORE_IDENTIFIERS: &[&[u8]] = &[
    b"active_missions",
    b"ActiveMissionRecord",
    b"load_mission_meta",
    b"save_mission_meta",
    b"MissionTimingAndCargoMeta",
    b"mission_initial_npc_counts",
    b"mission_initial_npc_types",
    b"missions_awaiting_downtime_respawn",
    b"mission_meta_note_id",
];

const DIRECT_MISSION_META_PREFIX: &[u8] = b"mission_meta:";

const DIRECT_MISSION_META_JSON_KEYS: &[&[u8]] = &[
    b"offered_at_unix",
    b"offer_expiry_unix",
    b"accepted_at_unix",
    b"accept_expiry_unix",
    b"bonus_expiry_unix",
    b"completed_at_unix",
    b"required_delivery_type_id",
    b"required_delivery_qty",
    b"granted_courier_item_id",
    b"granted_courier_qty",
    b"is_anomic_burner",
    b"chosen_branch",
    b"destination_station_id",
    b"origin_station_id",
    b"offered_level",
];

fn line_contains_mission_store_ident(line: &[u8]) -> bool {
    for &needle in DIRECT_MISSION_STORE_IDENTIFIERS {
        let mut i = 0usize;
        while i + needle.len() <= line.len() {
            if &line[i..i + needle.len()] == needle && ident_boundary(line, i, i + needle.len()) {
                return true;
            }
            i += 1;
        }
    }
    false
}

fn mark_line(hit_lines: &mut [bool], line: usize) {
    if let Some(slot) = hit_lines.get_mut(line) {
        *slot = true;
    }
}

fn record_mission_meta_literal_hits(content: &[u8], byte_lines: &[usize], hit_lines: &mut [bool]) {
    if content.is_empty() || byte_lines.is_empty() {
        return;
    }
    for (idx, w) in content.windows(DIRECT_MISSION_META_PREFIX.len()).enumerate() {
        if w == DIRECT_MISSION_META_PREFIX {
            mark_line(hit_lines, byte_lines[idx]);
        }
    }
    for &key in DIRECT_MISSION_META_JSON_KEYS {
        if content == key {
            mark_line(hit_lines, byte_lines[0]);
            continue;
        }
        if key.len() + 2 <= content.len() {
            let mut pos = 1usize;
            while pos + key.len() < content.len() {
                if content[pos - 1] == b'"'
                    && content[pos + key.len()] == b'"'
                    && &content[pos..pos + key.len()] == key
                {
                    mark_line(hit_lines, byte_lines[pos]);
                }
                pos += 1;
            }
        }
    }
}

/// Scans non-comment string/raw-string literals for `owner_notes` mission-metadata keys (`#1496`).
fn mark_mission_meta_literal_lines(src: &str, hit_lines: &mut [bool]) {
    let b = src.as_bytes();
    let n = b.len();
    let mut i = 0usize;
    let mut line = 0usize;

    while i < n {
        if b[i] == b'/' && i + 1 < n && b[i + 1] == b'/' {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i] == b'/' && i + 1 < n && b[i + 1] == b'*' {
            let mut depth = 1usize;
            i += 2;
            while i < n && depth > 0 {
                if i + 1 < n && b[i] == b'/' && b[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if i + 1 < n && b[i] == b'*' && b[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    if b[i] == b'\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
        } else if b[i] == b'r'
            && (i == 0
                || !is_ident_byte(b[i - 1])
                || ((b[i - 1] == b'b' || b[i - 1] == b'c')
                    && (i == 1 || !is_ident_byte(b[i - 2]))))
        {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while j < n && b[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && b[j] == b'"' {
                i = j + 1;
                let mut content = Vec::new();
                let mut byte_lines = Vec::new();
                while i < n {
                    if b[i] == b'"'
                        && i + hashes < n
                        && b[i + 1..=i + hashes].iter().all(|&c| c == b'#')
                    {
                        i += 1 + hashes;
                        break;
                    }
                    if b[i] == b'\n' {
                        line += 1;
                    }
                    content.push(b[i]);
                    byte_lines.push(line);
                    i += 1;
                }
                record_mission_meta_literal_hits(&content, &byte_lines, hit_lines);
            } else {
                i += 1;
            }
        } else if b[i] == b'\'' {
            if i + 1 < n && b[i + 1] == b'\\' {
                let mut k = i + 3;
                let limit = (i + 14).min(n);
                let mut closed = None;
                while k < limit {
                    if b[k] == b'\n' || b[k] == b'\r' {
                        break;
                    }
                    if b[k] == b'\'' {
                        closed = Some(k);
                        break;
                    }
                    k += 1;
                }
                if let Some(end) = closed {
                    i = end + 1;
                    continue;
                }
            } else if let Some(ch) = src[i + 1..].chars().next() {
                let close_idx = i + 1 + ch.len_utf8();
                if ch != '\\'
                    && ch != '\''
                    && ch != '\n'
                    && ch != '\r'
                    && close_idx < n
                    && b[close_idx] == b'\''
                {
                    i = close_idx + 1;
                    continue;
                }
            }
            i += 1;
        } else if b[i] == b'"' {
            i += 1;
            let mut content = Vec::new();
            let mut byte_lines = Vec::new();
            while i < n {
                let ch = b[i];
                if ch == b'\\' && i + 1 < n {
                    let esc = b[i + 1];
                    if esc == b'\n' {
                        line += 1;
                    } else {
                        let decoded = match esc {
                            b'n' => b'\n',
                            b'r' => b'\r',
                            b't' => b'\t',
                            b'\\' => b'\\',
                            b'"' => b'"',
                            b'\'' => b'\'',
                            other => other,
                        };
                        content.push(decoded);
                        byte_lines.push(line);
                    }
                    i += 2;
                    continue;
                }
                if ch == b'"' {
                    i += 1;
                    break;
                }
                if ch == b'\n' {
                    line += 1;
                }
                content.push(ch);
                byte_lines.push(line);
                i += 1;
            }
            record_mission_meta_literal_hits(&content, &byte_lines, hit_lines);
        } else {
            if b[i] == b'\n' {
                line += 1;
            }
            i += 1;
        }
    }
}

/// Counts lines in Rust source naming any direct mission-store identifier or direct
/// `owner_notes` mission-metadata JSON string literal outside comments (`#1496`).
pub fn count_direct_mission_store_access(src: &str) -> usize {
    let clean = strip_rust_comments_and_literals(src);
    let lines: Vec<&[u8]> = clean.as_bytes().split(|&b| b == b'\n').collect();
    let mut hit_lines = vec![false; lines.len()];
    for (idx, line) in lines.into_iter().enumerate() {
        if line_contains_mission_store_ident(line) {
            hit_lines[idx] = true;
        }
    }
    mark_mission_meta_literal_lines(src, &mut hit_lines);
    hit_lines.into_iter().filter(|&hit| hit).count()
}

/// Extracts only `#[cfg(test)]` attributed items from `raw` while preserving byte offsets and newlines.
fn extract_cfg_test_only_src(raw: &str) -> String {
    if !raw.contains("cfg") || !raw.contains("test") {
        return String::new();
    }
    let clean = strip_rust_comments_and_literals(raw);
    let ranges = cfg_test_item_ranges(&clean);
    if ranges.is_empty() {
        return String::new();
    }
    let raw_b = raw.as_bytes();
    let mut out = vec![b' '; raw_b.len()];
    for (i, &b) in raw_b.iter().enumerate() {
        if b == b'\n' {
            out[i] = b'\n';
        }
    }
    for (start, end) in ranges {
        let clamped_end = end.min(raw_b.len());
        if start <= clamped_end {
            out[start..clamped_end].copy_from_slice(&raw_b[start..clamped_end]);
        }
    }
    String::from_utf8(out).expect("ASCII masking outside #[cfg(test)] spans preserves UTF-8")
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

/// Scans `crates/eve-server/tests/`, test modules under `crates/eve-server/src/`, and inline
/// `#[cfg(test)]` blocks in non-test source files (excluding `mission_fixture.rs`) for lines
/// directly referencing mission persistence internals (`#1496`).
pub fn scan_direct_mission_store_access_counts(
    server_tests_dir: &Path,
    server_src_dir: &Path,
) -> Result<BTreeMap<String, usize>, String> {
    let mut observed = BTreeMap::new();
    let mut test_files = Vec::new();
    collect_rs_files_recursive(server_tests_dir, server_tests_dir, &mut test_files)?;
    test_files.sort_by(|a, b| a.0.cmp(&b.0));
    for (rel, abs) in test_files {
        let raw = std::fs::read_to_string(&abs)
            .map_err(|e| format!("failed reading {}: {e}", abs.display()))?;
        let count = count_direct_mission_store_access(&raw);
        if count > 0 {
            observed.insert(format!("crates/eve-server/tests/{rel}"), count);
        }
    }

    let mut src_files = Vec::new();
    collect_rs_files_recursive(server_src_dir, server_src_dir, &mut src_files)?;
    src_files.sort_by(|a, b| a.0.cmp(&b.0));
    for (rel, abs) in src_files {
        let Some(fname) = abs.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if fname == "mission_fixture.rs" {
            continue;
        }
        let raw = std::fs::read_to_string(&abs)
            .map_err(|e| format!("failed reading {}: {e}", abs.display()))?;
        let is_test_file = is_test_only_rel_path(&rel) || fname.contains("test");
        let count = if is_test_file {
            count_direct_mission_store_access(&raw)
        } else {
            let inline_tests = extract_cfg_test_only_src(&raw);
            if inline_tests.is_empty() {
                0
            } else {
                count_direct_mission_store_access(&inline_tests)
            }
        };
        if count > 0 {
            observed.insert(format!("crates/eve-server/src/{rel}"), count);
        }
    }
    Ok(observed)
}

/// Validates live `crates/eve-server/tests/` and unit-test fixture baselines against `test_fixture_ratchet.json`.
pub fn check_test_fixture_ratchet_with_dirs(
    ratchet_path: &Path,
    it_dir: &Path,
    server_tests_dir: Option<&Path>,
    server_src_dir: Option<&Path>,
) -> Result<(), String> {
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
    if let (Some(tests_dir), Some(src_dir)) = (server_tests_dir, server_src_dir) {
        let mission_pat = ratchet.patterns.get("direct_mission_store_access").ok_or("missing `direct_mission_store_access` pattern in test_fixture_ratchet.json")?;
        let mission_obs = scan_direct_mission_store_access_counts(tests_dir, src_dir)?;
        if let Err(msg) = validate_per_file_counts("direct_mission_store_access", mission_pat.zero_required, &mission_pat.files, &mission_obs) {
            errs.push(msg);
        }
    }
    if errs.is_empty() { Ok(()) } else { Err(errs.join("\n")) }
}

/// Validates live `crates/eve-server/tests/it/` (and mission test store baselines when sibling
/// `tests/` and `src/` directories exist) against `test_fixture_ratchet.json`.
pub fn check_test_fixture_ratchet(ratchet_path: &Path, it_dir: &Path) -> Result<(), String> {
    let tests_dir = it_dir.parent().filter(|p| p.is_dir());
    let src_dir = tests_dir
        .and_then(|p| p.parent())
        .map(|p| p.join("src"))
        .filter(|p| p.is_dir());
    check_test_fixture_ratchet_with_dirs(ratchet_path, it_dir, tests_dir, src_dir.as_deref())
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
            let _ = gm.call_wire("a", None, "F", &[], &kw).await;
            let _act_rep = harness.call_ok("dogmaIM", None, "Activate", &[], &kw).await;
            let _lock_rep = wire_session_call_ok(&mut s, "target", "AddTarget", &[]).await;
        "#;
        assert_eq!(scan_test_fixture_source(dirty_src), TestFixtureCounts { hand_built_sessions: 2, discarded_rpc_results: 7 });

        let return_type_only_src = r#"
            pub(crate) async fn login_demo(ctx: &ServerContext) -> SessionState {
                SessionState::new(1)
            }
            fn multiline(
                ctx: &ServerContext,
            ) ->
                SessionState
            {
                SessionState::new(2)
            }
            fn qualified() -> eve_server::router::SessionState {
                eve_server::router::SessionState::new(3)
            }
            fn global_path() -> ::eve_server::SessionState {
                SessionState::new(4)
            }
            fn with_comment() -> /* return type */ SessionState {
                SessionState::new(5)
            }
            let mk = |id: u64| -> SessionState { SessionState::new(id) };
        "#;
        assert_eq!(count_hand_built_sessions(return_type_only_src), 0);

        let mixed_return_and_literals_src = r#"
            fn make_sess(id: u64) -> SessionState {
                SessionState { session_id: id, ..Default::default() }
            }
            fn more_literals(flag: bool) -> eve_server::router::SessionState {
                let a = SessionState { session_id: 1 };
                let b = Some(eve_server::router::SessionState { session_id: 2 });
                let c = vec![a, SessionState { session_id: 3 }];
                let d = match flag {
                    true => SessionState { session_id: 4 },
                    false => return SessionState { session_id: 5 },
                };
                let _ = (b, c);
                d
            }
        "#;
        assert_eq!(count_hand_built_sessions(mixed_return_and_literals_src), 6);
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
    fn scanner_counts_direct_mission_store_access_by_line_and_ignores_clean_usage() {
        let clean_src = r#"
            // snap.active_missions.insert((char_id, mid), ActiveMissionRecord { ... });
            // note.note.contains("offer_expiry_unix") && label == "mission_meta:1:2"
            let msg = "MissionTimingAndCargoMeta load_mission_meta save_mission_meta";
            let assertion = "origin_station_id must match SDE station";
            let ok = Some(1).expect("granted_courier_item_id recorded");
            let inactive_missions = 1;
            let active_missions_count = 2;
            mission_fixture::insert(snap, MissionFixture::accepted(1, 2, 3, 4));
        "#;
        assert_eq!(count_direct_mission_store_access(clean_src), 0);

        let dirty_src = r##"
            use eve_db::ActiveMissionRecord;
            let _ = snap.active_missions.get(&(1, 2));
            let _ = (load_mission_meta(s, 1, 2), save_mission_meta(s, 1, &m));
            let _: Option<MissionTimingAndCargoMeta> = None;
            let _ = st.mission_initial_npc_counts.len();
            let _ = st.mission_initial_npc_types.len();
            let _ = st.missions_awaiting_downtime_respawn.len();
            let _ = mission_meta_note_id(1, 2);
            let _ = note.label.starts_with("mission_meta:");
            let _ = val["offer_expiry_unix"].as_u64();
            let _ = (val.get("accept_expiry_unix"), val.get("bonus_expiry_unix"));
            let _ = (val.get("required_delivery_type_id"), val.get("required_delivery_qty"));
            let _ = (val.get("granted_courier_item_id"), val.get("granted_courier_qty"));
            let _ = "{\"offered_at_unix\": 10}";
            let _ = r#"{"destination_station_id": 60003760}"#;
        "##;
        assert_eq!(count_direct_mission_store_access(dirty_src), 15);
    }

    #[test]
    fn inline_cfg_test_extraction_scans_only_cfg_test_blocks_in_non_test_files() {
        let prod_and_inline_test = r#"
            #[cfg(test)]
            use eve_proto::PyRep;

            pub fn prod_fn(snap: &WorldStateSnapshot) -> usize {
                snap.active_missions.len()
            }

            #[cfg(test)]
            mod tests {
                #[test]
                fn direct_probe() {
                    let _ = snap.active_missions.len();
                    let _ = note.note.contains("offer_expiry_unix");
                }
            }
        "#;
        let inline_only = extract_cfg_test_only_src(prod_and_inline_test);
        assert_eq!(count_direct_mission_store_access(&inline_only), 2);

        let clean_inline = r#"
            #[cfg(test)]
            use eve_proto::PyRep;

            pub fn prod_fn(snap: &WorldStateSnapshot) -> usize {
                snap.active_missions.len()
            }

            #[cfg(test)]
            mod tests {
                #[test]
                fn uses_fixture() {
                    let _ = mission_fixture::list(snap);
                }
            }
        "#;
        assert_eq!(count_direct_mission_store_access(&extract_cfg_test_only_src(clean_inline)), 0);
    }

    #[test]
    fn validate_per_file_counts_rejects_new_or_increased_direct_mission_store_access() {
        let baseline = BTreeMap::from([("crates/eve-server/tests/it/a.rs".to_string(), 1usize)]);
        let worse = BTreeMap::from([
            ("crates/eve-server/tests/it/a.rs".to_string(), 2usize),
            ("crates/eve-server/src/services/new_tests.rs".to_string(), 1usize),
        ]);
        let err = validate_per_file_counts("direct_mission_store_access", false, &baseline, &worse)
            .expect_err("regressions must fail");
        assert!(err.contains("a.rs") && err.contains("new_tests.rs"), "unexpected msg: {err}");
        validate_per_file_counts("direct_mission_store_access", false, &baseline, &BTreeMap::new())
            .expect("reductions must pass");
    }

    #[test]
    fn live_tests_it_meets_test_fixture_ratchet_baselines() {
        let (ratchet_path, it_dir, tests_dir, src_dir) = (
            default_ratchet_path(),
            default_it_dir(),
            default_server_tests_dir(),
            default_server_src_dir(),
        );
        if !it_dir.is_dir() { return; }
        assert!(tests_dir.is_dir() && src_dir.is_dir(), "expected crates/eve-server tests and src dirs");
        check_test_fixture_ratchet_with_dirs(&ratchet_path, &it_dir, Some(&tests_dir), Some(&src_dir))
            .unwrap_or_else(|e| panic!("test_fixture_ratchet regression:\n{e}"));
    }
}
