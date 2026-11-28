//! Static closure source guard enforcing `#1193` slash/domain architecture invariants.
//!
//! Scans production `crates/eve-server/src` sources (excluding comments, string/char literals
//! for identifier checks, and `#[cfg(test)]` items) and verifies:
//! 1. Zero direct `items`, `skills`, `skill_queues`, wallet `balance`, ballpark (`spawn_ball*`,
//!    `remove_ball*`, `spawn_asteroid`), `get_or_spawn(`, or dogma cache mutations inside
//!    slash-command handlers across `router/slash_registry.rs`, `services/auth_cache.rs`,
//!    `services/character_gm_tools.rs` (excluding non-GM character-creation RPCs),
//!    `services/character_gm_helpers.rs`, `services/notify_emitters_character_services.rs`, and
//!    `services/gm_soak_commands.rs`.
//! 2. Zero `domain/gm_*.rs` GM-only forks, and every `crates/eve-server/src/domain/*.rs` module
//!    has at least one production non-GM gameplay caller (`#1193#issuecomment-7091401173`).
//! 3. Zero `.strip_prefix("/` or `.starts_with("/` slash parsing outside `router/slash_registry.rs`
//!    (excluding HTTP route modules `sso.rs`, `bug_reporting.rs`, and `esi/`).

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::client_harness::{is_ident_byte, strip_rust_comments_and_literals};
use crate::notify_ratchet::{
    collect_rs_files_recursive, is_test_only_rel_path, strip_cfg_test_items,
};

/// Relative workspace paths under `crates/eve-server/src` covered by slash-handler scans.
pub const MONITORED_SLASH_FILES: &[&str] = &[
    "router/slash_registry.rs",
    "services/auth_cache.rs",
    "services/character_gm_tools.rs",
    "services/character_gm_helpers.rs",
    "services/notify_emitters_character_services.rs",
    "services/gm_soak_commands.rs",
];

/// Single architectural invariant violation reported by [`scan_slash_domain_closure`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashDomainViolation {
    pub rule: String,
    pub file: String,
    pub detail: String,
}

/// Summary report returned by [`scan_slash_domain_closure`] and [`verify_slash_domain_closure`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlashDomainGuardReport {
    pub scanned_slash_files: usize,
    pub scanned_domain_modules: usize,
    pub domain_non_gm_caller_counts: BTreeMap<String, usize>,
    pub violations: Vec<SlashDomainViolation>,
}

/// Default workspace path to `crates/eve-server/src`.
pub fn default_server_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("eve-server")
        .join("src")
}

/// Scans the live workspace `crates/eve-server/src` directory and fails if any `#1193`
/// slash-domain closure invariant is violated.
pub fn verify_default_slash_domain_closure() -> Result<SlashDomainGuardReport, String> {
    verify_slash_domain_closure(&default_server_src_dir())
}

/// Scans `server_src_dir` and returns `Err` when any `#1193` closure invariant is violated.
pub fn verify_slash_domain_closure(server_src_dir: &Path) -> Result<SlashDomainGuardReport, String> {
    let report = scan_slash_domain_closure(server_src_dir)?;
    if !report.violations.is_empty() {
        let formatted: Vec<String> = report
            .violations
            .iter()
            .take(12)
            .map(|v| format!("[{}] {}: {}", v.rule, v.file, v.detail))
            .collect();
        let extra = report.violations.len().saturating_sub(formatted.len());
        let suffix = if extra > 0 {
            format!(" (+ {extra} more)")
        } else {
            String::new()
        };
        return Err(format!(
            "slash-domain closure guard found {} violation(s): {}{suffix}",
            report.violations.len(),
            formatted.join("; ")
        ));
    }
    Ok(report)
}

