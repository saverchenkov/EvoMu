//! Falling per-file structural ratchet for `crates/eve-server/src/router/client_compat.rs`,
//! `crates/eve-server/src/outbound_sequencer.rs` post-producer `SetState` rewrites (`RT-92`),
//! and `:26000` wire-transport bypass guards (`#1189`, `RFC-3 C-9`, `#1538`).
//!
//! Stores per-file upper-bound ceilings (`counters`) and subset-only allowlists (`known_rules`,
//! `allowed_service_caller_trace_files`, `allowed_it_dispatch_rpc_files`) under schema version 2
//! so deletions/reductions never fail while `service_caller_trace_comparisons` remains strictly
//! pinned at `0` across all files and introducing `ctx.dispatch_rpc(` into new
//! `crates/eve-server/tests/it/*.rs` files is forbidden.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::arch_debt_ratchet::{validate_per_file_baseline, validate_per_file_counts};
use crate::client_harness::{
    is_ident_byte, scan_raw_dispatch_calls_in_dir, strip_rust_comments_and_literals,
};
use crate::notify_ratchet::{
    collect_rs_files_recursive, is_fn_def_header, is_test_only_rel_path, strip_cfg_test_items,
};

const DIRECT_LAYER_FNS: &[&str] = &[
    "normalize_macho_rpc_reply_at",
    "normalize_macho_rpc_reply",
    "normalize_macho_notification_with_sde",
    "normalize_macho_notification",
    "should_suppress_macho_notification",
];

/// Required counter keys in v2 `client_compat_ratchet.json`.
pub const CLIENT_COMPAT_COUNTER_KEYS: [&str; 10] = [
    "rpc_rules",
    "suppress_rules",
    "notify_arms",
    "destiny_rewrites",
    "production_lines",
    "transport_conditionals",
    "direct_layer_test_calls",
    "service_caller_trace_comparisons",
    "service_caller_trace_params",
    "sequencer_set_state_rewrites",
];

/// Static scan summary for `crates/eve-server` and `crates/eve-server/tests/it`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientCompatScanReport {
    pub rpc_rules: usize,
    pub suppress_rules: usize,
    pub notify_arms: usize,
    pub destiny_rewrites: usize,
    pub production_lines: usize,
    pub transport_conditionals: usize,
    pub direct_layer_test_calls: usize,
    pub service_caller_trace_comparisons: usize,
    pub service_caller_trace_params: usize,
    pub sequencer_set_state_rewrites: usize,
    pub counters: BTreeMap<String, BTreeMap<String, usize>>,
    pub known_rules: BTreeSet<String>,
    pub service_caller_trace_files: BTreeSet<String>,
    pub it_dispatch_rpc_files: BTreeSet<String>,
}

/// Falling v2 per-file ratchet stored in `crates/eve-testkit/ratchets/client_compat_ratchet.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientCompatRatchet {
    pub version: u32,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub owning_issue: Option<u64>,
    pub counters: BTreeMap<String, BTreeMap<String, usize>>,
    pub max_transport_differing_replies: Option<usize>,
    pub known_rules: Vec<String>,
    pub allowed_service_caller_trace_files: Vec<String>,
    pub allowed_it_dispatch_rpc_files: Vec<String>,
}

impl ClientCompatRatchet {
    /// Returns the repository path to `crates/eve-testkit/ratchets/client_compat_ratchet.json`.
    pub fn default_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ratchets")
            .join("client_compat_ratchet.json")
    }

    /// Returns the repository path to `crates/eve-server`.
    pub fn default_server_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../eve-server")
    }

    /// Returns the repository path to `crates/eve-server/tests/it`.
    pub fn default_it_tests_dir() -> PathBuf {
        Self::default_server_dir().join("tests/it")
    }

    /// Loads `client_compat_ratchet.json` from its default repository path.
    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&Self::default_path())
    }

    /// Loads a [`ClientCompatRatchet`] from `path` and validates v2 schema invariants.
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let parsed: Self = serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        parsed.validate_schema()?;
        Ok(parsed)
    }

    /// Returns the summed baseline across all files for `counter_key`.
    pub fn counter_sum(&self, counter_key: &str) -> usize {
        self.counters
            .get(counter_key)
            .map(|m| m.values().sum())
            .unwrap_or(0)
    }

    /// Scans `crates/eve-server` and `crates/eve-server/tests/it` and validates all caps
    /// and rule/file allowlists.
    pub fn verify_default(&self) -> Result<ClientCompatScanReport, String> {
        self.verify_dirs(&Self::default_server_dir(), &Self::default_it_tests_dir())
    }

    /// Scans `server_dir` and `it_dir` and asserts [`ClientCompatRatchet::assert_ratchet`].
    pub fn verify_dirs(
        &self,
        server_dir: &Path,
        it_dir: &Path,
    ) -> Result<ClientCompatScanReport, String> {
        let report = scan_client_compat_in_dirs(server_dir, it_dir)?;
        self.assert_ratchet(&report)?;
        Ok(report)
    }

    fn validate_schema(&self) -> Result<(), String> {
        if self.version != 2 {
            return Err(format!(
                "Unsupported client_compat_ratchet version {} (expected 2)",
                self.version
            ));
        }
        assert_sorted_unique("known_rules", &self.known_rules)?;
        assert_sorted_unique(
            "allowed_service_caller_trace_files",
            &self.allowed_service_caller_trace_files,
        )?;
        assert_sorted_unique(
            "allowed_it_dispatch_rpc_files",
            &self.allowed_it_dispatch_rpc_files,
        )?;
        for key in CLIENT_COMPAT_COUNTER_KEYS {
            let baseline = self.counters.get(key).ok_or_else(|| {
                format!("client_compat_ratchet.json missing required counter '{key}'")
            })?;
            validate_per_file_baseline(
                &format!("client_compat counter '{key}'"),
                is_zero_required_compat_counter(key),
                baseline,
            )?;
        }
        for key in self.counters.keys() {
            if !CLIENT_COMPAT_COUNTER_KEYS.contains(&key.as_str()) {
                return Err(format!(
                    "client_compat_ratchet.json contains unknown counter '{key}'"
                ));
            }
        }
        Ok(())
    }

    /// Enforces v2 falling-ratchet invariants on `report`:
    /// - per-file counter caps (`observed[file] <= baseline[file]`, `0` if unlisted);
    /// - `service_caller_trace_comparisons` remains strictly `0` across all files;
    /// - `known_rules`, `service_caller_trace_files`, and `it_dispatch_rpc_files` must be subsets
    ///   of their allowlists (removals are permitted; additions are rejected).
    pub fn assert_ratchet(&self, report: &ClientCompatScanReport) -> Result<(), String> {
        self.validate_schema()?;

        let empty = BTreeMap::new();
        for key in CLIENT_COMPAT_COUNTER_KEYS {
            let baseline = self
                .counters
                .get(key)
                .expect("validated by validate_schema");
            let observed = report.counters.get(key).unwrap_or(&empty);
            validate_per_file_counts(
                &format!("client_compat counter '{key}'"),
                is_zero_required_compat_counter(key),
                baseline,
                observed,
            )?;
        }

        assert_allowlist_subset(
            &report.known_rules,
            &self.known_rules,
            "Unlisted client_compat rules added without updating client_compat_ratchet.json",
        )?;
        assert_allowlist_subset(
            &report.service_caller_trace_files,
            &self.allowed_service_caller_trace_files,
            "New caller_trace usage appeared in crates/eve-server/src/services/",
        )?;
        assert_allowlist_subset(
            &report.it_dispatch_rpc_files,
            &self.allowed_it_dispatch_rpc_files,
            "Integration test files outside allowed_it_dispatch_rpc_files contain .dispatch_rpc( (use ClientHarness instead)",
        )?;

        Ok(())
    }
}

