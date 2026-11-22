//! Process-global mutable state, static-data cache, and Group G allowlist ratchet (`#1583`).
//!
//! Scans all production crates under `crates/*/src/` (excluding `crates/eve-testkit/`,
//! comments, string/char literals, `#[cfg(test)]` items, and test-only / `#[cfg(test)] mod`
//! files) for:
//! - `static mut`
//! - `static` items using interior mutability / once-initialization primitives (`OnceLock`,
//!   `LazyLock`, `OnceCell`, `LazyCell`, `Mutex`, `RwLock`, `RankedMutex`, `RankedRwLock`,
//!   `Atomic*`, `Cell`, `RefCell`, `UnsafeCell`, `SyncUnsafeCell`, `Once`)
//! - `thread_local!` / `std::thread_local!` blocks
//! - `tokio::task_local!` / `task_local!` blocks
//!
//! Also provides generic [`TwoContextPair`] and repository-level [`TwoRepositoryPair`] isolation
//! test helpers for side-by-side multi-context verification inside the same OS process.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use eve_db::WorldRepository;
use eve_sde::SdeRegistry;
use serde::{Deserialize, Serialize};

use crate::client_harness::{is_ident_byte, strip_rust_comments_and_literals};
use crate::notify_ratchet::{
    collect_rs_files_recursive, find_cfg_test_attr_end, is_test_only_rel_path,
    strip_cfg_test_items,
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
    let prod_only = strip_cfg_test_items(&no_lit);
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

fn collect_cfg_test_module_targets(
    src_dir: &Path,
    files: &[(String, PathBuf)],
) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    for (rel_unix, path) in files {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        if !raw.contains("cfg") || !raw.contains("test") || !raw.contains("mod ") {
            continue;
        }
        let no_lit = strip_rust_comments_and_literals(&raw);
        let raw_bytes = raw.as_bytes();
        let bytes = no_lit.as_bytes();
        let mut i = 0usize;
        while i + 1 < bytes.len() {
            if bytes[i] == b'#' && bytes[i + 1] == b'[' {
                if let Some(attr_end) = find_cfg_test_attr_end(bytes, i) {
                    if let Some((target_rel, end_pos)) =
                        parse_external_mod_after_cfg_test(src_dir, rel_unix, bytes, raw_bytes, attr_end + 1)
                    {
                        out.insert(target_rel);
                        i = end_pos;
                        continue;
                    }
                }
            }
            i += 1;
        }
    }
    Ok(out)
}

fn parse_external_mod_after_cfg_test(
    src_dir: &Path,
    parent_rel_unix: &str,
    clean_bytes: &[u8],
    raw_bytes: &[u8],
    mut pos: usize,
) -> Option<(String, usize)> {
    let mut path_override: Option<String> = None;
    loop {
        pos = skip_ws(clean_bytes, pos);
        if pos + 1 < clean_bytes.len() && clean_bytes[pos] == b'#' && clean_bytes[pos + 1] == b'[' {
            let attr_start = pos + 2;
            pos += 2;
            let mut bd = 1usize;
            while pos < clean_bytes.len() && bd > 0 {
                match clean_bytes[pos] {
                    b'[' => bd += 1,
                    b']' => bd -= 1,
                    _ => {}
                }
                pos += 1;
            }
            if let Ok(raw_attr) = std::str::from_utf8(&raw_bytes[attr_start..pos.saturating_sub(1)]) {
                if let Some(p) = extract_path_attr_value(raw_attr) {
                    path_override = Some(p);
                }
            }
            continue;
        }
        break;
    }

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

    let parent_dir = module_search_dir(src_dir, parent_rel_unix);
    if let Some(override_rel) = path_override {
        let candidate = parent_dir.join(&override_rel);
        if let Some(rel) = normalize_under_src(src_dir, &candidate) {
            return Some((rel, pos + 1));
        }
    }
    for candidate in [
        parent_dir.join(format!("{mod_name}.rs")),
        parent_dir.join(mod_name).join("mod.rs"),
    ] {
        if candidate.is_file() {
            if let Some(rel) = normalize_under_src(src_dir, &candidate) {
                return Some((rel, pos + 1));
            }
        }
    }
    None
}

