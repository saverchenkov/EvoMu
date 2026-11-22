//! Process-global mutable state, static-data cache, and Group G allowlist ratchet (`#1583`).
//!
//! Scans all production crates under `crates/*/src/` (excluding `crates/eve-testkit/`,
//! comments, string/char literals, `#[cfg(test)]` items, and test-only / `#[cfg(test)] mod`
//! files) for:
//! - `static mut`
//! - All non-`mut` `static` declarations except provably immutable scalar, reference, slice, array,
//!   or tuple types and the explicitly audited read-only tables in `IMMUTABLE_STATIC_TABLES`
//! - `thread_local!` / `std::thread_local!` blocks
//! - `tokio::task_local!` / `task_local!` blocks
//!
//! Also provides generic [`TwoContextPair`] and repository-level [`TwoRepositoryPair`] isolation
//! test helpers for side-by-side multi-context verification inside the same OS process.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use eve_db::WorldRepository;
use eve_sde::SdeRegistry;
use serde::{Deserialize, Serialize};

use crate::client_harness::{is_ident_byte, strip_rust_comments_and_literals};
use crate::notify_ratchet::{
    collect_rs_files_recursive, find_cfg_test_attr_end, is_test_only_rel_path,
};

/// Syntax form of a detected process-global declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessGlobalKind {
    StaticMut,
    StaticSync,
    ThreadLocal,
    TaskLocal,
}

impl fmt::Display for ProcessGlobalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::StaticMut => "static_mut",
            Self::StaticSync => "static_sync",
            Self::ThreadLocal => "thread_local",
            Self::TaskLocal => "task_local",
        };
        f.write_str(s)
    }
}

/// Issue `#1583` classification bucket for a process-global declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ProcessGlobalGroup {
    #[serde(rename = "A_game_state")]
    AGameState,
    #[serde(rename = "B_registries")]
    BRegistries,
    #[serde(rename = "C_counters")]
    CCounters,
    #[serde(rename = "D_ambient_scopes")]
    DAmbientScopes,
    #[serde(rename = "E_test_seams")]
    ETestSeams,
    #[serde(rename = "F_mutable_static_data_caches")]
    FMutableStaticDataCaches,
    #[serde(rename = "F_set_once_caches")]
    FSetOnceCaches,
    #[serde(rename = "set_once_infra")]
    SetOnceInfra,
    #[serde(rename = "G_process_wide_allowlist")]
    GProcessWideAllowlist,
}

impl ProcessGlobalGroup {
    /// All groups in canonical reporting order.
    pub const ALL: [Self; 9] = [
        Self::AGameState,
        Self::BRegistries,
        Self::CCounters,
        Self::DAmbientScopes,
        Self::ETestSeams,
        Self::FMutableStaticDataCaches,
        Self::FSetOnceCaches,
        Self::SetOnceInfra,
        Self::GProcessWideAllowlist,
    ];

    /// Stable JSON / diagnostic key name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AGameState => "A_game_state",
            Self::BRegistries => "B_registries",
            Self::CCounters => "C_counters",
            Self::DAmbientScopes => "D_ambient_scopes",
            Self::ETestSeams => "E_test_seams",
            Self::FMutableStaticDataCaches => "F_mutable_static_data_caches",
            Self::FSetOnceCaches => "F_set_once_caches",
            Self::SetOnceInfra => "set_once_infra",
            Self::GProcessWideAllowlist => "G_process_wide_allowlist",
        }
    }
}

impl fmt::Display for ProcessGlobalGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-group falling ceiling and strict zero/rationale policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupPolicy {
    pub zero_required: bool,
    pub max_count: usize,
    #[serde(default)]
    pub rationale_required: bool,
    pub description: String,
}

/// Baselined process-global declaration entry inside [`ProcessGlobalRatchet`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessGlobalBaselineEntry {
    pub crate_name: String,
    pub rel_file: String,
    pub symbol: String,
    pub kind: ProcessGlobalKind,
    pub group: ProcessGlobalGroup,
    pub max_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// Canonical inventory key `(crate_name, rel_file, symbol)`.
pub type DeclarationKey = (String, String, String);

/// Parsed JSON ratchet from `crates/eve-testkit/ratchets/process_global_ratchet.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessGlobalRatchet {
    pub version: u32,
    pub owning_issue: u32,
    pub description: String,
    pub excluded_crates: Vec<String>,
    pub groups: BTreeMap<ProcessGlobalGroup, GroupPolicy>,
    pub declarations: Vec<ProcessGlobalBaselineEntry>,
}

/// Single declaration found during a production crate scan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcessGlobalFinding {
    pub crate_name: String,
    pub rel_file: String,
    pub line: usize,
    pub kind: ProcessGlobalKind,
    pub symbol: String,
}

/// Aggregated scan report across all production crates in a workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessGlobalScanReport {
    pub scanned_crates: Vec<String>,
    pub scanned_files: usize,
    pub findings: Vec<ProcessGlobalFinding>,
    pub per_declaration_counts: BTreeMap<(String, String, String, ProcessGlobalKind), usize>,
    pub per_crate_counts: BTreeMap<String, usize>,
}

/// Verified report enriched with per-group live counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessGlobalVerifiedSummary {
    pub report: ProcessGlobalScanReport,
    pub per_group_counts: BTreeMap<ProcessGlobalGroup, usize>,
}