fn is_zero_required_compat_counter(key: &str) -> bool {
    key == "service_caller_trace_comparisons"
}

fn assert_allowlist_subset(
    observed: &BTreeSet<String>,
    allowed: &[String],
    msg_prefix: &str,
) -> Result<(), String> {
    let allowed_set: BTreeSet<String> = allowed.iter().cloned().collect();
    let unexpected: Vec<&String> = observed.difference(&allowed_set).collect();
    if !unexpected.is_empty() {
        return Err(format!("{msg_prefix}: {unexpected:?}"));
    }
    Ok(())
}

fn assert_sorted_unique(field: &str, items: &[String]) -> Result<(), String> {
    for pair in items.windows(2) {
        if pair[0] >= pair[1] {
            return Err(format!(
                "{field} in client_compat_ratchet.json must be strictly sorted and deduplicated (found {:?} before {:?})",
                pair[0], pair[1]
            ));
        }
    }
    Ok(())
}

/// Scans `server_dir` (`crates/eve-server`) and `it_dir` (`crates/eve-server/tests/it`)
/// and builds a [`ClientCompatScanReport`].
pub fn scan_client_compat_in_dirs(
    server_dir: &Path,
    it_dir: &Path,
) -> Result<ClientCompatScanReport, String> {
    let mut report = ClientCompatScanReport::default();
    for key in CLIENT_COMPAT_COUNTER_KEYS {
        report.counters.insert(key.to_string(), BTreeMap::new());
    }

    let compat_path = server_dir.join("src/router/client_compat.rs");
    let compat_raw = std::fs::read_to_string(&compat_path)
        .map_err(|e| format!("failed to read {}: {e}", compat_path.display()))?;
    scan_client_compat_source(&compat_raw, &mut report)?;

    let compat_rel = "crates/eve-server/src/router/client_compat.rs";
    for (key, val) in [
        ("rpc_rules", report.rpc_rules),
        ("suppress_rules", report.suppress_rules),
        ("notify_arms", report.notify_arms),
        ("destiny_rewrites", report.destiny_rewrites),
        ("production_lines", report.production_lines),
    ] {
        if val > 0 {
            report
                .counters
                .get_mut(key)
                .expect("initialized counter")
                .insert(compat_rel.to_string(), val);
        }
    }

    let sequencer_path = server_dir.join("src/outbound_sequencer.rs");
    if sequencer_path.is_file() {
        let sequencer_raw = std::fs::read_to_string(&sequencer_path)
            .map_err(|e| format!("failed to read {}: {e}", sequencer_path.display()))?;
        let sequencer_clean = strip_rust_comments_and_literals(&sequencer_raw);
        let sequencer_prod = strip_cfg_test_items(&sequencer_clean);
        let seq_count = count_sequencer_set_state_rewrites(&sequencer_prod);
        report.sequencer_set_state_rewrites = seq_count;
        if seq_count > 0 {
            report
                .counters
                .get_mut("sequencer_set_state_rewrites")
                .expect("initialized counter")
                .insert(
                    "crates/eve-server/src/outbound_sequencer.rs".to_string(),
                    seq_count,
                );
        }
    }

    let src_dir = server_dir.join("src");
    let mut src_files = Vec::new();
    collect_rs_files_recursive(&src_dir, &src_dir, &mut src_files)?;
    src_files.sort_by(|a, b| a.0.cmp(&b.0));

    for (rel_in_src, path) in src_files {
        if is_test_only_rel_path(&rel_in_src) {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let clean = strip_rust_comments_and_literals(&raw);
        let prod = strip_cfg_test_items(&clean);

        let full_rel = format!("crates/eve-server/src/{rel_in_src}");
        let cmp = count_caller_trace_comparisons(&prod);
        if cmp > 0 {
            report.transport_conditionals += cmp;
            report
                .counters
                .get_mut("transport_conditionals")
                .expect("initialized counter")
                .insert(full_rel.clone(), cmp);
        }

        if rel_in_src.starts_with("services/") {
            let params = count_caller_trace_params(&prod);
            if cmp > 0 {
                report.service_caller_trace_comparisons += cmp;
                report
                    .counters
                    .get_mut("service_caller_trace_comparisons")
                    .expect("initialized counter")
                    .insert(full_rel.clone(), cmp);
            }
            if params > 0 {
                report.service_caller_trace_params += params;
                report
                    .counters
                    .get_mut("service_caller_trace_params")
                    .expect("initialized counter")
                    .insert(full_rel.clone(), params);
            }
            if cmp > 0 || params > 0 {
                report.service_caller_trace_files.insert(full_rel);
            }
        }
    }

    let mut server_files = Vec::new();
    collect_rs_files_recursive(server_dir, server_dir, &mut server_files)?;
    server_files.sort_by(|a, b| a.0.cmp(&b.0));

    for (rel_in_server, path) in server_files {
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let clean = strip_rust_comments_and_literals(&raw);
        let dl = if is_test_only_rel_path(&rel_in_server) {
            count_direct_layer_calls(&clean)
        } else {
            let prod = strip_cfg_test_items(&clean);
            let total = count_direct_layer_calls(&clean);
            let prod_only = count_direct_layer_calls(&prod);
            total.saturating_sub(prod_only)
        };
        if dl > 0 {
            report.direct_layer_test_calls += dl;
            report
                .counters
                .get_mut("direct_layer_test_calls")
                .expect("initialized counter")
                .insert(format!("crates/eve-server/{rel_in_server}"), dl);
        }
    }

    let dispatch_report = scan_raw_dispatch_calls_in_dir(it_dir)?;
    report.it_dispatch_rpc_files = dispatch_report.files_with_dispatch_rpc;

    Ok(report)
}

fn scan_client_compat_source(
    raw: &str,
    report: &mut ClientCompatScanReport,
) -> Result<(), String> {
    let clean = strip_rust_comments_and_literals(raw);
    debug_assert_eq!(clean.len(), raw.len());

    report.production_lines = count_production_lines(&clean);

    report.rpc_rules = extract_rpc_rules(raw, &clean, &mut report.known_rules)?;
    report.suppress_rules = extract_suppress_rules(raw, &clean, &mut report.known_rules)?;
    report.notify_arms = extract_notify_arms(raw, &clean, &mut report.known_rules)?;
    report.destiny_rewrites = extract_destiny_rewrites(raw, &clean, &mut report.known_rules)?;

    Ok(())
}

fn count_production_lines(clean: &str) -> usize {
    for (idx, line) in clean.lines().enumerate() {
        let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.starts_with("#[cfg(test)]") {
            return idx;
        }
    }
    clean.lines().count()
}

fn is_word_at(bytes: &[u8], pos: usize, word: &[u8]) -> bool {
    let end = pos + word.len();
    end <= bytes.len()
        && &bytes[pos..end] == word
        && (pos == 0 || !is_ident_byte(bytes[pos - 1]))
        && (end == bytes.len() || !is_ident_byte(bytes[end]))
}

fn find_fn_body_span(clean: &str, fn_name: &str) -> Result<(usize, usize), String> {
    let b = clean.as_bytes();
    let needle = fn_name.as_bytes();
    let mut idx = 0usize;
    let mut found_fn = None;

    while let Some(rel) = clean[idx..].find(fn_name) {
        let start = idx + rel;
        if is_word_at(b, start, needle) && is_fn_def_header(b, start) {
            found_fn = Some(start);
            break;
        }
        idx = start + fn_name.len();
    }

    let fn_start = found_fn.ok_or_else(|| format!("missing function `{fn_name}`"))?;
    let mut pos = fn_start + needle.len();
    let mut pdepth = 0usize;
    let mut brdepth = 0usize;
    let mut body_open = None;

    while pos < b.len() {
        match b[pos] {
            b'(' => pdepth += 1,
            b')' => pdepth = pdepth.saturating_sub(1),
            b'[' => brdepth += 1,
            b']' => brdepth = brdepth.saturating_sub(1),
            b'{' if pdepth == 0 && brdepth == 0 => {
                body_open = Some(pos);
                break;
            }
            _ => {}
        }
        pos += 1;
    }

    let open = body_open.ok_or_else(|| format!("unopened body for `{fn_name}`"))?;
    let mut bdepth = 1usize;
    let mut cur = open + 1;
    while cur < b.len() && bdepth > 0 {
        match b[cur] {
            b'{' => bdepth += 1,
            b'}' => {
                bdepth -= 1;
                if bdepth == 0 {
                    return Ok((open, cur));
                }
            }
            _ => {}
        }
        cur += 1;
    }

    Err(format!("unclosed body for `{fn_name}`"))
}

fn find_if_header_end(clean: &[u8], if_pos: usize, limit: usize) -> Result<usize, String> {
    let mut k = if_pos + 2;
    let mut kp = 0usize;
    let mut kbr = 0usize;
    while k < limit {
        match clean[k] {
            b'(' => kp += 1,
            b')' => kp = kp.saturating_sub(1),
            b'[' => kbr += 1,
            b']' => kbr = kbr.saturating_sub(1),
            b'{' if kp == 0 && kbr == 0 => return Ok(k),
            _ => {}
        }
        k += 1;
    }
    Err(format!("unterminated `if` header at byte offset {if_pos}"))
}

fn collect_top_level_if_headers(
    clean: &str,
    fn_name: &str,
) -> Result<Vec<(usize, usize)>, String> {
    let (body_open, body_close) = find_fn_body_span(clean, fn_name)?;
    let b = clean.as_bytes();
    let mut bdepth = 1usize;
    let mut pdepth = 0usize;
    let mut brdepth = 0usize;
    let mut headers = Vec::new();
    let mut i = body_open + 1;

    while i < body_close {
        if bdepth == 1 && pdepth == 0 && brdepth == 0 && is_word_at(b, i, b"if") {
            let hdr_end = find_if_header_end(b, i, body_close)?;
            headers.push((i, hdr_end));
        }
        match b[i] {
            b'{' => bdepth += 1,
            b'}' => bdepth = bdepth.saturating_sub(1),
            b'(' => pdepth += 1,
            b')' => pdepth = pdepth.saturating_sub(1),
            b'[' => brdepth += 1,
            b']' => brdepth = brdepth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }

    Ok(headers)
}

fn parse_quoted_string_at(bytes: &[u8], open_quote: usize) -> Option<(String, usize)> {
    if bytes.get(open_quote) != Some(&b'"') {
        return None;
    }
    let mut out = Vec::new();
    let mut i = open_quote + 1;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == b'\\' && i + 1 < bytes.len() {
            let esc = bytes[i + 1];
            match esc {
                b'"' => out.push(b'"'),
                b'\\' => out.push(b'\\'),
                b'n' => out.push(b'\n'),
                b'r' => out.push(b'\r'),
                b't' => out.push(b'\t'),
                other => out.push(other),
            }
            i += 2;
            continue;
        }
        if ch == b'"' {
            let s = String::from_utf8(out).ok()?;
            return Some((s, i + 1));
        }
        out.push(ch);
        i += 1;
    }
    None
}

fn skip_ws_and_comments(bytes: &[u8], mut i: usize) -> usize {
    let n = bytes.len();
    while i < n {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
        } else if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
        } else if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let mut depth = 1usize;
            i += 2;
            while i < n && depth > 0 {
                if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if i + 1 < n && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else {
            break;
        }
    }
    i
}

