//! Falling repository-wide architecture-debt ratchet across production `eve-server` code (`#1197`).
//!
//! Scans non-test `.rs` sources under `crates/eve-server/src/` (ignoring `#[cfg(test)]` items,
//! comments, and—unless explicitly enabled per rule—string/char literals) for every tracked debt
//! counter in `crates/eve-testkit/ratchets/arch_debt_ratchet.json`. Non-zero rules act as upper
//! bounds (`exact: false`) for both occurrence counts and distinct file counts so parallel tracks
//! lowering debt do not break each other, while `zero_required: true` rules remain strictly pinned
//! at `0` (`exact: true`). Rules configured with `exact: true` also fail if counts drop below
//! their configured floor without tightening the JSON baseline in the same commit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::client_harness::{is_ident_byte, strip_rust_comments_and_literals};
use crate::notify_ratchet::{
    collect_rs_files_recursive, is_test_only_rel_path, strip_cfg_test_items,
};

/// Single rule specification loaded from `arch_debt_ratchet.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchDebtRule {
    pub max_occurrences: usize,
    pub max_files: usize,
    #[serde(default)]
    pub exact: bool,
    #[serde(default)]
    pub zero_required: bool,
    #[serde(default)]
    pub owning_issue: Option<u64>,
    #[serde(default)]
    pub strip_literals: Option<bool>,
    #[serde(default)]
    pub path_include: Option<String>,
    #[serde(default)]
    pub path_exclude_substring: Option<String>,
    #[serde(default)]
    pub regex: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

/// Top-level `crates/eve-testkit/ratchets/arch_debt_ratchet.json` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchDebtRatchet {
    pub version: u32,
    #[serde(default)]
    pub description: Option<String>,
    pub patterns: BTreeMap<String, ArchDebtRule>,
}

/// Per-pattern scan counts produced by [`scan_arch_debt_in_dir`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchDebtPatternReport {
    pub occurrences: usize,
    pub files: usize,
    pub per_file: BTreeMap<String, usize>,
}

/// Full scan report across all configured patterns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchDebtScanReport {
    pub scanned_files: usize,
    pub patterns: BTreeMap<String, ArchDebtPatternReport>,
}