impl ProcessGlobalRatchet {
    /// Returns the default path to `crates/eve-testkit/ratchets/process_global_ratchet.json`.
    pub fn default_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ratchets")
            .join("process_global_ratchet.json")
    }

    /// Returns the workspace root containing `crates/`.
    pub fn default_workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("CARGO_MANIFEST_DIR must be crates/eve-testkit")
            .to_path_buf()
    }

    /// Loads `process_global_ratchet.json` from its default repository location.
    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&Self::default_path())
    }

    /// Loads a [`ProcessGlobalRatchet`] from `path`.
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let ratchet: Self = serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        ratchet.validate()?;
        Ok(ratchet)
    }

    /// Scans the live workspace and asserts all production crates comply with this ratchet.
    pub fn verify_default(&self) -> Result<ProcessGlobalVerifiedSummary, String> {
        self.verify_workspace(&Self::default_workspace_root())
    }

    /// Scans `workspace_root/crates/*/src` and verifies compliance with this ratchet.
    pub fn verify_workspace(
        &self,
        workspace_root: &Path,
    ) -> Result<ProcessGlobalVerifiedSummary, String> {
        self.validate()?;
        let excluded: BTreeSet<String> = self.excluded_crates.iter().cloned().collect();
        let report = scan_workspace_process_globals(workspace_root, &excluded)?;
        self.assert_ratchet(&report)
    }

    /// Validates internal consistency of the ratchet configuration and allowlist rationales.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 {
            return Err(format!(
                "process_global_ratchet.json unsupported version {} (expected 1)",
                self.version
            ));
        }
        if !self.excluded_crates.iter().any(|c| c == "eve-testkit") {
            return Err(
                "process_global_ratchet.json must include `eve-testkit` in `excluded_crates`"
                    .to_string(),
            );
        }
        for group in ProcessGlobalGroup::ALL {
            if !self.groups.contains_key(&group) {
                return Err(format!(
                    "process_global_ratchet.json missing group policy `{group}`"
                ));
            }
        }

        let mut prev_key: Option<DeclarationKey> = None;
        let mut group_entry_sums: BTreeMap<ProcessGlobalGroup, usize> = BTreeMap::new();

        for entry in &self.declarations {
            if entry.max_count == 0 {
                return Err(format!(
                    "process_global_ratchet entry crates/{}/src/{} `{}` has invalid max_count=0 (remove solved entries instead)",
                    entry.crate_name, entry.rel_file, entry.symbol
                ));
            }
            let key = (
                entry.crate_name.clone(),
                entry.rel_file.clone(),
                entry.symbol.clone(),
            );
            if let Some(ref prev) = prev_key {
                if &key == prev {
                    return Err(format!(
                        "duplicate entry in process_global_ratchet.json for crates/{}/src/{} `{}`",
                        entry.crate_name, entry.rel_file, entry.symbol
                    ));
                }
                if &key < prev {
                    return Err(format!(
                        "process_global_ratchet.json declarations are not canonically sorted around crates/{}/src/{} `{}`",
                        entry.crate_name, entry.rel_file, entry.symbol
                    ));
                }
            }
            prev_key = Some(key);

            let policy = self
                .groups
                .get(&entry.group)
                .ok_or_else(|| format!("missing policy for group `{}`", entry.group))?;
            if policy.zero_required {
                return Err(format!(
                    "group `{}` has zero_required=true but still lists crates/{}/src/{} `{}` (max_count={})",
                    entry.group, entry.crate_name, entry.rel_file, entry.symbol, entry.max_count
                ));
            }
            validate_entry_rationale(entry, policy.rationale_required)?;
            *group_entry_sums.entry(entry.group).or_default() += entry.max_count;
        }

        for group in ProcessGlobalGroup::ALL {
            let policy = &self.groups[&group];
            let sum = group_entry_sums.get(&group).copied().unwrap_or(0);
            if policy.zero_required && policy.max_count != 0 {
                return Err(format!(
                    "group `{group}` has zero_required=true so max_count must be 0, found {}",
                    policy.max_count
                ));
            }
            if sum > policy.max_count {
                return Err(format!(
                    "group `{group}` baseline entries sum to {sum}, exceeding group max_count={}",
                    policy.max_count
                ));
            }
        }
        Ok(())
    }

    /// Asserts a [`ProcessGlobalScanReport`] adds no unlisted globals, does not raise any count
    /// above its ceiling, preserves declaration kinds, and respects every group's `max_count` and
    /// `zero_required` flag.
    pub fn assert_ratchet(
        &self,
        report: &ProcessGlobalScanReport,
    ) -> Result<ProcessGlobalVerifiedSummary, String> {
        self.validate()?;

        let mut index: BTreeMap<DeclarationKey, &ProcessGlobalBaselineEntry> = BTreeMap::new();
        for entry in &self.declarations {
            index.insert(
                (
                    entry.crate_name.clone(),
                    entry.rel_file.clone(),
                    entry.symbol.clone(),
                ),
                entry,
            );
        }

        for finding in &report.findings {
            let key = (
                finding.crate_name.clone(),
                finding.rel_file.clone(),
                finding.symbol.clone(),
            );
            let Some(baseline) = index.get(&key) else {
                return Err(format!(
                    "Unlisted process-global declaration detected at crates/{}/src/{}:{} (`{} {}`). Give it an explicit owner (ServerContext/WorldRepository/SystemActor/SdeRegistry per #1583) or classify it if genuinely process-wide.",
                    finding.crate_name, finding.rel_file, finding.line, finding.kind, finding.symbol
                ));
            };
            if baseline.kind != finding.kind {
                return Err(format!(
                    "Process-global declaration kind changed at crates/{}/src/{}:{} `{}`: baseline expects `{}`, found `{}`.",
                    finding.crate_name,
                    finding.rel_file,
                    finding.line,
                    finding.symbol,
                    baseline.kind,
                    finding.kind
                ));
            }
        }

        let mut per_group_counts: BTreeMap<ProcessGlobalGroup, usize> =
            ProcessGlobalGroup::ALL.into_iter().map(|g| (g, 0)).collect();

        for ((crate_name, rel_file, symbol, _kind), &actual_count) in &report.per_declaration_counts
        {
            let key = (crate_name.clone(), rel_file.clone(), symbol.clone());
            let baseline = index
                .get(&key)
                .expect("every finding key was validated against baseline index above");
            if actual_count > baseline.max_count {
                return Err(format!(
                    "Process-global declaration count regressed at crates/{crate_name}/src/{rel_file} `{symbol}` (group {}): found {actual_count}, allowed max_count={}.",
                    baseline.group, baseline.max_count
                ));
            }
            *per_group_counts.entry(baseline.group).or_default() += actual_count;
        }

        for group in ProcessGlobalGroup::ALL {
            let policy = &self.groups[&group];
            let actual = per_group_counts.get(&group).copied().unwrap_or(0);
            if policy.zero_required && actual > 0 {
                return Err(format!(
                    "Process-global group `{group}` has zero_required=true (#1583) but {actual} declaration(s) remain."
                ));
            }
            if actual > policy.max_count {
                return Err(format!(
                    "Process-global group `{group}` count regressed: found {actual}, max_count={}.",
                    policy.max_count
                ));
            }
        }

        Ok(ProcessGlobalVerifiedSummary {
            report: report.clone(),
            per_group_counts,
        })
    }
}

