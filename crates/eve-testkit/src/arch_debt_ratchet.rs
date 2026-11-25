//! Falling repository-wide per-file architecture-debt ratchet across production `eve-server` code (`#1197`, `#1538`).
//!
//! Scans non-test `.rs` sources under `crates/eve-server/src/` (ignoring `#[cfg(test)]` items,
//! comments, and—unless explicitly enabled per rule—string/char literals) for every tracked debt
//! counter in `crates/eve-testkit/ratchets/arch_debt_ratchet.json` (schema version 2). Non-zero
//! rules store per-file upper-bound ceilings (`observed <= baseline`, unlisted files default to
//! `0`) so parallel tracks lowering debt never collide or trigger false failures, while
//! `zero_required: true` rules remain strictly pinned at `0` across all files.

#[cfg(test)]
#[path = "arch_debt_ratchet_tests.rs"]
mod arch_debt_ratchet_tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::client_harness::{is_ident_byte, strip_rust_comments_and_literals};
use crate::notify_ratchet::{
    collect_rs_files_recursive, is_test_only_rel_path, strip_cfg_test_items,
};

/// Validates a v2 per-file ratchet baseline map (non-empty prohibited when `zero_required`, and
/// all baseline values must be positive integers).
pub(crate) fn validate_per_file_baseline(
    label: &str,
    zero_required: bool,
    baseline: &BTreeMap<String, usize>,
) -> Result<(), String> {
    if zero_required && !baseline.is_empty() {
        return Err(format!(
            "{label} has zero_required=true with non-empty per-file baseline ({} entries)",
            baseline.len()
        ));
    }
    for (rel_file, &max_count) in baseline {
        if max_count == 0 {
            return Err(format!(
                "{label} baseline entry '{rel_file}' must be a positive integer (omit 0 entries)"
            ));
        }
    }
    Ok(())
}

/// Validates a v2 per-file baseline and enforces `observed[file] <= baseline.get(file).unwrap_or(0)`
/// (or `<= 0` when `zero_required` is true). Decreases and file removals are always permitted.
pub(crate) fn validate_per_file_counts(
    label: &str,
    zero_required: bool,
    baseline: &BTreeMap<String, usize>,
    observed: &BTreeMap<String, usize>,
) -> Result<(), String> {
    validate_per_file_baseline(label, zero_required, baseline)?;
    let mut violations = Vec::new();
    for (rel_file, &obs_count) in observed {
        if obs_count == 0 {
            continue;
        }
        let max_count = if zero_required {
            0
        } else {
            baseline.get(rel_file).copied().unwrap_or(0)
        };
        if obs_count > max_count {
            violations.push(format!("{rel_file}: observed {obs_count} > max {max_count}"));
        }
    }
    if !violations.is_empty() {
        let shown: Vec<&str> = violations.iter().take(8).map(String::as_str).collect();
        let extra = violations.len().saturating_sub(shown.len());
        let suffix = if extra > 0 {
            format!(" (+ {extra} more)")
        } else {
            String::new()
        };
        return Err(format!(
            "{label} per-file ratchet regressed: [{}]{suffix}",
            shown.join(", ")
        ));
    }
    Ok(())
}

