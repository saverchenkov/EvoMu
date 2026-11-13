//! Falling ratchet for raw untyped notification calls in production `eve-server` code.
//!
//! Scans non-test `.rs` sources under `crates/eve-server/src/` (ignoring comments, string/char
//! literals, `#[cfg(test)]` items, and core transport plumbing modules) for legacy raw notification
//! calls (`.notify(`, `.push_notification(`, `build_macho_notification_packet(`, `notify_char(`,
//! `.notify_remote_message(`) so un-migrated call sites cannot silently increase.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::client_harness::strip_rust_comments_and_literals;

/// Scan summary produced by [`scan_raw_notify_calls_in_dir`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawNotifyScanReport {
    pub raw_notify_calls: usize,
    pub files_with_raw_notify: usize,
    pub per_file_raw_notify: BTreeMap<String, usize>,
}

/// Falling ratchet limiting raw untyped notification dispatches under `crates/eve-server/src/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawNotifyRatchet {
    pub version: u32,
    pub max_raw_notify_calls: usize,
    pub max_files_with_raw_notify: usize,
}

impl RawNotifyRatchet {
    /// Returns the repository path to `crates/eve-testkit/ratchets/raw_notify_ratchet.json`.
    pub fn default_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ratchets")
            .join("raw_notify_ratchet.json")
    }

    /// Returns the repository path to `crates/eve-server/src`.
    pub fn default_server_src_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../eve-server/src")
    }

    /// Loads `raw_notify_ratchet.json` from its default path.
    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&Self::default_path())
    }

    /// Loads a [`RawNotifyRatchet`] from `path`.
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))
    }

    /// Scans `crates/eve-server/src` and asserts raw notify counts remain within the ratchet.
    pub fn verify_default(&self) -> Result<RawNotifyScanReport, String> {
        let report = scan_raw_notify_calls_in_dir(&Self::default_server_src_dir())?;
        self.assert_ratchet(&report)?;
        Ok(report)
    }

    /// Enforces that both total raw notify calls and distinct files remain at or below the
    /// falling ratchet ceiling.
    pub fn assert_ratchet(&self, report: &RawNotifyScanReport) -> Result<(), String> {
        if report.raw_notify_calls > self.max_raw_notify_calls {
            return Err(format!(
                "Raw untyped notify call count regressed above falling ratchet: found {} calls across {} files, max_raw_notify_calls={}, max_files_with_raw_notify={}. Route new/modified notifications through router::notify_registry instead.",
                report.raw_notify_calls,
                report.files_with_raw_notify,
                self.max_raw_notify_calls,
                self.max_files_with_raw_notify
            ));
        }
        if report.files_with_raw_notify > self.max_files_with_raw_notify {
            return Err(format!(
                "Files containing raw untyped notify calls regressed above ratchet: found {} files ({} calls), max_files_with_raw_notify={}, max_raw_notify_calls={}.",
                report.files_with_raw_notify,
                report.raw_notify_calls,
                self.max_files_with_raw_notify,
                self.max_raw_notify_calls
            ));
        }
        Ok(())
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_test_only_rel_path(rel_unix: &str) -> bool {
    let file_name = rel_unix.rsplit('/').next().unwrap_or(rel_unix);
    rel_unix.starts_with("tests/")
        || rel_unix.contains("/tests/")
        || file_name == "tests.rs"
        || file_name.ends_with("_tests.rs")
        || file_name.ends_with("_test.rs")
}

fn is_notification_plumbing_file(rel_unix: &str) -> bool {
    matches!(
        rel_unix,
        "router/cache_notifications.rs"
            | "router/notify_registry.rs"
            | "router/session.rs"
            | "router/dispatch.rs"
            | "macho_listener.rs"
    )
}

/// Replaces `#[cfg(test)]` items (modules, functions, etc.) in already comment/literal-stripped
/// Rust source with spaces using brace/semicolon balancing so multiple inline test blocks per file
/// are all excluded.
pub fn strip_cfg_test_items(stripped_src: &str) -> String {
    let mut bytes = stripped_src.as_bytes().to_vec();
    let n = bytes.len();
    let mut i = 0usize;

    while i + 1 < n {
        if bytes[i] == b'#' && bytes[i + 1] == b'[' {
            if let Some(attr_end) = find_cfg_test_attr_end(&bytes, i) {
                let item_end = find_attributed_item_end(&bytes, attr_end + 1);
                for b in &mut bytes[i..item_end] {
                    if *b != b'\n' {
                        *b = b' ';
                    }
                }
                i = item_end;
                continue;
            }
        }
        i += 1;
    }

    String::from_utf8(bytes).expect("ASCII cfg(test) masking preserves UTF-8")
}

fn find_cfg_test_attr_end(bytes: &[u8], hash_idx: usize) -> Option<usize> {
    let n = bytes.len();
    let mut j = hash_idx + 2;
    let mut bracket_depth = 1usize;
    while j < n && bracket_depth > 0 {
        match bytes[j] {
            b'[' => bracket_depth += 1,
            b']' => {
                bracket_depth -= 1;
                if bracket_depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }
    if bracket_depth != 0 {
        return None;
    }
    let inner = std::str::from_utf8(&bytes[hash_idx + 2..j]).ok()?;
    let compact: String = inner.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if compact == "cfg(test)" {
        Some(j)
    } else {
        None
    }
}

fn find_attributed_item_end(bytes: &[u8], start: usize) -> usize {
    let n = bytes.len();
    let mut pos = start;
    // Skip whitespace and any sibling attributes `#[...]` before the item's `{` or `;`.
    loop {
        while pos < n && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos + 1 < n && bytes[pos] == b'#' && bytes[pos + 1] == b'[' {
            pos += 2;
            let mut bd = 1usize;
            while pos < n && bd > 0 {
                match bytes[pos] {
                    b'[' => bd += 1,
                    b']' => bd -= 1,
                    _ => {}
                }
                pos += 1;
            }
            continue;
        }
        break;
    }

    while pos < n {
        match bytes[pos] {
            b';' => return pos + 1,
            b'{' => {
                let mut depth = 1usize;
                pos += 1;
                while pos < n && depth > 0 {
                    match bytes[pos] {
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    pos += 1;
                }
                return pos;
            }
            _ => pos += 1,
        }
    }
    n
}

/// Counts raw untyped notification calls in a single comment/literal/cfg(test)-stripped buffer.
pub fn count_raw_notify_calls_in_clean_src(clean: &str) -> usize {
    let b = clean.as_bytes();
    let n = b.len();
    let mut count = 0usize;

    for method in [".notify", ".push_notification", ".notify_remote_message"] {
        let mut idx = 0usize;
        while let Some(rel) = clean[idx..].find(method) {
            let start = idx + rel;
            let after = start + method.len();
            let mut open = after;
            while open < n && b[open].is_ascii_whitespace() {
                open += 1;
            }
            if open < n && b[open] == b'(' {
                count += 1;
                idx = open + 1;
            } else {
                idx = after;
            }
        }
    }

    for func in ["build_macho_notification_packet", "notify_char"] {
        let mut idx = 0usize;
        while let Some(rel) = clean[idx..].find(func) {
            let start = idx + rel;
            let after = start + func.len();
            let prev_is_ident = start > 0 && is_ident_byte(b[start - 1]);
            let prev_is_dot = start > 0 && b[start - 1] == b'.';
            let mut open = after;
            while open < n && b[open].is_ascii_whitespace() {
                open += 1;
            }
            if !prev_is_ident && !prev_is_dot && open < n && b[open] == b'(' && !is_fn_def_header(b, start) {
                count += 1;
                idx = open + 1;
            } else {
                idx = after;
            }
        }
    }

    count
}

fn is_fn_def_header(bytes: &[u8], ident_start: usize) -> bool {
    let mut pos = ident_start;
    while pos > 0 && bytes[pos - 1].is_ascii_whitespace() {
        pos -= 1;
    }
    pos >= 2
        && &bytes[pos - 2..pos] == b"fn"
        && (pos == 2 || !is_ident_byte(bytes[pos - 3]))
}

fn collect_rs_files_recursive(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("failed to read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("failed reading dir entry in {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files_recursive(base, &path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let rel = path
                .strip_prefix(base)
                .map_err(|e| format!("strip_prefix failed for {}: {e}", path.display()))?;
            let rel_unix = rel.to_string_lossy().replace('\\', "/");
            out.push((rel_unix, path));
        }
    }
    Ok(())
}

/// Recursively scans production `.rs` files under `dir` (typically `crates/eve-server/src`)
/// for raw untyped notification calls.
pub fn scan_raw_notify_calls_in_dir(dir: &Path) -> Result<RawNotifyScanReport, String> {
    let mut files = Vec::new();
    collect_rs_files_recursive(dir, dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut report = RawNotifyScanReport::default();
    for (rel_unix, path) in files {
        if is_test_only_rel_path(&rel_unix) || is_notification_plumbing_file(&rel_unix) {
            continue;
        }
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let no_lit = strip_rust_comments_and_literals(&raw);
        let prod_only = strip_cfg_test_items(&no_lit);
        let count = count_raw_notify_calls_in_clean_src(&prod_only);
        if count > 0 {
            report.raw_notify_calls += count;
            report.files_with_raw_notify += 1;
            report.per_file_raw_notify.insert(rel_unix, count);
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanner_ignores_comments_strings_and_multiple_cfg_test_blocks() {
        let src = r#"
            pub fn prod_emitter(ctx: &ServerContext) {
                // ctx.notify(target, "OnCommented", "charid", vec![]);
                let _s = "ctx.notify(target, \"OnString\", \"charid\", vec![])";
                ctx.notify(target, "OnRealProd", "charid", vec![]);
                session.push_notification("OnProdPush", vec![]);
            }

            #[cfg(test)]
            mod tests_one {
                fn helper() {
                    ctx.notify(target, "OnInTestOne", "charid", vec![]);
                }
            }

            pub fn prod_after_test(ctx: &ServerContext) {
                notify_char(ctx, 1, "OnProdNotifyChar", vec![]);
                fn notify_char() {}
            }

            #[cfg(test)]
            #[path = "external_tests.rs"]
            mod external_tests;

            #[cfg(test)]
            fn standalone_test_fn() {
                build_macho_notification_packet(1, None, "OnTestPkt", "charid", vec![]);
            }
        "#;

        let no_lit = strip_rust_comments_and_literals(src);
        let prod_only = strip_cfg_test_items(&no_lit);
        assert_eq!(count_raw_notify_calls_in_clean_src(&prod_only), 3);
    }

    #[test]
    fn ratchet_rejects_regression_and_accepts_reduction() {
        let ratchet = RawNotifyRatchet {
            version: 1,
            max_raw_notify_calls: 10,
            max_files_with_raw_notify: 3,
        };
        let ok_report = RawNotifyScanReport {
            raw_notify_calls: 8,
            files_with_raw_notify: 2,
            per_file_raw_notify: BTreeMap::new(),
        };
        assert!(ratchet.assert_ratchet(&ok_report).is_ok());

        let regressed_calls = RawNotifyScanReport {
            raw_notify_calls: 11,
            files_with_raw_notify: 2,
            per_file_raw_notify: BTreeMap::new(),
        };
        let err = ratchet
            .assert_ratchet(&regressed_calls)
            .expect_err("must reject raw notify count regression");
        assert!(err.contains("regressed"));

        let regressed_files = RawNotifyScanReport {
            raw_notify_calls: 8,
            files_with_raw_notify: 4,
            per_file_raw_notify: BTreeMap::new(),
        };
        assert!(ratchet.assert_ratchet(&regressed_files).is_err());
    }
}