fn extract_path_attr_value(raw_attr: &str) -> Option<String> {
    let compact = raw_attr.trim();
    if !compact.starts_with("path") {
        return None;
    }
    let eq = compact.find('=')?;
    let rhs = compact[eq + 1..].trim();
    rhs.strip_prefix('"')?.strip_suffix('"').map(str::to_string)
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

fn normalize_under_src(src_dir: &Path, candidate: &Path) -> Option<String> {
    let rel = candidate.strip_prefix(src_dir).ok()?;
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
        let decl_slice = &bytes[i..semi_pos];
        let kind = if is_mut {
            Some(ProcessGlobalKind::StaticMut)
        } else if decl_uses_mutable_or_once_primitive(decl_slice) {
            Some(ProcessGlobalKind::StaticSync)
        } else {
            None
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

fn decl_uses_mutable_or_once_primitive(decl: &[u8]) -> bool {
    let mut i = 0usize;
    while i < decl.len() {
        if is_ident_start_byte(decl[i]) {
            let start = i;
            i += 1;
            while i < decl.len() && is_ident_byte(decl[i]) {
                i += 1;
            }
            let ident = &decl[start..i];
            if is_mutable_or_once_ident(ident) {
                return true;
            }
        } else {
            i += 1;
        }
    }
    false
}

fn is_mutable_or_once_ident(ident: &[u8]) -> bool {
    matches!(
        ident,
        b"OnceLock"
            | b"LazyLock"
            | b"OnceCell"
            | b"LazyCell"
            | b"Mutex"
            | b"RwLock"
            | b"RankedMutex"
            | b"RankedRwLock"
            | b"Cell"
            | b"RefCell"
            | b"UnsafeCell"
            | b"SyncUnsafeCell"
            | b"Once"
    ) || (ident.starts_with(b"Atomic") && ident.len() > b"Atomic".len())
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
            repos: TwoContextPair::from_factory(|| Arc::new(WorldRepository::new_empty())),
            sdes: TwoContextPair::from_factory(|| Arc::new(SdeRegistry::default())),
        };
        pair.assert_distinct_instances();
        pair
    }

    /// Creates two demo-seeded SQLite-backed [`WorldRepository`] instances backed by independent
    /// [`SdeRegistry`] defaults.
    pub fn with_demo_seed_isolated() -> Self {
        let pair = Self {
            repos: TwoContextPair::from_factory(|| Arc::new(WorldRepository::with_demo_seed())),
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
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_SEQ: AtomicU64 = AtomicU64::new(1);

    struct TempDirGuard(PathBuf);

    impl TempDirGuard {
        fn new(prefix: &str) -> Self {
            let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "eve_process_global_ratchet_{prefix}_{}_{}",
                std::process::id(),
                seq
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp workspace dir");
            Self(dir)
        }
    }

    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_test_file(root: &Path, rel: &str, contents: &str) {
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(full, contents).expect("write test file");
    }

    fn clean_empty_ratchet() -> ProcessGlobalRatchet {
        let mut groups = BTreeMap::new();
        for g in ProcessGlobalGroup::ALL {
            groups.insert(
                g,
                GroupPolicy {
                    zero_required: false,
                    max_count: 10,
                    rationale_required: g == ProcessGlobalGroup::GProcessWideAllowlist,
                    description: format!("test group {g}"),
                },
            );
        }
        ProcessGlobalRatchet {
            version: 1,
            owning_issue: 1583,
            description: "test".to_string(),
            excluded_crates: vec!["eve-testkit".to_string()],
            groups,
            declarations: Vec::new(),
        }
    }

    #[test]
    fn live_workspace_scan_matches_process_global_ratchet_cleanly() {
        let ratchet = ProcessGlobalRatchet::load_default()
            .expect("load default process_global_ratchet.json");
        let summary = ratchet
            .verify_default()
            .expect("live workspace scan must satisfy process_global_ratchet.json");

        assert_eq!(
            summary.report.scanned_crates,
            vec![
                "eve-db".to_string(),
                "eve-destiny".to_string(),
                "eve-dogma".to_string(),
                "eve-machonet".to_string(),
                "eve-proto".to_string(),
                "eve-sde".to_string(),
                "eve-server".to_string(),
            ]
        );
        assert!(
            !summary
                .report
                .findings
                .iter()
                .any(|f| f.rel_file.contains("r6_t1_p1_security_tests_combat")),
            "#[cfg(test)] external module files must be skipped"
        );
        let group_sum: usize = summary.per_group_counts.values().sum();
        assert_eq!(group_sum, summary.report.findings.len());
    }

    #[test]
    fn scanner_ignores_comments_strings_cfg_test_and_const_statics() {
        let src = r##"
            // static COMMENTED_OUT: OnceLock<u64> = OnceLock::new();
            /*
                static BLOCK_COMMENT: Mutex<u64> = Mutex::new(0);
                tokio::task_local! { static IN_BLOCK_COMMENT: u64; }
            */
            const RAW: &str = r#"thread_local! { static IN_STRING: Cell<u64> = const { Cell::new(0) }; }"#;
            static CONST_TABLE: &[u32] = &[1, 2, 3];
            static LIFE_USER: &'static str = "ok";
            fn uses_fn_once<F: FnOnce()>(_f: F) {}
            static MUT_COUNTER: AtomicU64 = AtomicU64::new(0);
            static MUT_GLOBAL_CELL: OnceLock<Mutex<u64>> = OnceLock::new();
            static ONCE_FLAG: std::sync::Once = std::sync::Once::new();
            static mut RAW_MUT: u32 = 0;

            thread_local! {
                static TL_ONE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
                static TL_TWO: std::cell::RefCell<Option<u64>> = const { std::cell::RefCell::new(None) };
            }

            tokio::task_local! {
                static TASK_SCOPE: bool;
            }

            #[cfg(test)]
            static TEST_ONLY_HOOK: OnceLock<Mutex<u64>> = OnceLock::new();

            #[cfg(test)]
            mod tests {
                static IN_MOD_TEST: AtomicU64 = AtomicU64::new(0);
                thread_local! { static TL_TEST: Cell<u64> = const { Cell::new(0) }; }
            }
        "##;

        let findings = scan_rust_source("eve-server", "sample.rs", src);
        let got: Vec<(ProcessGlobalKind, &str)> = findings
            .iter()
            .map(|f| (f.kind, f.symbol.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (ProcessGlobalKind::StaticSync, "MUT_COUNTER"),
                (ProcessGlobalKind::StaticSync, "MUT_GLOBAL_CELL"),
                (ProcessGlobalKind::StaticSync, "ONCE_FLAG"),
                (ProcessGlobalKind::StaticMut, "RAW_MUT"),
                (ProcessGlobalKind::ThreadLocal, "TL_ONE"),
                (ProcessGlobalKind::ThreadLocal, "TL_TWO"),
                (ProcessGlobalKind::TaskLocal, "TASK_SCOPE"),
            ]
        );
    }

    #[test]
    fn scanner_skips_cfg_test_external_modules_and_test_paths() {
        let tmp = TempDirGuard::new("skip_cfg_test_mod");
        write_test_file(
            &tmp.0,
            "crates/eve-server/src/lib.rs",
            "#[cfg(test)]\nmod security_helpers_combat;\n",
        );
        write_test_file(
            &tmp.0,
            "crates/eve-server/src/security_helpers_combat.rs",
            "static TEST_SDE: OnceLock<u64> = OnceLock::new();\n",
        );
        write_test_file(
            &tmp.0,
            "crates/eve-testkit/src/lib.rs",
            "static TESTKIT_OK: OnceLock<u64> = OnceLock::new();\n",
        );

        let excluded: BTreeSet<String> = ["eve-testkit".to_string()].into_iter().collect();
        let report =
            scan_workspace_process_globals(&tmp.0, &excluded).expect("scan temp workspace");
        assert!(report.findings.is_empty(), "got {:#?}", report.findings);
    }

    #[test]
    fn unlisted_static_oncelock_thread_local_and_task_local_fail_naming_declaration() {
        let cases = [
            (
                "eve-db",
                "store.rs",
                "static mut LEAKY_RAW: u64 = 0;\n",
                "static_mut",
                "LEAKY_RAW",
            ),
            (
                "eve-proto",
                "codec.rs",
                "static NEW_CACHE: OnceLock<Vec<u8>> = OnceLock::new();\n",
                "static_sync",
                "NEW_CACHE",
            ),
            (
                "eve-sde",
                "loader.rs",
                "thread_local! { static BAD_TL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }\n",
                "thread_local",
                "BAD_TL",
            ),
            (
                "eve-server",
                "router/scope.rs",
                "tokio::task_local! { static BAD_TASK_SCOPE: u64; }\n",
                "task_local",
                "BAD_TASK_SCOPE",
            ),
        ];

        let ratchet = clean_empty_ratchet();
        for (crate_name, rel_file, code, expected_kind, expected_sym) in cases {
            let tmp = TempDirGuard::new(expected_sym);
            write_test_file(
                &tmp.0,
                &format!("crates/{crate_name}/src/{rel_file}"),
                code,
            );
            let err = ratchet
                .verify_workspace(&tmp.0)
                .expect_err("unlisted process-global declaration must fail");
            assert!(
                err.contains(crate_name)
                    && err.contains(rel_file)
                    && err.contains(expected_kind)
                    && err.contains(expected_sym),
                "error must name crate, file, kind, and symbol; got: {err}"
            );
        }
    }

    #[test]
    fn classification_count_regression_kind_change_zero_required_and_allowlist_checks() {
        let mut ratchet = clean_empty_ratchet();
        ratchet.declarations.push(ProcessGlobalBaselineEntry {
            crate_name: "eve-db".to_string(),
            rel_file: "metrics.rs".to_string(),
            symbol: "METRICS_COUNT".to_string(),
            kind: ProcessGlobalKind::StaticSync,
            group: ProcessGlobalGroup::GProcessWideAllowlist,
            max_count: 1,
            rationale: Some("Process-wide metric counter.".to_string()),
        });
        ratchet.declarations.push(ProcessGlobalBaselineEntry {
            crate_name: "eve-server".to_string(),
            rel_file: "state.rs".to_string(),
            symbol: "MAP".to_string(),
            kind: ProcessGlobalKind::StaticSync,
            group: ProcessGlobalGroup::AGameState,
            max_count: 1,
            rationale: None,
        });

        let tmp = TempDirGuard::new("regression");
        write_test_file(
            &tmp.0,
            "crates/eve-server/src/state.rs",
            "fn a() { static MAP: OnceLock<u64> = OnceLock::new(); }\nfn b() { static MAP: OnceLock<u64> = OnceLock::new(); }\n",
        );
        let err = ratchet
            .verify_workspace(&tmp.0)
            .expect_err("count exceeding max_count must fail");
        assert!(
            err.contains("state.rs") && err.contains("MAP") && err.contains("A_game_state"),
            "unexpected err: {err}"
        );

        // Removing all declarations satisfies falling ratchet even before tightening JSON.
        write_test_file(&tmp.0, "crates/eve-server/src/state.rs", "");
        let ok_reduced = ratchet
            .verify_workspace(&tmp.0)
            .expect("reducing declarations below baseline must pass");
        assert_eq!(ok_reduced.per_group_counts[&ProcessGlobalGroup::AGameState], 0);

        // Kind mismatch fails loudly.
        write_test_file(
            &tmp.0,
            "crates/eve-server/src/state.rs",
            "thread_local! { static MAP: Cell<u64> = const { Cell::new(0) }; }\n",
        );
        let kind_err = ratchet
            .verify_workspace(&tmp.0)
            .expect_err("kind change must fail");
        assert!(
            kind_err.contains("kind changed") && kind_err.contains("MAP"),
            "unexpected kind_err: {kind_err}"
        );

        // Zero-required group enforcement rejects non-zero policy and remaining entries.
        ratchet
            .groups
            .get_mut(&ProcessGlobalGroup::AGameState)
            .unwrap()
            .zero_required = true;
        let zero_err = ratchet
            .validate()
            .expect_err("zero_required=true with baseline entry must fail");
        assert!(
            zero_err.contains("A_game_state") && zero_err.contains("zero_required"),
            "unexpected zero_err: {zero_err}"
        );

        // Clearing A_game_state entry & setting max_count=0 succeeds when zero_required=true.
        ratchet.declarations.retain(|d| d.symbol != "MAP");
        let policy_a = ratchet
            .groups
            .get_mut(&ProcessGlobalGroup::AGameState)
            .unwrap();
        policy_a.max_count = 0;
        assert!(ratchet.validate().is_ok());

        // Blank Group G rationale fails loudly.
        ratchet.declarations[0].rationale = Some("   ".to_string());
        let rat_err = ratchet
            .validate()
            .expect_err("Group G entry with blank rationale must fail");
        assert!(rat_err.contains("METRICS_COUNT") && rat_err.contains("rationale"));
    }

    #[test]
    fn two_repository_and_two_context_isolation_helpers_work_side_by_side() {
        let pair = TwoRepositoryPair::with_demo_seed_isolated();
        pair.assert_distinct_instances();

        let right_before_epoch = pair.right_repo().state_epoch();
        let right_before_commits = pair.right_repo().tx_commit_count();
        let shared_char_id = 90_000_001_i64;
        let right_before_sec = pair
            .right_repo()
            .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status));

        pair.left_repo()
            .tx("two_context_left_isolation", |tx| {
                let ch = tx
                    .snap
                    .characters
                    .get_mut(&shared_char_id)
                    .expect("demo seed character 90_000_001 must exist");
                ch.security_status = 4.25;
                Ok::<(), eve_db::TxError>(())
            })
            .expect("commit left tx");

        assert!(pair.left_repo().state_epoch() > right_before_epoch);
        assert_eq!(pair.right_repo().state_epoch(), right_before_epoch);
        assert_eq!(pair.right_repo().tx_commit_count(), right_before_commits);
        assert_ne!(right_before_sec, Some(4.25));
        assert_eq!(
            pair.left_repo()
                .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status)),
            Some(4.25)
        );
        assert_eq!(
            pair.right_repo()
                .with_snapshot(|s| s.characters.get(&shared_char_id).map(|c| c.security_status)),
            right_before_sec
        );

        let server_pair = TwoContextPair::from_factory(eve_server::router::TestContext::new);
        assert_ne!(
            server_pair.left().instance_id,
            server_pair.right().instance_id
        );
        assert_ne!(
            server_pair.left().db.instance_id(),
            server_pair.right().db.instance_id()
        );
    }
}