/// Single rule specification loaded from `arch_debt_ratchet.json` (version 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchDebtRule {
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
    pub files: BTreeMap<String, usize>,
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
        if parsed.version != 2 {
            return Err(format!(
                "Unsupported arch_debt_ratchet version {} in {} (expected 2)",
                parsed.version,
                path.display()
            ));
        }
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
            validate_per_file_baseline(
                &format!("Architecture debt '{name}'"),
                rule.zero_required,
                &rule.files,
            )?;
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

    /// Enforces all configured v2 rules (`zero_required` and per-file upper-bound ceilings)
    /// against `report`.
    pub fn assert_ratchet(&self, report: &ArchDebtScanReport) -> Result<(), String> {
        if self.version != 2 {
            return Err(format!(
                "Unsupported arch_debt_ratchet version {} (expected 2)",
                self.version
            ));
        }
        for (name, rule) in &self.patterns {
            let obs = report
                .patterns
                .get(name)
                .ok_or_else(|| format!("Missing scan output for rule '{name}'"))?;
            validate_per_file_counts(
                &format!("Architecture debt '{name}'"),
                rule.zero_required,
                &rule.files,
                &obs.per_file,
            )?;
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
                    zero_required: false,
                    owning_issue: None,
                    strip_literals: Some(k.default_strip_literals()),
                    path_include: None,
                    path_exclude_substring: None,
                    regex: k.expected_regex().map(str::to_string),
                    description: None,
                    files: BTreeMap::new(),
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
    ActorStateLocksOutsideSystemActor,
    SystemHandleSyncLockHelpers,
    HandwrittenItemChangeEmitters,
    HandwrittenAccountChangeEmitters,
    SystemTimeNowInGameplay,
    IsWriteLockedByCurrentThread,
    ProcessGlobalMutableState,
    RawBallparkNotifyEmits,
    ExternalSetCombatStateCalls,
    ManualModuleActivationRollback,
    ModuleActiveStates,
    DuplicateRequireUndockedHelpers,
    DuplicateIsNpcCorpHelpers,
    DuplicateIsExcludedCorpMemberHelpers,
    LegacyEffectiveCorpRolesAtFallback,
    HandlerCorpGrantPromotion,
    CorpGrantFoldCallSites,
    EffectiveCorpMemberRolesCallSites,
    CorpRoleTableReads,
    RequestCorpRoleMaskReads,
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
        Self::ActorStateLocksOutsideSystemActor,
        Self::SystemHandleSyncLockHelpers,
        Self::HandwrittenItemChangeEmitters,
        Self::HandwrittenAccountChangeEmitters,
        Self::SystemTimeNowInGameplay,
        Self::IsWriteLockedByCurrentThread,
        Self::ProcessGlobalMutableState,
        Self::RawBallparkNotifyEmits,
        Self::ExternalSetCombatStateCalls,
        Self::ManualModuleActivationRollback,
        Self::ModuleActiveStates,
        Self::DuplicateRequireUndockedHelpers,
        Self::DuplicateIsNpcCorpHelpers,
        Self::DuplicateIsExcludedCorpMemberHelpers,
        Self::LegacyEffectiveCorpRolesAtFallback,
        Self::HandlerCorpGrantPromotion,
        Self::CorpGrantFoldCallSites,
        Self::EffectiveCorpMemberRolesCallSites,
        Self::CorpRoleTableReads,
        Self::RequestCorpRoleMaskReads,
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
            Self::ActorStateLocksOutsideSystemActor => "actor_state_locks_outside_system_actor",
            Self::SystemHandleSyncLockHelpers => "system_handle_sync_lock_helpers",
            Self::HandwrittenItemChangeEmitters => "handwritten_item_change_emitters",
            Self::HandwrittenAccountChangeEmitters => "handwritten_account_change_emitters",
            Self::SystemTimeNowInGameplay => "system_time_now_in_gameplay",
            Self::IsWriteLockedByCurrentThread => "is_write_locked_by_current_thread",
            Self::ProcessGlobalMutableState => "process_global_mutable_state",
            Self::RawBallparkNotifyEmits => "raw_ballpark_notify_emits",
            Self::ExternalSetCombatStateCalls => "external_set_combat_state_calls",
            Self::ManualModuleActivationRollback => "manual_module_activation_rollback",
            Self::ModuleActiveStates => "module_active_states",
            Self::DuplicateRequireUndockedHelpers => "duplicate_require_undocked_helpers",
            Self::DuplicateIsNpcCorpHelpers => "duplicate_is_npc_corp_helpers",
            Self::DuplicateIsExcludedCorpMemberHelpers => {
                "duplicate_is_excluded_corp_member_helpers"
            }
            Self::LegacyEffectiveCorpRolesAtFallback => {
                "legacy_effective_corp_roles_at_fallback"
            }
            Self::HandlerCorpGrantPromotion => "handler_corp_grant_promotion",
            Self::CorpGrantFoldCallSites => "corp_grant_fold_call_sites",
            Self::EffectiveCorpMemberRolesCallSites => {
                "effective_corp_member_roles_call_sites"
            }
            Self::CorpRoleTableReads => "corp_role_table_reads",
            Self::RequestCorpRoleMaskReads => "request_corp_role_mask_reads",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == name)
    }

    fn default_strip_literals(self) -> bool {
        !matches!(
            self,
            Self::SlashCommandPrefixParse | Self::RawBallparkNotifyEmits
        )
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
            Self::ActorStateLocksOutsideSystemActor => {
                Some(r"\bactor_state\s*\.\s*(?:read|write|try_read|try_write|lock)\s*\(")
            }
            Self::SystemHandleSyncLockHelpers => {
                Some(r"\b(?:ballpark|actor_state)\s*\.\s*(?:read|write|try_read|try_write|lock)\s*\(")
            }
            Self::HandwrittenItemChangeEmitters => Some(
                r"\b(?:(?:notify_item_mutation|emit_inv_item_change|emit_inv_item_deleted|emit_item_change_routed|emit_item_deleted_routed|notify_item_record_change|emit_market_item_change|emit_ship_charge_item_diffs|notify_item_change|emit_item_change_with_industry_refresh|notify_item_record_change_with_target|notify_fitted_item_trashed|emit_item_change_with_target|emit_item_changes|emit_item_changes_with_sink|emit_store_item_mutations|emit_mission_item_reward_notification)\s*\(|(?:OnItemChange|OnMultipleItemChange|OnBlueprintsUpdated)\s*\{)",
            ),
            Self::HandwrittenAccountChangeEmitters => Some(
                r"\b(?:(?:notify_wallet_balance|emit_market_account_change|notify_account_change)\s*\(|OnAccountChange\s*\{)",
            ),
            Self::SystemTimeNowInGameplay => Some(r"\bSystemTime\s*::\s*now\s*\("),
            Self::IsWriteLockedByCurrentThread => Some(r"\bis_write_locked_by_current_thread\b"),
            Self::ProcessGlobalMutableState => Some(
                r"\bthread_local\s*!|(?<!')\bstatic\s+(?:mut\b|[^;=]*\b(?:OnceLock|LazyLock|Mutex|RwLock|Atomic[A-Za-z0-9_]*)\b)",
            ),
            Self::RawBallparkNotifyEmits => Some(
                r#"\b(?:notify(?:_in_scene)?(?:_with_source)?|notify_inner|notify_char|notify_remote_message|push_notification|push_char_notification_with_db|emit_notification|emit_registered_to_char|build_pending_registered_notification|build_macho_notification_packet)\s*\([^;{}"]*?"(?:OnDamageStateChange|OnDamageMessage|OnDamageMessages|OnSlimItemChange|OnTarget|OnTargetOBO|OnDroneStateChange|OnSpecialFX)""#,
            ),
            Self::ExternalSetCombatStateCalls => Some(r"\.\s*set_combat_state\s*\("),
            Self::ManualModuleActivationRollback => {
                Some(r"\brollback_post_cycle_activation(?:_snapshot)?\b")
            }
            Self::ModuleActiveStates => Some(r"\bmodule_active_states\b"),
            Self::DuplicateRequireUndockedHelpers => Some(r"\bfn\s+require_undocked_"),
            Self::DuplicateIsNpcCorpHelpers => {
                Some(r"\bfn\s+is_npc_corp(?:oration)?(?:_id)?\s*\(")
            }
            Self::DuplicateIsExcludedCorpMemberHelpers => {
                Some(r"\bfn\s+is_excluded_corp_member\s*\(")
            }
            Self::LegacyEffectiveCorpRolesAtFallback => {
                Some(r"\beffective_corp_roles_at\s*\(")
            }
            Self::HandlerCorpGrantPromotion => {
                Some(r"\bpromote_due_pending_corp_grants\s*\(")
            }
            Self::CorpGrantFoldCallSites => {
                Some(r"\bfold_due_pending_corp_grants_in_snap\s*\(")
            }
            Self::EffectiveCorpMemberRolesCallSites => {
                Some(r"\beffective_corp_member_roles\s*\(")
            }
            Self::CorpRoleTableReads => Some(
                r"\.\s*(?:corp_role_assignments|pending_role_grants|pending_title_role_grants|pending_scoped_role_grants|member_titles)\b",
            ),
            Self::RequestCorpRoleMaskReads => {
                Some(r"\bcorp_role_mask\b|\.\s*bitmask\b")
            }
        }
    }
}