fn validate_entry_rationale(
    entry: &ProcessGlobalBaselineEntry,
    required: bool,
) -> Result<(), String> {
    match entry.rationale.as_deref() {
        Some(text) => {
            if text.trim().is_empty() || text.contains('\n') || text.contains('\r') {
                return Err(format!(
                    "process_global_ratchet entry crates/{}/src/{} `{}` rationale must be a non-empty single line",
                    entry.crate_name, entry.rel_file, entry.symbol
                ));
            }
            Ok(())
        }
        None if required => Err(format!(
            "Group `{}` allowlist entry crates/{}/src/{} `{}` is missing required 1-line rationale",
            entry.group, entry.crate_name, entry.rel_file, entry.symbol
        )),
        None => Ok(()),
    }
}

/// Scans all production crate `src/` trees under `workspace_root/crates/` except `excluded_crates`.
pub fn scan_workspace_process_globals(
    workspace_root: &Path,
    excluded_crates: &BTreeSet<String>,
) -> Result<ProcessGlobalScanReport, String> {
    let crate_dirs = collect_production_crate_src_dirs(workspace_root, excluded_crates)?;
    let mut report = ProcessGlobalScanReport::default();

    for (crate_name, src_dir) in crate_dirs {
        report.scanned_crates.push(crate_name.clone());
        let mut files = Vec::new();
        collect_rs_files_recursive(&src_dir, &src_dir, &mut files)?;
        files.sort_by(|a, b| a.0.cmp(&b.0));

        let cfg_test_external_files = collect_cfg_test_module_targets(&src_dir, &files)?;

        for (rel_unix, path) in files {
            if is_test_only_rel_path(&rel_unix) || cfg_test_external_files.contains(&rel_unix) {
                continue;
            }
            report.scanned_files += 1;
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            let findings = scan_rust_source(&crate_name, &rel_unix, &raw);
            for finding in findings {
                *report
                    .per_declaration_counts
                    .entry((
                        finding.crate_name.clone(),
                        finding.rel_file.clone(),
                        finding.symbol.clone(),
                        finding.kind,
                    ))
                    .or_default() += 1;
                *report
                    .per_crate_counts
                    .entry(finding.crate_name.clone())
                    .or_default() += 1;
                report.findings.push(finding);
            }
        }
    }

    report.findings.sort();
    Ok(report)
}

