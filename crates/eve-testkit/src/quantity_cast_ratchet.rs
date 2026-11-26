//! Falling ratchet eliminating narrowing/wrapping `as i32` / `as u32` casts on item/run
//! quantities in `crates/eve-server/src/services/` (`#1186`).
//!
//! Two rules are enforced against non-test production Rust sources under `crates/eve-server/src/services/`:
//! - `direct_quantity_casts`: `as i32` / `as u32` casts directly narrowing a quantity/runs identifier,
//!   field access, deref, or `compute_*_qty(...)` call. Zero outside explicitly ratcheted overlap files.
//! - `clamped_quantity_casts`: shrink-only baseline for explicitly clamped/bounded quantity/runs casts
//!   (such as `.max(0) as u32`, `.clamp(...) as i32`, or float `.round() as i32`).
//!
//! Any file count above its baseline fails verification while reporting every offending cast with its
//! exact `crates/eve-server/src/services/<file>:<line>` location.

#![forbid(unsafe_code)]

use crate::arch_debt_ratchet::{validate_per_file_baseline, validate_per_file_counts};
use crate::client_harness::{is_ident_byte, strip_rust_comments_and_literals};
use crate::notify_ratchet::{
    collect_rs_files_recursive, find_cfg_test_attr_end, is_test_only_rel_path, strip_cfg_test_items,
};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

/// Rule key tracking direct narrowing casts on quantity/runs expressions in `services/`.
pub const RULE_DIRECT_QUANTITY_CASTS: &str = "direct_quantity_casts";

/// Rule key tracking clamped/bounded quantity/runs casts in `services/`.
pub const RULE_CLAMPED_QUANTITY_CASTS: &str = "clamped_quantity_casts";

/// Classification of a detected `as i32` / `as u32` quantity cast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum QuantityCastKind {
    Direct,
    Clamped,
}

/// Exact source location and matched expression for one quantity cast finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantityCastFinding {
    pub rel_file: String,
    pub line: usize,
    pub target_type: &'static str,
    pub lhs: String,
    pub kind: QuantityCastKind,
}

/// Full scan summary returned by [`scan_services_dir_for_quantity_casts`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuantityCastScanReport {
    pub scanned_files: usize,
    pub findings: Vec<QuantityCastFinding>,
    pub direct_counts: BTreeMap<String, usize>,
    pub clamped_counts: BTreeMap<String, usize>,
}

/// Per-rule baseline map inside [`QuantityCastRatchet`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuantityCastRuleRatchet {
    pub zero_required: bool,
    pub owning_issue: u32,
    pub description: String,
    pub files: BTreeMap<String, usize>,
}

/// Version-2 per-file ratchet specification (`crates/eve-testkit/ratchets/quantity_cast_ratchet.json`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuantityCastRatchet {
    pub version: u32,
    pub description: String,
    pub rules: BTreeMap<String, QuantityCastRuleRatchet>,
}

/// Observed vs baseline counts for a single quantity-cast ratchet rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantityCastRuleReport {
    pub baseline_total: usize,
    pub observed_total: usize,
    pub observed: BTreeMap<String, usize>,
}

/// Aggregate verification summary returned by [`QuantityCastRatchet::verify_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuantityCastVerifiedSummary {
    pub scanned_files: usize,
    pub rules: BTreeMap<String, QuantityCastRuleReport>,
    pub findings: Vec<QuantityCastFinding>,
}

/// Returns the default path to `crates/eve-testkit/ratchets/quantity_cast_ratchet.json`.
pub fn default_ratchet_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ratchets")
        .join("quantity_cast_ratchet.json")
}

/// Returns the default path to `crates/eve-server/src/services`.
pub fn default_services_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("eve-server")
        .join("src")
        .join("services")
}