fn extract_string_literals_ignoring_comments(raw_slice: &str) -> Vec<String> {
    let b = raw_slice.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < n {
        if i + 1 < n && b[i] == b'/' && (b[i + 1] == b'/' || b[i + 1] == b'*') {
            i = skip_ws_and_comments(b, i);
        } else if b[i] == b'"' {
            if let Some((lit, next_i)) = parse_quoted_string_at(b, i) {
                out.push(lit);
                i = next_i;
            } else {
                i += 1;
            }
        } else if b[i] == b'\'' {
            if i + 2 < n && b[i + 1] == b'\\' {
                let mut k = i + 2;
                let limit = (i + 10).min(n);
                while k < limit && b[k] != b'\'' && b[k] != b'\n' {
                    k += 1;
                }
                if k < limit && b[k] == b'\'' {
                    i = k + 1;
                    continue;
                }
            } else if i + 2 < n && b[i + 2] == b'\'' && b[i + 1] != b'\n' {
                i += 3;
                continue;
            }
            i += 1;
        } else {
            i += 1;
        }
    }

    out
}

fn skip_ws_backwards(bytes: &[u8], mut pos: usize) -> usize {
    while pos > 0 && bytes[pos - 1].is_ascii_whitespace() {
        pos -= 1;
    }
    pos
}