/// Scans `server_src_dir` without failing early so callers and tests can inspect all violations.
pub fn scan_slash_domain_closure(server_src_dir: &Path) -> Result<SlashDomainGuardReport, String> {
    let mut rs_files: Vec<(String, PathBuf)> = Vec::new();
    collect_rs_files_recursive(server_src_dir, server_src_dir, &mut rs_files)?;
    rs_files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut prod_sources: BTreeMap<String, (String, String)> = BTreeMap::new();
    for (rel_unix, abs_path) in rs_files {
        if is_test_only_rel_path(&rel_unix) {
            continue;
        }
        let raw = std::fs::read_to_string(&abs_path)
            .map_err(|e| format!("failed to read {}: {e}", abs_path.display()))?;
        let code_only = strip_cfg_test_items(&strip_rust_comments_and_literals(&raw));
        prod_sources.insert(rel_unix, (raw, code_only));
    }

    let mut report = SlashDomainGuardReport::default();

    // 1. Check domain modules: no `domain/gm_*.rs` forks and every `domain/*.rs` has >= 1
    //    production non-GM gameplay caller (`#7091401173`).
    let mut domain_modules: Vec<String> = Vec::new();
    for rel_unix in prod_sources.keys() {
        let Some(rest) = rel_unix.strip_prefix("domain/") else {
            continue;
        };
        if rest.contains('/') {
            continue;
        }
        let stem = rest.strip_suffix(".rs").unwrap_or(rest);
        if stem == "mod" {
            continue;
        }
        if stem.starts_with("gm_") {
            report.violations.push(SlashDomainViolation {
                rule: "domain_gm_module_forbidden".to_string(),
                file: rel_unix.clone(),
                detail: format!("GM-only domain module `domain/{rest}` is prohibited"),
            });
            continue;
        }
        domain_modules.push(stem.to_string());
    }
    report.scanned_domain_modules = domain_modules.len();

    let slash_file_set: BTreeSet<&str> = MONITORED_SLASH_FILES.iter().copied().collect();
    for module_name in &domain_modules {
        let mut caller_count = 0usize;
        for (rel_unix, (_, code_only)) in &prod_sources {
            if rel_unix.starts_with("domain/")
                || slash_file_set.contains(rel_unix.as_str())
                || rel_unix.contains("_gm_")
            {
                continue;
            }
            if contains_domain_module_reference(code_only, module_name) {
                caller_count += 1;
            }
        }
        report
            .domain_non_gm_caller_counts
            .insert(module_name.clone(), caller_count);
        if caller_count == 0 {
            report.violations.push(SlashDomainViolation {
                rule: "domain_missing_non_gm_caller".to_string(),
                file: format!("domain/{module_name}.rs"),
                detail: format!(
                    "domain::{module_name} has 0 non-GM gameplay callers outside slash/GM modules"
                ),
            });
        }
    }

    // 2. Ban slash-command prefix parsing outside `router/slash_registry.rs` (and HTTP routes).
    for (rel_unix, (raw, code_only)) in &prod_sources {
        if rel_unix == "router/slash_registry.rs" || is_http_route_module(rel_unix) {
            continue;
        }
        for method in [".strip_prefix", ".starts_with"] {
            if has_slash_literal_string_call(raw, code_only, method) {
                report.violations.push(SlashDomainViolation {
                    rule: "slash_parsing_outside_registry".to_string(),
                    file: rel_unix.clone(),
                    detail: format!(
                        "Found `{method}(\"/...` outside `router/slash_registry.rs`"
                    ),
                });
            }
        }
    }

    // 3. Enforce domain boundary inside slash handlers across the 6 monitored modules.
    for &rel_unix in MONITORED_SLASH_FILES {
        let Some((_, code_only)) = prod_sources.get(rel_unix) else {
            continue;
        };
        report.scanned_slash_files += 1;
        let slices = collect_slash_handler_code_slices(rel_unix, code_only);
        for slice in slices {
            let normalized = normalize_code_for_pattern_scan(&slice);
            check_slash_code_patterns(rel_unix, &normalized, &mut report.violations);
        }
    }

    Ok(report)
}

fn is_http_route_module(rel_unix: &str) -> bool {
    rel_unix == "sso.rs" || rel_unix == "bug_reporting.rs" || rel_unix.starts_with("esi/")
}

fn contains_domain_module_reference(code_only: &str, module_name: &str) -> bool {
    let needle_direct = format!("domain::{module_name}");
    if contains_ident_delimited(code_only, &needle_direct) {
        return true;
    }
    let b = code_only.as_bytes();
    let prefix = b"domain::{";
    let mut search_from = 0usize;
    while let Some(rel_idx) = find_bytes_from(b, prefix, search_from) {
        let start = rel_idx + prefix.len();
        if let Some(close_rel) = b[start..].iter().position(|&c| c == b'}') {
            let inner = &code_only[start..start + close_rel];
            if contains_ident_delimited(inner, module_name) {
                return true;
            }
        }
        search_from = start;
    }
    false
}