/// Discovers every `crates/<crate>/src/` directory not listed in `excluded_crates`.
pub fn collect_production_crate_src_dirs(
    workspace_root: &Path,
    excluded_crates: &BTreeSet<String>,
) -> Result<Vec<(String, PathBuf)>, String> {
    let crates_dir = workspace_root.join("crates");
    let entries = std::fs::read_dir(&crates_dir)
        .map_err(|e| format!("failed to read {}: {e}", crates_dir.display()))?;

    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("failed to read dir entry: {e}"))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if excluded_crates.contains(name) {
            continue;
        }
        let src_dir = path.join("src");
        if src_dir.is_dir() {
            out.push((name.to_string(), src_dir));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Strips comments, string/char literals, and `#[cfg(test)]` items from `raw_src`, then returns
/// every process-global declaration found in production code.
pub fn scan_rust_source(
    crate_name: &str,
    rel_file: &str,
    raw_src: &str,
) -> Vec<ProcessGlobalFinding> {
    if !raw_src.contains("static")
        && !raw_src.contains("thread_local")
        && !raw_src.contains("task_local")
    {
        return Vec::new();
    }
    let no_lit = strip_rust_comments_and_literals(raw_src);
    let prod_only = strip_cfg_test_items_depth_aware(&no_lit);
    scan_clean_rust_source(crate_name, rel_file, &prod_only)
}

fn scan_clean_rust_source(
    crate_name: &str,
    rel_file: &str,
    prod_only: &str,
) -> Vec<ProcessGlobalFinding> {
    let line_lookup = LineLookup::new(prod_only);
    let mut work = prod_only.as_bytes().to_vec();
    let mut findings = Vec::new();

    extract_local_macro_findings(
        crate_name,
        rel_file,
        &line_lookup,
        &mut work,
        &mut findings,
    );
    extract_static_findings(
        crate_name,
        rel_file,
        &line_lookup,
        &work,
        &mut findings,
    );

    findings.sort();
    findings
}

fn strip_cfg_test_items_depth_aware(stripped_src: &str) -> String {
    let mut bytes = stripped_src.as_bytes().to_vec();
    let n = bytes.len();
    let mut i = 0usize;

    while i + 1 < n {
        if bytes[i] == b'#' && bytes[i + 1] == b'[' {
            if let Some(attr_end) = find_cfg_test_attr_end(&bytes, i) {
                let item_end = find_attributed_item_end_depth_aware(&bytes, attr_end + 1);
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

fn find_attributed_item_end_depth_aware(bytes: &[u8], start: usize) -> usize {
    let n = bytes.len();
    let mut pos = skip_outer_attrs_and_ws(bytes, start);
    let mode = classify_attributed_item(bytes, pos);

    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut brace = 0usize;
    let mut angle = 0usize;
    let mut seen_top_colon = false;
    let mut seen_fat_arrow = false;

    while pos < n {
        let b = bytes[pos];
        if paren == 0 && bracket == 0 && brace == 0 {
            if !seen_fat_arrow && b == b'=' && bytes.get(pos + 1) == Some(&b'>') {
                seen_fat_arrow = true;
                angle = 0;
                pos += 2;
                continue;
            }
            if b == b':' && bytes.get(pos + 1) != Some(&b':') && (pos == 0 || bytes[pos - 1] != b':') {
                seen_top_colon = true;
            }
            match b {
                b'}' => return pos,
                b';' => return pos + 1,
                b',' if matches!(mode, AttributedItemMode::FieldVariantOrArm) && angle == 0 => {
                    return pos + 1;
                }
                b'{' => {
                    brace = 1;
                    pos += 1;
                    while pos < n && brace > 0 {
                        match bytes[pos] {
                            b'{' => brace += 1,
                            b'}' => brace -= 1,
                            _ => {}
                        }
                        pos += 1;
                    }
                    if matches!(mode, AttributedItemMode::SemiTerminated) {
                        continue;
                    }
                    if matches!(mode, AttributedItemMode::FieldVariantOrArm) {
                        let after_ws = skip_ws(bytes, pos);
                        if !seen_fat_arrow
                            && (bytes.get(after_ws..).is_some_and(|rest| rest.starts_with(b"=>"))
                                || starts_with_word(bytes, after_ws, b"if"))
                        {
                            continue;
                        }
                        if bytes.get(after_ws) == Some(&b',') {
                            return after_ws + 1;
                        }
                    }
                    return pos;
                }
                b'<' if matches!(mode, AttributedItemMode::FieldVariantOrArm)
                    && seen_top_colon
                    && !seen_fat_arrow =>
                {
                    angle += 1;
                }
                b'>' if matches!(mode, AttributedItemMode::FieldVariantOrArm)
                    && seen_top_colon
                    && !seen_fat_arrow =>
                {
                    let is_arrow = pos > 0 && (bytes[pos - 1] == b'-' || bytes[pos - 1] == b'=');
                    if !is_arrow {
                        angle = angle.saturating_sub(1);
                    }
                }
                b'(' => paren += 1,
                b')' => paren = paren.saturating_sub(1),
                b'[' => bracket += 1,
                b']' => bracket = bracket.saturating_sub(1),
                _ => {}
            }
        } else {
            match b {
                b'(' => paren += 1,
                b')' => paren = paren.saturating_sub(1),
                b'[' => bracket += 1,
                b']' => bracket = bracket.saturating_sub(1),
                b'{' => brace += 1,
                b'}' => brace = brace.saturating_sub(1),
                _ => {}
            }
        }
        pos += 1;
    }
    n
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AttributedItemMode {
    /// `static`, non-`fn` `const`, `type`, `use`, `let`: terminate only at depth-0 `;`.
    SemiTerminated,
    /// `fn`, `mod`, `struct`, `enum`, `trait`, `impl`, `macro_rules`, `extern`: `{...}` or `;`.
    BlockOrSemiItem,
    /// Struct field, enum variant, or match arm: `,`, `{...}`, `;`, or unmatched `}`.
    FieldVariantOrArm,
}

fn classify_attributed_item(bytes: &[u8], mut pos: usize) -> AttributedItemMode {
    pos = skip_visibility(bytes, pos);
    pos = skip_ws(bytes, pos);
    loop {
        if starts_with_word(bytes, pos, b"async") {
            pos = skip_ws(bytes, pos + 5);
        } else if starts_with_word(bytes, pos, b"unsafe") {
            pos = skip_ws(bytes, pos + 6);
        } else if starts_with_word(bytes, pos, b"default") {
            pos = skip_ws(bytes, pos + 7);
        } else {
            break;
        }
    }
    if starts_with_word(bytes, pos, b"const") {
        let after_const = skip_ws(bytes, pos + 5);
        if starts_with_word(bytes, after_const, b"fn")
            || starts_with_word(bytes, after_const, b"unsafe")
            || starts_with_word(bytes, after_const, b"extern")
        {
            return AttributedItemMode::BlockOrSemiItem;
        }
        return AttributedItemMode::SemiTerminated;
    }
    let Some((kw, _)) = read_ident(bytes, pos) else {
        return AttributedItemMode::FieldVariantOrArm;
    };
    match kw {
        "static" | "type" | "use" | "let" => AttributedItemMode::SemiTerminated,
        "fn" | "mod" | "struct" | "enum" | "trait" | "impl" | "macro_rules" | "extern" => {
            AttributedItemMode::BlockOrSemiItem
        }
        _ => AttributedItemMode::FieldVariantOrArm,
    }
}

fn skip_outer_attrs_and_ws(bytes: &[u8], mut pos: usize) -> usize {
    loop {
        pos = skip_ws(bytes, pos);
        if pos + 1 < bytes.len() && bytes[pos] == b'#' && bytes[pos + 1] == b'[' {
            pos = find_matching_delim_end(bytes, pos + 2, b'[', b']');
            continue;
        }
        return pos;
    }
}

fn collect_cfg_test_module_targets(
    src_dir: &Path,
    files: &[(String, PathBuf)],
) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    for (rel_unix, path) in files {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        if !raw.contains("cfg") || !raw.contains("test") || !raw.contains("mod") {
            continue;
        }
        let no_lit = strip_rust_comments_and_literals(&raw);
        let raw_bytes = raw.as_bytes();
        let bytes = no_lit.as_bytes();
        let mut i = 0usize;
        while i + 1 < bytes.len() {
            if bytes[i] == b'#' && bytes[i + 1] == b'[' {
                let (has_cfg_test, path_override, after_attrs) =
                    collect_contiguous_outer_attrs(bytes, raw_bytes, i);
                if has_cfg_test {
                    if let Some((target_rel, end_pos)) = parse_external_mod_item(
                        src_dir,
                        rel_unix,
                        bytes,
                        after_attrs,
                        path_override.as_deref(),
                    ) {
                        out.insert(target_rel);
                        i = end_pos;
                        continue;
                    }
                }
                i = after_attrs;
                continue;
            }
            i += 1;
        }
    }
    Ok(out)
}

fn collect_contiguous_outer_attrs(
    clean_bytes: &[u8],
    raw_bytes: &[u8],
    mut pos: usize,
) -> (bool, Option<String>, usize) {
    let mut has_cfg_test = false;
    let mut path_override: Option<String> = None;

    loop {
        pos = skip_ws(clean_bytes, pos);
        if pos + 1 < clean_bytes.len() && clean_bytes[pos] == b'#' && clean_bytes[pos + 1] == b'[' {
            let attr_start = pos;
            if find_cfg_test_attr_end(clean_bytes, attr_start).is_some() {
                has_cfg_test = true;
            }
            let attr_end = find_matching_delim_end(clean_bytes, attr_start + 2, b'[', b']');
            let slice_start = (attr_start + 2).min(raw_bytes.len());
            let slice_end = attr_end
                .saturating_sub(1)
                .min(raw_bytes.len())
                .max(slice_start);
            if let Ok(raw_attr) = std::str::from_utf8(&raw_bytes[slice_start..slice_end]) {
                if let Some(p) = extract_path_attr_value(raw_attr) {
                    path_override = Some(p);
                }
            }
            pos = attr_end;
            continue;
        }
        break;
    }

    (has_cfg_test, path_override, pos)
}

fn parse_external_mod_item(
    src_dir: &Path,
    parent_rel_unix: &str,
    clean_bytes: &[u8],
    mut pos: usize,
    path_override: Option<&str>,
) -> Option<(String, usize)> {
    pos = skip_visibility(clean_bytes, pos);
    pos = skip_ws(clean_bytes, pos);
    if !starts_with_word(clean_bytes, pos, b"mod") {
        return None;
    }
    pos = skip_ws(clean_bytes, pos + 3);
    let (mod_name, after_name) = read_ident(clean_bytes, pos)?;
    pos = skip_ws(clean_bytes, after_name);
    if clean_bytes.get(pos) != Some(&b';') {
        return None;
    }

    if let Some(override_rel) = path_override {
        // Under Rust's #[path] rules on an external mod item, #[path = "..."] resolves relative
        // to the directory containing the declaring file (even for non-mod.rs files).
        let abs = src_dir.join(parent_rel_unix);
        let file_dir = abs.parent().unwrap_or(src_dir);
        let candidate = file_dir.join(override_rel);
        if let Some(norm) = normalize_lexical(&candidate) {
            if norm.is_file() {
                if let Some(rel) = normalize_under_src(src_dir, &norm) {
                    return Some((rel, pos + 1));
                }
            }
        }
        return None;
    }

    let parent_dir = module_search_dir(src_dir, parent_rel_unix);
    for candidate in [
        parent_dir.join(format!("{mod_name}.rs")),
        parent_dir.join(mod_name).join("mod.rs"),
    ] {
        if let Some(norm) = normalize_lexical(&candidate) {
            if norm.is_file() {
                if let Some(rel) = normalize_under_src(src_dir, &norm) {
                    return Some((rel, pos + 1));
                }
            }
        }
    }
    None
}

fn extract_path_attr_value(raw_attr: &str) -> Option<String> {
    let compact = raw_attr.trim();
    let rest = compact.strip_prefix("path")?;
    let after_key = rest.trim_start();
    if !after_key.starts_with('=') {
        return None;
    }
    let rhs = after_key[1..].trim();
    rhs.strip_prefix("r\"")
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| rhs.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
        .map(str::to_string)
}

fn module_search_dir(src_dir: &Path, parent_rel_unix: &str) -> PathBuf {
    let rel_path = Path::new(parent_rel_unix);
    let file_name = rel_path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let abs = src_dir.join(rel_path);
    let dir = abs.parent().unwrap_or(src_dir);
    if matches!(file_name, "mod.rs" | "lib.rs" | "main.rs") {
        dir.to_path_buf()
    } else {
        dir.join(rel_path.file_stem().unwrap_or_default())
    }
}

fn normalize_lexical(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

fn normalize_under_src(src_dir: &Path, candidate: &Path) -> Option<String> {
    let norm_src = normalize_lexical(src_dir)?;
    let norm_cand = normalize_lexical(candidate)?;
    let rel = norm_cand.strip_prefix(&norm_src).ok()?;
    Some(rel.to_string_lossy().replace('\\', "/"))
}

struct LineLookup {
    line_starts: Vec<usize>,
}

impl LineLookup {
    fn new(src: &str) -> Self {
        let mut line_starts = vec![0usize];
        for (idx, b) in src.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(idx + 1);
            }
        }
        Self { line_starts }
    }

    fn line_of(&self, byte_idx: usize) -> usize {
        self.line_starts.partition_point(|&start| start <= byte_idx)
    }
}

fn extract_local_macro_findings(
    crate_name: &str,
    rel_file: &str,
    lines: &LineLookup,
    work: &mut [u8],
    out: &mut Vec<ProcessGlobalFinding>,
) {
    let mut i = 0usize;
    while i < work.len() {
        let matched = if starts_with_word(work, i, b"thread_local") {
            Some((b"thread_local".len(), ProcessGlobalKind::ThreadLocal))
        } else if starts_with_word(work, i, b"task_local") {
            Some((b"task_local".len(), ProcessGlobalKind::TaskLocal))
        } else {
            None
        };

        let Some((kw_len, kind)) = matched else {
            i += 1;
            continue;
        };

        let after_kw = skip_ws(work, i + kw_len);
        if work.get(after_kw) != Some(&b'!') {
            i += kw_len;
            continue;
        }
        let open_pos = skip_ws(work, after_kw + 1);
        let Some(&open_ch) = work.get(open_pos) else {
            break;
        };
        let Some(close_ch) = matching_delim(open_ch) else {
            i = after_kw + 1;
            continue;
        };

        let body_start = open_pos + 1;
        let end_pos = find_matching_delim_end(work, body_start, open_ch, close_ch);
        let body_end = end_pos.saturating_sub(1);
        collect_macro_static_symbols(
            crate_name,
            rel_file,
            lines,
            work,
            body_start,
            body_end,
            kind,
            out,
        );

        let mask_start = rewind_macro_path_prefix(work, i);
        for b in &mut work[mask_start..end_pos] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        i = end_pos;
    }
}

fn rewind_macro_path_prefix(bytes: &[u8], mut start: usize) -> usize {
    loop {
        let mut p = start;
        while p > 0 && bytes[p - 1].is_ascii_whitespace() {
            p -= 1;
        }
        if p >= 2 && bytes[p - 2] == b':' && bytes[p - 1] == b':' {
            p -= 2;
            while p > 0 && bytes[p - 1].is_ascii_whitespace() {
                p -= 1;
            }
            let ident_end = p;
            while p > 0 && is_ident_byte(bytes[p - 1]) {
                p -= 1;
            }
            if p < ident_end {
                start = p;
                continue;
            }
        }
        break;
    }
    start
}

#[allow(clippy::too_many_arguments)]
fn collect_macro_static_symbols(
    crate_name: &str,
    rel_file: &str,
    lines: &LineLookup,
    bytes: &[u8],
    body_start: usize,
    body_end: usize,
    kind: ProcessGlobalKind,
    out: &mut Vec<ProcessGlobalFinding>,
) {
    let mut pos = body_start;
    let mut brace_depth = 0usize;
    while pos < body_end {
        match bytes[pos] {
            b'{' => {
                brace_depth += 1;
                pos += 1;
                continue;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                pos += 1;
                continue;
            }
            _ => {}
        }
        if brace_depth == 0
            && is_static_decl_keyword(bytes, pos)
            && pos + 6 < body_end
        {
            let mut cursor = skip_ws(bytes, pos + 6);
            if starts_with_word(bytes, cursor, b"mut") {
                cursor = skip_ws(bytes, cursor + 3);
            }
            if let Some((sym, after_sym)) = read_ident(bytes, cursor) {
                let colon_pos = skip_ws(bytes, after_sym);
                if bytes.get(colon_pos) == Some(&b':') {
                    out.push(ProcessGlobalFinding {
                        crate_name: crate_name.to_string(),
                        rel_file: rel_file.to_string(),
                        line: lines.line_of(pos),
                        kind,
                        symbol: sym.to_string(),
                    });
                    pos = colon_pos + 1;
                    continue;
                }
            }
        }
        pos += 1;
    }
}

fn extract_static_findings(
    crate_name: &str,
    rel_file: &str,
    lines: &LineLookup,
    bytes: &[u8],
    out: &mut Vec<ProcessGlobalFinding>,
) {
    let mut i = 0usize;
    while i < bytes.len() {
        if !is_static_decl_keyword(bytes, i) {
            i += 1;
            continue;
        }
        let mut cursor = skip_ws(bytes, i + 6);
        let is_mut = if starts_with_word(bytes, cursor, b"mut") {
            cursor = skip_ws(bytes, cursor + 3);
            true
        } else {
            false
        };
        let Some((sym, after_sym)) = read_ident(bytes, cursor) else {
            i += 6;
            continue;
        };
        let colon_pos = skip_ws(bytes, after_sym);
        if bytes.get(colon_pos) != Some(&b':') {
            i = after_sym;
            continue;
        }
        let semi_pos = find_static_semicolon(bytes, colon_pos + 1);
        let type_and_init = &bytes[colon_pos + 1..semi_pos];
        let type_end_rel = find_top_level_assign_eq(type_and_init).unwrap_or(type_and_init.len());
        let type_slice = &type_and_init[..type_end_rel];
        let kind = if is_mut {
            Some(ProcessGlobalKind::StaticMut)
        } else if is_provably_immutable_static(crate_name, rel_file, sym, type_slice) {
            None
        } else {
            Some(ProcessGlobalKind::StaticSync)
        };
        if let Some(kind) = kind {
            out.push(ProcessGlobalFinding {
                crate_name: crate_name.to_string(),
                rel_file: rel_file.to_string(),
                line: lines.line_of(i),
                kind,
                symbol: sym.to_string(),
            });
        }
        i = semi_pos + 1;
    }
}

fn find_static_semicolon(bytes: &[u8], start: usize) -> usize {
    let mut pos = start;
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut brace = 0usize;
    let mut bracket = 0usize;
    let mut in_initializer = false;

    while pos < bytes.len() {
        match bytes[pos] {
            b';' if angle == 0 && paren == 0 && brace == 0 && bracket == 0 => return pos,
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b'{' => brace += 1,
            b'}' => brace = brace.saturating_sub(1),
            b'[' => bracket += 1,
            b']' => bracket = bracket.saturating_sub(1),
            b'=' if !in_initializer
                && angle == 0
                && paren == 0
                && brace == 0
                && bracket == 0 =>
            {
                in_initializer = true;
            }
            b'<' if !in_initializer => angle += 1,
            b'>' if !in_initializer => {
                let is_arrow = pos > 0 && bytes[pos - 1] == b'-';
                if !is_arrow {
                    angle = angle.saturating_sub(1);
                }
            }
            _ => {}
        }
        pos += 1;
    }
    bytes.len()
}

fn find_top_level_assign_eq(bytes: &[u8]) -> Option<usize> {
    let mut angle = 0usize;
    let mut paren = 0usize;
    let mut brace = 0usize;
    let mut bracket = 0usize;
    for (pos, &b) in bytes.iter().enumerate() {
        match b {
            b'=' if angle == 0
                && paren == 0
                && brace == 0
                && bracket == 0
                && bytes.get(pos + 1) != Some(&b'>') =>
            {
                return Some(pos);
            }
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b'{' => brace += 1,
            b'}' => brace = brace.saturating_sub(1),
            b'[' => bracket += 1,
            b']' => bracket = bracket.saturating_sub(1),
            b'<' => angle += 1,
            b'>' => {
                let is_arrow = pos > 0 && (bytes[pos - 1] == b'-' || bytes[pos - 1] == b'=');
                if !is_arrow {
                    angle = angle.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
    None
}

fn is_provably_immutable_static(
    crate_name: &str,
    rel_file: &str,
    symbol: &str,
    type_slice: &[u8],
) -> bool {
    is_provably_immutable_type_expr(type_slice)
        || is_allowlisted_immutable_static_table(crate_name, rel_file, symbol, type_slice)
}

fn is_provably_immutable_type_expr(raw: &[u8]) -> bool {
    let s = trim_ascii_bytes(raw);
    if s.is_empty() {
        return false;
    }

    if outer_delim_wraps(s, b'(', b')') {
        let inner = trim_ascii_bytes(&s[1..s.len() - 1]);
        if inner.is_empty() {
            return true;
        }
        let parts = split_top_level(inner, b',');
        return parts
            .into_iter()
            .filter(|part| !trim_ascii_bytes(part).is_empty())
            .all(is_provably_immutable_type_expr);
    }

    if let Some(after_amp) = s.strip_prefix(b"&") {
        let mut rest = trim_ascii_bytes(after_amp);
        if let Some(after_tick) = rest.strip_prefix(b"'") {
            let mut idx = 0usize;
            while idx < after_tick.len() && is_ident_byte(after_tick[idx]) {
                idx += 1;
            }
            if idx == 0 {
                return false;
            }
            rest = trim_ascii_bytes(&after_tick[idx..]);
        }
        if starts_with_word(rest, 0, b"mut") {
            return false;
        }
        if rest == b"str" {
            return true;
        }
        if outer_delim_wraps(rest, b'[', b']') {
            let inner = &rest[1..rest.len() - 1];
            return match find_top_level_byte(inner, b';') {
                Some(semi) => is_provably_immutable_type_expr(&inner[..semi]),
                None => is_provably_immutable_type_expr(inner),
            };
        }
        return is_provably_immutable_type_expr(rest);
    }

    if outer_delim_wraps(s, b'[', b']') {
        let inner = &s[1..s.len() - 1];
        let Some(semi) = find_top_level_byte(inner, b';') else {
            return false;
        };
        return is_provably_immutable_type_expr(&inner[..semi]);
    }

    is_scalar_primitive_ident(s)
}

fn is_scalar_primitive_ident(s: &[u8]) -> bool {
    matches!(
        s,
        b"bool"
            | b"char"
            | b"i8"
            | b"i16"
            | b"i32"
            | b"i64"
            | b"i128"
            | b"isize"
            | b"u8"
            | b"u16"
            | b"u32"
            | b"u64"
            | b"u128"
            | b"usize"
            | b"f32"
            | b"f64"
    )
}

fn trim_ascii_bytes(mut s: &[u8]) -> &[u8] {
    while let Some((&first, rest)) = s.split_first() {
        if first.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    while let Some((&last, rest)) = s.split_last() {
        if last.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    s
}

fn outer_delim_wraps(s: &[u8], open: u8, close: u8) -> bool {
    if s.len() < 2 || s[0] != open || s[s.len() - 1] != close {
        return false;
    }
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut brace = 0usize;
    for (idx, &b) in s.iter().enumerate() {
        match b {
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b'[' => bracket += 1,
            b']' => bracket = bracket.saturating_sub(1),
            b'{' => brace += 1,
            b'}' => brace = brace.saturating_sub(1),
            _ => {}
        }
        if paren == 0 && bracket == 0 && brace == 0 {
            return idx == s.len() - 1;
        }
    }
    false
}

fn find_top_level_byte(bytes: &[u8], target: u8) -> Option<usize> {
    let mut paren = 0usize;
    let mut bracket = 0usize;
    let mut brace = 0usize;
    let mut angle = 0usize;
    for (idx, &b) in bytes.iter().enumerate() {
        if b == target && paren == 0 && bracket == 0 && brace == 0 && angle == 0 {
            return Some(idx);
        }
        match b {
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b'[' => bracket += 1,
            b']' => bracket = bracket.saturating_sub(1),
            b'{' => brace += 1,
            b'}' => brace = brace.saturating_sub(1),
            b'<' => angle += 1,
            b'>' => {
                let is_arrow = idx > 0 && (bytes[idx - 1] == b'-' || bytes[idx - 1] == b'=');
                if !is_arrow {
                    angle = angle.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level(bytes: &[u8], delim: u8) -> Vec<&[u8]> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    while let Some(rel) = find_top_level_byte(&bytes[start..], delim) {
        parts.push(&bytes[start..start + rel]);
        start += rel + 1;
    }
    parts.push(&bytes[start..]);
    parts
}

/// Explicitly audited immutable read-only static descriptor/fallback tables in production code.
/// Any newly added non-primitive `static` outside these exact declarations must be baselined.
const IMMUTABLE_STATIC_TABLES: &[(&str, &str, &str, &str)] = &[
    ("eve-proto", "marshal.rs", "ENCODE_NONE_FALLBACK", "PyRep"),
    ("eve-sde", "cfsd_nested_layouts.rs", "MILESTONE", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts.rs", "SKILL_REQ", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts.rs", "TAX_BY_VALUE", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts.rs", "TYPE_AMOUNT", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts.rs", "PLAYER_QUANTITY", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts.rs", "REWARD_TABLE", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts.rs", "TASK_PARAMETER", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts.rs", "RACE_DUNGEONS", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts.rs", "CFSD_NESTED_LAYOUTS", "&[CfsdNestedLayout]"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "LOGIN_REWARD", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "CHALLENGE_REWARD", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "ATOM_VALUE", "[(u64,CfsdType);19]"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "ATOM_INPUT", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "NODE_INPUT", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "PARAM_OUTPUT", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "ATOM_PARAMS", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "NODE_PARAMS", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "SUBTITLE", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "PORTS", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "CHAIN_DICT_VALUE", "[(u64,CfsdType);5]"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "CHAIN_LIST_ITEM", "[(u64,CfsdType);1]"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "CHAIN_VALUE", "[(u64,CfsdType);12]"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "CHAIN_PARAM", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "CHAIN_TRIGGER", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "TASK_OVERRIDE", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "CHAIN_OBJECTIVE", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "TYPE_PARAM", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "TASK_TRIGGER", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "OBJECTIVE_TASK", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "OPERATION_REWARD", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "SKIP_CONDITION", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "BLACKBOARD_MESSAGE", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "TASK_REWARD", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "LICENSE_GRANT", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "SEQUENCE_BINDER", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "SPLASH", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "LOOT_BY_RACE", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "LINE_HINT", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "CONVERSATION_LINE", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "CONTRIBUTION_METHOD_PARAM", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "DUNGEON_BLACKBOARD_PARAM", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "DUNGEON_OBJECTIVE_OVERRIDE", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "DUNGEON_CLIENT_OBJECTIVES", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "SURVEY_ITEM_REWARD", "CfsdObject"),
    ("eve-sde", "cfsd_nested_layouts_ops.rs", "CFSD_NESTED_LAYOUTS_OPS", "&[CfsdNestedLayout]"),
];

fn is_allowlisted_immutable_static_table(
    crate_name: &str,
    rel_file: &str,
    symbol: &str,
    type_slice: &[u8],
) -> bool {
    let compact: String = type_slice
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .map(char::from)
        .collect();
    IMMUTABLE_STATIC_TABLES
        .iter()
        .any(|&(c, f, s, t)| c == crate_name && f == rel_file && s == symbol && t == compact)
}

fn is_static_decl_keyword(bytes: &[u8], idx: usize) -> bool {
    if !starts_with_word(bytes, idx, b"static") {
        return false;
    }
    if idx > 0 && (bytes[idx - 1] == b'\'' || bytes[idx - 1] == b'#') {
        return false;
    }
    true
}

fn starts_with_word(bytes: &[u8], idx: usize, word: &[u8]) -> bool {
    if idx + word.len() > bytes.len() {
        return false;
    }
    if &bytes[idx..idx + word.len()] != word {
        return false;
    }
    let left_ok = idx == 0 || !is_ident_byte(bytes[idx - 1]);
    let right_ok = idx + word.len() == bytes.len() || !is_ident_byte(bytes[idx + word.len()]);
    left_ok && right_ok
}

fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    pos
}

fn skip_visibility(bytes: &[u8], mut pos: usize) -> usize {
    pos = skip_ws(bytes, pos);
    if !starts_with_word(bytes, pos, b"pub") {
        return pos;
    }
    pos = skip_ws(bytes, pos + 3);
    if bytes.get(pos) == Some(&b'(') {
        pos = find_matching_delim_end(bytes, pos + 1, b'(', b')');
    }
    pos
}

fn is_ident_start_byte(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn read_ident(bytes: &[u8], pos: usize) -> Option<(&str, usize)> {
    let &first = bytes.get(pos)?;
    if !is_ident_start_byte(first) {
        return None;
    }
    let mut end = pos + 1;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    let s = std::str::from_utf8(&bytes[pos..end]).ok()?;
    Some((s, end))
}

fn matching_delim(open: u8) -> Option<u8> {
    match open {
        b'{' => Some(b'}'),
        b'(' => Some(b')'),
        b'[' => Some(b']'),
        _ => None,
    }
}

fn find_matching_delim_end(bytes: &[u8], mut pos: usize, open: u8, close: u8) -> usize {
    let mut depth = 1usize;
    while pos < bytes.len() && depth > 0 {
        if bytes[pos] == open {
            depth += 1;
        } else if bytes[pos] == close {
            depth -= 1;
        }
        pos += 1;
    }
    pos
}

// ---------------------------------------------------------------------------
// Two-context side-by-side isolation test helpers (#1583 Track 1)
// ---------------------------------------------------------------------------

/// Generic side-by-side container holding two independent test contexts (`left` and `right`)
/// running concurrently within the same OS process (`#1583` Track 1).
///
/// Placed in `eve-testkit` so both DB/SDE tests here and `ServerContext`-level tests in
/// `eve-server/tests/it/common.rs` can share the same pair semantics without reversing the
/// workspace dependency direction (`eve-server` -> `eve-testkit`).
#[derive(Debug, Clone)]
pub struct TwoContextPair<C> {
    pub left: C,
    pub right: C,
}

impl<C> TwoContextPair<C> {
    /// Builds two independent contexts by invoking `make_ctx` twice.
    pub fn from_factory<F>(mut make_ctx: F) -> Self
    where
        F: FnMut() -> C,
    {
        Self {
            left: make_ctx(),
            right: make_ctx(),
        }
    }

    /// Wraps two pre-constructed contexts `(left, right)`.
    pub fn from_parts(left: C, right: C) -> Self {
        Self { left, right }
    }

    /// Borrows the left context.
    pub fn left(&self) -> &C {
        &self.left
    }

    /// Borrows the right context.
    pub fn right(&self) -> &C {
        &self.right
    }

    /// Borrows both contexts `(left, right)`.
    pub fn both(&self) -> (&C, &C) {
        (&self.left, &self.right)
    }

    /// Consumes the pair and returns `(left, right)`.
    pub fn into_parts(self) -> (C, C) {
        (self.left, self.right)
    }

    /// Maps each side independently using `f`.
    pub fn map<T>(self, mut f: impl FnMut(C) -> T) -> TwoContextPair<T> {
        TwoContextPair {
            left: f(self.left),
            right: f(self.right),
        }
    }
}

/// Pair of completely independent [`WorldRepository`] and [`SdeRegistry`] instances for
/// unit/smoke verification of multi-context isolation in `eve-db` and `eve-sde` consumers.
pub struct TwoRepositoryPair {
    pub repos: TwoContextPair<Arc<WorldRepository>>,
    pub sdes: TwoContextPair<Arc<SdeRegistry>>,
}

impl TwoRepositoryPair {
    /// Creates two empty in-memory [`WorldRepository`] instances backed by independent
    /// [`SdeRegistry`] defaults.
    pub fn new_empty_isolated() -> Self {
        let pair = Self {
            repos: TwoContextPair::from_factory(|| {
                Arc::new(
                    WorldRepository::new_in_memory()
                        .expect("create isolated in-memory WorldRepository"),
                )
            }),
            sdes: TwoContextPair::from_factory(|| Arc::new(SdeRegistry::default())),
        };
        pair.assert_distinct_instances();
        pair
    }

    /// Creates two demo-seeded in-memory [`WorldRepository`] instances backed by independent
    /// [`SdeRegistry`] defaults without reading `EVOMU_DB_PATH` or `EVOMU_SNAPSHOT_PATH`.
    pub fn with_demo_seed_isolated() -> Self {
        let pair = Self {
            repos: TwoContextPair::from_factory(|| {
                Arc::new(WorldRepository::with_demo_seed_in_memory())
            }),
            sdes: TwoContextPair::from_factory(|| Arc::new(SdeRegistry::default())),
        };
        pair.assert_distinct_instances();
        pair
    }

    /// Creates an isolated pair with explicit left/right [`SdeRegistry`] instances and custom
    /// repository factories.
    pub fn with_sdes(
        left_sde: Arc<SdeRegistry>,
        right_sde: Arc<SdeRegistry>,
        make_left_repo: impl FnOnce() -> Arc<WorldRepository>,
        make_right_repo: impl FnOnce() -> Arc<WorldRepository>,
    ) -> Self {
        let pair = Self {
            repos: TwoContextPair::from_parts(make_left_repo(), make_right_repo()),
            sdes: TwoContextPair::from_parts(left_sde, right_sde),
        };
        pair.assert_distinct_instances();
        pair
    }

    /// Borrows the left [`WorldRepository`].
    pub fn left_repo(&self) -> &Arc<WorldRepository> {
        self.repos.left()
    }

    /// Borrows the right [`WorldRepository`].
    pub fn right_repo(&self) -> &Arc<WorldRepository> {
        self.repos.right()
    }

    /// Borrows the left [`SdeRegistry`].
    pub fn left_sde(&self) -> &Arc<SdeRegistry> {
        self.sdes.left()
    }

    /// Borrows the right [`SdeRegistry`].
    pub fn right_sde(&self) -> &Arc<SdeRegistry> {
        self.sdes.right()
    }

    /// Verifies that the two repositories have distinct monotonic `instance_id()` values and
    /// distinct backing allocations.
    pub fn assert_distinct_instances(&self) {
        assert_ne!(
            self.repos.left.instance_id(),
            self.repos.right.instance_id(),
            "two isolated repositories must allocate distinct instance_id values"
        );
        assert!(
            !Arc::ptr_eq(&self.repos.left, &self.repos.right),
            "left and right WorldRepository Arcs must not alias"
        );
    }
}

#[cfg(test)]
#[path = "process_global_ratchet_tests.rs"]
mod tests;