fn is_preceded_by_matches_open(clean: &[u8], var_pos: usize) -> bool {
    let before_var = skip_ws_backwards(clean, var_pos);
    if before_var == 0 || clean[before_var - 1] != b'(' {
        return false;
    }
    let before_paren = skip_ws_backwards(clean, before_var - 1);
    let macro_tok = b"matches!";
    before_paren >= macro_tok.len()
        && &clean[before_paren - macro_tok.len()..before_paren] == macro_tok
        && (before_paren == macro_tok.len() || !is_ident_byte(clean[before_paren - macro_tok.len() - 1]))
}

fn extract_var_string_matches(clean_slice: &str, raw_slice: &str, var_name: &str) -> Vec<String> {
    let cb = clean_slice.as_bytes();
    let rb = raw_slice.as_bytes();
    let needle = var_name.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < cb.len() {
        if is_word_at(cb, i, needle) {
            let mut after = i + needle.len();
            while after < cb.len() && cb[after].is_ascii_whitespace() {
                after += 1;
            }
            if is_preceded_by_matches_open(cb, i) && after < cb.len() && cb[after] == b',' {
                let pat_start = after + 1;
                let mut k = pat_start;
                let mut kp = 1usize;
                let mut kbr = 0usize;
                let mut pat_end = cb.len();
                while k < cb.len() && kp > 0 {
                    if kp == 1 && kbr == 0 && is_word_at(cb, k, b"if") {
                        pat_end = k;
                        break;
                    }
                    match cb[k] {
                        b'(' => kp += 1,
                        b')' => {
                            kp -= 1;
                            if kp == 0 {
                                pat_end = k;
                                break;
                            }
                        }
                        b'[' => kbr += 1,
                        b']' => kbr = kbr.saturating_sub(1),
                        _ => {}
                    }
                    k += 1;
                }
                out.extend(extract_string_literals_ignoring_comments(
                    &raw_slice[pat_start..pat_end],
                ));
                i = pat_end;
                continue;
            } else if after + 2 <= cb.len()
                && &cb[after..after + 2] == b"=="
                && cb.get(after + 2) != Some(&b'=')
            {
                let lit_pos = skip_ws_and_comments(rb, after + 2);
                if let Some((lit, next_i)) = parse_quoted_string_at(rb, lit_pos) {
                    out.push(lit);
                    i = next_i;
                    continue;
                }
            }
        }
        i += 1;
    }

    out
}

fn extract_rpc_rules(
    raw: &str,
    clean: &str,
    known_rules: &mut BTreeSet<String>,
) -> Result<usize, String> {
    let headers = collect_top_level_if_headers(clean, "normalize_macho_rpc_reply_at")?;
    for &(s, e) in &headers {
        let svcs = extract_var_string_matches(&clean[s..e], &raw[s..e], "service");
        let meths = extract_var_string_matches(&clean[s..e], &raw[s..e], "method");
        if svcs.is_empty() || meths.is_empty() {
            return Err(format!(
                "could not extract (service, method) literals from RPC compat header: {:?}",
                raw[s..e].trim()
            ));
        }
        for svc in &svcs {
            for meth in &meths {
                known_rules.insert(format!("rpc:{svc}.{meth}"));
            }
        }
    }
    Ok(headers.len())
}

fn extract_suppress_rules(
    raw: &str,
    clean: &str,
    known_rules: &mut BTreeSet<String>,
) -> Result<usize, String> {
    let headers = collect_top_level_if_headers(clean, "should_suppress_macho_notification")?;
    for &(s, e) in &headers {
        let events = extract_var_string_matches(&clean[s..e], &raw[s..e], "event");
        if events.is_empty() {
            return Err(format!(
                "could not extract `event` literals from suppress header: {:?}",
                raw[s..e].trim()
            ));
        }
        for ev in events {
            known_rules.insert(format!("suppress:{ev}"));
        }
    }
    Ok(headers.len())
}

fn extract_notify_arms(
    raw: &str,
    clean: &str,
    known_rules: &mut BTreeSet<String>,
) -> Result<usize, String> {
    let (body_open, body_close) = find_fn_body_span(clean, "normalize_macho_notification_with_sde")?;
    let b = clean.as_bytes();
    let mut pos = body_open + 1;
    let mut brace_open = None;

    while pos < body_close {
        if is_word_at(b, pos, b"match") {
            let mut after = pos + b"match".len();
            while after < body_close && b[after].is_ascii_whitespace() {
                after += 1;
            }
            if is_word_at(b, after, b"event") {
                let mut open = after + b"event".len();
                while open < body_close && b[open].is_ascii_whitespace() {
                    open += 1;
                }
                if b.get(open) == Some(&b'{') {
                    brace_open = Some(open);
                    break;
                }
            }
        }
        pos += 1;
    }

    let match_open = brace_open
        .ok_or_else(|| "missing `match event {` in `normalize_macho_notification_with_sde`".to_string())?;
    let mut bdepth = 0usize;
    let mut pdepth = 0usize;
    let mut brdepth = 0usize;
    let mut pat_start = Some(match_open + 1);
    let mut notify_arms = 0usize;
    let mut i = match_open + 1;

    while i < body_close {
        if bdepth == 0 && pdepth == 0 && brdepth == 0 {
            if b[i] == b'}' {
                break;
            }
            if i + 1 < body_close && b[i] == b'=' && b[i + 1] == b'>' {
                let ps = pat_start.ok_or_else(|| {
                    format!("unexpected `=>` without pattern start at byte offset {i}")
                })?;
                let events = extract_string_literals_ignoring_comments(&raw[ps..i]);
                if !events.is_empty() {
                    notify_arms += 1;
                    for ev in events {
                        known_rules.insert(format!("notify:{ev}"));
                    }
                }
                pat_start = None;
                i += 2;
                continue;
            }
            if b[i] == b',' {
                pat_start = Some(i + 1);
                i += 1;
                continue;
            }
        }

        match b[i] {
            b'{' => bdepth += 1,
            b'}' => {
                bdepth = bdepth.saturating_sub(1);
                if bdepth == 0 && pdepth == 0 && brdepth == 0 {
                    pat_start = Some(i + 1);
                }
            }
            b'(' => pdepth += 1,
            b')' => pdepth = pdepth.saturating_sub(1),
            b'[' => brdepth += 1,
            b']' => brdepth = brdepth.saturating_sub(1),
            _ => {}
        }
        i += 1;
    }

    Ok(notify_arms)
}