impl QuantityCastRatchet {
    /// Loads `quantity_cast_ratchet.json` from its default workspace location.
    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&default_ratchet_path())
    }

    /// Loads a [`QuantityCastRatchet`] from `path`.
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let ratchet: Self = serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        ratchet.validate()?;
        Ok(ratchet)
    }

    /// Validates rule schemas and per-file baseline invariants.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 2 {
            return Err(format!(
                "quantity_cast_ratchet.json unsupported version {} (expected 2)",
                self.version
            ));
        }
        for key in [RULE_DIRECT_QUANTITY_CASTS, RULE_CLAMPED_QUANTITY_CASTS] {
            if !self.rules.contains_key(key) {
                return Err(format!(
                    "quantity_cast_ratchet.json missing required rule `{key}`"
                ));
            }
        }
        for (name, rule) in &self.rules {
            validate_per_file_baseline(name, rule.zero_required, &rule.files)?;
        }
        Ok(())
    }

    /// Scans live `crates/eve-server/src/services/` and enforces all ratchet rules.
    pub fn verify_default(&self) -> Result<QuantityCastVerifiedSummary, String> {
        self.verify_dir(&default_services_dir())
    }

    /// Scans `services_dir` and asserts observed counts do not exceed per-file baselines.
    pub fn verify_dir(&self, services_dir: &Path) -> Result<QuantityCastVerifiedSummary, String> {
        self.validate()?;
        let report = scan_services_dir_for_quantity_casts(services_dir)?;
        self.assert_ratchet(&report)
    }

    /// Compares a scan report against this ratchet, reporting exact `file:line` details on failure.
    pub fn assert_ratchet(
        &self,
        report: &QuantityCastScanReport,
    ) -> Result<QuantityCastVerifiedSummary, String> {
        let mut summaries = BTreeMap::new();
        let mut errors = Vec::new();

        for (rule_name, kind, observed) in [
            (
                RULE_DIRECT_QUANTITY_CASTS,
                QuantityCastKind::Direct,
                &report.direct_counts,
            ),
            (
                RULE_CLAMPED_QUANTITY_CASTS,
                QuantityCastKind::Clamped,
                &report.clamped_counts,
            ),
        ] {
            let rule = self
                .rules
                .get(rule_name)
                .ok_or_else(|| format!("missing rule `{rule_name}`"))?;
            if let Err(err) =
                validate_per_file_counts(rule_name, rule.zero_required, &rule.files, observed)
            {
                let mut detail = vec![err];
                for (rel_file, &obs_count) in observed {
                    let max_count = if rule.zero_required {
                        0
                    } else {
                        rule.files.get(rel_file).copied().unwrap_or(0)
                    };
                    if obs_count > max_count {
                        for finding in report.findings.iter().filter(|f| {
                            f.kind == kind && f.rel_file == *rel_file
                        }) {
                            detail.push(format!(
                                "  - crates/eve-server/src/services/{}:{} (`{} as {}`)",
                                finding.rel_file,
                                finding.line,
                                finding.lhs.split_whitespace().collect::<Vec<_>>().join(" "),
                                finding.target_type
                            ));
                        }
                    }
                }
                errors.push(detail.join("\n"));
            }
            summaries.insert(
                rule_name.to_string(),
                QuantityCastRuleReport {
                    baseline_total: rule.files.values().sum(),
                    observed_total: observed.values().sum(),
                    observed: observed.clone(),
                },
            );
        }

        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }

        Ok(QuantityCastVerifiedSummary {
            scanned_files: report.scanned_files,
            rules: summaries,
            findings: report.findings.clone(),
        })
    }
}