fn count_pattern_in_source(kind: PatternKind, rel_key: &str, text: &str) -> usize {
    match kind {
        PatternKind::MasterWallet => count_whole_word(text, "master_wallet"),
        PatternKind::DirectMemberRolesGet => {
            count_word_dot_method_lparen(text, "member_roles", &[b"get"])
        }
        PatternKind::MemberBaseRolesCalls => {
            crate::arch_debt_ratchet_corp_roles::count_member_base_roles_calls(text)
        }
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
        PatternKind::ActorStateLocksOutsideSystemActor => {
            if rel_key.contains("system_actor") {
                0
            } else {
                count_word_dot_method_lparen(
                    text,
                    "actor_state",
                    &[b"read", b"write", b"try_read", b"try_write", b"lock"],
                )
            }
        }
        PatternKind::SystemHandleSyncLockHelpers => {
            count_word_dot_method_lparen(
                text,
                "ballpark",
                &[b"read", b"write", b"try_read", b"try_write", b"lock"],
            ) + count_word_dot_method_lparen(
                text,
                "actor_state",
                &[b"read", b"write", b"try_read", b"try_write", b"lock"],
            )
        }
        PatternKind::HandwrittenItemChangeEmitters => {
            if rel_key.contains("router/delta_") {
                0
            } else {
                count_handwritten_change_emitters(text, true)
            }
        }
        PatternKind::HandwrittenAccountChangeEmitters => {
            if rel_key.contains("router/delta_") {
                0
            } else {
                count_handwritten_change_emitters(text, false)
            }
        }
        PatternKind::SystemTimeNowInGameplay => count_system_time_now(text),
        PatternKind::IsWriteLockedByCurrentThread => {
            count_whole_word(text, "is_write_locked_by_current_thread")
        }
        PatternKind::ProcessGlobalMutableState => count_process_global_mutable_state(text),
        PatternKind::RawBallparkNotifyEmits => count_raw_ballpark_notify_emits(text),
        PatternKind::ExternalSetCombatStateCalls => {
            count_dot_method_call(text, "set_combat_state")
        }
        PatternKind::ManualModuleActivationRollback => {
            count_whole_word(text, "rollback_post_cycle_activation")
                + count_whole_word(text, "rollback_post_cycle_activation_snapshot")
        }
        PatternKind::ModuleActiveStates => count_whole_word(text, "module_active_states"),
        PatternKind::DuplicateRequireUndockedHelpers => {
            if rel_key.contains("router/session.rs") {
                0
            } else {
                count_fn_require_undocked(text)
            }
        }
        PatternKind::DuplicateIsNpcCorpHelpers => {
            crate::arch_debt_ratchet_corp_roles::count_fn_is_npc_corp(text)
        }
        PatternKind::DuplicateIsExcludedCorpMemberHelpers => {
            crate::arch_debt_ratchet_corp_roles::count_fn_is_excluded_corp_member(text)
        }
        PatternKind::LegacyEffectiveCorpRolesAtFallback => {
            crate::arch_debt_ratchet_corp_roles::count_legacy_effective_corp_roles_at_fallback(
                text,
            )
        }
        PatternKind::HandlerCorpGrantPromotion => {
            count_word_lparen(text, "promote_due_pending_corp_grants")
        }
        PatternKind::CorpGrantFoldCallSites => {
            count_word_lparen(text, "fold_due_pending_corp_grants_in_snap")
        }
        PatternKind::EffectiveCorpMemberRolesCallSites => {
            crate::arch_debt_ratchet_corp_roles::count_effective_corp_member_roles_call_sites(
                text,
            )
        }
        PatternKind::CorpRoleTableReads => {
            crate::arch_debt_ratchet_corp_roles::count_corp_role_table_reads(text)
        }
        PatternKind::RequestCorpRoleMaskReads => {
            crate::arch_debt_ratchet_corp_roles::count_request_corp_role_mask_reads(text)
        }
    }
}

pub(crate) fn count_fn_decl(text: &str, hint: &str, matches_after_ws: impl Fn(&[u8]) -> bool) -> usize {
    if !text.contains(hint) {
        return 0;
    }
    let b = text.as_bytes();
    let mut count = 0usize;
    for (i, _) in text.match_indices("fn") {
        if !has_word_boundary_before(b, i) {
            continue;
        }
        let after_fn = i + 2;
        let ws_end = skip_ws(b, after_fn);
        if ws_end > after_fn && matches_after_ws(&b[ws_end..]) {
            count += 1;
        }
    }
    count
}

pub(crate) fn has_lparen_after_prefix(rest: &[u8], prefix_len: usize) -> bool {
    let lp = skip_ws(rest, prefix_len);
    lp < rest.len() && rest[lp] == b'('
}

fn count_fn_require_undocked(text: &str) -> usize {
    count_fn_decl(text, "require_undocked_", |rest| {
        rest.starts_with(b"require_undocked_")
    })
}

fn count_dot_method_call(text: &str, method: &str) -> usize {
    if !text.contains(method) {
        return 0;
    }
    let b = text.as_bytes();
    let mlen = method.len();
    let mut count = 0usize;
    for (i, _) in text.match_indices(method) {
        let mut dot = i;
        while dot > 0 && b[dot - 1].is_ascii_whitespace() {
            dot -= 1;
        }
        if dot == 0 || b[dot - 1] != b'.' {
            continue;
        }
        let after = skip_ws(b, i + mlen);
        if after < b.len() && b[after] == b'(' {
            count += 1;
        }
    }
    count
}

#[inline]
fn has_word_boundary_before(b: &[u8], idx: usize) -> bool {
    idx == 0 || !is_ident_byte(b[idx - 1])
}

#[inline]
pub(crate) fn has_word_boundary_after(b: &[u8], end: usize) -> bool {
    end >= b.len() || !is_ident_byte(b[end])
}