fn extract_destiny_rewrites(
    raw: &str,
    clean: &str,
    known_rules: &mut BTreeSet<String>,
) -> Result<usize, String> {
    let mut rewrites = 0usize;
    let b = clean.as_bytes();

    // 1. `normalize_destiny_update_arg`: top-level `if` checks on `name` (0 when deleted).
    if let Ok((u_open, u_close)) = find_fn_body_span(clean, "normalize_destiny_update_arg") {
        let mut pdepth = 0usize;
        let mut brdepth = 0usize;
        let mut i = u_open + 1;
        while i < u_close {
            if pdepth == 0 && brdepth == 0 && is_word_at(b, i, b"if") {
                let hdr_end = find_if_header_end(b, i, u_close)?;
                let names = extract_var_string_matches(&clean[i..hdr_end], &raw[i..hdr_end], "name");
                if !names.is_empty() {
                    rewrites += 1;
                    for name in names {
                        known_rules.insert(format!("destiny:update:{name}"));
                    }
                }
            }
            match b[i] {
                b'(' => pdepth += 1,
                b')' => pdepth = pdepth.saturating_sub(1),
                b'[' => brdepth += 1,
                b']' => brdepth = brdepth.saturating_sub(1),
                _ => {}
            }
            i += 1;
        }
    }

    // 2. `normalize_destiny_payload_arg` (0 when deleted or trivial):
    //    (a) `fname` filter clauses in `PyRep::List(items)`
    //    (b) condition headers in `PyRep::Tuple(mut items)` (`name` branches + `effects.*` guards)
    if let Ok((p_open, p_close)) = find_fn_body_span(clean, "normalize_destiny_payload_arg") {
        let mut filter_pos = p_open + 1;
        let mut closure_span = None;
        while let Some(rel) = clean[filter_pos..p_close].find(".filter") {
            let pos = filter_pos + rel;
            let after_dot = pos + ".filter".len();
            if !b.get(after_dot).copied().is_some_and(is_ident_byte) {
                let open_paren = skip_ws_and_comments(b, after_dot);
                if b.get(open_paren) == Some(&b'(') {
                    let mut cur = open_paren + 1;
                    let mut cp = 1usize;
                    while cur < p_close && cp > 0 {
                        if cp == 1 && b[cur] == b'{' {
                            let c_start = cur + 1;
                            let mut cb = 1usize;
                            let mut c_end = c_start;
                            while c_end < p_close && cb > 0 {
                                match b[c_end] {
                                    b'{' => cb += 1,
                                    b'}' => {
                                        cb -= 1;
                                        if cb == 0 {
                                            break;
                                        }
                                    }
                                    _ => {}
                                }
                                c_end += 1;
                            }
                            closure_span = Some((c_start, c_end));
                            break;
                        }
                        match b[cur] {
                            b'(' => cp += 1,
                            b')' => cp -= 1,
                            _ => {}
                        }
                        cur += 1;
                    }
                    if closure_span.is_some() {
                        break;
                    }
                }
            }
            filter_pos = after_dot;
        }

        if let Some((c_start, c_end)) = closure_span {
            let mut last_semi = None;
            let mut lb = 0usize;
            let mut lp = 0usize;
            let mut lbr = 0usize;
            for k in c_start..c_end {
                match b[k] {
                    b'{' => lb += 1,
                    b'}' => lb = lb.saturating_sub(1),
                    b'(' => lp += 1,
                    b')' => lp = lp.saturating_sub(1),
                    b'[' => lbr += 1,
                    b']' => lbr = lbr.saturating_sub(1),
                    b';' if lb == 0 && lp == 0 && lbr == 0 => last_semi = Some(k),
                    _ => {}
                }
            }
            let expr_start = last_semi.map_or(c_start, |s| s + 1);
            let mut conj_start = expr_start;
            let mut cp = 0usize;
            let mut cb = 0usize;
            let mut cbr = 0usize;
            let mut conjuncts = Vec::new();
            let mut k = expr_start;
            while k < c_end {
                if cb == 0
                    && cp == 0
                    && cbr == 0
                    && k + 1 < c_end
                    && b[k] == b'&'
                    && b[k + 1] == b'&'
                {
                    conjuncts.push((conj_start, k));
                    conj_start = k + 2;
                    k += 2;
                    continue;
                }
                match b[k] {
                    b'{' => cb += 1,
                    b'}' => cb = cb.saturating_sub(1),
                    b'(' => cp += 1,
                    b')' => cp = cp.saturating_sub(1),
                    b'[' => cbr += 1,
                    b']' => cbr = cbr.saturating_sub(1),
                    _ => {}
                }
                k += 1;
            }
            conjuncts.push((conj_start, c_end));

            for (cs, ce) in conjuncts {
                let mut has_fname = false;
                for pos in cs..ce {
                    if is_word_at(b, pos, b"fname") {
                        has_fname = true;
                        break;
                    }
                }
                if has_fname {
                    rewrites += 1;
                    for lit in extract_string_literals_ignoring_comments(&raw[cs..ce]) {
                        known_rules.insert(format!("destiny:payload:{lit}"));
                    }
                }
            }
        }

        let mut tuple_search = p_open;
        let mut tuple_arm_open = None;
        while let Some(tuple_rel) = clean[tuple_search..p_close].find("PyRep::Tuple") {
            let mut after_pat =
                skip_ws_and_comments(b, tuple_search + tuple_rel + "PyRep::Tuple".len());
            if b.get(after_pat) == Some(&b'(') {
                let mut pat_p = 1usize;
                after_pat += 1;
                while after_pat < p_close && pat_p > 0 {
                    match b[after_pat] {
                        b'(' => pat_p += 1,
                        b')' => pat_p -= 1,
                        _ => {}
                    }
                    after_pat += 1;
                }
                let arrow_pos = skip_ws_and_comments(b, after_pat);
                if arrow_pos + 1 < p_close && b[arrow_pos] == b'=' && b[arrow_pos + 1] == b'>' {
                    let open = skip_ws_and_comments(b, arrow_pos + 2);
                    if b.get(open) == Some(&b'{') {
                        tuple_arm_open = Some(open);
                        break;
                    }
                }
            }
            tuple_search = tuple_search + tuple_rel + "PyRep::Tuple".len();
        }

        if let Some(arm_open) = tuple_arm_open {
            let mut arm_b = 1usize;
            let mut arm_close = arm_open + 1;
            while arm_close < p_close && arm_b > 0 {
                match b[arm_close] {
                    b'{' => arm_b += 1,
                    b'}' => {
                        arm_b -= 1;
                        if arm_b == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                arm_close += 1;
            }

            let mut tp = 0usize;
            let mut tbr = 0usize;
            let mut ti = arm_open + 1;
            while ti < arm_close {
                if tp == 0 && tbr == 0 && is_word_at(b, ti, b"if") {
                    let hdr_end = find_if_header_end(b, ti, arm_close)?;
                    let names =
                        extract_var_string_matches(&clean[ti..hdr_end], &raw[ti..hdr_end], "name");
                    if !names.is_empty() {
                        rewrites += 1;
                    }
                    for lit in extract_string_literals_ignoring_comments(&raw[ti..hdr_end]) {
                        known_rules.insert(format!("destiny:payload:{lit}"));
                    }
                }
                match b[ti] {
                    b'(' => tp += 1,
                    b')' => tp = tp.saturating_sub(1),
                    b'[' => tbr += 1,
                    b']' => tbr = tbr.saturating_sub(1),
                    _ => {}
                }
                ti += 1;
            }
        }
    }

    Ok(rewrites)
}

fn count_caller_trace_comparisons(prod_clean: &str) -> usize {
    let b = prod_clean.as_bytes();
    let needle = b"caller_trace";
    let mut count = 0usize;
    let mut idx = 0usize;

    while let Some(rel) = prod_clean[idx..].find("caller_trace") {
        let start = idx + rel;
        if is_word_at(b, start, needle) {
            let mut after = start + needle.len();
            while after < b.len() && b[after].is_ascii_whitespace() {
                after += 1;
            }
            let forward_cmp = (after + 2 <= b.len()
                && (&b[after..after + 2] == b"==" || &b[after..after + 2] == b"!=")
                && b.get(after + 2) != Some(&b'='))
                || (is_preceded_by_matches_open(b, start)
                    && b.get(after) == Some(&b','));

            let before = skip_ws_backwards(b, start);
            let backward_cmp = before >= 2
                && (&b[before - 2..before] == b"==" || &b[before - 2..before] == b"!=")
                && (before == 2 || (b[before - 3] != b'=' && b[before - 3] != b'!'));

            if forward_cmp || backward_cmp {
                count += 1;
            }
        }
        idx = start + needle.len();
    }

    count
}

fn count_caller_trace_params(prod_clean: &str) -> usize {
    let b = prod_clean.as_bytes();
    let needle = b"caller_trace";
    let mut count = 0usize;
    let mut idx = 0usize;

    while let Some(rel) = prod_clean[idx..].find("caller_trace") {
        let start = idx + rel;
        if is_word_at(b, start, needle) {
            let mut after = start + needle.len();
            while after < b.len() && b[after].is_ascii_whitespace() {
                after += 1;
            }
            if b.get(after) == Some(&b':') && b.get(after + 1) != Some(&b':') {
                count += 1;
            }
        }
        idx = start + needle.len();
    }

    count
}

fn count_direct_layer_calls(clean_src: &str) -> usize {
    let b = clean_src.as_bytes();
    let mut count = 0usize;

    for func in DIRECT_LAYER_FNS {
        let needle = func.as_bytes();
        let mut idx = 0usize;
        while let Some(rel) = clean_src[idx..].find(func) {
            let start = idx + rel;
            let after = start + needle.len();
            let prev_is_dot = start > 0 && b[start - 1] == b'.';
            let mut open = after;
            while open < b.len() && b[open].is_ascii_whitespace() {
                open += 1;
            }
            if is_word_at(b, start, needle)
                && !prev_is_dot
                && b.get(open) == Some(&b'(')
                && !is_fn_def_header(b, start)
            {
                count += 1;
                idx = open + 1;
            } else {
                idx = after;
            }
        }
    }

    count
}

/// Counts production post-producer `SetState` mutation helpers, call sites, and state-header
/// byte-slice rewrites in `crates/eve-server/src/outbound_sequencer.rs` (`RT-92`).
fn count_sequencer_set_state_rewrites(prod_clean: &str) -> usize {
    const PREFIXES: &[&[u8]] = &[b"restamp_", b"rewrite_set_state", b"patch_set_state"];
    let b = prod_clean.as_bytes();
    let mut count = 0usize;
    let mut idx = 0usize;

    while idx < b.len() {
        let is_start = is_ident_byte(b[idx]) && (idx == 0 || !is_ident_byte(b[idx - 1]));
        if !is_start {
            idx += 1;
            continue;
        }
        let mut end = idx + 1;
        while end < b.len() && is_ident_byte(b[end]) {
            end += 1;
        }
        let ident = &b[idx..end];
        let mut after = end;
        while after < b.len() && b[after].is_ascii_whitespace() {
            after += 1;
        }

        if PREFIXES.iter().any(|p| ident.starts_with(p)) {
            if is_fn_def_header(b, idx) || b.get(after) == Some(&b'(') {
                count += 1;
            }
        } else if ident == b"copy_from_slice" && b.get(after) == Some(&b'(') {
            let before = skip_ws_backwards(b, idx);
            if before > 0 && b[before - 1] == b'.' && (before == 1 || b[before - 2] != b'.') {
                count += 1;
            }
        }

        idx = end;
    }

    count
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    fn make_temp_tree(name: &str) -> PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "evomu_client_compat_ratchet_{name}_{}_{}",
            std::process::id(),
            seq
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/router")).unwrap();
        std::fs::create_dir_all(dir.join("src/services")).unwrap();
        std::fs::create_dir_all(dir.join("tests/it")).unwrap();
        dir
    }

    fn write_minimal_compat_tree(root: &Path, extra_compat_body: &str) {
        let compat = format!(
            r#"
pub fn normalize_macho_rpc_reply_at(service: &str, method: &str, rpc_res: i32) -> i32 {{
    if service == "svcA" && method == "actOne" {{
        return rpc_res;
    }}
    {extra_compat_body}
    rpc_res
}}

pub fn should_suppress_macho_notification(event: &str, args: &[i32]) -> bool {{
    if matches!(event, "EvA" | "EvB") && !args.is_empty() {{
        return true;
    }}
    false
}}

fn normalize_destiny_update_arg(name: &str) {{
    if name == "OnDockingAccepted" {{}}
}}

fn normalize_destiny_payload_arg(items: Vec<&str>, name: &str) {{
    let _: Vec<_> = items
        .into_iter()
        .filter(|it| {{
            let fname = Some(*it);
            fname != Some("OnDockingAccepted")
                && !(true && matches!(fname, Some("SetBallInteractive" | "PackagedAction")))
        }})
        .collect();
    match (name, items.len()) {{
        PyRep::Tuple(_) => {{
            if let Some(n) = Some(name) {{
                if n == "CloakBall" {{}}
                if name == "CloakBall" {{}}
            }}
        }}
        _ => {{}}
    }}
}}

pub fn normalize_macho_notification_with_sde(event: &str) {{
    match event {{
        "EvNotify" => {{}},
        _ => {{}},
    }}
}}

#[cfg(test)]
mod tests {{
    #[test]
    fn dummy() {{
        super::normalize_macho_rpc_reply_at("a", "b", 1);
    }}
}}
"#
        );
        std::fs::write(root.join("src/router/client_compat.rs"), compat).unwrap();
    }

    #[test]
    fn repo_client_compat_ratchet_matches_baseline() {
        let ratchet = ClientCompatRatchet::load_default().expect("load client_compat_ratchet.json");
        assert_eq!(ratchet.version, 2, "client_compat_ratchet.json must be v2");
        assert!(
            ratchet.max_transport_differing_replies.is_some(),
            "max_transport_differing_replies must remain populated for rpc_client_contracts callers"
        );
        let report = ratchet
            .verify_default()
            .expect("client_compat_ratchet.json must match repository baseline");
        for (key, observed_total) in [
            ("rpc_rules", report.rpc_rules),
            ("suppress_rules", report.suppress_rules),
            ("notify_arms", report.notify_arms),
            ("destiny_rewrites", report.destiny_rewrites),
            ("production_lines", report.production_lines),
            ("transport_conditionals", report.transport_conditionals),
            ("direct_layer_test_calls", report.direct_layer_test_calls),
            ("service_caller_trace_params", report.service_caller_trace_params),
            (
                "sequencer_set_state_rewrites",
                report.sequencer_set_state_rewrites,
            ),
        ] {
            let cap = ratchet.counter_sum(key);
            assert!(
                observed_total <= cap,
                "client_compat counter '{key}' exceeded baseline: {observed_total} > {cap}"
            );
            let per_file_sum: usize = report.counters[key].values().sum();
            assert_eq!(
                observed_total, per_file_sum,
                "internal scalar vs per-file sum mismatch for '{key}'"
            );
        }
        assert_eq!(
            report.service_caller_trace_comparisons,
            0,
            "service_caller_trace_comparisons must stay strictly 0"
        );
        assert!(ratchet
            .counters
            .get("service_caller_trace_comparisons")
            .expect("counter exists")
            .is_empty());
        assert!(!report.it_dispatch_rpc_files.contains("client_shape_pins.rs"));
    }

    #[test]
    fn assert_ratchet_enforces_per_file_caps_subset_allowlists_and_zero_pins() {
        let ratchet = ClientCompatRatchet::load_default().expect("load client_compat_ratchet.json");
        let base_report = scan_client_compat_in_dirs(
            &ClientCompatRatchet::default_server_dir(),
            &ClientCompatRatchet::default_it_tests_dir(),
        )
        .expect("scan default dirs");
        ratchet.assert_ratchet(&base_report).expect("baseline ok");

        // Regression on each existing baselined file fails naming the counter and file.
        for key in CLIENT_COMPAT_COUNTER_KEYS {
            let mut regressed = base_report.clone();
            let map = regressed.counters.entry(key.to_string()).or_default();
            if let Some((_file, count)) = map.iter_mut().next() {
                *count += 1;
            } else {
                map.insert("crates/eve-server/src/services/extra.rs".to_string(), 1);
            }
            let err = ratchet.assert_ratchet(&regressed).unwrap_err();
            assert!(
                err.contains(key),
                "expected counter '{key}' in error: {err}"
            );
        }

        // Regression on an unlisted file also fails even when another baselined file drops.
        let mut unlisted_file = base_report.clone();
        if let Some(map) = unlisted_file.counters.get_mut("direct_layer_test_calls") {
            if let Some(first_key) = map.keys().next().cloned() {
                map.remove(&first_key);
            }
            map.insert("crates/eve-server/tests/it/brand_new.rs".to_string(), 1);
            let err = ratchet.assert_ratchet(&unlisted_file).unwrap_err();
            assert!(err.contains("brand_new.rs"), "err={err}");
        }

        // Decreasing any non-zero per-file counter or removing a file passes without JSON edits.
        let mut lowered = base_report.clone();
        if let Some(rpc_map) = lowered.counters.get_mut("rpc_rules") {
            if let Some(v) = rpc_map.values_mut().next() {
                *v = v.saturating_sub(1);
            }
        }
        if let Some(dl_map) = lowered.counters.get_mut("direct_layer_test_calls") {
            if let Some(first_key) = dl_map.keys().next().cloned() {
                dl_map.remove(&first_key);
            }
        }
        ratchet
            .assert_ratchet(&lowered)
            .expect("per-file decreases and deletions must pass without modifying baseline");

        // Unlisted rule fails; deleting an allowed rule passes without modifying baseline.
        let mut unlisted = base_report.clone();
        unlisted.known_rules.insert("rpc:fakeSvc.FakeMethod".into());
        let err = ratchet.assert_ratchet(&unlisted).unwrap_err();
        assert!(err.contains("Unlisted client_compat rules"), "err={err}");

        let mut deleted_rule = base_report.clone();
        let first_rule = deleted_rule.known_rules.iter().next().unwrap().clone();
        deleted_rule.known_rules.remove(&first_rule);
        ratchet
            .assert_ratchet(&deleted_rule)
            .expect("deleting an allowlisted rule must pass (subset-only)");

        // Adding a service caller_trace file fails; removing one passes (subset-only).
        let mut added_svc = base_report.clone();
        added_svc
            .service_caller_trace_files
            .insert("crates/eve-server/src/services/unlisted_service.rs".into());
        assert!(ratchet
            .assert_ratchet(&added_svc)
            .unwrap_err()
            .contains("unlisted_service.rs"));

        if let Some(first_svc) = base_report.service_caller_trace_files.iter().next().cloned() {
            let mut removed_svc = base_report.clone();
            removed_svc.service_caller_trace_files.remove(&first_svc);
            ratchet
                .assert_ratchet(&removed_svc)
                .expect("removing a service from allowed_service_caller_trace_files must pass");
        }

        // Brand-new tests/it/*.rs introducing ctx.dispatch_rpc( fails.
        let mut new_it = base_report.clone();
        new_it
            .it_dispatch_rpc_files
            .insert("brand_new_bypass_test.rs".into());
        let err = ratchet.assert_ratchet(&new_it).unwrap_err();
        assert!(
            err.contains("brand_new_bypass_test.rs"),
            "expected offending IT file in error: {err}"
        );

        // Existing IT file migrating away from dispatch_rpc is allowed.
        let mut migrated_it = base_report;
        if let Some(any_existing) = migrated_it.it_dispatch_rpc_files.iter().next().cloned() {
            migrated_it.it_dispatch_rpc_files.remove(&any_existing);
            ratchet
                .assert_ratchet(&migrated_it)
                .expect("migrating existing IT file away from dispatch_rpc must pass");
        }

        // Non-empty zero_required counter baseline must fail validation.
        let mut bad_zero_ratchet = ratchet.clone();
        bad_zero_ratchet
            .counters
            .get_mut("service_caller_trace_comparisons")
            .unwrap()
            .insert("crates/eve-server/src/services/foo.rs".into(), 1);
        let zero_err = bad_zero_ratchet.validate_schema().unwrap_err();
        assert!(zero_err.contains("zero_required=true"), "{zero_err}");

        // Schema loader must reject v1 versions and unknown legacy v1 fields.
        let tmp = make_temp_tree("schema_errs");
        let v1_path = tmp.join("v1.json");
        let mut v1_ratchet = ratchet.clone();
        v1_ratchet.version = 1;
        std::fs::write(&v1_path, serde_json::to_string(&v1_ratchet).unwrap()).unwrap();
        assert!(ClientCompatRatchet::load_from_path(&v1_path)
            .unwrap_err()
            .contains("expected 2"));

        let legacy_path = tmp.join("legacy.json");
        let mut legacy_val = serde_json::to_value(&ratchet).unwrap();
        legacy_val
            .as_object_mut()
            .unwrap()
            .insert("max_rpc_rules".into(), serde_json::json!(10));
        std::fs::write(&legacy_path, serde_json::to_string(&legacy_val).unwrap()).unwrap();
        assert!(ClientCompatRatchet::load_from_path(&legacy_path).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn scanner_detects_services_caller_trace_and_new_it_dispatch_rpc_in_synthetic_tree() {
        let root = make_temp_tree("synth");
        write_minimal_compat_tree(&root, "");
        std::fs::write(
            root.join("src/services/bad_service.rs"),
            r#"
pub fn handle(caller_trace: Option<&str>) {
    // caller_trace == Some("machoNet:26000") in comment must be ignored
    if caller_trace == Some("machoNet:26000") {}
}
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("tests/it/new_dispatch_test.rs"),
            r#"
async fn test_bypass() {
    let _ = ctx.dispatch_rpc(session, "svc", None, "m", vec![], None, None).await;
}
"#,
        )
        .unwrap();
        std::fs::write(
            root.join("src/outbound_sequencer.rs"),
            r#"
// restamp_comment() must be ignored
fn restamp_state_header(bytes: &mut [u8]) {
    bytes[1..5].copy_from_slice(&[0, 0, 0, 0]);
}
fn flush_batch(buf: &mut [u8]) {
    restamp_state_header(buf);
}
#[cfg(test)]
mod tests {
    #[test]
    fn ignored() {
        restamp_state_header(&mut []);
    }
}
"#,
        )
        .unwrap();

        let report =
            scan_client_compat_in_dirs(&root, &root.join("tests/it")).expect("scan synth");
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(report.rpc_rules, 1);
        assert_eq!(report.suppress_rules, 1);
        assert_eq!(report.notify_arms, 1);
        assert_eq!(report.destiny_rewrites, 4);
        assert_eq!(report.transport_conditionals, 1);
        assert_eq!(report.service_caller_trace_comparisons, 1);
        assert_eq!(report.service_caller_trace_params, 1);
        assert_eq!(report.direct_layer_test_calls, 1);
        assert_eq!(report.sequencer_set_state_rewrites, 3);
        assert!(report
            .known_rules
            .contains("destiny:payload:SetBallInteractive"));
        assert!(report.known_rules.contains("destiny:payload:PackagedAction"));
        assert!(report
            .service_caller_trace_files
            .contains("crates/eve-server/src/services/bad_service.rs"));
        assert!(report
            .it_dispatch_rpc_files
            .contains("new_dispatch_test.rs"));
    }

    #[test]
    fn extract_destiny_rewrites_supports_zero_state_and_legacy_rewrite_counts() {
        let zero_root = make_temp_tree("zero_destiny");
        std::fs::write(
            zero_root.join("src/router/client_compat.rs"),
            r#"
pub fn normalize_macho_rpc_reply_at(service: &str, method: &str, rpc_res: i32) -> i32 {
    if service == "svcA" && method == "actOne" {
        return rpc_res;
    }
    rpc_res
}

pub fn should_suppress_macho_notification(event: &str, args: &[i32]) -> bool {
    if event == "DoDestinyUpdate" && args.is_empty() {
        return true;
    }
    false
}

pub fn normalize_macho_notification_with_sde(event: &str) {
    match event {
        "DoDestinyUpdates" => {}
        _ => {}
    }
}
"#,
        )
        .unwrap();
        let zero_report = scan_client_compat_in_dirs(&zero_root, &zero_root.join("tests/it"))
            .expect("scan zero-destiny compat tree");
        let _ = std::fs::remove_dir_all(&zero_root);
        assert_eq!(zero_report.destiny_rewrites, 0);
        assert!(zero_report.counters["destiny_rewrites"].is_empty());
        assert!(!zero_report
            .known_rules
            .iter()
            .any(|rule| rule.starts_with("destiny:")));
        assert!(zero_report.known_rules.contains("suppress:DoDestinyUpdate"));
        assert!(zero_report.known_rules.contains("notify:DoDestinyUpdates"));

        let legacy_root = make_temp_tree("legacy_destiny");
        write_minimal_compat_tree(&legacy_root, "");
        let legacy_report =
            scan_client_compat_in_dirs(&legacy_root, &legacy_root.join("tests/it"))
                .expect("scan legacy-destiny compat tree");
        let _ = std::fs::remove_dir_all(&legacy_root);
        assert_eq!(legacy_report.destiny_rewrites, 4);
        assert_eq!(
            legacy_report.counters["destiny_rewrites"]
                .get("crates/eve-server/src/router/client_compat.rs"),
            Some(&4)
        );
        assert!(legacy_report
            .known_rules
            .contains("destiny:update:OnDockingAccepted"));
        assert!(legacy_report
            .known_rules
            .contains("destiny:payload:CloakBall"));
    }
}