impl ArchDebtRatchet {
    /// Returns the repository path to `crates/eve-testkit/ratchets/arch_debt_ratchet.json`.
    pub fn default_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ratchets")
            .join("arch_debt_ratchet.json")
    }

    /// Returns the repository path to `crates/eve-server/src`.
    pub fn default_server_src_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../eve-server/src")
    }

    /// Loads `arch_debt_ratchet.json` from its default repository path.
    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&Self::default_path())
    }

    /// Loads an [`ArchDebtRatchet`] from `path`.
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let parsed: Self = serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        for (name, rule) in &parsed.patterns {
            let kind = PatternKind::from_name(name).ok_or_else(|| {
                format!(
                    "Unknown arch_debt_ratchet pattern key '{name}' in {}",
                    path.display()
                )
            })?;
            match (rule.regex.as_deref(), kind.expected_regex()) {
                (Some(actual), Some(expected)) if actual == expected => {}
                (None, None) => {}
                (actual, expected) => {
                    return Err(format!(
                        "Rule '{name}' regex mismatch in {}: got {:?}, expected {:?}",
                        path.display(),
                        actual,
                        expected
                    ));
                }
            }
        }
        Ok(parsed)
    }

    /// Scans `crates/eve-server/src` and asserts every configured rule.
    pub fn verify_default(&self) -> Result<ArchDebtScanReport, String> {
        self.verify_dir(&Self::default_server_src_dir())
    }

    /// Scans `dir` and validates every configured rule against observed counts.
    pub fn verify_dir(&self, dir: &Path) -> Result<ArchDebtScanReport, String> {
        let report = scan_arch_debt_in_dir_for_rules(dir, &self.patterns)?;
        self.assert_ratchet(&report)?;
        Ok(report)
    }

    /// Enforces all configured rules (`zero_required`, regression caps, and `exact` bounds)
    /// against `report`.
    pub fn assert_ratchet(&self, report: &ArchDebtScanReport) -> Result<(), String> {
        for (name, rule) in &self.patterns {
            if rule.zero_required && (rule.max_occurrences > 0 || rule.max_files > 0) {
                return Err(format!(
                    "Arch debt rule '{name}' has zero_required=true with non-zero ceiling (max_occurrences={}, max_files={})",
                    rule.max_occurrences, rule.max_files
                ));
            }
            let obs = report
                .patterns
                .get(name)
                .ok_or_else(|| format!("Missing scan output for rule '{name}'"))?;
            if obs.occurrences > rule.max_occurrences {
                let sample: Vec<String> = obs
                    .per_file
                    .iter()
                    .take(5)
                    .map(|(f, c)| format!("{f}({c})"))
                    .collect();
                return Err(format!(
                    "Architecture debt '{name}' regressed: {} occurrences (max {}), files={} (max {}). Top files: [{}]",
                    obs.occurrences,
                    rule.max_occurrences,
                    obs.files,
                    rule.max_files,
                    sample.join(", ")
                ));
            }
            if obs.files > rule.max_files {
                return Err(format!(
                    "Architecture debt '{name}' file count regressed: {} files (max {}), occurrences={} (max {}).",
                    obs.files, rule.max_files, obs.occurrences, rule.max_occurrences
                ));
            }
            if rule.exact {
                if obs.occurrences < rule.max_occurrences {
                    return Err(format!(
                        "Architecture debt '{name}' dropped below exact ratchet ({} < {}); lower max_occurrences in arch_debt_ratchet.json in the same commit.",
                        obs.occurrences, rule.max_occurrences
                    ));
                }
                if obs.files < rule.max_files {
                    return Err(format!(
                        "Architecture debt '{name}' file count dropped below exact ratchet ({} < {}); lower max_files in arch_debt_ratchet.json in the same commit.",
                        obs.files, rule.max_files
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Scans `dir` for all known `#1197` patterns using a single pass over non-test `.rs` files.
pub fn scan_arch_debt_in_dir(dir: &Path) -> Result<ArchDebtScanReport, String> {
    let all: BTreeMap<String, ArchDebtRule> = PatternKind::ALL
        .iter()
        .map(|k| {
            (
                k.as_str().to_string(),
                ArchDebtRule {
                    max_occurrences: usize::MAX,
                    max_files: usize::MAX,
                    exact: false,
                    zero_required: false,
                    owning_issue: None,
                    strip_literals: Some(k.default_strip_literals()),
                    path_include: None,
                    path_exclude_substring: None,
                    regex: k.expected_regex().map(str::to_string),
                    description: None,
                },
            )
        })
        .collect();
    scan_arch_debt_in_dir_for_rules(dir, &all)
}

fn scan_arch_debt_in_dir_for_rules(
    dir: &Path,
    rules: &BTreeMap<String, ArchDebtRule>,
) -> Result<ArchDebtScanReport, String> {
    let mut active = Vec::with_capacity(rules.len());
    let mut needs_keep_literals = false;
    for (name, rule) in rules {
        let kind = PatternKind::from_name(name)
            .ok_or_else(|| format!("Unknown arch_debt_ratchet pattern key '{name}'"))?;
        if let Some(ref actual) = rule.regex {
            if kind.expected_regex() != Some(actual.as_str()) {
                return Err(format!(
                    "Rule '{name}' regex mismatch: got {:?}, expected {:?}",
                    Some(actual.as_str()),
                    kind.expected_regex()
                ));
            }
        }
        let keep_literals = !rule.strip_literals.unwrap_or(kind.default_strip_literals());
        needs_keep_literals |= keep_literals;
        active.push((name.clone(), kind, keep_literals, rule));
    }

    let mut files = Vec::new();
    collect_rs_files_recursive(dir, dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let non_test_files: Vec<(String, PathBuf)> = files
        .into_iter()
        .filter(|(rel_key, _)| !is_test_only_rel_path(rel_key))
        .collect();
    let total_files = non_test_files.len();
    let workers = if total_files >= 16 { 4usize } else { 1usize };
    let chunk_size = total_files.div_ceil(workers).max(1);

    let per_chunk: Vec<Result<Vec<(usize, String, usize)>, String>> = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for chunk in non_test_files.chunks(chunk_size) {
            let active_ref = &active;
            handles.push(scope.spawn(move || -> Result<Vec<(usize, String, usize)>, String> {
                let mut hits = Vec::new();
                for (rel_key, path) in chunk {
                    let raw = std::fs::read_to_string(path)
                        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
                    let fully_stripped = strip_rust_comments_and_literals(&raw);
                    let prod_stripped =
                        if fully_stripped.contains("cfg") && fully_stripped.contains("test") {
                            strip_cfg_test_items(&fully_stripped)
                        } else {
                            fully_stripped.clone()
                        };
                    let prod_keep_literals = if needs_keep_literals {
                        Some(strip_comments_and_cfg_tests_keep_literals(
                            &raw,
                            &fully_stripped,
                            &prod_stripped,
                        ))
                    } else {
                        None
                    };

                    for (rule_idx, (_, kind, keep_literals, rule)) in
                        active_ref.iter().enumerate()
                    {
                        if let Some(ref inc) = rule.path_include {
                            if rel_key != inc {
                                continue;
                            }
                        }
                        if let Some(ref exc) = rule.path_exclude_substring {
                            if rel_key.contains(exc.as_str()) {
                                continue;
                            }
                        }
                        let count = if *keep_literals {
                            match prod_keep_literals.as_deref() {
                                Some(src) => count_pattern_in_source(*kind, rel_key, src),
                                None => 0,
                            }
                        } else {
                            count_pattern_in_source(*kind, rel_key, &prod_stripped)
                        };
                        if count > 0 {
                            hits.push((rule_idx, rel_key.clone(), count));
                        }
                    }
                }
                Ok(hits)
            }));
        }
        handles
            .into_iter()
            .map(|h| h.join().map_err(|_| "arch_debt_ratchet worker thread panicked".to_string())?)
            .collect()
    });

    let mut report = ArchDebtScanReport {
        scanned_files: total_files,
        patterns: BTreeMap::new(),
    };
    for (name, _, _, _) in &active {
        report
            .patterns
            .insert(name.clone(), ArchDebtPatternReport::default());
    }
    for chunk_res in per_chunk {
        for (rule_idx, rel_key, count) in chunk_res? {
            let entry = report
                .patterns
                .get_mut(&active[rule_idx].0)
                .expect("pattern report initialized");
            entry.occurrences += count;
            entry.files += 1;
            entry.per_file.insert(rel_key, count);
        }
    }

    Ok(report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatternKind {
    MasterWallet,
    DirectMemberRolesGet,
    MemberBaseRolesCalls,
    HardcodedFallbackEntityIds,
    ServiceStateBlobsRefs,
    DirectLocationOrShipAssign,
    DirectItemsMapMut,
    ItemsInsert,
    ItemsRemove,
    ItemsGetMut,
    PositionalRpcArgsIndex,
    StringlyBridgeRpcErrorCode,
    SlashCommandPrefixParse,
    RouterBindingGraceHelpers,
    BallparkLocksOutsideSystemActor,
    HandwrittenItemChangeEmitters,
    HandwrittenAccountChangeEmitters,
    SystemTimeNowInGameplay,
    IsWriteLockedByCurrentThread,
}

impl PatternKind {
    const ALL: &'static [Self] = &[
        Self::MasterWallet,
        Self::DirectMemberRolesGet,
        Self::MemberBaseRolesCalls,
        Self::HardcodedFallbackEntityIds,
        Self::ServiceStateBlobsRefs,
        Self::DirectLocationOrShipAssign,
        Self::DirectItemsMapMut,
        Self::ItemsInsert,
        Self::ItemsRemove,
        Self::ItemsGetMut,
        Self::PositionalRpcArgsIndex,
        Self::StringlyBridgeRpcErrorCode,
        Self::SlashCommandPrefixParse,
        Self::RouterBindingGraceHelpers,
        Self::BallparkLocksOutsideSystemActor,
        Self::HandwrittenItemChangeEmitters,
        Self::HandwrittenAccountChangeEmitters,
        Self::SystemTimeNowInGameplay,
        Self::IsWriteLockedByCurrentThread,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::MasterWallet => "master_wallet",
            Self::DirectMemberRolesGet => "direct_member_roles_get",
            Self::MemberBaseRolesCalls => "member_base_roles_calls",
            Self::HardcodedFallbackEntityIds => "hardcoded_fallback_entity_ids",
            Self::ServiceStateBlobsRefs => "service_state_blobs_refs",
            Self::DirectLocationOrShipAssign => "direct_location_or_ship_assign",
            Self::DirectItemsMapMut => "direct_items_map_mut",
            Self::ItemsInsert => "items_insert",
            Self::ItemsRemove => "items_remove",
            Self::ItemsGetMut => "items_get_mut",
            Self::PositionalRpcArgsIndex => "positional_rpc_args_index",
            Self::StringlyBridgeRpcErrorCode => "stringly_bridge_rpc_error_code",
            Self::SlashCommandPrefixParse => "slash_command_prefix_parse",
            Self::RouterBindingGraceHelpers => "router_binding_grace_helpers",
            Self::BallparkLocksOutsideSystemActor => "ballpark_locks_outside_system_actor",
            Self::HandwrittenItemChangeEmitters => "handwritten_item_change_emitters",
            Self::HandwrittenAccountChangeEmitters => "handwritten_account_change_emitters",
            Self::SystemTimeNowInGameplay => "system_time_now_in_gameplay",
            Self::IsWriteLockedByCurrentThread => "is_write_locked_by_current_thread",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == name)
    }

    fn default_strip_literals(self) -> bool {
        !matches!(self, Self::SlashCommandPrefixParse)
    }

    fn expected_regex(self) -> Option<&'static str> {
        match self {
            Self::MasterWallet => Some(r"\bmaster_wallet\b"),
            Self::DirectMemberRolesGet => Some(r"\bmember_roles\s*\.\s*get\s*\("),
            Self::MemberBaseRolesCalls => Some(r"\.\s*member_base_roles(?:_opt)?\s*\("),
            Self::HardcodedFallbackEntityIds => Some(
                r"\.unwrap_or(?:_else\s*\(\s*(?:move\s+)?\|\s*(?:_\s*)?\|\s*(?:\{\s*)?|\s*\(\s*)(?:90_?000_?001|30_?000_?142|60_?003_?760)(?:_?[iu](?:8|16|32|64|128|size))?\b",
            ),
            Self::ServiceStateBlobsRefs => Some(r"\bservice_state_blobs\b"),
            Self::DirectLocationOrShipAssign => {
                Some(r"\.\s*(?:solar_system_id|station_id|structure_id|active_ship_id)\s*=(?!=)")
            }
            Self::DirectItemsMapMut => Some(r"\bitems\s*\.\s*(?:remove|get_mut|insert)\s*\("),
            Self::ItemsInsert => Some(r"\bitems\s*\.\s*insert\s*\("),
            Self::ItemsRemove => Some(r"\bitems\s*\.\s*remove\s*\("),
            Self::ItemsGetMut => Some(r"\bitems\s*\.\s*get_mut\s*\("),
            Self::PositionalRpcArgsIndex => {
                Some(r"\bargs\s*\.\s*(?:first\s*\(\s*\)|get\s*\(\s*\d+\s*\))")
            }
            Self::StringlyBridgeRpcErrorCode => Some(
                r"\bcode\s*:\s*(?: {3,}\.(?:into|to_string)\s*\(|String\s*::\s*from\s*\(\s* {3,}\s*\))",
            ),
            Self::SlashCommandPrefixParse
            | Self::RouterBindingGraceHelpers
            | Self::BallparkLocksOutsideSystemActor => None,
            Self::HandwrittenItemChangeEmitters => {
                Some(r"\b(?:emit|notify)_item_change[A-Za-z0-9_]*\s*\(")
            }
            Self::HandwrittenAccountChangeEmitters => {
                Some(r"\b(?:emit|notify)_[A-Za-z0-9_]*account_change[A-Za-z0-9_]*\s*\(")
            }
            Self::SystemTimeNowInGameplay => Some(r"\bSystemTime\s*::\s*now\s*\("),
            Self::IsWriteLockedByCurrentThread => Some(r"\bis_write_locked_by_current_thread\b"),
        }
    }
}

fn count_pattern_in_source(kind: PatternKind, rel_key: &str, text: &str) -> usize {
    match kind {
        PatternKind::MasterWallet => count_whole_word(text, "master_wallet"),
        PatternKind::DirectMemberRolesGet => {
            count_word_dot_method_lparen(text, "member_roles", &[b"get"])
        }
        PatternKind::MemberBaseRolesCalls => count_member_base_roles_calls(text),
        PatternKind::HardcodedFallbackEntityIds => count_hardcoded_fallback_entity_ids(text),
        PatternKind::ServiceStateBlobsRefs => count_whole_word(text, "service_state_blobs"),
        PatternKind::DirectLocationOrShipAssign => count_direct_location_or_ship_assign(text),
        PatternKind::DirectItemsMapMut => {
            count_word_dot_method_lparen(text, "items", &[b"remove", b"get_mut", b"insert"])
        }
        PatternKind::ItemsInsert => count_word_dot_method_lparen(text, "items", &[b"insert"]),
        PatternKind::ItemsRemove => count_word_dot_method_lparen(text, "items", &[b"remove"]),
        PatternKind::ItemsGetMut => count_word_dot_method_lparen(text, "items", &[b"get_mut"]),
        PatternKind::PositionalRpcArgsIndex => count_positional_rpc_args_index(text),
        PatternKind::StringlyBridgeRpcErrorCode => count_stringly_bridge_rpc_error_code(text),
        PatternKind::SlashCommandPrefixParse => count_slash_command_prefix_parse(text),
        PatternKind::RouterBindingGraceHelpers => {
            if rel_key == "router/binding.rs" {
                count_router_binding_grace_helpers(text)
            } else {
                0
            }
        }
        PatternKind::BallparkLocksOutsideSystemActor => {
            if rel_key.contains("system_actor") {
                0
            } else {
                count_word_dot_method_lparen(
                    text,
                    "ballpark",
                    &[b"read", b"write", b"try_read", b"try_write", b"lock"],
                )
            }
        }
        PatternKind::HandwrittenItemChangeEmitters => count_handwritten_change_emitters(text, true),
        PatternKind::HandwrittenAccountChangeEmitters => {
            count_handwritten_change_emitters(text, false)
        }
        PatternKind::SystemTimeNowInGameplay => count_system_time_now(text),
        PatternKind::IsWriteLockedByCurrentThread => {
            count_whole_word(text, "is_write_locked_by_current_thread")
        }
    }
}

#[inline]
fn has_word_boundary_before(b: &[u8], idx: usize) -> bool {
    idx == 0 || !is_ident_byte(b[idx - 1])
}

#[inline]
fn has_word_boundary_after(b: &[u8], end: usize) -> bool {
    end >= b.len() || !is_ident_byte(b[end])
}

#[inline]
fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn count_whole_word(text: &str, word: &str) -> usize {
    if !text.contains(word) {
        return 0;
    }
    let b = text.as_bytes();
    let wlen = word.len();
    text.match_indices(word)
        .filter(|&(i, _)| has_word_boundary_before(b, i) && has_word_boundary_after(b, i + wlen))
        .count()
}

fn count_word_dot_method_lparen(text: &str, obj: &str, methods: &[&[u8]]) -> usize {
    if !text.contains(obj) {
        return 0;
    }
    let b = text.as_bytes();
    let olen = obj.len();
    let mut count = 0usize;
    for (i, _) in text.match_indices(obj) {
        if !has_word_boundary_before(b, i) || !has_word_boundary_after(b, i + olen) {
            continue;
        }
        let j = skip_ws(b, i + olen);
        if j < b.len() && b[j] == b'.' {
            let k = skip_ws(b, j + 1);
            if let Some(&m) = methods.iter().find(|&&m| {
                k + m.len() <= b.len()
                    && &b[k..k + m.len()] == m
                    && has_word_boundary_after(b, k + m.len())
            }) {
                let after = skip_ws(b, k + m.len());
                if after < b.len() && b[after] == b'(' {
                    count += 1;
                }
            }
        }
    }
    count
}

fn count_member_base_roles_calls(text: &str) -> usize {
    if !text.contains("member_base_roles") {
        return 0;
    }
    const STEM: &str = "member_base_roles";
    let b = text.as_bytes();
    let mut count = 0usize;
    for (i, _) in text.match_indices(STEM) {
        let mut dot = i;
        while dot > 0 && b[dot - 1].is_ascii_whitespace() {
            dot -= 1;
        }
        if dot == 0 || b[dot - 1] != b'.' {
            continue;
        }
        let mut end = i + STEM.len();
        if end + 4 <= b.len() && &b[end..end + 4] == b"_opt" {
            end += 4;
        }
        let lp = skip_ws(b, end);
        if lp < b.len() && b[lp] == b'(' {
            count += 1;
        }
    }
    count
}

fn count_hardcoded_fallback_entity_ids(text: &str) -> usize {
    if !text.contains(".unwrap_or") {
        return 0;
    }
    const PREFIX_LEN: usize = ".unwrap_or".len();
    const FALLBACK_IDS: &[&[u8]] = &[
        b"90_000_001",
        b"90000001",
        b"30_000_142",
        b"30000142",
        b"60_003_760",
        b"60003760",
    ];
    const INT_SUFFIXES: &[&[u8]] = &[
        b"i8", b"i16", b"i32", b"i64", b"i128", b"isize",
        b"u8", b"u16", b"u32", b"u64", b"u128", b"usize",
    ];
    let b = text.as_bytes();
    let mut count = 0usize;
    for (i, _) in text.match_indices(".unwrap_or") {
        let after = i + PREFIX_LEN;
        let expr_start = if has_word_boundary_after(b, after) {
            let lp = skip_ws(b, after);
            if lp >= b.len() || b[lp] != b'(' {
                continue;
            }
            skip_ws(b, lp + 1)
        } else if after + 5 <= b.len()
            && &b[after..after + 5] == b"_else"
            && has_word_boundary_after(b, after + 5)
        {
            let lp = skip_ws(b, after + 5);
            if lp >= b.len() || b[lp] != b'(' {
                continue;
            }
            let mut p = skip_ws(b, lp + 1);
            if p + 4 <= b.len()
                && &b[p..p + 4] == b"move"
                && has_word_boundary_after(b, p + 4)
            {
                p = skip_ws(b, p + 4);
            }
            if p >= b.len() || b[p] != b'|' {
                continue;
            }
            p = skip_ws(b, p + 1);
            if p < b.len() && b[p] == b'_' {
                p = skip_ws(b, p + 1);
            }
            if p >= b.len() || b[p] != b'|' {
                continue;
            }
            p = skip_ws(b, p + 1);
            if p < b.len() && b[p] == b'{' {
                p = skip_ws(b, p + 1);
            }
            p
        } else {
            continue;
        };

        if let Some(id_end) = FALLBACK_IDS.iter().find_map(|&id| {
            (expr_start + id.len() <= b.len() && &b[expr_start..expr_start + id.len()] == id)
                .then_some(expr_start + id.len())
        }) {
            let matches_suffix = if has_word_boundary_after(b, id_end) {
                true
            } else {
                let suf_start = if id_end < b.len() && b[id_end] == b'_' {
                    id_end + 1
                } else {
                    id_end
                };
                INT_SUFFIXES.iter().any(|&suf| {
                    suf_start + suf.len() <= b.len()
                        && &b[suf_start..suf_start + suf.len()] == suf
                        && has_word_boundary_after(b, suf_start + suf.len())
                })
            };
            if matches_suffix {
                count += 1;
            }
        }
    }
    count
}

fn count_direct_location_or_ship_assign(text: &str) -> usize {
    const FIELDS: &[&str] = &[
        "solar_system_id",
        "station_id",
        "structure_id",
        "active_ship_id",
    ];
    let b = text.as_bytes();
    let mut count = 0usize;
    for &field in FIELDS {
        if !text.contains(field) {
            continue;
        }
        for (idx, _) in text.match_indices(field) {
            if !has_word_boundary_before(b, idx) || !has_word_boundary_after(b, idx + field.len()) {
                continue;
            }
            let mut dot_pos = idx;
            while dot_pos > 0 && b[dot_pos - 1].is_ascii_whitespace() {
                dot_pos -= 1;
            }
            if dot_pos == 0 || b[dot_pos - 1] != b'.' {
                continue;
            }
            let eq = skip_ws(b, idx + field.len());
            if eq < b.len() && b[eq] == b'=' && b.get(eq + 1) != Some(&b'=') {
                count += 1;
            }
        }
    }
    count
}

fn count_positional_rpc_args_index(text: &str) -> usize {
    if !text.contains("args") {
        return 0;
    }
    const OBJ_LEN: usize = "args".len();
    let b = text.as_bytes();
    let mut count = 0usize;
    for (i, _) in text.match_indices("args") {
        if !has_word_boundary_before(b, i) || !has_word_boundary_after(b, i + OBJ_LEN) {
            continue;
        }
        let j = skip_ws(b, i + OBJ_LEN);
        if j >= b.len() || b[j] != b'.' {
            continue;
        }
        let k = skip_ws(b, j + 1);
        if k + 5 <= b.len() && &b[k..k + 5] == b"first" && has_word_boundary_after(b, k + 5) {
            let lp = skip_ws(b, k + 5);
            if lp < b.len() && b[lp] == b'(' {
                let rp = skip_ws(b, lp + 1);
                if rp < b.len() && b[rp] == b')' {
                    count += 1;
                }
            }
        } else if k + 3 <= b.len() && &b[k..k + 3] == b"get" && has_word_boundary_after(b, k + 3) {
            let lp = skip_ws(b, k + 3);
            if lp < b.len() && b[lp] == b'(' {
                let mut d = skip_ws(b, lp + 1);
                let d_start = d;
                while d < b.len() && b[d].is_ascii_digit() {
                    d += 1;
                }
                if d > d_start {
                    let rp = skip_ws(b, d);
                    if rp < b.len() && b[rp] == b')' {
                        count += 1;
                    }
                }
            }
        }
    }
    count
}

fn count_stringly_bridge_rpc_error_code(text: &str) -> usize {
    if !text.contains("code")
        || (!text.contains(".into") && !text.contains(".to_string") && !text.contains("String"))
    {
        return 0;
    }
    const WORD_LEN: usize = "code".len();
    let b = text.as_bytes();
    let mut count = 0usize;
    for (i, _) in text.match_indices("code") {
        if !has_word_boundary_before(b, i) || !has_word_boundary_after(b, i + WORD_LEN) {
            continue;
        }
        let j = skip_ws(b, i + WORD_LEN);
        if j >= b.len() || b[j] != b':' {
            continue;
        }
        let k = skip_ws(b, j + 1);
        let trailing_spaces = b[j + 1..k].iter().rev().take_while(|&&c| c == b' ').count();
        if trailing_spaces >= 3 {
            let method_end = if k + 5 <= b.len()
                && &b[k..k + 5] == b".into"
                && has_word_boundary_after(b, k + 5)
            {
                Some(k + 5)
            } else if k + 10 <= b.len()
                && &b[k..k + 10] == b".to_string"
                && has_word_boundary_after(b, k + 10)
            {
                Some(k + 10)
            } else {
                None
            };
            if let Some(end) = method_end {
                let lp = skip_ws(b, end);
                if lp < b.len() && b[lp] == b'(' {
                    count += 1;
                    continue;
                }
            }
        }
        if k + 6 <= b.len()
            && &b[k..k + 6] == b"String"
            && has_word_boundary_after(b, k + 6)
        {
            let colons = skip_ws(b, k + 6);
            if colons + 1 < b.len() && b[colons] == b':' && b[colons + 1] == b':' {
                let from_pos = skip_ws(b, colons + 2);
                if from_pos + 4 <= b.len()
                    && &b[from_pos..from_pos + 4] == b"from"
                    && has_word_boundary_after(b, from_pos + 4)
                {
                    let lp = skip_ws(b, from_pos + 4);
                    if lp < b.len() && b[lp] == b'(' {
                        let inner_end = skip_ws(b, lp + 1);
                        if inner_end < b.len()
                            && b[inner_end] == b')'
                            && b[lp + 1..inner_end].windows(3).any(|w| w == b"   ")
                        {
                            count += 1;
                        }
                    }
                }
            }
        }
    }
    count
}

fn count_slash_command_prefix_parse(text: &str) -> usize {
    let b = text.as_bytes();
    let mut count = 0usize;
    for method in ["strip_prefix", "starts_with"] {
        if !text.contains(method) {
            continue;
        }
        for (i, _) in text.match_indices(method) {
            if !has_word_boundary_after(b, i + method.len()) {
                continue;
            }
            let lp = skip_ws(b, i + method.len());
            if lp < b.len() && b[lp] == b'(' {
                let arg = skip_ws(b, lp + 1);
                if arg + 1 < b.len() && b[arg] == b'"' && b[arg + 1] == b'/' {
                    count += 1;
                }
            }
        }
    }
    count
}

fn count_router_binding_grace_helpers(text: &str) -> usize {
    if !text.contains("grace") {
        return 0;
    }
    let b = text.as_bytes();
    let mut count = 0usize;
    for (i, _) in text.match_indices("fn") {
        if !has_word_boundary_before(b, i) || !b.get(i + 2).is_some_and(u8::is_ascii_whitespace) {
            continue;
        }
        let name_start = skip_ws(b, i + 2);
        let mut name_end = name_start;
        while name_end < b.len() && is_ident_byte(b[name_end]) {
            name_end += 1;
        }
        if name_end > name_start {
            if let Ok(name) = std::str::from_utf8(&b[name_start..name_end]) {
                let is_per_method = (name.starts_with("allocate_transition_grace_")
                    && name.ends_with("_binding")
                    && name != "allocate_transition_grace_binding_for_departed_kinds")
                    || name.starts_with("grace_bound_object_for_inflight_")
                    || (name.starts_with("has_transition_grace_") && name.ends_with("_binding"));
                if is_per_method {
                    count += 1;
                }
            }
        }
    }
    count
}

fn count_handwritten_change_emitters(text: &str, item_change: bool) -> usize {
    let needle = if item_change { "item_change" } else { "account_change" };
    if !text.contains(needle) {
        return 0;
    }
    let b = text.as_bytes();
    let mut count = 0usize;
    for prefix in ["emit_", "notify_"] {
        if !text.contains(prefix) {
            continue;
        }
        for (i, _) in text.match_indices(prefix) {
            if !has_word_boundary_before(b, i) {
                continue;
            }
            let mut ident_end = i + prefix.len();
            while ident_end < b.len() && is_ident_byte(b[ident_end]) {
                ident_end += 1;
            }
            if let Ok(tail) = std::str::from_utf8(&b[i + prefix.len()..ident_end]) {
                let matches_kind = if item_change {
                    tail.starts_with("item_change")
                } else {
                    tail.contains("account_change")
                };
                if matches_kind {
                    let lp = skip_ws(b, ident_end);
                    if lp < b.len() && b[lp] == b'(' {
                        count += 1;
                    }
                }
            }
        }
    }
    count
}

fn count_system_time_now(text: &str) -> usize {
    if !text.contains("SystemTime") {
        return 0;
    }
    const TYPE_LEN: usize = "SystemTime".len();
    let b = text.as_bytes();
    let mut count = 0usize;
    for (i, _) in text.match_indices("SystemTime") {
        if !has_word_boundary_before(b, i) || !has_word_boundary_after(b, i + TYPE_LEN) {
            continue;
        }
        let colons = skip_ws(b, i + TYPE_LEN);
        if colons + 1 < b.len() && b[colons] == b':' && b[colons + 1] == b':' {
            let now_pos = skip_ws(b, colons + 2);
            if now_pos + 3 <= b.len()
                && &b[now_pos..now_pos + 3] == b"now"
                && has_word_boundary_after(b, now_pos + 3)
            {
                let lp = skip_ws(b, now_pos + 3);
                if lp < b.len() && b[lp] == b'(' {
                    count += 1;
                }
            }
        }
    }
    count
}

/// Strips Rust line/block comments and `#[cfg(test)]` items while keeping string/char literals intact.
fn strip_comments_and_cfg_tests_keep_literals(
    raw: &str,
    fully_stripped: &str,
    prod_stripped: &str,
) -> String {
    let raw_b = raw.as_bytes();
    let n = raw_b.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0usize;
    while i < n {
        if raw_b[i] == b'/' && i + 1 < n && raw_b[i + 1] == b'/' {
            while i < n && raw_b[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
        } else if raw_b[i] == b'/' && i + 1 < n && raw_b[i + 1] == b'*' {
            let mut depth = 1usize;
            out.extend_from_slice(b"  ");
            i += 2;
            while i < n && depth > 0 {
                if i + 1 < n && raw_b[i] == b'/' && raw_b[i + 1] == b'*' {
                    depth += 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                } else if i + 1 < n && raw_b[i] == b'*' && raw_b[i + 1] == b'/' {
                    depth -= 1;
                    out.extend_from_slice(b"  ");
                    i += 2;
                } else {
                    out.push(if raw_b[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            }
        } else if raw_b[i] == b'r'
            && (i == 0
                || !is_ident_byte(raw_b[i - 1])
                || (raw_b[i - 1] == b'b' && (i == 1 || !is_ident_byte(raw_b[i - 2]))))
        {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while j < n && raw_b[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && raw_b[j] == b'"' {
                out.extend_from_slice(&raw_b[i..=j]);
                i = j + 1;
                while i < n {
                    let ch = raw_b[i];
                    out.push(ch);
                    if ch == b'"'
                        && i + hashes < n
                        && raw_b[i + 1..=i + hashes].iter().all(|&c| c == b'#')
                    {
                        out.extend_from_slice(&raw_b[i + 1..=i + hashes]);
                        i += 1 + hashes;
                        break;
                    }
                    i += 1;
                }
            } else {
                out.push(raw_b[i]);
                i += 1;
            }
        } else if raw_b[i] == b'\'' {
            if i + 1 < n && raw_b[i + 1] == b'\\' {
                let mut k = i + 3;
                let limit = (i + 14).min(n);
                let mut closed = None;
                while k < limit {
                    if raw_b[k] == b'\n' || raw_b[k] == b'\r' {
                        break;
                    }
                    if raw_b[k] == b'\'' {
                        closed = Some(k);
                        break;
                    }
                    k += 1;
                }
                if let Some(end) = closed {
                    out.extend_from_slice(&raw_b[i..=end]);
                    i = end + 1;
                    continue;
                }
            } else if let Some(ch) = raw[i + 1..].chars().next() {
                let close_idx = i + 1 + ch.len_utf8();
                if ch != '\\'
                    && ch != '\''
                    && ch != '\n'
                    && ch != '\r'
                    && close_idx < n
                    && raw_b[close_idx] == b'\''
                {
                    out.extend_from_slice(&raw_b[i..=close_idx]);
                    i = close_idx + 1;
                    continue;
                }
            }
            out.push(raw_b[i]);
            i += 1;
        } else if raw_b[i] == b'"' {
            out.push(raw_b[i]);
            i += 1;
            while i < n {
                let ch = raw_b[i];
                out.push(ch);
                i += 1;
                if ch == b'\\' && i < n {
                    out.push(raw_b[i]);
                    i += 1;
                    continue;
                }
                if ch == b'"' {
                    break;
                }
            }
        } else {
            out.push(raw_b[i]);
            i += 1;
        }
    }

    let fully_b = fully_stripped.as_bytes();
    let prod_b = prod_stripped.as_bytes();
    if out.len() == n && fully_b.len() == n && prod_b.len() == n && fully_b != prod_b {
        blank_cfg_test_spans(fully_b, prod_b, &mut out);
    }

    String::from_utf8(out).expect("comment/cfg(test) masking preserves UTF-8")
}

fn blank_cfg_test_spans(fully_b: &[u8], prod_b: &[u8], out: &mut [u8]) {
    let n = out.len();
    let mut i = 0usize;
    while i + 1 < n {
        if fully_b[i] == b'#' && fully_b[i + 1] == b'[' && prod_b[i] == b' ' {
            let start = i;
            let mut j = i + 2;
            let mut br = 1usize;
            while j < n && br > 0 {
                match fully_b[j] {
                    b'[' => br += 1,
                    b']' => br -= 1,
                    _ => {}
                }
                j += 1;
            }
            let mut depth = 0usize;
            let mut end = j;
            while end < n {
                match fully_b[end] {
                    b'{' => depth += 1,
                    b'}' => {
                        if depth <= 1 {
                            end += 1;
                            break;
                        }
                        depth -= 1;
                    }
                    b';' if depth == 0 => {
                        end += 1;
                        break;
                    }
                    _ => {}
                }
                end += 1;
            }
            for b in &mut out[start..end.min(n)] {
                if *b != b'\n' {
                    *b = b' ';
                }
            }
            i = end.max(start + 1);
            continue;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "evomu_arch_debt_test_{label}_{}_{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }

    #[test]
    fn arch_debt_ratchet_holds_and_forbids_unratcheted_growth() {
        // 1. Live repository baseline must pass with every expected #1197 / scope-revision pattern.
        let ratchet = ArchDebtRatchet::load_default().expect("load arch_debt_ratchet.json");
        let report = ratchet
            .verify_default()
            .expect("live crates/eve-server/src must match arch_debt_ratchet.json");
        assert!(report.scanned_files >= 100, "expected >= 100 scanned files");
        for required_key in [
            "master_wallet",
            "direct_member_roles_get",
            "member_base_roles_calls",
            "hardcoded_fallback_entity_ids",
            "service_state_blobs_refs",
            "direct_location_or_ship_assign",
            "direct_items_map_mut",
            "items_insert",
            "items_remove",
            "items_get_mut",
            "positional_rpc_args_index",
            "stringly_bridge_rpc_error_code",
            "slash_command_prefix_parse",
            "router_binding_grace_helpers",
            "ballpark_locks_outside_system_actor",
            "handwritten_item_change_emitters",
            "handwritten_account_change_emitters",
            "system_time_now_in_gameplay",
            "is_write_locked_by_current_thread",
        ] {
            assert!(
                ratchet.patterns.contains_key(required_key),
                "arch_debt_ratchet.json missing required pattern '{required_key}'"
            );
        }
        assert_eq!(report.patterns["master_wallet"].occurrences, 0);
        assert_eq!(report.patterns["direct_member_roles_get"].occurrences, 0);
        assert_eq!(report.patterns["hardcoded_fallback_entity_ids"].occurrences, 0);
        assert_eq!(report.patterns["hardcoded_fallback_entity_ids"].files, 0);
        for (name, rule) in &ratchet.patterns {
            if rule.zero_required {
                assert!(
                    rule.exact && rule.max_occurrences == 0 && rule.max_files == 0,
                    "zero_required rule '{name}' must remain pinned at 0 with exact=true: {rule:?}"
                );
            } else {
                assert!(
                    !rule.exact,
                    "non-zero rule '{name}' must use upper-bound ceiling (exact=false): {rule:?}"
                );
            }
        }

        // 2. Synthetic tempdir workspace verifies comment/cfg(test)/literal filtering, regression
        //    blocking, upper-bound drops (exact=false), exact-drop rejection (exact=true, both
        //    occurrence and file count), and zero_required guard.
        let tmp = make_temp_dir("synth");
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("router")).expect("create router dir");
        std::fs::create_dir_all(src.join("services")).expect("create services dir");

        std::fs::write(
            src.join("services/sample.rs"),
            r#"
pub fn ok(args: &[i64], cmd: &str, mut x: u64) -> Option<i64> {
    // master_wallet and corp.member_roles.get(&1) in comment must be ignored
    let _msg = "args.get(0) and ballpark.write() and .unwrap_or(30_000_142) in literal";
    let _slash = cmd.strip_prefix("/help");
    x = args.first().copied().unwrap_or(1) as u64;
    let _ = Some(1).unwrap_or(30_000_142);
    let _g = x + 1;
    None
}
#[cfg(test)]
mod tests {
    #[test]
    fn ignored_in_cfg_test() {
        let _ = "/ignored".strip_prefix("/");
        let _ = Some(1).unwrap_or(60_003_760);
    }
}
"#,
        )
        .expect("write sample.rs");
        std::fs::write(
            src.join("services/sample_tests.rs"),
            r#"pub fn t() { let _ = Some(1).unwrap_or(30_000_142); }"#,
        )
        .expect("write sample_tests.rs");
        std::fs::write(
            src.join("router/binding.rs"),
            r#"
pub fn allocate_transition_grace_binding_for_departed_kinds() {}
pub fn grace_bound_object_for_inflight_invbroker() {}
"#,
        )
        .expect("write binding.rs");
        std::fs::write(
            src.join("services/system_actor_loop.rs"),
            r#"pub fn ok(b: &Obj) { let _ = b.ballpark.write(); }"#,
        )
        .expect("write system_actor_loop.rs");

        let mk_rule = |occ, files, exact, zero_req| ArchDebtRule {
            max_occurrences: occ,
            max_files: files,
            exact,
            zero_required: zero_req,
            owning_issue: Some(1197),
            strip_literals: None,
            path_include: None,
            path_exclude_substring: None,
            regex: None,
            description: None,
        };
        let mut rules = BTreeMap::new();
        rules.insert("master_wallet".into(), mk_rule(0, 0, true, true));
        rules.insert("direct_member_roles_get".into(), mk_rule(0, 0, true, true));
        rules.insert("hardcoded_fallback_entity_ids".into(), mk_rule(1, 1, false, false));
        rules.insert("positional_rpc_args_index".into(), mk_rule(1, 1, false, false));
        rules.insert("slash_command_prefix_parse".into(), mk_rule(1, 1, false, false));
        let mut grace_rule = mk_rule(1, 1, false, false);
        grace_rule.path_include = Some("router/binding.rs".into());
        rules.insert("router_binding_grace_helpers".into(), grace_rule);
        let mut bp_rule = mk_rule(0, 0, false, false);
        bp_rule.path_exclude_substring = Some("system_actor".into());
        rules.insert("ballpark_locks_outside_system_actor".into(), bp_rule);

        let synth_ratchet = ArchDebtRatchet {
            version: 1,
            description: None,
            patterns: rules.clone(),
        };
        synth_ratchet
            .verify_dir(&src)
            .expect("synthetic baseline must pass");

        // Regression check: adding a master_wallet or ballpark lock outside system_actor must fail.
        std::fs::write(
            src.join("services/extra.rs"),
            "pub fn bad(b: &Obj) { let _ = b.ballpark.write(); }\n",
        )
        .expect("write extra.rs");
        let err = synth_ratchet.verify_dir(&src).unwrap_err();
        assert!(err.contains("ballpark_locks_outside_system_actor"), "{err}");
        std::fs::remove_file(src.join("services/extra.rs")).expect("remove extra.rs");

        // Upper-bound drop check (exact=false): lowering occurrences and files passes without JSON edits.
        std::fs::write(src.join("router/binding.rs"), "pub fn clean() {}\n")
            .expect("clean binding.rs");
        synth_ratchet
            .verify_dir(&src)
            .expect("exact=false upper-bound rule must allow occurrence/file drops");

        // Exact drop check (exact=true): lowering occurrences without tightening JSON fails.
        let mut exact_ratchet = synth_ratchet.clone();
        exact_ratchet
            .patterns
            .get_mut("router_binding_grace_helpers")
            .expect("router_binding_grace_helpers rule")
            .exact = true;
        let drop_err = exact_ratchet.verify_dir(&src).unwrap_err();
        assert!(
            drop_err.contains("dropped below exact ratchet")
                && drop_err.contains("router_binding_grace_helpers"),
            "{drop_err}"
        );

        // File-count-only drop check (occurrences unchanged, files 2 -> 1).
        let mut file_drop_rules = BTreeMap::new();
        file_drop_rules.insert("master_wallet".into(), mk_rule(2, 2, true, false));
        let file_drop_ratchet = ArchDebtRatchet {
            version: 1,
            description: None,
            patterns: file_drop_rules,
        };
        std::fs::write(
            src.join("services/sample.rs"),
            "pub fn a() { let _ = (master_wallet, master_wallet); }\n",
        )
        .expect("rewrite sample.rs");
        let file_err = file_drop_ratchet.verify_dir(&src).unwrap_err();
        assert!(file_err.contains("file count dropped below exact ratchet"), "{file_err}");

        // zero_required with non-zero ceiling must be rejected.
        let mut bad_zero_rules = BTreeMap::new();
        bad_zero_rules.insert("master_wallet".into(), mk_rule(2, 1, true, true));
        let bad_zero = ArchDebtRatchet {
            version: 1,
            description: None,
            patterns: bad_zero_rules,
        };
        let zero_err = bad_zero.verify_dir(&src).unwrap_err();
        assert!(zero_err.contains("zero_required=true"), "{zero_err}");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn arch_debt_ratchet_scanners_catch_char_literals_suffixes_closures_strings_and_member_base_roles() {
        let tmp = make_temp_dir("bf02_variants");
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("services")).expect("create services dir");

        // 1. Char literals ('\"', '\'', b'\"', b'"', '"', '🦀') + lifetime must never corrupt string state;
        //    suffixes, closures, .to_string()/String::from, .member_base_roles[_opt], and non-slash strip_literals=false must all count accurately.
        std::fs::write(
            src.join("services/variants.rs"),
            r#"
pub struct Err { pub code: String }
pub fn f<'a>(cmd: &'a str, corp: &Corp, err_var: &str, v: Option<i64>) {
    let _c1 = '\"';
    let _c2 = '\'';
    let _c3 = b'\"';
    let _c4 = b'"';
    let _c5 = '"';
    let _c6 = '🦀';
    let _slash = cmd.strip_prefix("/help");
    let _fb1 = v.unwrap_or(30_000_142_i64);
    let _fb2 = v.unwrap_or(60_003_760i32);
    let _fb3 = v.unwrap_or_else(|| 90_000_001);
    let _fb4 = v.unwrap_or_else(|_| 60_003_760_i64);
    let _fb5 = v.unwrap_or_else(move || { 30_000_142_u64 });
    let _fb_ignored = v.unwrap_or(30_000_142_custom());
    let _e1 = Err { code: "BadA".into() };
    let _e2 = Err { code: "BadB".to_string() };
    let _e3 = Err { code: String::from("BadC") };
    let _e_var1 = Err { code: err_var.to_string() };
    let _e_var2 = Err { code: String::from(err_var) };
    let _r1 = corp.member_base_roles(1);
    let _r2 = corp.member_base_roles_opt(2);
    let _comment = "// master_wallet";
}
pub fn member_base_roles() {}
"#,
        )
        .expect("write variants.rs");

        // File with NO strip_prefix/starts_with verifying strip_literals=false works on any rule.
        std::fs::write(
            src.join("services/lit_only.rs"),
            "pub fn g() -> &'static str { \"master_wallet\" }\n",
        )
        .expect("write lit_only.rs");

        let report = scan_arch_debt_in_dir(&src).expect("scan variants");
        assert_eq!(report.patterns["slash_command_prefix_parse"].occurrences, 1);
        assert_eq!(report.patterns["hardcoded_fallback_entity_ids"].occurrences, 5);
        assert_eq!(report.patterns["stringly_bridge_rpc_error_code"].occurrences, 3);
        assert_eq!(report.patterns["member_base_roles_calls"].occurrences, 2);
        assert_eq!(report.patterns["master_wallet"].occurrences, 0);

        let lit_rules = BTreeMap::from([(
            "master_wallet".to_string(),
            ArchDebtRule {
                max_occurrences: 2,
                max_files: 2,
                exact: false,
                zero_required: false,
                owning_issue: Some(1197),
                strip_literals: Some(false),
                path_include: None,
                path_exclude_substring: None,
                regex: None,
                description: None,
            },
        )]);
        let lit_report = scan_arch_debt_in_dir_for_rules(&src, &lit_rules)
            .expect("strip_literals=false must scan unstripped literals on any rule");
        assert_eq!(lit_report.patterns["master_wallet"].occurrences, 2);
        assert_eq!(lit_report.patterns["master_wallet"].files, 2);

        // 2. Unknown JSON fields and mismatched regex strings must fail load_from_path.
        let bad_field_json = tmp.join("bad_field.json");
        std::fs::write(
            &bad_field_json,
            r#"{"version":1,"patterns":{"master_wallet":{"max_occurrences":0,"max_files":0,"exact":true,"zero_required":true,"regex":"\\bmaster_wallet\\b","literal_regex":"dead"}}}"#,
        )
        .expect("write bad_field.json");
        assert!(ArchDebtRatchet::load_from_path(&bad_field_json).is_err());

        let bad_regex_json = tmp.join("bad_regex.json");
        std::fs::write(
            &bad_regex_json,
            r#"{"version":1,"patterns":{"master_wallet":{"max_occurrences":0,"max_files":0,"exact":true,"zero_required":true,"regex":"\\bwrong\\b"}}}"#,
        )
        .expect("write bad_regex.json");
        let rx_err = ArchDebtRatchet::load_from_path(&bad_regex_json).unwrap_err();
        assert!(rx_err.contains("regex mismatch"), "{rx_err}");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