#[inline]
pub(crate) fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

pub(crate) fn count_whole_word(text: &str, word: &str) -> usize {
    if !text.contains(word) {
        return 0;
    }
    let b = text.as_bytes();
    let wlen = word.len();
    text.match_indices(word)
        .filter(|&(i, _)| has_word_boundary_before(b, i) && has_word_boundary_after(b, i + wlen))
        .count()
}

pub(crate) fn count_word_lparen(text: &str, word: &str) -> usize {
    if !text.contains(word) {
        return 0;
    }
    let b = text.as_bytes();
    let wlen = word.len();
    text.match_indices(word)
        .filter(|&(i, _)| {
            has_word_boundary_before(b, i)
                && has_word_boundary_after(b, i + wlen)
                && {
                    let after = skip_ws(b, i + wlen);
                    after < b.len() && b[after] == b'('
                }
        })
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
    const ITEM_HELPERS: &[&str] = &[
        "notify_item_mutation",
        "emit_inv_item_change",
        "emit_inv_item_deleted",
        "emit_item_change_routed",
        "emit_item_deleted_routed",
        "notify_item_record_change",
        "emit_market_item_change",
        "emit_ship_charge_item_diffs",
        "notify_item_change",
        "emit_item_change_with_industry_refresh",
        "notify_item_record_change_with_target",
        "notify_fitted_item_trashed",
        "emit_item_change_with_target",
        "emit_item_changes",
        "emit_item_changes_with_sink",
        "emit_store_item_mutations",
        "emit_mission_item_reward_notification",
    ];
    const ITEM_STRUCTS: &[&str] = &["OnItemChange", "OnMultipleItemChange", "OnBlueprintsUpdated"];
    const WALLET_HELPERS: &[&str] = &[
        "notify_wallet_balance",
        "emit_market_account_change",
        "notify_account_change",
    ];
    const WALLET_STRUCTS: &[&str] = &["OnAccountChange"];

    if !text.contains("emit_") && !text.contains("notify_") && !text.contains("On") {
        return 0;
    }
    let (helpers, structs) = if item_change {
        (ITEM_HELPERS, ITEM_STRUCTS)
    } else {
        (WALLET_HELPERS, WALLET_STRUCTS)
    };
    let b = text.as_bytes();
    let mut count = 0usize;
    for prefix in ["emit_", "notify_", "On"] {
        if !text.contains(prefix) {
            continue;
        }
        let want_delim = if prefix == "On" { b'{' } else { b'(' };
        let candidates = if prefix == "On" { structs } else { helpers };
        for (i, _) in text.match_indices(prefix) {
            if !has_word_boundary_before(b, i) {
                continue;
            }
            let mut ident_end = i + prefix.len();
            while ident_end < b.len() && is_ident_byte(b[ident_end]) {
                ident_end += 1;
            }
            if let Ok(ident) = std::str::from_utf8(&b[i..ident_end]) {
                if candidates.contains(&ident) {
                    let delim_pos = skip_ws(b, ident_end);
                    if delim_pos < b.len() && b[delim_pos] == want_delim {
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

fn match_mutable_global_type_keyword_end(b: &[u8], idx: usize) -> Option<usize> {
    if !has_word_boundary_before(b, idx) {
        return None;
    }
    const EXACT_KEYWORDS: &[&[u8]] = &[b"OnceLock", b"LazyLock", b"Mutex", b"RwLock"];
    for &kw in EXACT_KEYWORDS {
        let end = idx + kw.len();
        if b[idx..].starts_with(kw) && has_word_boundary_after(b, end) {
            return Some(end);
        }
    }
    const ATOMIC_PREFIX: &[u8] = b"Atomic";
    if b[idx..].starts_with(ATOMIC_PREFIX) {
        let mut end = idx + ATOMIC_PREFIX.len();
        while end < b.len() && is_ident_byte(b[end]) {
            end += 1;
        }
        return Some(end);
    }
    None
}

fn count_process_global_mutable_state(text: &str) -> usize {
    if !text.contains("thread_local") && !text.contains("static") {
        return 0;
    }
    const THREAD_LOCAL: &[u8] = b"thread_local";
    const STATIC: &[u8] = b"static";
    const MUT: &[u8] = b"mut";

    let b = text.as_bytes();
    let mut count = 0usize;
    let mut pos = 0usize;

    while pos < b.len() {
        if b[pos..].starts_with(THREAD_LOCAL) && has_word_boundary_before(b, pos) {
            let bang = skip_ws(b, pos + THREAD_LOCAL.len());
            if bang < b.len() && b[bang] == b'!' {
                count += 1;
                pos = bang + 1;
                continue;
            }
        }

        if b[pos..].starts_with(STATIC)
            && (pos == 0 || b[pos - 1] != b'\'')
            && has_word_boundary_before(b, pos)
        {
            let after_static = pos + STATIC.len();
            let body_start = skip_ws(b, after_static);
            if body_start > after_static {
                if b[body_start..].starts_with(MUT)
                    && has_word_boundary_after(b, body_start + MUT.len())
                {
                    count += 1;
                    pos = body_start + MUT.len();
                    continue;
                }

                let mut limit = body_start;
                while limit < b.len() && b[limit] != b';' && b[limit] != b'=' {
                    limit += 1;
                }
                let mut last_match_end: Option<usize> = None;
                let mut i = body_start;
                while i < limit {
                    if let Some(end) = match_mutable_global_type_keyword_end(b, i) {
                        last_match_end = Some(end);
                        i = end;
                    } else {
                        i += 1;
                    }
                }
                if let Some(match_end) = last_match_end {
                    count += 1;
                    pos = match_end;
                    continue;
                }
            }
        }

        pos += 1;
    }

    count
}

fn count_raw_ballpark_notify_emits(text: &str) -> usize {
    const BALLPARK_EVENTS: &[&[u8]] = &[
        b"OnDamageStateChange",
        b"OnDamageMessage",
        b"OnDamageMessages",
        b"OnSlimItemChange",
        b"OnTarget",
        b"OnTargetOBO",
        b"OnDroneStateChange",
        b"OnSpecialFX",
    ];
    const RAW_NOTIFY_FNS: &[&[u8]] = &[
        b"notify_in_scene_with_source",
        b"notify_with_source",
        b"notify_in_scene",
        b"notify_inner",
        b"notify_char",
        b"notify_remote_message",
        b"notify",
        b"push_notification",
        b"push_char_notification_with_db",
        b"emit_notification",
        b"emit_registered_to_char",
        b"build_pending_registered_notification",
        b"build_macho_notification_packet",
    ];
    if !BALLPARK_EVENTS
        .iter()
        .any(|ev| text.contains(std::str::from_utf8(ev).unwrap_or("")))
    {
        return 0;
    }
    let b = text.as_bytes();
    let n = b.len();
    let mut count = 0usize;
    let mut i = 0usize;
    while i < n {
        if !matches!(b[i], b'n' | b'p' | b'e' | b'b') || !has_word_boundary_before(b, i) {
            i += 1;
            continue;
        }
        let mut matched_end = None;
        for &fn_name in RAW_NOTIFY_FNS {
            if i + fn_name.len() > n || &b[i..i + fn_name.len()] != fn_name {
                continue;
            }
            let mut lp = i + fn_name.len();
            while lp < n && matches!(b[lp], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
                lp += 1;
            }
            if lp >= n || b[lp] != b'(' {
                continue;
            }
            let mut k = lp + 1;
            while k < n {
                match b[k] {
                    b';' | b'{' | b'}' => break,
                    b'"' => {
                        if let Some(&ev) = BALLPARK_EVENTS.iter().find(|&&ev| {
                            k + 1 + ev.len() < n
                                && &b[k + 1..k + 1 + ev.len()] == ev
                                && b[k + 1 + ev.len()] == b'"'
                        }) {
                            matched_end = Some(k + 1 + ev.len() + 1);
                        }
                        break;
                    }
                    _ => k += 1,
                }
            }
            break;
        }
        if let Some(next_i) = matched_end {
            count += 1;
            i = next_i;
        } else {
            i += 1;
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
        // 1. Live repository baseline must pass with v2 per-file baselines and every expected pattern.
        let ratchet = ArchDebtRatchet::load_default().expect("load arch_debt_ratchet.json");
        assert_eq!(ratchet.version, 2, "arch_debt_ratchet.json must be version 2");
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
            "process_global_mutable_state",
            "external_set_combat_state_calls",
            "manual_module_activation_rollback",
            "module_active_states",
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
                    rule.files.is_empty(),
                    "zero_required rule '{name}' must have empty files baseline: {rule:?}"
                );
            } else {
                for (rel_file, &max_count) in &rule.files {
                    assert!(
                        max_count > 0,
                        "rule '{name}' file '{rel_file}' must have positive integer baseline"
                    );
                }
            }
        }

        // 2. Synthetic tempdir workspace verifies comment/cfg(test)/literal filtering, per-file
        //    regression blocking (existing file increase and unlisted file addition), per-file
        //    decrease tolerance without baseline edits, zero_required guard, and 0-entry guard.
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

        let mk_rule = |files: &[(&str, usize)], zero_req| ArchDebtRule {
            zero_required: zero_req,
            owning_issue: Some(1197),
            strip_literals: None,
            path_include: None,
            path_exclude_substring: None,
            regex: None,
            description: None,
            files: files
                .iter()
                .map(|(k, v)| ((*k).to_string(), *v))
                .collect(),
        };
        let mut rules = BTreeMap::new();
        rules.insert("master_wallet".into(), mk_rule(&[], true));
        rules.insert("direct_member_roles_get".into(), mk_rule(&[], true));
        rules.insert(
            "hardcoded_fallback_entity_ids".into(),
            mk_rule(&[("services/sample.rs", 1)], false),
        );
        rules.insert(
            "positional_rpc_args_index".into(),
            mk_rule(&[("services/sample.rs", 1)], false),
        );
        rules.insert(
            "slash_command_prefix_parse".into(),
            mk_rule(&[("services/sample.rs", 1)], false),
        );
        let mut grace_rule = mk_rule(&[("router/binding.rs", 1)], false);
        grace_rule.path_include = Some("router/binding.rs".into());
        rules.insert("router_binding_grace_helpers".into(), grace_rule);
        let mut bp_rule = mk_rule(&[], false);
        bp_rule.path_exclude_substring = Some("system_actor".into());
        rules.insert("ballpark_locks_outside_system_actor".into(), bp_rule);

        let synth_ratchet = ArchDebtRatchet {
            version: 2,
            description: None,
            patterns: rules,
        };
        synth_ratchet
            .verify_dir(&src)
            .expect("synthetic baseline must pass");

        // Adding a site in an unlisted file must fail immediately.
        std::fs::write(
            src.join("services/extra.rs"),
            "pub fn bad(b: &Obj) { let _ = b.ballpark.write(); }\n",
        )
        .expect("write extra.rs");
        let err = synth_ratchet.verify_dir(&src).unwrap_err();
        assert!(err.contains("ballpark_locks_outside_system_actor"), "{err}");
        assert!(err.contains("services/extra.rs"), "{err}");
        std::fs::remove_file(src.join("services/extra.rs")).expect("remove extra.rs");

        // Adding a site to a zero_required rule must fail immediately.
        std::fs::write(
            src.join("services/zero_viol.rs"),
            "pub fn bad() { let _ = master_wallet; }\n",
        )
        .expect("write zero_viol.rs");
        let zero_hit_err = synth_ratchet.verify_dir(&src).unwrap_err();
        assert!(zero_hit_err.contains("master_wallet"), "{zero_hit_err}");
        std::fs::remove_file(src.join("services/zero_viol.rs")).expect("remove zero_viol.rs");

        // Adding a second site in an already-baselined file (1 -> 2 > max 1) must fail, even
        // though total global occurrences could stay flat if another file dropped!
        std::fs::write(
            src.join("services/sample2.rs"),
            "pub fn f() { let _ = Some(1).unwrap_or(30_000_142); }\n",
        )
        .expect("write sample2.rs");
        let per_file_rules = BTreeMap::from([(
            "hardcoded_fallback_entity_ids".into(),
            mk_rule(
                &[("services/sample.rs", 2), ("services/sample2.rs", 1)],
                false,
            ),
        )]);
        let per_file_ratchet = ArchDebtRatchet {
            version: 2,
            description: None,
            patterns: per_file_rules,
        };
        std::fs::write(
            src.join("services/sample2.rs"),
            "pub fn f() { let _ = Some(1).unwrap_or(30_000_142); let _ = Some(2).unwrap_or(30_000_142); }\n",
        )
        .expect("bump sample2.rs");
        let split_err = per_file_ratchet.verify_dir(&src).unwrap_err();
        assert!(
            split_err.contains("services/sample2.rs: observed 2 > max 1"),
            "{split_err}"
        );
        std::fs::remove_file(src.join("services/sample2.rs")).expect("remove sample2.rs");

        // Decreasing counts or deleting all sites in a file passes without modifying baseline.
        std::fs::write(src.join("router/binding.rs"), "pub fn clean() {}\n")
            .expect("clean binding.rs");
        synth_ratchet
            .verify_dir(&src)
            .expect("decreasing a per-file count or dropping a file to 0 must pass without JSON edits");

        // zero_required with non-empty baseline must be rejected.
        let bad_zero = ArchDebtRatchet {
            version: 2,
            description: None,
            patterns: BTreeMap::from([(
                "master_wallet".into(),
                mk_rule(&[("services/sample.rs", 1)], true),
            )]),
        };
        let zero_err = bad_zero.verify_dir(&src).unwrap_err();
        assert!(zero_err.contains("zero_required=true"), "{zero_err}");

        // Explicit 0 entry in baseline must be rejected.
        let zero_entry = ArchDebtRatchet {
            version: 2,
            description: None,
            patterns: BTreeMap::from([(
                "hardcoded_fallback_entity_ids".into(),
                mk_rule(&[("services/sample.rs", 0)], false),
            )]),
        };
        let entry_err = zero_entry.verify_dir(&src).unwrap_err();
        assert!(entry_err.contains("positive integer"), "{entry_err}");

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
                zero_required: false,
                owning_issue: Some(1197),
                strip_literals: Some(false),
                path_include: None,
                path_exclude_substring: None,
                regex: None,
                description: None,
                files: BTreeMap::from([
                    ("services/lit_only.rs".to_string(), 1),
                    ("services/variants.rs".to_string(), 1),
                ]),
            },
        )]);
        let lit_report = scan_arch_debt_in_dir_for_rules(&src, &lit_rules)
            .expect("strip_literals=false must scan unstripped literals on any rule");
        assert_eq!(lit_report.patterns["master_wallet"].occurrences, 2);
        assert_eq!(lit_report.patterns["master_wallet"].files, 2);

        // 2. Unknown JSON fields, mismatched regex strings, v1 versions, and 0 file entries
        //    must fail load_from_path.
        let bad_field_json = tmp.join("bad_field.json");
        std::fs::write(
            &bad_field_json,
            r#"{"version":2,"patterns":{"master_wallet":{"zero_required":true,"regex":"\\bmaster_wallet\\b","files":{},"literal_regex":"dead"}}}"#,
        )
        .expect("write bad_field.json");
        assert!(ArchDebtRatchet::load_from_path(&bad_field_json).is_err());

        let bad_regex_json = tmp.join("bad_regex.json");
        std::fs::write(
            &bad_regex_json,
            r#"{"version":2,"patterns":{"master_wallet":{"zero_required":true,"regex":"\\bwrong\\b","files":{}}}}"#,
        )
        .expect("write bad_regex.json");
        let rx_err = ArchDebtRatchet::load_from_path(&bad_regex_json).unwrap_err();
        assert!(rx_err.contains("regex mismatch"), "{rx_err}");

        let v1_json = tmp.join("v1.json");
        std::fs::write(
            &v1_json,
            r#"{"version":1,"patterns":{"master_wallet":{"zero_required":true,"regex":"\\bmaster_wallet\\b","files":{}}}}"#,
        )
        .expect("write v1.json");
        assert!(ArchDebtRatchet::load_from_path(&v1_json).is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn arch_debt_ratchet_scanners_cover_all_931_item_and_wallet_emitters_and_exclude_router_delta() {
        let tmp = make_temp_dir("931_emitters");
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("router")).expect("create router dir");
        std::fs::create_dir_all(src.join("services")).expect("create services dir");

        std::fs::write(
            src.join("services/item_emitters.rs"),
            r#"
pub fn all_item_helpers_and_structs(ctx: &Ctx) {
    ctx.notify_item_mutation(1);
    ctx.emit_inv_item_change (2);
    emit_inv_item_deleted(3);
    ctx.emit_item_change_routed(4);
    ctx.emit_item_deleted_routed(5);
    ctx.notify_item_record_change(6);
    emit_market_item_change(7);
    emit_ship_charge_item_diffs(8);
    ctx.notify_item_change(9);
    ctx.emit_item_change_with_industry_refresh(10);
    ctx.notify_item_record_change_with_target(11);
    ctx.notify_fitted_item_trashed(12);
    ctx.emit_item_change_with_target(13);
    emit_item_changes(14);
    emit_item_changes_with_sink(15);
    emit_store_item_mutations(16);
    emit_mission_item_reward_notification(17);
    let _a = crate::router::notify_registry::OnItemChange { x: 1 };
    let _b = OnMultipleItemChange
    { x: 2 };
    let _c = OnBlueprintsUpdated { owner_id: 3 };
    // Near-misses and ignored sites:
    let _ignored_comment = "// notify_item_mutation(1) OnItemChange {";
    let _ignored_str = "emit_inv_item_change(1) OnMultipleItemChange {";
    let _ignored_prefix = my_notify_item_mutation(1);
    let _ignored_suffix = notify_item_mutation_extra(1);
    let _ignored_other = emit_beacon_slim_item_change(1);
    let _ignored_const = OnItemChange::EVENT;
    let _ignored_ty: Option<OnItemChange> = None;
}
#[cfg(test)]
mod tests {
    #[test]
    fn ignored() {
        notify_item_mutation(1);
        let _ = OnItemChange { x: 1 };
    }
}
"#,
        )
        .expect("write item_emitters.rs");

        std::fs::write(
            src.join("services/wallet_emitters.rs"),
            r#"
pub fn notify_special_site_account_changes(ctx: &Ctx) {
    ctx.notify_wallet_balance(1);
    emit_market_account_change (2);
    ctx.notify_account_change(3);
    let _d = crate::router::notify_registry::OnAccountChange { balance: 0.0 };
    let _ignored = my_notify_account_change(4);
    let _ignored_suffix = notify_wallet_balance_v2(5);
    let _ignored_const = OnAccountChange::DEFAULT_ID_TYPE;
}
"#,
        )
        .expect("write wallet_emitters.rs");

        // `router/delta_*` must be excluded; legacy `router/cache_notifications.rs` &
        // `router/context.rs` must remain counted; `router/notify_registry.rs` macro arms must not match.
        std::fs::write(
            src.join("router/delta_projection.rs"),
            "pub fn p() { let _ = OnItemChange {}; let _ = OnBlueprintsUpdated {}; let _ = OnAccountChange {}; }\n",
        )
        .expect("write delta_projection.rs");
        std::fs::write(
            src.join("router/delta_commit_listener.rs"),
            "pub fn d(ctx: &Ctx) { ctx.notify_wallet_balance(1); let _ = OnAccountChange {}; }\n",
        )
        .expect("write delta_commit_listener.rs");
        std::fs::write(
            src.join("router/cache_notifications.rs"),
            "pub fn c(ctx: &Ctx) { ctx.notify_item_mutation(1); }\n",
        )
        .expect("write cache_notifications.rs");
        std::fs::write(
            src.join("router/context.rs"),
            "pub fn x(ctx: &Ctx) { ctx.notify_account_change(1); }\n",
        )
        .expect("write context.rs");
        std::fs::write(
            src.join("router/notify_registry.rs"),
            "notify_def!(OnItemChange => \"OnItemChange\" (\"charid\") {}, OnAccountChange => \"OnAccountChange\" (\"charid\") {});\n",
        )
        .expect("write notify_registry.rs");

        let report = scan_arch_debt_in_dir(&src).expect("scan 931 emitters");
        assert_eq!(report.patterns["handwritten_item_change_emitters"].occurrences, 21);
        assert_eq!(report.patterns["handwritten_item_change_emitters"].files, 2);
        assert_eq!(
            report.patterns["handwritten_item_change_emitters"].per_file.get("services/item_emitters.rs"),
            Some(&20)
        );
        assert_eq!(
            report.patterns["handwritten_item_change_emitters"].per_file.get("router/cache_notifications.rs"),
            Some(&1)
        );
        assert!(
            !report.patterns["handwritten_item_change_emitters"].per_file.contains_key("router/delta_projection.rs")
        );

        assert_eq!(report.patterns["handwritten_account_change_emitters"].occurrences, 5);
        assert_eq!(report.patterns["handwritten_account_change_emitters"].files, 2);
        assert_eq!(
            report.patterns["handwritten_account_change_emitters"].per_file.get("services/wallet_emitters.rs"),
            Some(&4)
        );
        assert_eq!(
            report.patterns["handwritten_account_change_emitters"].per_file.get("router/context.rs"),
            Some(&1)
        );
        assert!(
            !report.patterns["handwritten_account_change_emitters"].per_file.contains_key("router/delta_commit_listener.rs")
        );
        assert!(
            !report.patterns["handwritten_account_change_emitters"].per_file.contains_key("router/delta_projection.rs")
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn arch_debt_ratchet_process_global_mutable_state_and_exemptions() {
        let tmp = make_temp_dir("1541_global_state");
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("services")).expect("create services dir");

        std::fs::write(
            src.join("globals.rs"),
            r#"
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64};
use std::sync::{LazyLock, Mutex, Once, OnceLock, RwLock};

thread_local! {
    static TL_CELL: Cell<u32> = const { Cell::new(0) };
}
std::thread_local ! (
    static TL_REF: RefCell<u32> = RefCell::new(0);
);

static mut RAW_GLOBAL: u64 = 0;
pub static MY_ONCE: OnceLock<u64> = OnceLock::new();
pub(crate) static MULTI_LINE_ONCE:
    std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
static MY_LAZY: LazyLock<String> = LazyLock::new(String::new);
static MY_MUTEX: Mutex<u32> = Mutex::new(0);
static MY_RW: RwLock<u32> = RwLock::new(0);
static MY_ATOMIC: AtomicU64 = AtomicU64::new(0);
static MY_ATOMIC_PTR: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());
static NESTED_GLOBAL: LazyLock<Mutex<AtomicBool>> =
    LazyLock::new(|| Mutex::new(AtomicBool::new(false)));

// Near-misses and exempted patterns (must all stay 0):
pub fn lifetime_ref(&self) -> &'static Mutex<u32> {
    todo!()
}
tokio::task_local! {
    static TASK_VAL: u32;
}
static IMMUT_TABLE: &[&str] = &["a", "b"];
static INIT_ONLY: Once = Once::new();
static ONCE_LIKE: CustomOnceLockWrapper = CustomOnceLockWrapper;
static MUT_LIKE: MutWrapper = MutWrapper;
static mut_prefix: MyType = MyType;
pub fn param_name(static_like: Mutex<u32>) {}
const CONST_MUTEX: Mutex<u32> = Mutex::new(0);
static RHS_ONLY: MyWrapper = Mutex::new(0);

pub fn strings_and_comments() {
    // static FAKE_COMMENT: OnceLock<u8> = OnceLock::new(); thread_local! {}
    let _ = "static FAKE_STR: Mutex<u8> = Mutex::new(0); thread_local! {}";
}

#[cfg(test)]
mod tests {
    use super::*;
    static TEST_ONLY_ONCE: OnceLock<u8> = OnceLock::new();
    thread_local! {
        static TEST_TL: Cell<u8> = const { Cell::new(0) };
    }
}
"#,
        )
        .expect("write globals.rs");

        // Test-only files (`*_tests.rs`) must be excluded.
        std::fs::write(
            src.join("services/global_helper_tests.rs"),
            "static TEST_FILE_MUTEX: Mutex<u8> = Mutex::new(0);\n",
        )
        .expect("write test file");

        let report = scan_arch_debt_in_dir(&src).expect("scan 1541 globals");
        let entry = &report.patterns["process_global_mutable_state"];
        assert_eq!(entry.occurrences, 11);
        assert_eq!(entry.files, 1);
        assert_eq!(entry.per_file.get("globals.rs"), Some(&11));
        assert!(!entry.per_file.contains_key("services/global_helper_tests.rs"));

        // Direct boundary / non-overlapping edge-case parity with regex engine.
        assert_eq!(
            count_process_global_mutable_state("static mut A: Mutex<u8> = Mutex::new(0);"),
            1
        );
        assert_eq!(
            count_process_global_mutable_state("static X: MutWrapper = ...; thread_local!{}"),
            1
        );
        assert_eq!(
            count_process_global_mutable_state("let _ = &'static Mutex<u8>;"),
            0
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn arch_debt_ratchet_raw_ballpark_notify_emits_and_exemptions() {
        let tmp = make_temp_dir("1190_raw_ballpark_notify");
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("services")).expect("create services dir");

        std::fs::write(
            src.join("bad.rs"),
            r#"
pub fn emit_raw(ctx: &ServerContext, cid: i64) {
    ctx.notify(NotificationTarget::Character(cid), "OnSpecialFX", "charid", vec![]);
    ctx.notify_in_scene(
        NotificationTarget::Character(cid),
        "OnTarget",
        "charid",
        vec![],
        None,
    );
    notify_char(ctx, cid, "OnDamageMessage", "charid", vec![]);
    build_macho_notification_packet(cid, None, "OnSlimItemChange", "charid", vec![]);
    push_char_notification_with_db(db, pend, tx, cid, "OnTarget", vec![]);
}
"#,
        )
        .expect("write bad.rs");

        std::fs::write(
            src.join("services/good.rs"),
            r#"
pub fn emit_typed_or_non_ballpark(ctx: &ServerContext, cid: i64) {
    // ctx.notify(NotificationTarget::Character(cid), "OnSpecialFX", "charid", vec![]);
    ctx.notify(NotificationTarget::Character(cid), "OnSkillQueueChanged", "charid", vec![]);
    let _ = ("OnTarget", "OnDamageStateChange", "OnDroneStateChange");
    let _ = OnMultiEvent { events: vec![PyRep::String("OnTarget".into())] };
    ctx.emit_typed(NotificationTarget::Character(cid), OnSpecialFX { .. });
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_raw_ok_in_cfg_test() {
        ctx.notify(NotificationTarget::Character(1), "OnSpecialFX", "charid", vec![]);
    }
}
"#,
        )
        .expect("write good.rs");

        std::fs::write(
            src.join("services/unit_tests.rs"),
            r#"pub fn in_test_file() { ctx.notify(t, "OnTarget", "charid", vec![]); }"#,
        )
        .expect("write test file");

        let report = scan_arch_debt_in_dir(&src).expect("scan 1190 raw ballpark");
        let entry = &report.patterns["raw_ballpark_notify_emits"];
        assert_eq!(entry.occurrences, 5);
        assert_eq!(entry.files, 1);
        assert_eq!(entry.per_file.get("bad.rs"), Some(&5));
        assert!(!entry.per_file.contains_key("services/good.rs"));
        assert!(!entry.per_file.contains_key("services/unit_tests.rs"));
        assert!(!PatternKind::RawBallparkNotifyEmits.default_strip_literals());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn arch_debt_ratchet_1486_set_combat_state_rollback_and_module_active_states() {
        let tmp = make_temp_dir("1486_ratchets");
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("services")).expect("create services dir");
        std::fs::write(
            src.join("services/sample.rs"),
            r#"
pub fn f(sys: &Sys) {
    // sys.set_combat_state(1, cs); rollback_post_cycle_activation_snapshot(); module_active_states
    let _lit = "sys.set_combat_state(1, cs) rollback_post_cycle_activation module_active_states";
    sys.set_combat_state(1, cs);
    sys . set_combat_state (2, cs);
    rollback_post_cycle_activation();
    rollback_post_cycle_activation_snapshot();
    let _x = snap.module_active_states.contains(&1);
}
#[cfg(test)]
mod tests {
    fn ignored() {
        sys.set_combat_state(3, cs);
        rollback_post_cycle_activation();
        snap.module_active_states.insert(1);
    }
}
"#,
        )
        .expect("write test file");

        let report = scan_arch_debt_in_dir(&src).expect("scan 1486 ratchets");
        assert_eq!(report.patterns["external_set_combat_state_calls"].occurrences, 2);
        assert_eq!(report.patterns["manual_module_activation_rollback"].occurrences, 2);
        assert_eq!(report.patterns["module_active_states"].occurrences, 1);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn arch_debt_ratchet_1194_duplicate_require_undocked_helpers() {
        let tmp = make_temp_dir("1194_undocked");
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("router")).expect("create router dir");
        std::fs::create_dir_all(src.join("services")).expect("create services dir");
        std::fs::write(
            src.join("router/session.rs"),
            "pub fn require_undocked_space_system() {}\npub fn require_undocked_space_ship() {}\n",
        )
        .expect("write router/session.rs");
        std::fs::write(
            src.join("services/sample.rs"),
            r#"
// fn require_undocked_comment() {}
const S: &str = "fn require_undocked_string() {}";
fn require_undocked_dupe() {}
#[cfg(test)]
mod tests {
    fn require_undocked_test_only() {}
}
"#,
        )
        .expect("write services/sample.rs");

        let report = scan_arch_debt_in_dir(&src).expect("scan 1194 undocked");
        let entry = &report.patterns["duplicate_require_undocked_helpers"];
        assert_eq!(entry.occurrences, 1);
        assert_eq!(entry.per_file.get("services/sample.rs"), Some(&1));
        assert!(!entry.per_file.contains_key("router/session.rs"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