fn find_bytes_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from + needle.len() > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|offset| from + offset)
}

fn contains_ident_delimited(src: &str, ident_expr: &str) -> bool {
    let b = src.as_bytes();
    let needle = ident_expr.as_bytes();
    let mut pos = 0usize;
    while let Some(idx) = find_bytes_from(b, needle, pos) {
        let before_ok = idx == 0 || !is_ident_byte(b[idx - 1]);
        let end = idx + needle.len();
        let after_ok = end == b.len() || !is_ident_byte(b[end]);
        if before_ok && after_ok {
            return true;
        }
        pos = idx + 1;
    }
    false
}

fn has_slash_literal_string_call(raw: &str, code_only: &str, method: &str) -> bool {
    let raw_bytes = raw.as_bytes();
    let code_bytes = code_only.as_bytes();
    let method_bytes = method.as_bytes();
    if code_bytes.len() != raw_bytes.len() {
        return false;
    }

    let mut pos = 0usize;
    while let Some(idx) = find_bytes_from(code_bytes, method_bytes, pos) {
        let mut cursor = idx + method_bytes.len();
        while cursor < code_bytes.len() && code_bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor < code_bytes.len() && code_bytes[cursor] == b'(' {
            cursor += 1;
            while cursor < raw_bytes.len() && raw_bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if raw_bytes[cursor..].starts_with(b"\"/")
                || raw_bytes[cursor..].starts_with(b"r\"/")
                || raw_bytes[cursor..].starts_with(b"r#\"/")
            {
                return true;
            }
        }
        pos = idx + 1;
    }
    false
}

fn is_monitored_slash_fn(rel_unix: &str, fn_name: &str) -> bool {
    match rel_unix {
        "services/auth_cache.rs" => fn_name.contains("slash"),
        "services/character_gm_tools.rs" => {
            fn_name.contains("slash")
                || fn_name == "rollback_spawned_entities"
                || fn_name == "parse_optional_target_char_and_value"
        }
        "services/notify_emitters_character_services.rs" => {
            fn_name.contains("slash")
                || fn_name == "fast_forward_and_sync_space_clock"
                || fn_name == "resolve_optional_target_char"
                || fn_name == "resolve_target_solar_system"
        }
        _ => true,
    }
}

fn collect_slash_handler_code_slices(rel_unix: &str, code_only: &str) -> Vec<String> {
    match rel_unix {
        "router/slash_registry.rs"
        | "services/character_gm_helpers.rs"
        | "services/gm_soak_commands.rs" => vec![code_only.to_string()],
        _ => extract_top_level_functions(code_only)
            .into_iter()
            .filter(|(name, _)| is_monitored_slash_fn(rel_unix, name))
            .map(|(_, body)| body)
            .collect(),
    }
}