/// Scans all non-test `.rs` files in `services_dir` for quantity-narrowing `as i32` / `as u32` casts.
pub fn scan_services_dir_for_quantity_casts(
    services_dir: &Path,
) -> Result<QuantityCastScanReport, String> {
    let mut files = Vec::new();
    collect_rs_files_recursive(services_dir, services_dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let cfg_test_targets = collect_cfg_test_module_targets(&files)?;
    let mut report = QuantityCastScanReport::default();

    for (rel_file, path) in files {
        if is_test_only_rel_path(&rel_file) || cfg_test_targets.contains(&rel_file) {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let clean = strip_cfg_test_items(&strip_rust_comments_and_literals(&raw));
        report.scanned_files += 1;

        for finding in scan_clean_source_for_quantity_casts(&rel_file, &clean) {
            match finding.kind {
                QuantityCastKind::Direct => {
                    *report.direct_counts.entry(finding.rel_file.clone()).or_insert(0) += 1;
                }
                QuantityCastKind::Clamped => {
                    *report.clamped_counts.entry(finding.rel_file.clone()).or_insert(0) += 1;
                }
            }
            report.findings.push(finding);
        }
    }

    Ok(report)
}

/// Scans one comment-, literal-, and `#[cfg(test)]`-stripped source buffer (with preserved newlines)
/// and returns all quantity-related `as i32` / `as u32` casts in ascending line order.
pub fn scan_clean_source_for_quantity_casts(
    rel_file: &str,
    clean: &str,
) -> Vec<QuantityCastFinding> {
    let bytes = clean.as_bytes();
    let n = bytes.len();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i + 5 <= n {
        if bytes[i] == b'a'
            && bytes[i + 1] == b's'
            && (i == 0 || !is_ident_byte(bytes[i - 1]))
            && bytes[i + 2].is_ascii_whitespace()
        {
            let mut j = i + 2;
            while j < n && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j + 3 <= n {
                let tail = &bytes[j..j + 3];
                let target = if tail == b"i32" {
                    Some("i32")
                } else if tail == b"u32" {
                    Some("u32")
                } else {
                    None
                };
                if let Some(target_type) = target {
                    if j + 3 == n || !is_ident_byte(bytes[j + 3]) {
                        let lhs = extract_lhs_operand(clean, i);
                        if let Some(kind) = classify_lhs(&lhs) {
                            let line = bytes[..i].iter().filter(|&&b| b == b'\n').count() + 1;
                            out.push(QuantityCastFinding {
                                rel_file: rel_file.to_string(),
                                line,
                                target_type,
                                lhs,
                                kind,
                            });
                        }
                        i = j + 3;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }

    out
}

fn extract_lhs_operand(clean: &str, as_idx: usize) -> String {
    let bytes = clean.as_bytes();
    let mut k = as_idx;
    while k > 0 && bytes[k - 1].is_ascii_whitespace() {
        k -= 1;
    }
    let end = k;
    let mut depth = 0usize;

    while k > 0 {
        let ch = bytes[k - 1];
        if ch == b')' || ch == b']' {
            depth += 1;
            k -= 1;
        } else if ch == b'(' || ch == b'[' {
            if depth == 0 {
                break;
            }
            depth -= 1;
            k -= 1;
        } else if depth > 0 {
            k -= 1;
        } else if is_ident_byte(ch) {
            k -= 1;
        } else if ch == b'.' {
            k -= 1;
            let mut t = k;
            while t > 0 && bytes[t - 1].is_ascii_whitespace() {
                t -= 1;
            }
            if t > 0
                && (bytes[t - 1] == b')' || bytes[t - 1] == b']' || is_ident_byte(bytes[t - 1]))
            {
                k = t;
            } else {
                k += 1;
                break;
            }
        } else if ch == b'*' && (k == 1 || !is_ident_byte(bytes[k - 2])) {
            k -= 1;
        } else {
            break;
        }
    }

    clean[k..end].trim().to_string()
}

fn strip_outer_parens(mut s: &str) -> &str {
    loop {
        s = s.trim();
        let bytes = s.as_bytes();
        if bytes.len() < 2 || bytes[0] != b'(' || bytes[bytes.len() - 1] != b')' {
            return s;
        }
        let mut depth = 0usize;
        let mut matched = true;
        for (idx, &b) in bytes.iter().enumerate() {
            if b == b'(' {
                depth += 1;
            } else if b == b')' {
                depth -= 1;
            }
            if depth == 0 && idx + 1 != bytes.len() {
                matched = false;
                break;
            }
        }
        if !matched || depth != 0 {
            return s;
        }
        s = &s[1..bytes.len() - 1];
    }
}

fn unwrap_expr(mut s: &str) -> &str {
    loop {
        let no_parens = strip_outer_parens(s);
        let no_deref = no_parens.trim_start_matches('*').trim();
        if no_deref == s {
            return no_deref;
        }
        s = no_deref;
    }
}

fn split_top_level_dots(s: &str) -> Vec<&str> {
    let bytes = s.as_bytes();
    let mut segs = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (idx, &b) in bytes.iter().enumerate() {
        match b {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b'.' if depth == 0 => {
                let seg = s[start..idx].trim();
                if !seg.is_empty() {
                    segs.push(seg);
                }
                start = idx + 1;
            }
            _ => {}
        }
    }
    let tail = s[start..].trim();
    if !tail.is_empty() {
        segs.push(tail);
    }
    segs
}

fn is_qty_ident(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    if bytes.is_empty() || bytes[0].is_ascii_digit() || !bytes.iter().all(|&b| is_ident_byte(b)) {
        return false;
    }
    seg.split('_')
        .filter(|part| !part.is_empty())
        .any(|part| {
            part.eq_ignore_ascii_case("qty")
                || part.eq_ignore_ascii_case("quantity")
                || part.eq_ignore_ascii_case("runs")
        })
}

fn is_compute_qty_call(seg: &str) -> bool {
    let Some(open) = seg.find('(') else {
        return false;
    };
    if !seg.ends_with(')') {
        return false;
    }
    let callee = seg[..open].trim();
    let fn_name = callee.rsplit("::").next().unwrap_or(callee).trim();
    let bytes = fn_name.as_bytes();
    !bytes.is_empty()
        && bytes.iter().all(|&b| is_ident_byte(b))
        && fn_name.starts_with("compute_")
        && fn_name.ends_with("_qty")
}

fn is_clamping_seg(seg: &str) -> bool {
    let Some(open) = seg.find('(') else {
        return false;
    };
    if !seg.ends_with(')') {
        return false;
    }
    let method = seg[..open].trim();
    matches!(
        method,
        "max" | "min" | "clamp" | "unwrap_or" | "unwrap_or_default" | "floor" | "round" | "ceil"
    )
}

fn contains_qty_word_ident(s: &str) -> bool {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0usize;
    while i < n {
        if is_ident_byte(bytes[i]) && !bytes[i].is_ascii_digit() {
            let start = i;
            i += 1;
            while i < n && is_ident_byte(bytes[i]) {
                i += 1;
            }
            if is_qty_ident(&s[start..i]) {
                return true;
            }
        } else {
            i += 1;
        }
    }
    false
}

fn classify_lhs(lhs: &str) -> Option<QuantityCastKind> {
    let unwrapped = unwrap_expr(lhs);
    let segs = split_top_level_dots(unwrapped);
    if segs.is_empty() {
        return None;
    }
    if segs.len() == 1 {
        let seg = unwrap_expr(segs[0]);
        if is_qty_ident(seg) || is_compute_qty_call(seg) {
            return Some(QuantityCastKind::Direct);
        }
        return None;
    }

    let last = segs[segs.len() - 1];
    if is_qty_ident(last) || is_compute_qty_call(last) {
        return Some(QuantityCastKind::Direct);
    }

    if is_clamping_seg(last) {
        let mut idx = segs.len();
        while idx > 0 && is_clamping_seg(segs[idx - 1]) {
            idx -= 1;
        }
        if idx > 0 {
            let base_tail = unwrap_expr(segs[idx - 1]);
            if is_qty_ident(base_tail) || is_compute_qty_call(base_tail) {
                return Some(QuantityCastKind::Clamped);
            }
            if idx == 1 && contains_qty_word_ident(base_tail) {
                return Some(QuantityCastKind::Clamped);
            }
        }
    }

    None
}

/// Discovers external files wired into compilation exclusively under `#[cfg(test)] mod ...;`
/// (for example `r6_t1_p1_security_tests_combat.rs`) so non-`_tests.rs` test modules are excluded.
fn collect_cfg_test_module_targets(files: &[(String, PathBuf)]) -> Result<HashSet<String>, String> {
    let mut targets = HashSet::new();
    for (rel_file, path) in files {
        if is_test_only_rel_path(rel_file) {
            continue;
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let stripped = strip_rust_comments_keeping_strings(&raw);
        let bytes = stripped.as_bytes();
        let n = bytes.len();
        let mut i = 0usize;

        while i + 1 < n {
            if bytes[i] == b'\'' {
                i = skip_char_literal(bytes, i);
                continue;
            }
            if let Some(next) = skip_raw_string(bytes, i) {
                i = next;
                continue;
            }
            if bytes[i] == b'"' {
                i = skip_quoted_string(bytes, i);
                continue;
            }
            if bytes[i] == b'#' && bytes[i + 1] == b'[' {
                if let Some(attr_end) = find_cfg_test_attr_end(bytes, i) {
                    if let Some((path_attr, mod_name)) =
                        parse_following_external_mod(bytes, &stripped, attr_end + 1)
                    {
                        let parent = Path::new(rel_file).parent().unwrap_or_else(|| Path::new(""));
                        if let Some(explicit_rel) = path_attr {
                            targets.insert(normalize_rel_unix(&parent.join(explicit_rel)));
                        } else {
                            let stem = Path::new(rel_file)
                                .file_stem()
                                .and_then(|s| s.to_str())
                                .unwrap_or("");
                            let sub_base = if stem == "mod" || stem == "lib" || stem == "main" {
                                parent.to_path_buf()
                            } else {
                                parent.join(stem)
                            };
                            targets.insert(normalize_rel_unix(&sub_base.join(format!("{mod_name}.rs"))));
                            targets.insert(normalize_rel_unix(
                                &sub_base.join(mod_name).join("mod.rs"),
                            ));
                        }
                    }
                    i = attr_end + 1;
                    continue;
                }
            }
            i += 1;
        }
    }
    Ok(targets)
}

fn strip_rust_comments_keeping_strings(src: &str) -> String {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        if b[i] == b'\'' {
            let next = skip_char_literal(b, i);
            out.extend_from_slice(&b[i..next]);
            i = next;
        } else if let Some(next) = skip_raw_string(b, i) {
            out.extend_from_slice(&b[i..next]);
            i = next;
        } else if b[i] == b'"' {
            let next = skip_quoted_string(b, i);
            out.extend_from_slice(&b[i..next]);
            i = next;
        } else if b[i] == b'/' && i + 1 < n && b[i + 1] == b'/' {
            while i < n && b[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
        } else if b[i] == b'/' && i + 1 < n && b[i + 1] == b'*' {
            let mut depth = 1usize;
            out.extend_from_slice(b"  ");
            i += 2;
            while i < n && depth > 0 {
                if i + 1 < n && b[i] == b'/' && b[i + 1] == b'*' {
                    depth += 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                } else if i + 1 < n && b[i] == b'*' && b[i + 1] == b'/' {
                    depth -= 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                } else {
                    out.push(if b[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("ASCII comment stripping preserves UTF-8")
}

fn skip_raw_string(bytes: &[u8], start: usize) -> Option<usize> {
    let n = bytes.len();
    if bytes[start] != b'r' {
        return None;
    }
    let valid_prefix = start == 0
        || !is_ident_byte(bytes[start - 1])
        || (bytes[start - 1] == b'b' && (start == 1 || !is_ident_byte(bytes[start - 2])));
    if !valid_prefix {
        return None;
    }
    let mut j = start + 1;
    let mut hashes = 0usize;
    while j < n && bytes[j] == b'#' {
        hashes += 1;
        j += 1;
    }
    if j >= n || bytes[j] != b'"' {
        return None;
    }
    let mut k = j + 1;
    while k < n {
        if bytes[k] == b'"'
            && k + hashes < n
            && bytes[k + 1..=k + hashes].iter().all(|&c| c == b'#')
        {
            return Some(k + 1 + hashes);
        }
        k += 1;
    }
    Some(n)
}

fn skip_char_literal(bytes: &[u8], start: usize) -> usize {
    let n = bytes.len();
    if start + 2 < n && bytes[start + 1] == b'\\' {
        let mut k = start + 2;
        let limit = (start + 10).min(n);
        while k < limit && bytes[k] != b'\'' && bytes[k] != b'\n' {
            k += 1;
        }
        if k < limit && bytes[k] == b'\'' {
            return k + 1;
        }
    } else if start + 2 < n && bytes[start + 2] == b'\'' && bytes[start + 1] != b'\n' {
        return start + 3;
    }
    start + 1
}

fn skip_quoted_string(bytes: &[u8], start: usize) -> usize {
    let n = bytes.len();
    let mut i = start + 1;
    while i < n {
        if bytes[i] == b'\\' && i + 1 < n {
            i += 2;
            continue;
        }
        if bytes[i] == b'"' {
            return i + 1;
        }
        i += 1;
    }
    n
}

fn parse_following_external_mod(
    bytes: &[u8],
    src: &str,
    mut pos: usize,
) -> Option<(Option<String>, String)> {
    let n = bytes.len();
    let mut path_attr: Option<String> = None;

    loop {
        while pos < n && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos + 1 < n && bytes[pos] == b'#' && bytes[pos + 1] == b'[' {
            let attr_start = pos + 2;
            pos += 2;
            let mut depth = 1usize;
            while pos < n && depth > 0 {
                match bytes[pos] {
                    b'"' => pos = skip_quoted_string(bytes, pos),
                    b'[' => {
                        depth += 1;
                        pos += 1;
                    }
                    b']' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        pos += 1;
                    }
                    _ => pos += 1,
                }
            }
            if depth != 0 {
                return None;
            }
            if let Some(p) = extract_path_attr(&src[attr_start..pos]) {
                path_attr = Some(p);
            }
            pos += 1;
            continue;
        }
        break;
    }

    // Skip optional visibility (`pub`, `pub(crate)`, etc.).
    if src[pos..].starts_with("pub")
        && (pos + 3 == n || !is_ident_byte(bytes[pos + 3]))
    {
        pos += 3;
        while pos < n && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos < n && bytes[pos] == b'(' {
            let mut pd = 1usize;
            pos += 1;
            while pos < n && pd > 0 {
                match bytes[pos] {
                    b'(' => pd += 1,
                    b')' => pd -= 1,
                    _ => {}
                }
                pos += 1;
            }
            while pos < n && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
        }
    }

    if !src[pos..].starts_with("mod") || pos + 3 >= n || !bytes[pos + 3].is_ascii_whitespace() {
        return None;
    }
    pos += 3;
    while pos < n && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    let name_start = pos;
    while pos < n && is_ident_byte(bytes[pos]) {
        pos += 1;
    }
    if pos == name_start {
        return None;
    }
    let mod_name = src[name_start..pos].to_string();
    while pos < n && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    if pos < n && bytes[pos] == b';' {
        Some((path_attr, mod_name))
    } else {
        None
    }
}

fn extract_path_attr(inner: &str) -> Option<String> {
    let trimmed = inner.trim();
    let rest = trimmed.strip_prefix("path")?.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let after_open = rest.strip_prefix('"')?;
    let close = after_open.find('"')?;
    Some(after_open[..close].to_string())
}

fn normalize_rel_unix(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(os) => parts.push(os.to_string_lossy().into_owned()),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    struct TempDirGuard {
        root: PathBuf,
    }

    impl TempDirGuard {
        fn new(tag: &str) -> Self {
            let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "evomu_qty_cast_ratchet_{tag}_{}_{seq}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("create temp ratchet dir");
            Self { root }
        }

        fn path(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn write_rel(base: &Path, rel: &str, contents: &str) {
        let dst = base.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(dst, contents).expect("write test fixture file");
    }

    fn synthetic_ratchet(
        direct_files: BTreeMap<String, usize>,
        clamped_files: BTreeMap<String, usize>,
    ) -> QuantityCastRatchet {
        let mut rules = BTreeMap::new();
        rules.insert(
            RULE_DIRECT_QUANTITY_CASTS.to_string(),
            QuantityCastRuleRatchet {
                zero_required: direct_files.is_empty(),
                owning_issue: 1186,
                description: "synthetic direct rule".to_string(),
                files: direct_files,
            },
        );
        rules.insert(
            RULE_CLAMPED_QUANTITY_CASTS.to_string(),
            QuantityCastRuleRatchet {
                zero_required: false,
                owning_issue: 1186,
                description: "synthetic clamped rule".to_string(),
                files: clamped_files,
            },
        );
        QuantityCastRatchet {
            version: 2,
            description: "synthetic".to_string(),
            rules,
        }
    }

    #[test]
    fn quantity_cast_ratchet_holds_against_live_services() {
        let ratchet =
            QuantityCastRatchet::load_default().expect("load quantity_cast_ratchet.json");
        assert_eq!(ratchet.version, 2);
        let summary = ratchet
            .verify_default()
            .expect("live crates/eve-server/src/services must satisfy quantity_cast_ratchet.json");
        assert!(summary.scanned_files >= 100);
        assert!(
            ratchet.rules[RULE_DIRECT_QUANTITY_CASTS].zero_required,
            "direct_quantity_casts must remain locked with zero_required = true"
        );
        assert_eq!(
            summary.rules[RULE_DIRECT_QUANTITY_CASTS].observed_total,
            0,
            "direct_quantity_casts observed_total must be 0 across live services"
        );
        for name in [RULE_DIRECT_QUANTITY_CASTS, RULE_CLAMPED_QUANTITY_CASTS] {
            let rule_report = &summary.rules[name];
            assert!(
                rule_report.observed_total <= rule_report.baseline_total,
                "rule '{name}' total regressed ({} > {})",
                rule_report.observed_total,
                rule_report.baseline_total
            );
        }
    }

    #[test]
    fn scanner_ignores_comments_strings_cfg_test_external_tests_and_non_qty_casts() {
        let temp = TempDirGuard::new("clean");
        write_rel(
            temp.path(),
            "mod.rs",
            "#[cfg(test)]\nmod security_extra;\n#[cfg(test)]\n#[path = \"alt_extra.rs\"]\nmod alt_mod;\n",
        );
        write_rel(temp.path(), "security_extra.rs", "pub fn t(qty: i64) -> i32 { qty as i32 }\n");
        write_rel(temp.path(), "alt_extra.rs", "pub fn t2(qty: i64) -> u32 { qty as u32 }\n");
        write_rel(temp.path(), "foo_tests.rs", "pub fn t3(qty: i64) -> i32 { qty as i32 }\n");
        write_rel(
            temp.path(),
            "prod.rs",
            r##"
// let _ = qty as i32;
/* let _ = item.quantity as u32; */
pub fn safe(mission_id: i64, tier: u8, tid: i64, duration_seconds: u64) -> (i32, u32) {
    let _s1 = "qty as i32";
    let _s2 = r#"compute_reward_qty(1) as u32"#;
    let a = ((mission_id % 7) as i32) + (tier as i32);
    let b = (tid as u32).saturating_add(duration_seconds as u32);
    (a, b)
}
#[cfg(test)]
mod tests {
    #[test]
    fn inside_cfg_test() {
        let qty = 5_i64;
        assert_eq!(qty as i32, 5);
    }
}
"##,
        );

        let ratchet = synthetic_ratchet(BTreeMap::new(), BTreeMap::new());
        let summary = ratchet
            .verify_dir(temp.path())
            .expect("clean comments/strings/test-blocks/non-qty casts should pass");
        assert_eq!(summary.rules[RULE_DIRECT_QUANTITY_CASTS].observed_total, 0);
        assert_eq!(summary.rules[RULE_CLAMPED_QUANTITY_CASTS].observed_total, 0);
    }

    #[test]
    fn introducing_qty_as_i32_or_multiline_or_compute_fails_naming_exact_file_and_line() {
        let temp = TempDirGuard::new("regression");
        write_rel(
            temp.path(),
            "bad_service.rs",
            concat!(
                "// line 1 comment\n",
                "pub fn convert(qty: i64, loot: &Can, lvl: u32) -> i32 {\n",
                "    let ok = lvl as i32;\n",
                "    let bad_bare = qty as i32;\n",
                "    let bad_field = loot.quantity\n",
                "        as i32;\n",
                "    let bad_deref = *&qty as u32;\n",
                "    let bad_fn = compute_bonus_qty(lvl) as i32;\n",
                "    ok + bad_bare + bad_field + (bad_deref as i32) + bad_fn\n",
                "}\n",
            ),
        );

        let ratchet = synthetic_ratchet(BTreeMap::new(), BTreeMap::new());
        let err = ratchet
            .verify_dir(temp.path())
            .expect_err("introducing qty as i32 must fail verification");
        assert!(
            err.contains("crates/eve-server/src/services/bad_service.rs:4"),
            "missing exact line 4 location in:\n{err}"
        );
        assert!(
            err.contains("crates/eve-server/src/services/bad_service.rs:6"),
            "missing exact multi-line location line 6 in:\n{err}"
        );
        assert!(
            err.contains("crates/eve-server/src/services/bad_service.rs:8"),
            "missing compute_*_qty call location line 8 in:\n{err}"
        );
    }

    #[test]
    fn clamped_quantity_cast_regression_fails_with_exact_file_and_line() {
        let temp = TempDirGuard::new("clamped_regression");
        write_rel(
            temp.path(),
            "clamped_svc.rs",
            "pub fn f(qty: i64) -> i32 {\n    qty.max(1) as i32\n}\n",
        );

        let ratchet = synthetic_ratchet(BTreeMap::new(), BTreeMap::new());
        let err = ratchet
            .verify_dir(temp.path())
            .expect_err("unratcheted clamped quantity cast must fail");
        assert!(
            err.contains("crates/eve-server/src/services/clamped_svc.rs:2"),
            "missing exact clamped line 2 in:\n{err}"
        );
    }
}