fn extract_top_level_functions(code_only: &str) -> Vec<(String, String)> {
    let b = code_only.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;

    while i < n {
        match b[i] {
            b'{' => {
                brace_depth += 1;
                i += 1;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                i += 1;
            }
            b'(' => {
                paren_depth += 1;
                i += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                i += 1;
            }
            _ if brace_depth == 0
                && paren_depth == 0
                && i + 2 < n
                && &b[i..i + 2] == b"fn"
                && (i == 0 || !is_ident_byte(b[i - 1]))
                && b[i + 2].is_ascii_whitespace() =>
            {
                let mut name_start = i + 2;
                while name_start < n && b[name_start].is_ascii_whitespace() {
                    name_start += 1;
                }
                let mut name_end = name_start;
                while name_end < n && is_ident_byte(b[name_end]) {
                    name_end += 1;
                }
                if name_end == name_start {
                    i += 2;
                    continue;
                }
                let fn_name = String::from_utf8_lossy(&b[name_start..name_end]).to_string();
                let mut scan = name_end;
                let mut sig_parens = 0usize;
                let mut body_start = None;
                while scan < n {
                    match b[scan] {
                        b'(' => sig_parens += 1,
                        b')' => sig_parens = sig_parens.saturating_sub(1),
                        b'{' if sig_parens == 0 => {
                            body_start = Some(scan);
                            break;
                        }
                        b';' if sig_parens == 0 => break,
                        _ => {}
                    }
                    scan += 1;
                }
                let Some(open_brace) = body_start else {
                    i = scan.max(name_end);
                    continue;
                };
                let mut depth = 1usize;
                let mut end = open_brace + 1;
                while end < n && depth > 0 {
                    match b[end] {
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    end += 1;
                }
                out.push((fn_name, code_only[i..end].to_string()));
                i = end;
            }
            _ => {
                i += 1;
            }
        }
    }

    out
}

/// Collapses ASCII whitespace runs to a single `' '` and strips spaces around `.` and before `(`
/// so multi-line chained calls like `snap\n .items\n .insert (` normalize deterministically.
fn normalize_code_for_pattern_scan(code: &str) -> String {
    let mut single_spaced = String::with_capacity(code.len());
    let mut prev_ws = false;
    for ch in code.chars() {
        if ch.is_ascii_whitespace() {
            if !prev_ws {
                single_spaced.push(' ');
                prev_ws = true;
            }
        } else {
            single_spaced.push(ch);
            prev_ws = false;
        }
    }
    let bytes = single_spaced.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b' ' {
            let prev = out.as_bytes().last().copied();
            let next = bytes.get(i + 1).copied();
            if matches!(prev, Some(b'.')) || matches!(next, Some(b'.' | b'(')) {
                i += 1;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn check_slash_code_patterns(
    rel_unix: &str,
    normalized: &str,
    violations: &mut Vec<SlashDomainViolation>,
) {
    let checks: &[(&str, &[&str])] = &[
        (
            "direct_item_mutation",
            &[
                ".items.insert(",
                ".items.remove(",
                ".items.get_mut(",
                ".items.values_mut(",
            ],
        ),
        (
            "direct_skill_mutation",
            &[".skills.insert(", ".skills.remove(", ".skills.get_mut("],
        ),
        (
            "direct_skill_queue_mutation",
            &[
                ".skill_queues.insert(",
                ".skill_queues.remove(",
                ".skill_queues.get_mut(",
            ],
        ),
        (
            "direct_dogma_cache_mutation",
            &[
                ".ship_fittings.write(",
                ".ship_fit_revs.write(",
                ".fitted_modules.insert(",
                ".fitted_modules.remove(",
                ".fitted_modules.get_mut(",
            ],
        ),
    ];

    for &(rule, patterns) in checks {
        for &pat in patterns {
            if normalized.contains(pat) {
                violations.push(SlashDomainViolation {
                    rule: rule.to_string(),
                    file: rel_unix.to_string(),
                    detail: format!("Found prohibited `{pat}` inside slash handler"),
                });
            }
        }
    }

    for fn_pat in [
        "store_ship_fitting(",
    ] {
        if contains_word_prefix(normalized, fn_pat) {
            violations.push(SlashDomainViolation {
                rule: "direct_dogma_cache_mutation".to_string(),
                file: rel_unix.to_string(),
                detail: format!("Found prohibited `{fn_pat}` inside slash handler"),
            });
        }
    }

    if has_wallet_balance_write(normalized) {
        violations.push(SlashDomainViolation {
            rule: "direct_wallet_balance_write".to_string(),
            file: rel_unix.to_string(),
            detail: "Found prohibited direct wallet `.balance` mutation inside slash handler"
                .to_string(),
        });
    }

    for call in [
        "spawn_ball(",
        "spawn_ball_with_meta(",
        "spawn_ball_with_meta_tagged(",
        "try_spawn_ball(",
        "remove_ball(",
        "remove_ball_if_token(",
        "try_remove_ball(",
        "spawn_asteroid(",
    ] {
        if contains_word_prefix(normalized, call) {
            violations.push(SlashDomainViolation {
                rule: "direct_ballpark_mutation".to_string(),
                file: rel_unix.to_string(),
                detail: format!("Found prohibited ballpark call `{call}` inside slash handler"),
            });
        }
    }

    for wake in ["get_or_spawn(", "get_or_spawn_if_runtime("] {
        if contains_word_prefix(normalized, wake) {
            violations.push(SlashDomainViolation {
                rule: "direct_get_or_spawn".to_string(),
                file: rel_unix.to_string(),
                detail: format!("Found prohibited `{wake}` inside slash handler"),
            });
        }
    }
}

fn contains_word_prefix(src: &str, needle: &str) -> bool {
    let b = src.as_bytes();
    let n = needle.as_bytes();
    let mut pos = 0usize;
    while let Some(idx) = find_bytes_from(b, n, pos) {
        if idx == 0 || !is_ident_byte(b[idx - 1]) {
            return true;
        }
        pos = idx + 1;
    }
    false
}

fn has_wallet_balance_write(normalized: &str) -> bool {
    let b = normalized.as_bytes();
    let needle = b".balance";
    let mut pos = 0usize;
    while let Some(idx) = find_bytes_from(b, needle, pos) {
        let after_ident = idx + needle.len();
        if after_ident < b.len() && is_ident_byte(b[after_ident]) {
            pos = idx + 1;
            continue;
        }
        let mut cursor = after_ident;
        while cursor < b.len() && b[cursor] == b' ' {
            cursor += 1;
        }
        if cursor < b.len() && (b[cursor..].starts_with(b"+=") || b[cursor..].starts_with(b"-=")) {
            return true;
        }
        if cursor < b.len()
            && b[cursor] == b'='
            && !b[cursor..].starts_with(b"==")
        {
            return true;
        }
        pos = idx + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "evomu_slash_domain_guard_{label}_{}_{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).expect("create temp guard dir");
            Self(dir)
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_src(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(p, body).expect("write fixture rs");
    }

    fn seed_clean_minimal_workspace(src: &Path) {
        write_src(
            src,
            "domain/items.rs",
            "pub fn set_attr() {}\n",
        );
        write_src(
            src,
            "services/mining_cycle.rs",
            "pub fn apply() { crate::domain::items::set_attr(); }\n",
        );
        write_src(
            src,
            "router/slash_registry.rs",
            "pub fn parse(cmd: &str) -> Option<&str> { cmd.strip_prefix(\"/\") }\n",
        );
        write_src(
            src,
            "services/auth_cache.rs",
            "pub fn dispatch_help_slash() {}\npub fn login() { snap.items.insert(1, x); }\n",
        );
        write_src(
            src,
            "services/character_gm_tools.rs",
            "pub fn create_character_with_doll_with_race() { snap.items.insert(1, x); snap.skills.insert(1, s); c.balance += 1; }\n\
             pub fn dispatch_spawn_slash() { crate::domain::items::set_attr(); }\n",
        );
        write_src(
            src,
            "services/character_gm_helpers.rs",
            "pub fn helper() {}\n#[cfg(test)]\nmod tests { fn t() { snap.items.insert(1, x); ctx.systems.get_or_spawn(1); } }\n",
        );
        write_src(
            src,
            "services/notify_emitters_character_services.rs",
            "pub fn emit_other() { snap.items.get_mut(&1); }\npub fn dispatch_advancetime_slash() {}\n",
        );
        write_src(
            src,
            "services/gm_soak_commands.rs",
            "pub fn dispatch_soak_slash() {}\n",
        );
    }

    #[test]
    fn live_workspace_passes_slash_domain_closure_guard() {
        let report = verify_default_slash_domain_closure()
            .expect("live workspace must pass all slash-domain closure invariants");
        assert_eq!(report.scanned_slash_files, MONITORED_SLASH_FILES.len());
        assert!(
            report.scanned_domain_modules >= 10,
            "expected all domain modules scanned, got {}",
            report.scanned_domain_modules
        );
        for required_domain in ["combat_ops", "items", "skills", "transitions", "fitting"] {
            let count = report
                .domain_non_gm_caller_counts
                .get(required_domain)
                .copied()
                .unwrap_or(0);
            assert!(
                count >= 1,
                "domain::{required_domain} must have >= 1 non-GM caller, got {count}"
            );
        }
    }

    #[test]
    fn guard_allows_non_gm_code_and_rejects_synthetic_violations() {
        let tmp = TempDirGuard::new("clean_baseline");
        seed_clean_minimal_workspace(&tmp.0);
        verify_slash_domain_closure(&tmp.0)
            .expect("clean synthetic workspace excluding char creation must pass");

        // 1. Direct snap.items.insert inside slash handler must fail.
        let tmp_items = TempDirGuard::new("direct_items");
        seed_clean_minimal_workspace(&tmp_items.0);
        write_src(
            &tmp_items.0,
            "services/gm_soak_commands.rs",
            "pub fn dispatch_spawnwreck_slash() { snap.items.insert(1, rec); }\n",
        );
        let rep = scan_slash_domain_closure(&tmp_items.0).expect("scan");
        assert!(
            rep.violations.iter().any(|v| v.rule == "direct_item_mutation"),
            "expected direct_item_mutation, got {:?}",
            rep.violations
        );

        // 2. Direct spawn_ball inside slash handler must fail.
        let tmp_ball = TempDirGuard::new("direct_ball");
        seed_clean_minimal_workspace(&tmp_ball.0);
        write_src(
            &tmp_ball.0,
            "services/character_gm_tools.rs",
            "pub fn dispatch_spawn_slash() { handle.spawn_ball_with_meta(b, c, m); }\n",
        );
        let rep = scan_slash_domain_closure(&tmp_ball.0).expect("scan");
        assert!(
            rep.violations.iter().any(|v| v.rule == "direct_ballpark_mutation"),
            "expected direct_ballpark_mutation, got {:?}",
            rep.violations
        );

        // 3. domain/gm_fake.rs must fail.
        let tmp_gm_fork = TempDirGuard::new("gm_fork");
        seed_clean_minimal_workspace(&tmp_gm_fork.0);
        write_src(&tmp_gm_fork.0, "domain/gm_fake.rs", "pub fn nope() {}\n");
        let rep = scan_slash_domain_closure(&tmp_gm_fork.0).expect("scan");
        assert!(
            rep.violations
                .iter()
                .any(|v| v.rule == "domain_gm_module_forbidden"),
            "expected domain_gm_module_forbidden, got {:?}",
            rep.violations
        );

        // 4. get_or_spawn( inside slash handler must fail.
        let tmp_wake = TempDirGuard::new("get_or_spawn");
        seed_clean_minimal_workspace(&tmp_wake.0);
        write_src(
            &tmp_wake.0,
            "services/auth_cache.rs",
            "pub fn handle_slash_spawn_npc() { let _ = ctx.systems.get_or_spawn(sys_id); }\n",
        );
        let rep = scan_slash_domain_closure(&tmp_wake.0).expect("scan");
        assert!(
            rep.violations.iter().any(|v| v.rule == "direct_get_or_spawn"),
            "expected direct_get_or_spawn, got {:?}",
            rep.violations
        );

        // 5. strip_prefix("/ outside router/slash_registry.rs must fail.
        let tmp_strip = TempDirGuard::new("strip_prefix");
        seed_clean_minimal_workspace(&tmp_strip.0);
        write_src(
            &tmp_strip.0,
            "services/gm_soak_commands.rs",
            "pub fn parse(s: &str) { let _ = s.strip_prefix(\"/soak\"); }\n",
        );
        let rep = scan_slash_domain_closure(&tmp_strip.0).expect("scan");
        assert!(
            rep.violations
                .iter()
                .any(|v| v.rule == "slash_parsing_outside_registry"),
            "expected slash_parsing_outside_registry, got {:?}",
            rep.violations
        );

        // 6. Direct dogma cache write, wallet balance write, skill mutation, and orphan domain fail.
        let tmp_extra = TempDirGuard::new("extra_rules");
        seed_clean_minimal_workspace(&tmp_extra.0);
        write_src(&tmp_extra.0, "domain/orphan_only_gm.rs", "pub fn f() {}\n");
        write_src(
            &tmp_extra.0,
            "services/gm_soak_commands.rs",
            "pub fn bad() {\n\
               snap.skills.insert(1, x);\n\
               snap.skill_queues.remove(&1);\n\
               ch.balance += 100;\n\
               ctx.ship_fittings.write();\n\
             }\n",
        );
        let rep = scan_slash_domain_closure(&tmp_extra.0).expect("scan");
        for expected in [
            "domain_missing_non_gm_caller",
            "direct_skill_mutation",
            "direct_skill_queue_mutation",
            "direct_wallet_balance_write",
            "direct_dogma_cache_mutation",
        ] {
            assert!(
                rep.violations.iter().any(|v| v.rule == expected),
                "expected `{expected}`, got {:?}",
                rep.violations
            );
        }
    }
}
