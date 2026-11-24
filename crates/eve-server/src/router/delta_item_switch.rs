//! Always-on projection switches for character-owned item mutations, deletions,
//! outside-RPC class `"tick"`, fitted slots (`"fitted"`), Track 5B generalized
//! character-owned items & blueprints, and Track 5C permanently-enabled corporation &
//! Upwell structure projection (`#931` Track 3B / 3C-1 / 3C-2 / `"tick"` / Track 5B / Track 5C).
//!
//! Guardrails enforced by this module:
//! 1. **Per-class switches:** `char_same_scene_item_mutation`, `char_same_scene_item_deleted`,
//!    `char_same_scene_item_outside_rpc` (class `"tick"`, marker-free outside-RPC commits),
//!    `char_same_scene_item_fitted` (class `"fitted"`), `char_all_items` (Track 5B generalized
//!    character-owned items & blueprints), and `corp_all_items` (Track 5C corporation & Upwell
//!    structure items & blueprints) are permanently enabled in production (`from_env()` enables
//!    all six; tests may selectively override via [`ProjectedItemSwitchConfig::disabled()`],
//!    `.with_outside_rpc(false)`, `.with_char_same_scene_item_fitted(false)`,
//!    `.with_char_all_items(false)`, or `.with_corp_all_items(false)`). Both
//!    `EVOMU_SWITCH_CHAR_ALL_ITEMS` and `EVOMU_SWITCH_CORP_ALL_ITEMS` are retired.
//! 2. **Same-scene / transition scoping:** Online human character owners (and qualifying corp
//!    members when `corp_all_items` is active) with no scene transition or in-space `active_ship_id`
//!    change on the transition commit itself; subsequent commits in the same dispatch only suppress
//!    the transitioned character's active ship (`change.key == ch.active_ship_id`).
//! 3. **Character & Corp/Structure scope:** Human character owners require `snap.characters` and
//!    absence from player/NPC corporation tables; `corp_all_items` generalizes to materialized player
//!    corporations (`snap.corporations`, all flags) and NPC corp hangar/locked/blueprint items.
//! 4. **One single-row fact per `(pilot, item, is_corp_target)`:** Inside a dispatch, kept until the
//!    dispatch ends; outside a dispatch, kept until the next commit touching the item or until the
//!    pilot goes offline. Both `"charid"` and `"corpid"` scopes stay isolated on cross-owner transfers.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use eve_db::{
    ChangeSet, CharacterRecord, InventoryFlag, InventoryItemRecord, RowChange, WorldStateSnapshot,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;

use super::cache_notifications::register_dispatch_switch_handle;
use super::delta_item_compare::{CompareOriginScope, TxItemEffectKeys};
use super::delta_item_corp::{
    collect_switched_item_recipients_for_change, is_corp_blueprint_owner_eligible,
    resolve_online_non_transitioned_corp_recipients,
};
use super::delta_item_outside_rpc::{
    collect_touched_monitor_types, invalidate_covered_monitors_for_change,
    invalidate_covered_monitors_for_types, merge_covered_industry_monitors,
    outside_rpc_method_label, retain_monitors_excluding_chars, OutsideRpcLedgerState,
};
use super::delta_item_switch_report::SwitchTelemetryState;
use super::delta_projection::{
    build_projected_item_change_args, compute_item_change_entries, is_blueprint_type,
    OnlineCharacterLookup,
};
use super::ServerContext;

pub(crate) use super::delta_item_corp::{
    build_switched_item_commit_notifications, SwitchedCommitBatch,
    SwitchedProjectedBlueprintSpec, SwitchedProjectedItemSpec,
};
pub use super::delta_item_outside_rpc::SwitchScopeOutcomeCounts;
pub(crate) use super::delta_item_outside_rpc::{
    classify_hand_written_not_covered_reason_in_snap,
    classify_hand_written_not_covered_reason_with_fitted_in_snap,
    evaluate_hand_written_covered_class_in_snap, evaluate_hand_written_first_set_and_next_in_snap,
    evaluate_hand_written_outside_first_set_in_snap,
};
#[cfg(test)]
pub(crate) use super::delta_item_outside_rpc::evaluate_hand_written_first_set_in_snap;
pub use super::delta_item_switch_report::{
    AccountSwitchCallSiteCounts, AccountSwitchSummary, ProjectedItemSwitchSummary,
    SwitchCallSiteCounts, SwitchTelemetryCounts, WouldQualifyClassCounts,
};
pub(crate) use super::delta_item_switch_report::{
    format_item_compare_markdown_report, NotCoveredReason, WouldQualifyFlags,
};

const MAX_CONTAINER_WALK_DEPTH: usize = 8;

#[inline]
const fn default_true() -> bool {
    true
}

/// Explicit per-class projection toggles stored on [`ServerContext`] (`#931` Track 3B / 3C-1 / `"tick"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectedItemSwitchConfig {
    /// Projects character-owned first-set item creations/updates (`OnItemChange`) and paired
    /// companions (`OnIndustryMaterials`, `OnMultipleItemChange`) from commit deltas instead of
    /// hand-written `OnItemChange` emitters. Default: `true`.
    pub char_same_scene_item_mutation: bool,
    /// Projects character-owned first-set item deletions (`OnItemChange` tombstone) and paired
    /// companions (`OnIndustryMaterials`, `OnMultipleItemChange`) from commit deltas instead of
    /// hand-written deletion emitters. Default: `true`.
    pub char_same_scene_item_deleted: bool,
    /// Projects character-owned first-set item creations/updates/deletions occurring outside an RPC
    /// dispatch (marker-free class `"tick"`, `dispatch_scope_info.is_none()`). Permanently
    /// enabled in production (`EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC` is retired).
    #[serde(default = "default_true")]
    pub char_same_scene_item_outside_rpc: bool,
    /// Projects character-owned active-ship fitted slot and charge item changes (`#931` Class
    /// `"fitted"`, `evaluate_fitted_coords_pair`), covering in-dispatch commits always and
    /// outside-RPC commits when [`Self::char_same_scene_item_outside_rpc`] is also `true`.
    /// Permanently enabled in production (`EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED` is retired).
    #[serde(default = "default_true")]
    pub char_same_scene_item_fitted: bool,
    /// Wave F1 `#931` Track 5B switch generalizing character-owned `OnItemChange` /
    /// `OnMultipleItemChange` projection across all locations/flags and cross-owner transfers,
    /// plus character-owned `OnBlueprintsUpdated` projection (permanently enabled in production;
    /// `EVOMU_SWITCH_CHAR_ALL_ITEMS` is retired; `.with_char_all_items(false)` is test-only).
    #[serde(default = "default_true")]
    pub char_all_items: bool,
    /// Wave F1 `#931` Track 5C switch generalizing corporation & Upwell structure
    /// `OnItemChange` / `OnMultipleItemChange` (`id_type = "corpid"`), corporation
    /// `OnBlueprintsUpdated(corp_id)` (`id_type = "ownerID"`), and role-gated corporation
    /// `OnIndustryMaterials` projection (permanently enabled in production;
    /// `EVOMU_SWITCH_CORP_ALL_ITEMS` is retired; `.with_corp_all_items(false)` is test-only).
    #[serde(default = "default_true")]
    pub corp_all_items: bool,
}

impl Default for ProjectedItemSwitchConfig {
    #[inline]
    fn default() -> Self {
        Self::enabled()
    }
}

impl ProjectedItemSwitchConfig {
    /// Returns the production-default item projection switches (all six switches enabled).
    #[inline]
    pub const fn enabled() -> Self {
        Self {
            char_same_scene_item_mutation: true,
            char_same_scene_item_deleted: true,
            char_same_scene_item_outside_rpc: true,
            char_same_scene_item_fitted: true,
            char_all_items: true,
            corp_all_items: true,
        }
    }

    /// Returns all item projection switches (including `char_all_items` and `corp_all_items`) enabled.
    #[inline]
    pub const fn all_enabled() -> Self {
        Self {
            char_same_scene_item_mutation: true,
            char_same_scene_item_deleted: true,
            char_same_scene_item_outside_rpc: true,
            char_same_scene_item_fitted: true,
            char_all_items: true,
            corp_all_items: true,
        }
    }

    /// Returns all item projection switches disabled (tests only: compare testing of remaining
    /// legacy call sites).
    #[inline]
    pub const fn disabled() -> Self {
        Self {
            char_same_scene_item_mutation: false,
            char_same_scene_item_deleted: false,
            char_same_scene_item_outside_rpc: false,
            char_same_scene_item_fitted: false,
            char_all_items: false,
            corp_all_items: false,
        }
    }

    /// Returns a copy with `char_same_scene_item_outside_rpc` set to `enabled`.
    #[inline]
    pub const fn with_outside_rpc(mut self, enabled: bool) -> Self {
        self.char_same_scene_item_outside_rpc = enabled;
        self
    }

    /// Returns a copy with `char_same_scene_item_fitted` set to `enabled`.
    #[inline]
    pub const fn with_char_same_scene_item_fitted(mut self, enabled: bool) -> Self {
        self.char_same_scene_item_fitted = enabled;
        self
    }

    /// Returns a copy with `char_all_items` set to `enabled` (test overrides only).
    #[inline]
    pub const fn with_char_all_items(mut self, enabled: bool) -> Self {
        self.char_all_items = enabled;
        self
    }

    /// Returns a copy with `corp_all_items` set to `enabled`.
    #[inline]
    pub const fn with_corp_all_items(mut self, enabled: bool) -> Self {
        self.corp_all_items = enabled;
        self
    }

    /// Parses a boolean switch env value using `default` when `raw` is `None`, empty, or
    /// unrecognized (`1`/`true`/`yes`/`on` enable, `0`/`false`/`no`/`off` disable).
    #[inline]
    pub fn parse_env_flag_default(raw: Option<&str>, default: bool) -> bool {
        let Some(val) = raw else {
            return default;
        };
        let trimmed = val.trim();
        if trimmed == "1"
            || trimmed.eq_ignore_ascii_case("true")
            || trimmed.eq_ignore_ascii_case("yes")
            || trimmed.eq_ignore_ascii_case("on")
        {
            true
        } else if trimmed == "0"
            || trimmed.eq_ignore_ascii_case("false")
            || trimmed.eq_ignore_ascii_case("no")
            || trimmed.eq_ignore_ascii_case("off")
        {
            false
        } else {
            default
        }
    }

    /// Parses an opt-in switch env value (`1`, `true`, `yes`, `on` enable; unset/other values are `false`).
    #[inline]
    pub fn parse_env_flag(raw: Option<&str>) -> bool {
        Self::parse_env_flag_default(raw, false)
    }

    /// Returns any retired item projection kill-switch env var names set to an off value.
    pub(crate) fn retired_off_env_vars_lookup<F: Fn(&str) -> Option<String>>(
        get: F,
    ) -> Vec<&'static str> {
        let mut ignored = Vec::new();
        for key in [
            "EVOMU_PROJECTED_ITEM_MUTATION",
            "EVOMU_PROJECTED_ITEM_DELETED",
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC",
            "EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED",
            "EVOMU_SWITCH_CHAR_ALL_ITEMS",
            "EVOMU_SWITCH_CORP_ALL_ITEMS",
        ] {
            if let Some(val) = get(key) {
                if !Self::parse_env_flag_default(Some(&val), true) {
                    ignored.push(key);
                }
            }
        }
        ignored
    }

    #[cfg(test)]
    pub(crate) fn from_env_lookup<F: Fn(&str) -> Option<String>>(get: F) -> Self {
        let _ = Self::retired_off_env_vars_lookup(&get);
        Self::enabled()
    }

    /// Returns startup switch configuration with all production item projection classes permanently
    /// enabled (`Self::enabled()`).
    ///
    /// `EVOMU_PROJECTED_ITEM_MUTATION`, `EVOMU_PROJECTED_ITEM_DELETED`,
    /// `EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_OUTSIDE_RPC`,
    /// `EVOMU_SWITCH_CHAR_SAME_SCENE_ITEM_FITTED`, `EVOMU_SWITCH_CHAR_ALL_ITEMS`, and
    /// `EVOMU_SWITCH_CORP_ALL_ITEMS` are retired; if any is set to an off value
    /// (`0`/`false`/`no`/`off`), one startup warning is logged saying it is ignored because item
    /// projection is permanently enabled.
    pub fn from_env() -> Self {
        let ignored = Self::retired_off_env_vars_lookup(|key| std::env::var(key).ok());
        if !ignored.is_empty() {
            tracing::warn!(
                vars = %ignored.join(", "),
                "Ignoring retired item projection kill-switch env var(s) ({}); item projection is permanently enabled",
                ignored.join(", ")
            );
        }
        Self::enabled()
    }

    /// Returns `true` when at least one projection switch is enabled.
    #[inline]
    pub const fn is_any_enabled(self) -> bool {
        self.char_same_scene_item_mutation
            || self.char_same_scene_item_deleted
            || self.char_same_scene_item_outside_rpc
            || self.char_same_scene_item_fitted
            || self.char_all_items
            || self.corp_all_items
    }

    /// Returns `true` when the legacy per-kind switch flag is enabled (`mutation` or `deleted`).
    #[inline]
    pub(crate) const fn is_kind_enabled(self, kind: ProjectedItemOpKind) -> bool {
        match kind {
            ProjectedItemOpKind::CreatedOrUpdated => self.char_same_scene_item_mutation,
            ProjectedItemOpKind::Deleted => self.char_same_scene_item_deleted,
        }
    }

    /// Returns `true` when active projection is enabled for `(in_dispatch, kind)`.
    #[inline]
    pub(crate) const fn is_kind_enabled_in_scope(
        self,
        in_dispatch: bool,
        kind: ProjectedItemOpKind,
    ) -> bool {
        if in_dispatch {
            self.is_kind_enabled(kind)
        } else {
            self.char_same_scene_item_outside_rpc
        }
    }

    /// Returns `true` when Class `"fitted"` projection is active for `in_dispatch` (in-dispatch
    /// requires `char_same_scene_item_fitted`; outside-RPC requires both
    /// `char_same_scene_item_fitted` and `char_same_scene_item_outside_rpc`).
    #[inline]
    pub(crate) const fn is_fitted_enabled_in_scope(self, in_dispatch: bool) -> bool {
        self.char_same_scene_item_fitted && (in_dispatch || self.char_same_scene_item_outside_rpc)
    }

    /// Returns `true` when Track 5B generalized `char_all_items` projection is active for
    /// `in_dispatch` (in-dispatch requires `char_all_items`; outside-RPC requires both
    /// `char_all_items` and `char_same_scene_item_outside_rpc`).
    #[inline]
    pub(crate) const fn is_char_all_items_enabled_in_scope(self, in_dispatch: bool) -> bool {
        self.char_all_items && (in_dispatch || self.char_same_scene_item_outside_rpc)
    }

    /// Returns `true` when Track 5C `corp_all_items` projection is active for `in_dispatch`
    /// (in-dispatch requires `corp_all_items`; outside-RPC requires both `corp_all_items` and
    /// `char_same_scene_item_outside_rpc`).
    #[inline]
    pub(crate) const fn is_corp_all_items_enabled_in_scope(self, in_dispatch: bool) -> bool {
        self.corp_all_items && (in_dispatch || self.char_same_scene_item_outside_rpc)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectedItemOpKind {
    CreatedOrUpdated,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ItemWireFingerprint {
    pub type_id: u32,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i64,
    pub wire_qty: i64,
}

fn wire_row_col_i64(row: &PyRep, col: &str) -> Option<i64> {
    match row {
        PyRep::DBRow { columns, .. } => columns
            .iter()
            .find(|(name, _)| name == col)
            .and_then(|(_, v)| v.as_i64()),
        PyRep::KeyVal(map) => map.get(col).and_then(PyRep::as_i64),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) | PyRep::Token(s) if s == col => v.as_i64(),
            _ => None,
        }),
        _ => None,
    }
}

impl ItemWireFingerprint {
    #[inline]
    pub(crate) fn from_record(item: &InventoryItemRecord) -> Self {
        let wire_qty = if item.is_blueprint_copy {
            -2
        } else if item.singleton {
            -1
        } else {
            item.quantity as i64
        };
        Self {
            type_id: item.type_id,
            owner_id: item.owner_id,
            location_id: item.location_id,
            flag_id: item.flag as i64,
            wire_qty,
        }
    }

    pub(crate) fn from_pyrep_row(row: &PyRep) -> Option<(i64, Self)> {
        let item_id = wire_row_col_i64(row, "itemID")?;
        let type_id = u32::try_from(wire_row_col_i64(row, "typeID")?).ok()?;
        let owner_id = wire_row_col_i64(row, "ownerID")?;
        let location_id = wire_row_col_i64(row, "locationID")?;
        let flag_id = wire_row_col_i64(row, "flagID")?;
        let wire_qty = wire_row_col_i64(row, "quantity")?;
        Some((
            item_id,
            Self {
                type_id,
                owner_id,
                location_id,
                flag_id,
                wire_qty,
            },
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub(crate) enum CanonicalDictKey {
    ColIx(i64),
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum CanonicalDictVal {
    Int(i64),
    Bool(bool),
    Str(String),
    None,
    Other(String),
}

/// Key-order-independent canonical representation of `OnItemChange`'s 2nd-arg change dictionary.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CanonicalChangeDict(pub(crate) BTreeMap<CanonicalDictKey, CanonicalDictVal>);

impl CanonicalChangeDict {
    fn key_from_pyrep(rep: &PyRep) -> CanonicalDictKey {
        if let Some(ix) = rep.as_i64() {
            CanonicalDictKey::ColIx(ix)
        } else {
            CanonicalDictKey::Other(format!("{rep:?}"))
        }
    }

    fn val_from_pyrep(rep: &PyRep) -> CanonicalDictVal {
        match rep {
            PyRep::Bool(b) => CanonicalDictVal::Bool(*b),
            PyRep::Integer(i) => CanonicalDictVal::Int(*i),
            PyRep::String(s) | PyRep::Token(s) => CanonicalDictVal::Str(s.clone()),
            PyRep::None => CanonicalDictVal::None,
            other => match other.as_i64() {
                Some(i) => CanonicalDictVal::Int(i),
                None => CanonicalDictVal::Other(format!("{other:?}")),
            },
        }
    }

    pub(crate) fn from_pyrep(rep: &PyRep) -> Option<Self> {
        let PyRep::Dict(entries) = rep else {
            return None;
        };
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            map.insert(Self::key_from_pyrep(k), Self::val_from_pyrep(v));
        }
        Some(Self(map))
    }

    pub(crate) fn from_entries(entries: &[(i32, PyRep)]) -> Self {
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            map.insert(CanonicalDictKey::ColIx(i64::from(*k)), Self::val_from_pyrep(v));
        }
        Self(map)
    }

    pub(crate) fn col_i64(&self, col_ix: i64) -> Option<i64> {
        match self.0.get(&CanonicalDictKey::ColIx(col_ix))? {
            CanonicalDictVal::Int(i) => Some(*i),
            CanonicalDictVal::Bool(b) => Some(i64::from(*b)),
            _ => None,
        }
    }

    pub(crate) fn format_compact(&self) -> String {
        let mut out = String::from("{");
        for (idx, (k, v)) in self.0.iter().enumerate() {
            if idx > 0 {
                out.push(',');
            }
            match k {
                CanonicalDictKey::ColIx(i) => out.push_str(&i.to_string()),
                CanonicalDictKey::Other(s) => out.push_str(s),
            }
            out.push(':');
            match v {
                CanonicalDictVal::Int(i) => out.push_str(&i.to_string()),
                CanonicalDictVal::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                CanonicalDictVal::Str(s) => {
                    out.push('"');
                    out.push_str(s);
                    out.push('"');
                }
                CanonicalDictVal::None => out.push_str("None"),
                CanonicalDictVal::Other(s) => out.push_str(s),
            }
        }
        out.push('}');
        out
    }
}

#[inline]
#[track_caller]
fn caller_call_site() -> String {
    let loc = std::panic::Location::caller();
    format!("{}:{}", loc.file().replace('\\', "/"), loc.line())
}

#[derive(Debug, Clone)]
pub(super) struct ProjectedDedupeEntry {
    pub(super) kind: ProjectedItemOpKind,
    pub(super) fingerprint: ItemWireFingerprint,
    pub(super) change_dict: Option<CanonicalChangeDict>,
    pub(super) class_key: String,
    pub(super) rpc_method: String,
    /// `true` once at least one matching hand-written call has claimed this commit fact as
    /// `replaced`, preventing `sw.new` from double-counting on expiry or overwrite while keeping
    /// repeated `set + equal` calls suppressed.
    pub(super) replaced_claimed: bool,
}

impl ProjectedDedupeEntry {
    #[inline]
    pub(super) fn from_commit_token(tok: CommitProjectedToken) -> Self {
        Self {
            kind: tok.kind,
            fingerprint: tok.fingerprint,
            change_dict: tok.change_dict,
            class_key: tok.class_key,
            rpc_method: tok.rpc_method,
            replaced_claimed: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DedupeLookupOutcome {
    /// Fact is set for `(char_id, item_id)` with matching row -> suppress (`replaced`).
    Replaced,
    /// Fact is set for `(char_id, item_id)` with differing row -> suppress (`superseded`).
    Superseded,
    /// Fact is not set for `(char_id, item_id)` -> send normally (`no projected event` if first-set).
    NotSet,
}

#[derive(Debug, Default)]
struct DispatchTokenBucket {
    /// Single fact per `(char_id, item_id, is_corp_target)` in this dispatch: "the last commit that
    /// touched this item was announced inside this dispatch for this scope, with this row."
    facts: HashMap<(i64, i64, bool), ProjectedDedupeEntry>,
    /// `(char_id, owner_id)` blueprint-update dedupe facts announced inside this dispatch.
    blueprint_facts: HashSet<(i64, i64)>,
    transitioned_chars: HashSet<i64>,
    /// Characters transitioned in the most recent item-mutating or transition commit inside this
    /// dispatch (`#931` `P2-03`), distinguishing Commit 1 (`PilotMovedInThisCommit` for all items)
    /// from subsequent commits (where only `ch.active_ship_id` and items actually touched in a
    /// transition commit remain suppressed).
    latest_commit_transitioned_chars: HashSet<i64>,
    /// `(char_id, item_id)` items touched in a commit where `char_id` transitioned (`#931` R2 P2 #2),
    /// kept across non-transition commits in the same dispatch until another commit touches `item_id`
    /// so deferred Commit 1 emissions continue recording `PilotMovedInThisCommit`.
    transition_suppressed_items: HashSet<(i64, i64)>,
    /// `monitor_id -> (char_id, owner_id, set of type_ids)` already covered by coalesced commit-time
    /// `OnIndustryMaterials` emissions in this dispatch scope (`#931` `BF-931-3C1-02` / `BF-01`).
    covered_industry_monitors: HashMap<i64, (i64, i64, HashSet<u32>)>,
    /// `(char_id, item_id)` pairs skipped from commit projection due to explicit tx effects.
    explicit_effect_items: HashSet<(i64, i64)>,
    /// `item_id`s skipped from commit projection due to active fuel rollback reserves.
    fuel_reserved_items: HashSet<i64>,
}

#[derive(Debug, Default)]
struct ProjectedItemDedupeLedger {
    dispatch_buckets: HashMap<u64, DispatchTokenBucket>,
    outside: OutsideRpcLedgerState,
    telemetry: SwitchTelemetryState,
    account_telemetry: AccountSwitchSummary,
}

pub(crate) struct CommitProjectedToken {
    pub char_id: i64,
    pub item_id: i64,
    pub is_corp_target: bool,
    pub kind: ProjectedItemOpKind,
    pub fingerprint: ItemWireFingerprint,
    pub change_dict: Option<CanonicalChangeDict>,
    pub class_key: String,
    pub rpc_method: String,
}

#[inline]
pub(super) fn entry_matches_wire_fp(
    entry: &ProjectedDedupeEntry,
    kind: ProjectedItemOpKind,
    fp: ItemWireFingerprint,
    tombstone_wildcard_deleted: bool,
) -> bool {
    entry.kind == kind
        && (entry.fingerprint == fp
            || (tombstone_wildcard_deleted
                && kind == ProjectedItemOpKind::Deleted
                && fp.location_id == 6
                && entry.fingerprint.owner_id == fp.owner_id
                && entry.fingerprint.type_id == fp.type_id))
}

pub(super) fn remove_item_from_facts(
    telemetry: Option<&mut SwitchTelemetryState>,
    facts: &mut HashMap<(i64, i64, bool), ProjectedDedupeEntry>,
    item_id: i64,
) {
    if facts.is_empty() {
        return;
    }
    let stale_keys: Vec<(i64, i64, bool)> = facts
        .keys()
        .copied()
        .filter(|&(_, eid, _)| eid == item_id)
        .collect();
    if let Some(telem) = telemetry {
        for key in stale_keys {
            if let Some(old_entry) = facts.remove(&key) {
                if !old_entry.replaced_claimed {
                    telem.record_new(key.1, &old_entry);
                }
            }
        }
    } else {
        for key in stale_keys {
            facts.remove(&key);
        }
    }
}

pub(super) fn remove_items_from_facts(
    telemetry: Option<&mut SwitchTelemetryState>,
    facts: &mut HashMap<(i64, i64, bool), ProjectedDedupeEntry>,
    item_ids: &HashSet<i64>,
) {
    if facts.is_empty() || item_ids.is_empty() {
        return;
    }
    let stale_keys: Vec<(i64, i64, bool)> = facts
        .keys()
        .copied()
        .filter(|&(_, eid, _)| item_ids.contains(&eid))
        .collect();
    if let Some(telem) = telemetry {
        for key in stale_keys {
            if let Some(old_entry) = facts.remove(&key) {
                if !old_entry.replaced_claimed {
                    telem.record_new(key.1, &old_entry);
                }
            }
        }
    } else {
        for key in stale_keys {
            facts.remove(&key);
        }
    }
}

impl ProjectedItemDedupeLedger {
    fn invalidate_item_everywhere(&mut self, change: &RowChange<i64, InventoryItemRecord>) {
        let item_id = change.key;
        for bucket in self.dispatch_buckets.values_mut() {
            remove_item_from_facts(Some(&mut self.telemetry), &mut bucket.facts, item_id);
            if !bucket.transition_suppressed_items.is_empty() {
                bucket
                    .transition_suppressed_items
                    .retain(|&(_, eid)| eid != item_id);
            }
            invalidate_covered_monitors_for_change(&mut bucket.covered_industry_monitors, change);
        }
        self.outside.invalidate_item(change, &mut self.telemetry);
    }

    fn invalidate_changes_everywhere(
        &mut self,
        changes: &ChangeSet,
        invalidated_bp_owners: &HashSet<i64>,
    ) {
        if changes.items.len() <= 1 {
            for change in &changes.items {
                self.invalidate_item_everywhere(change);
            }
        } else {
            let has_any_dispatch_facts = self
                .dispatch_buckets
                .values()
                .any(|b| !b.facts.is_empty() || !b.transition_suppressed_items.is_empty());
            let has_any_dispatch_monitors = self
                .dispatch_buckets
                .values()
                .any(|b| !b.covered_industry_monitors.is_empty());
            let has_any_outside_facts = !self.outside.live.facts.is_empty()
                || !self.outside.shadow.facts.is_empty()
                || !self.outside.live.explicit_effect_items.is_empty()
                || !self.outside.shadow.explicit_effect_items.is_empty()
                || !self.outside.live.fuel_reserved_items.is_empty()
                || !self.outside.shadow.fuel_reserved_items.is_empty();
            if has_any_dispatch_facts || has_any_outside_facts {
                let item_ids: HashSet<i64> = changes.items.iter().map(|c| c.key).collect();
                for bucket in self.dispatch_buckets.values_mut() {
                    remove_items_from_facts(
                        Some(&mut self.telemetry),
                        &mut bucket.facts,
                        &item_ids,
                    );
                    if !bucket.transition_suppressed_items.is_empty() {
                        bucket
                            .transition_suppressed_items
                            .retain(|&(_, eid)| !item_ids.contains(&eid));
                    }
                }
                self.outside
                    .invalidate_items_batch(&item_ids, &mut self.telemetry);
            }
            if has_any_dispatch_monitors || !self.outside.covered_industry_monitors.is_empty() {
                let touched_types = collect_touched_monitor_types(changes);
                if has_any_dispatch_monitors {
                    for bucket in self.dispatch_buckets.values_mut() {
                        invalidate_covered_monitors_for_types(
                            &mut bucket.covered_industry_monitors,
                            &touched_types,
                        );
                    }
                }
                invalidate_covered_monitors_for_types(
                    &mut self.outside.covered_industry_monitors,
                    &touched_types,
                );
            }
        }
        if !invalidated_bp_owners.is_empty() {
            for bucket in self.dispatch_buckets.values_mut() {
                if !bucket.blueprint_facts.is_empty() {
                    bucket
                        .blueprint_facts
                        .retain(|&(_cid, oid)| !invalidated_bp_owners.contains(&oid));
                }
            }
            self.outside.invalidate_blueprints(invalidated_bp_owners);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_commit_updates(
        &mut self,
        scope_id: Option<u64>,
        origin_scope: CompareOriginScope,
        outside_rpc_enabled: bool,
        changes: &ChangeSet,
        transitioned_chars: &[i64],
        tokens: Vec<CommitProjectedToken>,
        invalidated_bp_owners: &HashSet<i64>,
        blueprint_tokens: &[(i64, i64)],
        explicit_effect_items: &[(i64, i64)],
        fuel_reserved_items: &[i64],
    ) {
        self.invalidate_changes_everywhere(changes, invalidated_bp_owners);
        if let Some(sid) = scope_id {
            if !self.dispatch_buckets.contains_key(&sid)
                && transitioned_chars.is_empty()
                && tokens.is_empty()
                && blueprint_tokens.is_empty()
                && explicit_effect_items.is_empty()
                && fuel_reserved_items.is_empty()
            {
                return;
            }
            let bucket = self.dispatch_buckets.entry(sid).or_default();
            if !changes.items.is_empty() {
                bucket.latest_commit_transitioned_chars.clear();
            }
            for &cid in transitioned_chars {
                bucket.transitioned_chars.insert(cid);
                bucket.latest_commit_transitioned_chars.insert(cid);
                for ch in &changes.items {
                    bucket.transition_suppressed_items.insert((cid, ch.key));
                }
            }
            for &key in explicit_effect_items {
                bucket.explicit_effect_items.insert(key);
            }
            for &item_id in fuel_reserved_items {
                bucket.fuel_reserved_items.insert(item_id);
            }
            for tok in tokens {
                let key = (tok.char_id, tok.item_id, tok.is_corp_target);
                let entry = ProjectedDedupeEntry::from_commit_token(tok);
                self.telemetry.record_emitted(
                    CompareOriginScope::Dispatch,
                    true,
                    &entry.class_key,
                    &entry.rpc_method,
                );
                if let Some(old) = bucket.facts.insert(key, entry) {
                    if !old.replaced_claimed {
                        self.telemetry.record_new(key.1, &old);
                    }
                }
            }
            retain_monitors_excluding_chars(&mut bucket.covered_industry_monitors, transitioned_chars);
            if !transitioned_chars.is_empty() && !bucket.blueprint_facts.is_empty() {
                bucket
                    .blueprint_facts
                    .retain(|&(cid, _)| !transitioned_chars.contains(&cid));
            }
            for &bp_key in blueprint_tokens {
                bucket.blueprint_facts.insert(bp_key);
            }
            return;
        }

        self.outside.invalidate_transitioned_chars(transitioned_chars);
        self.outside.apply_commit(
            origin_scope,
            outside_rpc_enabled,
            tokens,
            blueprint_tokens,
            explicit_effect_items,
            fuel_reserved_items,
            &mut self.telemetry,
        );
    }

    fn record_covered_industry_monitors(
        &mut self,
        scope_id: Option<u64>,
        outside_rpc_enabled: bool,
        covered: &HashMap<i64, (i64, i64, Vec<u32>)>,
    ) {
        if covered.is_empty() {
            return;
        }
        if let Some(sid) = scope_id {
            let bucket = self.dispatch_buckets.entry(sid).or_default();
            merge_covered_industry_monitors(&mut bucket.covered_industry_monitors, covered);
        } else if outside_rpc_enabled {
            self.outside.record_covered_industry_monitors(covered);
        }
    }

    fn is_industry_monitor_covered(
        &self,
        scope_id: Option<u64>,
        outside_rpc_enabled: bool,
        monitor_id: i64,
        tids: &[u32],
    ) -> bool {
        if tids.is_empty() {
            return false;
        }
        if let Some(sid) = scope_id {
            return self.dispatch_buckets.get(&sid).is_some_and(|bucket| {
                bucket
                    .covered_industry_monitors
                    .get(&monitor_id)
                    .is_some_and(|(_, _, covered)| tids.iter().all(|tid| covered.contains(tid)))
            });
        }
        outside_rpc_enabled && self.outside.is_industry_monitor_covered(monitor_id, tids)
    }

    fn expire_dispatch_scope(&mut self, scope_id: u64) {
        let Some(bucket) = self.dispatch_buckets.remove(&scope_id) else {
            return;
        };
        for ((_, item_id, _), entry) in bucket.facts {
            if !entry.replaced_claimed {
                self.telemetry.record_new(item_id, &entry);
            }
        }
    }

    fn clear_outside_dispatch_chars(&mut self, offline_chars: &[i64]) {
        self.outside
            .clear_offline_chars(offline_chars, &mut self.telemetry);
    }

    #[allow(clippy::too_many_arguments)]
    fn check_in_scope(
        &mut self,
        scope_id: Option<u64>,
        origin_scope: CompareOriginScope,
        char_id: i64,
        item_id: i64,
        is_corp_target: bool,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        helper: &str,
        call_site: &str,
        actual_dict: Option<&CanonicalChangeDict>,
    ) -> DedupeLookupOutcome {
        let facts = match scope_id {
            Some(sid) => {
                let Some(bucket) = self.dispatch_buckets.get_mut(&sid) else {
                    return DedupeLookupOutcome::NotSet;
                };
                &mut bucket.facts
            }
            None => &mut self.outside.live.facts,
        };
        let key = (char_id, item_id, is_corp_target);
        let Some(entry) = facts.get_mut(&key) else {
            return DedupeLookupOutcome::NotSet;
        };
        if entry_matches_wire_fp(entry, kind, fp, tombstone_wildcard_deleted) {
            let count_live = !entry.replaced_claimed;
            entry.replaced_claimed = true;
            let class_key = entry.class_key.clone();
            let rpc_method = entry.rpc_method.clone();
            let expected_dict = entry.change_dict.clone();
            self.telemetry.record_replaced(
                origin_scope,
                count_live,
                &class_key,
                &rpc_method,
                helper,
                call_site,
                item_id,
                expected_dict.as_ref(),
                actual_dict,
            );
            DedupeLookupOutcome::Replaced
        } else {
            let class_key = entry.class_key.clone();
            let rpc_method = entry.rpc_method.clone();
            let expected_fp = entry.fingerprint;
            self.telemetry.record_superseded(
                origin_scope,
                true,
                &class_key,
                &rpc_method,
                helper,
                call_site,
                char_id,
                item_id,
                kind,
                expected_fp,
                fp,
                actual_dict,
            );
            DedupeLookupOutcome::Superseded
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_shadow_outside_fact(
        &mut self,
        origin_scope: CompareOriginScope,
        char_id: i64,
        item_id: i64,
        is_corp_target: bool,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
    ) -> DedupeLookupOutcome {
        self.outside.check_shadow_outcome(
            origin_scope,
            char_id,
            item_id,
            is_corp_target,
            kind,
            fp,
            tombstone_wildcard_deleted,
            &mut self.telemetry,
        )
    }

    fn has_fact_in_scope(
        &self,
        scope_id: Option<u64>,
        outside_rpc_enabled: bool,
        char_id: i64,
        item_id: i64,
        is_corp_target: bool,
    ) -> bool {
        match scope_id {
            Some(sid) => self
                .dispatch_buckets
                .get(&sid)
                .is_some_and(|b| b.facts.contains_key(&(char_id, item_id, is_corp_target))),
            None if outside_rpc_enabled => self
                .outside
                .live
                .facts
                .contains_key(&(char_id, item_id, is_corp_target)),
            None => self
                .outside
                .shadow
                .facts
                .contains_key(&(char_id, item_id, is_corp_target)),
        }
    }

    fn has_blueprint_fact_in_scope(
        &self,
        scope_id: Option<u64>,
        char_id: i64,
        owner_id: i64,
    ) -> bool {
        match scope_id {
            Some(sid) => self
                .dispatch_buckets
                .get(&sid)
                .is_some_and(|b| b.blueprint_facts.contains(&(char_id, owner_id))),
            None => self
                .outside
                .live
                .blueprint_facts
                .contains(&(char_id, owner_id)),
        }
    }

    fn has_transitioned_char(&self, scope_id: u64, char_id: i64) -> bool {
        self.dispatch_buckets
            .get(&scope_id)
            .is_some_and(|b| b.transitioned_chars.contains(&char_id))
    }

    fn is_item_transition_suppressed_in_scope(
        &self,
        scope_id: u64,
        char_id: i64,
        item_id: i64,
        active_ship_id: i64,
    ) -> bool {
        self.dispatch_buckets.get(&scope_id).is_some_and(|b| {
            b.latest_commit_transitioned_chars.contains(&char_id)
                || b.transition_suppressed_items.contains(&(char_id, item_id))
                || (active_ship_id > 0
                    && item_id == active_ship_id
                    && b.transitioned_chars.contains(&char_id))
        })
    }

    fn transitioned_chars_in_scope(&self, scope_id: u64) -> HashSet<i64> {
        self.dispatch_buckets
            .get(&scope_id)
            .map(|b| b.transitioned_chars.clone())
            .unwrap_or_default()
    }

    fn is_explicit_effect_item(&self, scope_id: u64, char_id: i64, item_id: i64) -> bool {
        self.dispatch_buckets
            .get(&scope_id)
            .is_some_and(|b| b.explicit_effect_items.contains(&(char_id, item_id)))
    }

    fn is_fuel_reserved_item(&self, scope_id: u64, item_id: i64) -> bool {
        self.dispatch_buckets
            .get(&scope_id)
            .is_some_and(|b| b.fuel_reserved_items.contains(&item_id))
    }

    fn record_account_call_site(&mut self, call_site: &str, replaced: u64, passed_through: u64) {
        if replaced == 0 && passed_through == 0 {
            return;
        }
        self.account_telemetry.replaced += replaced;
        self.account_telemetry.passed_through += passed_through;
        let entry = self
            .account_telemetry
            .by_call_site
            .entry(call_site.to_string())
            .or_default();
        entry.replaced += replaced;
        entry.passed_through += passed_through;
    }

    fn clear_active_buckets(&mut self) {
        self.dispatch_buckets.clear();
        self.outside.clear_all();
    }
}

/// Thread-safe, per-[`ServerContext`] switch state and per-scope fact/telemetry ledger.
#[derive(Debug)]
pub struct ProjectedItemSwitchHandle {
    char_same_scene_item_mutation: AtomicBool,
    char_same_scene_item_deleted: AtomicBool,
    char_same_scene_item_outside_rpc: AtomicBool,
    char_same_scene_item_fitted: AtomicBool,
    char_all_items: AtomicBool,
    corp_all_items: AtomicBool,
    dedupe: Mutex<ProjectedItemDedupeLedger>,
}

impl Default for ProjectedItemSwitchHandle {
    fn default() -> Self {
        Self::new(ProjectedItemSwitchConfig::default())
    }
}

impl ProjectedItemSwitchHandle {
    /// Creates a new handle initialized with `config`.
    pub fn new(config: ProjectedItemSwitchConfig) -> Self {
        Self {
            char_same_scene_item_mutation: AtomicBool::new(config.char_same_scene_item_mutation),
            char_same_scene_item_deleted: AtomicBool::new(config.char_same_scene_item_deleted),
            char_same_scene_item_outside_rpc: AtomicBool::new(
                config.char_same_scene_item_outside_rpc,
            ),
            char_same_scene_item_fitted: AtomicBool::new(config.char_same_scene_item_fitted),
            char_all_items: AtomicBool::new(config.char_all_items),
            corp_all_items: AtomicBool::new(config.corp_all_items),
            dedupe: Mutex::new(ProjectedItemDedupeLedger::default()),
        }
    }

    /// Reads the active [`ProjectedItemSwitchConfig`].
    #[inline]
    pub fn config(&self) -> ProjectedItemSwitchConfig {
        ProjectedItemSwitchConfig {
            char_same_scene_item_mutation: self
                .char_same_scene_item_mutation
                .load(Ordering::Acquire),
            char_same_scene_item_deleted: self
                .char_same_scene_item_deleted
                .load(Ordering::Acquire),
            char_same_scene_item_outside_rpc: self
                .char_same_scene_item_outside_rpc
                .load(Ordering::Acquire),
            char_same_scene_item_fitted: self
                .char_same_scene_item_fitted
                .load(Ordering::Acquire),
            char_all_items: self.char_all_items.load(Ordering::Acquire),
            corp_all_items: self.corp_all_items.load(Ordering::Acquire),
        }
    }

    /// Updates switch flags (clearing active dispatch/outside facts and resetting test telemetry).
    pub fn set_config(&self, config: ProjectedItemSwitchConfig) {
        self.char_same_scene_item_mutation
            .store(config.char_same_scene_item_mutation, Ordering::Release);
        self.char_same_scene_item_deleted
            .store(config.char_same_scene_item_deleted, Ordering::Release);
        self.char_same_scene_item_outside_rpc
            .store(config.char_same_scene_item_outside_rpc, Ordering::Release);
        self.char_same_scene_item_fitted
            .store(config.char_same_scene_item_fitted, Ordering::Release);
        self.char_all_items
            .store(config.char_all_items, Ordering::Release);
        self.corp_all_items
            .store(config.corp_all_items, Ordering::Release);
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.clear_active_buckets();
        if !config.is_any_enabled() {
            guard.telemetry = SwitchTelemetryState::default();
        }
    }

    #[cfg(test)]
    pub(crate) fn apply_commit_dedupe_updates(
        &self,
        scope_id: Option<u64>,
        changes: &ChangeSet,
        transitioned_chars: &[i64],
        tokens: Vec<CommitProjectedToken>,
    ) {
        let origin_scope = if scope_id.is_some() {
            CompareOriginScope::Dispatch
        } else {
            CompareOriginScope::Neither
        };
        let outside_rpc = self.config().char_same_scene_item_outside_rpc;
        let empty_bp = HashSet::new();
        self.apply_commit_dedupe_updates_with_mode(
            scope_id,
            origin_scope,
            outside_rpc,
            changes,
            transitioned_chars,
            tokens,
            &empty_bp,
            &[],
            &[],
            &[],
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_commit_dedupe_updates_with_mode(
        &self,
        scope_id: Option<u64>,
        origin_scope: CompareOriginScope,
        outside_rpc_enabled: bool,
        changes: &ChangeSet,
        transitioned_chars: &[i64],
        tokens: Vec<CommitProjectedToken>,
        invalidated_bp_owners: &HashSet<i64>,
        blueprint_tokens: &[(i64, i64)],
        explicit_effect_items: &[(i64, i64)],
        fuel_reserved_items: &[i64],
    ) {
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.apply_commit_updates(
            scope_id,
            origin_scope,
            outside_rpc_enabled,
            changes,
            transitioned_chars,
            tokens,
            invalidated_bp_owners,
            blueprint_tokens,
            explicit_effect_items,
            fuel_reserved_items,
        );
    }

    pub(crate) fn has_fact_in_scope(
        &self,
        scope_id: Option<u64>,
        char_id: i64,
        item_id: i64,
        is_corp_target: bool,
    ) -> bool {
        let outside_rpc_enabled = self.config().char_same_scene_item_outside_rpc;
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.has_fact_in_scope(scope_id, outside_rpc_enabled, char_id, item_id, is_corp_target)
    }

    pub(crate) fn has_blueprint_fact_in_scope(
        &self,
        scope_id: Option<u64>,
        char_id: i64,
        owner_id: i64,
    ) -> bool {
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.has_blueprint_fact_in_scope(scope_id, char_id, owner_id)
    }

    pub(crate) fn record_covered_industry_monitors(
        &self,
        scope_id: Option<u64>,
        covered: &HashMap<i64, (i64, i64, Vec<u32>)>,
    ) {
        if covered.is_empty() {
            return;
        }
        let outside_rpc = self.config().char_same_scene_item_outside_rpc;
        if scope_id.is_none() && !outside_rpc {
            return;
        }
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.record_covered_industry_monitors(scope_id, outside_rpc, covered);
    }

    pub(crate) fn is_industry_monitor_covered_in_scope(
        &self,
        scope_id: Option<u64>,
        monitor_id: i64,
        tids: &[u32],
    ) -> bool {
        if tids.is_empty() {
            return false;
        }
        let outside_rpc = self.config().char_same_scene_item_outside_rpc;
        if scope_id.is_none() && !outside_rpc {
            return false;
        }
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.is_industry_monitor_covered(scope_id, outside_rpc, monitor_id, tids)
    }

    /// Expires all unconsumed dedupe facts belonging to `scope_id` (`DispatchNotificationStage::drop`).
    pub(crate) fn expire_dispatch_scope(&self, scope_id: u64) {
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.expire_dispatch_scope(scope_id);
    }

    /// Clears outside-dispatch facts held for `char_id` when that character goes offline.
    pub(crate) fn clear_outside_dispatch_char(&self, char_id: i64) {
        if char_id <= 0 {
            return;
        }
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.clear_outside_dispatch_chars(&[char_id]);
    }

    /// Clears outside-dispatch facts held for any character in `char_ids` going offline.
    pub(crate) fn clear_outside_dispatch_chars(&self, char_ids: &[i64]) {
        if char_ids.is_empty() {
            return;
        }
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.clear_outside_dispatch_chars(char_ids);
    }

    pub(crate) fn has_transitioned_char(&self, scope_id: u64, char_id: i64) -> bool {
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.has_transitioned_char(scope_id, char_id)
    }

    pub(crate) fn is_item_transition_suppressed_in_scope(
        &self,
        scope_id: u64,
        char_id: i64,
        item_id: i64,
        active_ship_id: i64,
    ) -> bool {
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.is_item_transition_suppressed_in_scope(scope_id, char_id, item_id, active_ship_id)
    }

    pub(crate) fn transitioned_chars_in_scope(&self, scope_id: u64) -> HashSet<i64> {
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.transitioned_chars_in_scope(scope_id)
    }

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub(crate) fn try_check_in_scope(
        &self,
        scope_id: Option<u64>,
        origin_scope: CompareOriginScope,
        char_id: i64,
        item_id: i64,
        is_corp_target: bool,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        helper: &str,
        actual_dict: Option<&CanonicalChangeDict>,
    ) -> DedupeLookupOutcome {
        let call_site = caller_call_site();
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.check_in_scope(
            scope_id,
            origin_scope,
            char_id,
            item_id,
            is_corp_target,
            kind,
            fp,
            tombstone_wildcard_deleted,
            helper,
            &call_site,
            actual_dict,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn check_shadow_outside_outcome(
        &self,
        origin_scope: CompareOriginScope,
        char_id: i64,
        item_id: i64,
        is_corp_target: bool,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
    ) -> DedupeLookupOutcome {
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.check_shadow_outside_fact(
            origin_scope,
            char_id,
            item_id,
            is_corp_target,
            kind,
            fp,
            tombstone_wildcard_deleted,
        )
    }

    pub(crate) fn record_shadow_outside_no_projected_event(
        &self,
        origin_scope: CompareOriginScope,
    ) {
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.telemetry.record_shadow_no_projected_event(origin_scope);
    }

    pub(crate) fn is_outside_explicit_or_fuel_skipped(
        &self,
        outside_rpc_enabled: bool,
        char_id: i64,
        item_id: i64,
    ) -> (bool, bool) {
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .outside
            .explicit_or_fuel_flags(outside_rpc_enabled, char_id, item_id)
    }

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub(crate) fn record_helper_without_token(
        &self,
        origin_scope: CompareOriginScope,
        count_live: bool,
        class_key: &str,
        rpc_method: &str,
        helper: &str,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        actual_dict: Option<&CanonicalChangeDict>,
    ) {
        let call_site = caller_call_site();
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.telemetry.record_helper_without_token(
            origin_scope,
            count_live,
            class_key,
            rpc_method,
            helper,
            &call_site,
            char_id,
            item_id,
            kind,
            actual_dict,
        );
    }

    pub(crate) fn is_explicit_effect_in_scope(&self, scope_id: u64, char_id: i64, item_id: i64) -> bool {
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.is_explicit_effect_item(scope_id, char_id, item_id)
    }

    pub(crate) fn is_fuel_reserved_in_scope(&self, scope_id: u64, item_id: i64) -> bool {
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.is_fuel_reserved_item(scope_id, item_id)
    }

    #[track_caller]
    pub(crate) fn record_non_first_set_emitted(&self) {
        let call_site = caller_call_site();
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.telemetry.record_non_first_set_emitted(&call_site);
    }

    #[track_caller]
    pub(crate) fn record_not_covered(&self, reason: NotCoveredReason) {
        let call_site = caller_call_site();
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.telemetry.record_not_covered(&call_site, reason);
    }

    #[track_caller]
    pub(crate) fn record_account_call_site(&self, replaced: u64, passed_through: u64) {
        let call_site = caller_call_site();
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.record_account_call_site(&call_site, replaced, passed_through);
    }

    /// Returns a snapshot of switched-run telemetry counters.
    pub fn summary(&self) -> ProjectedItemSwitchSummary {
        let cfg = self.config();
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        let mut summary = guard.telemetry.to_summary(cfg);
        summary.account_switches = guard.account_telemetry.clone();
        summary
    }

    /// Emits a structured `tracing::info!` summary when any switch is enabled or recorded activity.
    pub fn log_summary(&self, msg: &str) {
        let summary = self.summary();
        if !summary.has_activity() {
            return;
        }
        tracing::info!(
            target: "delta_item_switch",
            config = ?summary.config,
            emitted = summary.emitted,
            replaced = summary.replaced,
            superseded = summary.superseded,
            new = summary.new,
            non_first_set_emitted = summary.non_first_set_emitted,
            helper_without_token = summary.helper_without_token,
            helper_without_token_with_projected_event = summary.helper_without_token_with_projected_event,
            helper_without_token_no_projected_event = summary.helper_without_token_no_projected_event,
            dictionary_same = summary.dictionary_same,
            dictionary_differs = summary.dictionary_differs,
            evictions = summary.evictions,
            by_scope = ?summary.by_scope,
            by_class = ?summary.by_class,
            by_dispatch_method = ?summary.by_dispatch_method,
            by_helper = ?summary.by_helper,
            by_call_site = ?summary.by_call_site,
            not_covered_by_reason = ?summary.not_covered_by_reason,
            not_covered_by_call_site_reason = ?summary.not_covered_by_call_site_reason,
            account_switches = ?summary.account_switches,
            "{msg}"
        );
    }
}

/// Canonical spatial scene of a character or inventory item (`#931` Guardrail 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemScene {
    Station { station_id: i64 },
    Structure { structure_id: i64 },
    Space { solar_system_id: u32 },
}

/// Resolves a character's active spatial scene from `station_id`, `structure_id`, or `solar_system_id`.
#[inline]
pub fn character_scene(ch: &CharacterRecord) -> Option<ItemScene> {
    if let Some(station_id) = ch.station_id.filter(|&id| id > 0) {
        Some(ItemScene::Station { station_id })
    } else if let Some(structure_id) = ch.structure_id.filter(|&id| id > 0) {
        Some(ItemScene::Structure { structure_id })
    } else if ch.solar_system_id > 0 {
        Some(ItemScene::Space {
            solar_system_id: ch.solar_system_id,
        })
    } else {
        None
    }
}

/// Indexes `changes.characters` by `character_id` -> `(before, after)`.
pub(crate) fn index_changed_characters(
    changes: &ChangeSet,
) -> HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)> {
    changes
        .characters
        .iter()
        .map(|c| (c.key, (c.before.as_deref(), c.after.as_deref())))
        .collect()
}

/// Returns `true` when `char_id` was created, deleted, changed spatial location (`solar_system_id`,
/// `station_id`, `structure_id`), or switched `active_ship_id` while in space in this commit.
pub fn is_character_scene_or_ship_transition(
    char_id: i64,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
) -> bool {
    let Some(&(before_opt, after_opt)) = changed_chars.get(&char_id) else {
        return false;
    };
    let (Some(before), Some(after)) = (before_opt, after_opt) else {
        return true;
    };
    before.solar_system_id != after.solar_system_id
        || before.station_id != after.station_id
        || before.structure_id != after.structure_id
        || (before.active_ship_id != after.active_ship_id
            && before.station_id.is_none()
            && before.structure_id.is_none())
}

/// Strict human character ownership check (`Guardrail 3`): requires `owner_id` to be present in
/// `snap_after.characters`, not transitioning character existence, and absent from player and NPC
/// corporation registries.
pub fn is_known_human_character_owner(
    owner_id: i64,
    snap_after: &WorldStateSnapshot,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
    sde: &SdeRegistry,
) -> bool {
    if owner_id <= 0
        || !snap_after.characters.contains_key(&owner_id)
        || snap_after.corporations.contains_key(&owner_id)
        || sde.npc_corporations.contains_key(&owner_id)
    {
        return false;
    }
    if let Some(&(before_opt, after_opt)) = changed_chars.get(&owner_id) {
        if before_opt.is_none() || after_opt.is_none() {
            return false;
        }
    }
    true
}

/// Resolves an item's root spatial scene by walking its container `location_id` chain.
///
/// When `before_overrides` is `Some`, parent container lookups check pre-commit `before` snapshots
/// before falling back to `snap_after.items`, preserving exact pre-commit location chains even if a
/// container moved or was removed in the same transaction.
pub fn resolve_item_scene_in_view(
    item: &InventoryItemRecord,
    owner_char: &CharacterRecord,
    snap_after: &WorldStateSnapshot,
    before_overrides: Option<&HashMap<i64, &InventoryItemRecord>>,
    sde: &SdeRegistry,
) -> Option<ItemScene> {
    let mut loc = item.location_id;
    let mut current_flag = item.flag;
    for _ in 0..MAX_CONTAINER_WALK_DEPTH {
        if loc <= 0 {
            return None;
        }
        if loc == owner_char.character_id {
            return matches!(
                current_flag,
                InventoryFlag::Skill | InventoryFlag::Implant | InventoryFlag::Booster
            )
            .then(|| character_scene(owner_char))
            .flatten();
        }
        if sde.stations.contains_key(&loc) {
            return Some(ItemScene::Station { station_id: loc });
        }
        if snap_after.structures.contains_key(&loc) {
            return Some(ItemScene::Structure { structure_id: loc });
        }
        if let Ok(sys_id) = u32::try_from(loc) {
            if sde.solar_systems.contains_key(&sys_id) {
                return Some(ItemScene::Space {
                    solar_system_id: sys_id,
                });
            }
        }
        let parent = before_overrides
            .and_then(|m| m.get(&loc).copied())
            .or_else(|| snap_after.items.get(&loc))?;
        if parent.location_id == loc {
            return None;
        }
        loc = parent.location_id;
        current_flag = parent.flag;
    }
    None
}

pub(crate) type ItemBeforeAfter<'a> = (
    Option<&'a InventoryItemRecord>,
    Option<&'a InventoryItemRecord>,
);

fn is_structure_location(
    loc_id: i64,
    snap: &WorldStateSnapshot,
    changes: &ChangeSet,
    item_changes_by_id: &HashMap<i64, ItemBeforeAfter<'_>>,
    sde: &SdeRegistry,
) -> bool {
    if snap.structures.contains_key(&loc_id) || changes.structures.iter().any(|c| c.key == loc_id) {
        return true;
    }
    let type_id = snap.items.get(&loc_id).map(|r| r.type_id).or_else(|| {
        item_changes_by_id
            .get(&loc_id)
            .and_then(|(before, after)| after.or(*before).map(|r| r.type_id))
    });
    type_id.is_some_and(|tid| sde.types.get(&tid).is_some_and(|t| t.category_id == 65))
}

pub(crate) fn classify_location_kind(
    loc_id: Option<i64>,
    snap: &WorldStateSnapshot,
    changes: &ChangeSet,
    item_changes_by_id: &HashMap<i64, ItemBeforeAfter<'_>>,
    sde: &SdeRegistry,
) -> &'static str {
    let Some(id) = loc_id else {
        return "none";
    };
    if id == 0 {
        return "none";
    }
    if id == 6 {
        return "recycling_loc_6";
    }
    if is_structure_location(id, snap, changes, item_changes_by_id, sde) {
        return "structure";
    }
    let known_item = snap.items.contains_key(&id)
        || item_changes_by_id
            .get(&id)
            .is_some_and(|(before, after)| before.is_some() || after.is_some());
    if known_item {
        return "ship_or_container";
    }
    if sde.stations.contains_key(&id) || (60_000_000..=64_000_000).contains(&id) {
        return "station";
    }
    if u32::try_from(id)
        .ok()
        .is_some_and(|sid| sde.solar_systems.contains_key(&sid))
        || (30_000_000..=39_999_999).contains(&id)
    {
        return "solar_system";
    }
    "other"
}

pub(crate) fn classify_owner_kind(
    owner_id: i64,
    snap: &WorldStateSnapshot,
    changes: &ChangeSet,
    sde: &SdeRegistry,
) -> &'static str {
    if owner_id <= 0
        || sde.npc_corporations.contains_key(&owner_id)
        || (1_000_000..=1_999_999).contains(&owner_id)
    {
        return "npc_corp_or_other";
    }
    if snap.characters.contains_key(&owner_id)
        || changes
            .characters
            .iter()
            .any(|c| c.key == owner_id && (c.before.is_some() || c.after.is_some()))
        || (90_000_000..98_000_000).contains(&owner_id)
    {
        return "character";
    }
    if snap.corporations.contains_key(&owner_id) || (98_000_000..99_000_000).contains(&owner_id) {
        return "player_corp";
    }
    "npc_corp_or_other"
}

/// Canonical class key for an `OnItemChange` event, including `before_flag` when recycled to `loc 6`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn classify_item_row_kind_key(
    before_loc: Option<i64>,
    before_flag: Option<i64>,
    owner_id: i64,
    after_loc: i64,
    flag_id: i64,
    wire_qty: i64,
    snap: &WorldStateSnapshot,
    changes: &ChangeSet,
    item_changes_by_id: &HashMap<i64, ItemBeforeAfter<'_>>,
    sde: &SdeRegistry,
) -> String {
    let loc_before = classify_location_kind(before_loc, snap, changes, item_changes_by_id, sde);
    let loc_after = classify_location_kind(Some(after_loc), snap, changes, item_changes_by_id, sde);
    let owner_kind = classify_owner_kind(owner_id, snap, changes, sde);
    let singleton = wire_qty < 0;
    if loc_after == "recycling_loc_6" {
        match before_flag {
            Some(bf) => format!(
                "before={loc_before},after={loc_after},flag={flag_id},before_flag={bf},owner={owner_kind},singleton={singleton}"
            ),
            None => format!(
                "before={loc_before},after={loc_after},flag={flag_id},before_flag=unknown,owner={owner_kind},singleton={singleton}"
            ),
        }
    } else {
        format!(
            "before={loc_before},after={loc_after},flag={flag_id},owner={owner_kind},singleton={singleton}"
        )
    }
}

/// Returns `true` when `location_id` equals `ch`'s docked station/structure OR active ship ID.
#[inline]
pub(crate) fn is_first_set_direct_slot_location(location_id: i64, ch: &CharacterRecord) -> bool {
    let docked_facility = ch
        .station_id
        .filter(|&id| id > 0)
        .or_else(|| ch.structure_id.filter(|&id| id > 0));
    docked_facility.is_some_and(|fac| location_id == fac)
        || (ch.active_ship_id > 0 && location_id == ch.active_ship_id)
}

/// Returns `true` when `(location_id, flag_id)` sits directly in the docked station/structure
/// hangar (`flag 4`) or the active ship cargo hold (`flag 5`) of `ch`.
#[inline]
pub(crate) fn is_first_set_direct_slot_coords(
    location_id: i64,
    flag_id: i64,
    ch: &CharacterRecord,
) -> bool {
    let docked_facility = ch
        .station_id
        .filter(|&id| id > 0)
        .or_else(|| ch.structure_id.filter(|&id| id > 0));
    let in_docked_hangar =
        docked_facility.is_some_and(|fac| location_id == fac && flag_id == InventoryFlag::Hangar as i64);
    let in_active_cargo = ch.active_ship_id > 0
        && location_id == ch.active_ship_id
        && flag_id == InventoryFlag::Cargo as i64;
    in_docked_hangar || in_active_cargo
}

/// Returns `true` when `(location_id, flag_id)` sits directly in a fitted module/rig/subsystem
/// slot on `ch`'s active ship (`#931` Track 3C-2 `"fitted"` candidate class).
#[inline]
pub(crate) fn is_active_ship_fitted_slot_coords(location_id: i64, flag_id: i64, ch: &CharacterRecord) -> bool {
    ch.active_ship_id > 0
        && location_id == ch.active_ship_id
        && u16::try_from(flag_id).is_ok_and(|f| matches!(f, 11..=34 | 92..=94 | 125..=132))
}

#[inline]
pub(crate) fn evaluate_fitted_coords_pair(before_opt: Option<(i64, i64)>, after_opt: Option<(i64, i64)>, ch: &CharacterRecord) -> bool {
    match (before_opt, after_opt) {
        (Some(b), Some(a)) => match (is_active_ship_fitted_slot_coords(b.0, b.1, ch), is_active_ship_fitted_slot_coords(a.0, a.1, ch)) {
            (true, true) => true,
            (true, false) => is_first_set_direct_slot_coords(a.0, a.1, ch),
            (false, true) => is_first_set_direct_slot_coords(b.0, b.1, ch),
            (false, false) => false,
        },
        (None, Some(a)) => is_active_ship_fitted_slot_coords(a.0, a.1, ch),
        (Some(b), None) => is_active_ship_fitted_slot_coords(b.0, b.1, ch),
        (None, None) => false,
    }
}

/// Returns `true` when `item` sits directly in `ch`'s docked station/structure hangar (`flag 4`)
/// or active ship cargo hold (`flag 5`).
#[inline]
pub(crate) fn is_first_set_direct_slot(item: &InventoryItemRecord, ch: &CharacterRecord) -> bool {
    is_first_set_direct_slot_coords(item.location_id, item.flag as i64, ch)
}

pub(crate) fn pair_owner_and_op_kind(before_opt: Option<&InventoryItemRecord>, after_opt: Option<&InventoryItemRecord>) -> Option<(i64, ProjectedItemOpKind)> {
    match (before_opt, after_opt) {
        (None, Some(new_item)) => Some((new_item.owner_id, ProjectedItemOpKind::CreatedOrUpdated)),
        (Some(old_item), Some(new_item)) => (old_item.owner_id == new_item.owner_id).then_some((new_item.owner_id, ProjectedItemOpKind::CreatedOrUpdated)),
        (Some(old_item), None) => Some((old_item.owner_id, ProjectedItemOpKind::Deleted)),
        (None, None) => None,
    }
}

pub(crate) fn online_non_transitioned_owner<'a>(
    owner_id: i64,
    recipient_char_id: i64,
    snap_after: &'a WorldStateSnapshot,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> Option<&'a CharacterRecord> {
    if owner_id != recipient_char_id
        || !is_known_human_character_owner(owner_id, snap_after, changed_chars, sde)
        || is_character_scene_or_ship_transition(owner_id, changed_chars)
    {
        return None;
    }
    let ch = snap_after.characters.get(&owner_id)?;
    online.is_character_online(owner_id, ch.user_id).then_some(ch)
}

/// Shared marker-free direct-slot first-set qualification check (`#931`): online human owner
/// with no cross-owner transfer or scene/ship transition where both endpoints sit directly inside
/// the pilot's docked hangar (`flag 4`) or active ship cargo (`flag 5`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn is_first_set_direct_slot_pair(
    recipient_char_id: i64,
    before_opt: Option<&InventoryItemRecord>,
    after_opt: Option<&InventoryItemRecord>,
    snap_after: &WorldStateSnapshot,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> Option<ProjectedItemOpKind> {
    let (owner_id, op_kind) = pair_owner_and_op_kind(before_opt, after_opt)?;
    let ch = online_non_transitioned_owner(
        owner_id,
        recipient_char_id,
        snap_after,
        changed_chars,
        online,
        sde,
    )?;
    (before_opt.is_none_or(|r| is_first_set_direct_slot(r, ch))
        && after_opt.is_none_or(|r| is_first_set_direct_slot(r, ch)))
    .then_some(op_kind)
}

/// Shared first-set qualification check (`#931` Check #28–#30) scoped to in-dispatch for compare
/// baseline while sharing the marker-free direct-slot predicate.
#[allow(clippy::too_many_arguments)]
pub(crate) fn is_first_set_item_change_pair(
    in_dispatch: bool,
    recipient_char_id: i64,
    before_opt: Option<&InventoryItemRecord>,
    after_opt: Option<&InventoryItemRecord>,
    snap_after: &WorldStateSnapshot,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> Option<ProjectedItemOpKind> {
    if !in_dispatch {
        return None;
    }
    is_first_set_direct_slot_pair(
        recipient_char_id,
        before_opt,
        after_opt,
        snap_after,
        changed_chars,
        online,
        sde,
    )
}

/// Evaluates Track 3C-2 telemetry-only `would_qualify` flags (`"fitted"` and `"tick"`) on a
/// committed item pair (`#931` Section 4). Class `"tick"` is marker-free (`!in_dispatch`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_projected_next_classes(
    in_dispatch: bool,
    in_tick: bool,
    recipient_char_id: i64,
    before_opt: Option<&InventoryItemRecord>,
    after_opt: Option<&InventoryItemRecord>,
    snap_after: &WorldStateSnapshot,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> WouldQualifyFlags {
    let Some((owner_id, _)) = pair_owner_and_op_kind(before_opt, after_opt) else {
        return WouldQualifyFlags::default();
    };
    let Some(ch) = online_non_transitioned_owner(
        owner_id,
        recipient_char_id,
        snap_after,
        changed_chars,
        online,
        sde,
    ) else {
        return WouldQualifyFlags::default();
    };
    let direct_slot = before_opt.is_none_or(|r| is_first_set_direct_slot(r, ch))
        && after_opt.is_none_or(|r| is_first_set_direct_slot(r, ch));
    WouldQualifyFlags {
        fitted: (in_dispatch || in_tick)
            && evaluate_fitted_coords_pair(
                before_opt.map(|r| (r.location_id, r.flag as i64)),
                after_opt.map(|r| (r.location_id, r.flag as i64)),
                ch,
            ),
        tick: !in_dispatch && direct_slot,
    }
}

/// Evaluates `changes.items` under active [`ProjectedItemSwitchConfig`], updating dispatch/outside
/// item & blueprint facts and per-scope telemetry and returning `OnItemChange` and
/// `OnBlueprintsUpdated` specs.
///
/// When `char_same_scene_item_outside_rpc` is `false` and `dispatch_scope_info` is `None`,
/// dry-run evaluates into `shadow_outside_facts` and `by_scope.{tick, neither}.would_announce`
/// while returning no emission specs.
#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_switched_char_same_scene_item_specs(
    switches: &std::sync::Arc<ProjectedItemSwitchHandle>,
    pend: &dyn eve_db::lock_rank::AnyRwLock<
        HashMap<i64, Vec<super::cache_notifications::PendingMachoNotification>>,
    >,
    notify_tx: &tokio::sync::broadcast::Sender<(
        i64,
        super::cache_notifications::PendingMachoNotification,
    )>,
    dispatch_scope_info: Option<&(u64, Option<String>)>,
    origin_scope: CompareOriginScope,
    online: &impl OnlineCharacterLookup,
    snap_after: &WorldStateSnapshot,
    changes: &ChangeSet,
    sde: &SdeRegistry,
    suppressed_effects: &TxItemEffectKeys,
    active_fuel_reserves: &crate::services::tactical_modules::ActiveFuelReserveTracker,
    db_key: usize,
) -> SwitchedCommitBatch {
    let cfg = switches.config();
    if !cfg.is_any_enabled() {
        return SwitchedCommitBatch::default();
    }

    let changed_chars = index_changed_characters(changes);
    let transitioned_chars: Vec<i64> = changed_chars
        .keys()
        .copied()
        .filter(|&cid| is_character_scene_or_ship_transition(cid, &changed_chars))
        .collect();
    let in_dispatch = dispatch_scope_info.is_some();
    let scope_id = dispatch_scope_info.map(|(sid, _)| *sid);
    let effective_origin_scope = if in_dispatch {
        CompareOriginScope::Dispatch
    } else {
        origin_scope
    };
    let shadow_outside_dry_run = !in_dispatch && !cfg.char_same_scene_item_outside_rpc;

    if scope_id.is_some() {
        let _ = register_dispatch_switch_handle(pend, notify_tx, switches);
    }
    let empty_bp = HashSet::new();
    if changes.items.is_empty() {
        if !transitioned_chars.is_empty() {
            switches.apply_commit_dedupe_updates_with_mode(
                scope_id,
                effective_origin_scope,
                cfg.char_same_scene_item_outside_rpc,
                changes,
                &transitioned_chars,
                Vec::new(),
                &empty_bp,
                &[],
                &[],
                &[],
            );
        }
        return SwitchedCommitBatch::default();
    }

    let rpc_method = match dispatch_scope_info {
        Some((_, rpc_opt)) => rpc_opt
            .clone()
            .unwrap_or_else(|| crate::flight_recorder::UNKNOWN_RPC_LABEL.to_string()),
        None => outside_rpc_method_label(effective_origin_scope).to_string(),
    };

    // Snapshot characters already transitioned earlier in this dispatch so subsequent commits in
    // the same dispatch that omit `changes.characters` still stay excluded under Guardrail 2.
    let prior_transitioned_chars = scope_id
        .map(|sid| switches.transitioned_chars_in_scope(sid))
        .unwrap_or_default();

    let mut item_changes_by_id: HashMap<i64, ItemBeforeAfter<'_>> =
        HashMap::with_capacity(changes.items.len());
    for ch in &changes.items {
        item_changes_by_id
            .entry(ch.key)
            .and_modify(|entry| entry.1 = ch.after.as_deref())
            .or_insert((ch.before.as_deref(), ch.after.as_deref()));
    }

    let mut specs = Vec::new();
    let mut tokens = Vec::new();
    let mut invalidated_bp_owners: HashSet<i64> = HashSet::new();
    let mut blueprint_tokens_set: BTreeSet<(i64, i64)> = BTreeSet::new();
    let mut blueprint_specs_map: BTreeMap<(i64, i64), u64> = BTreeMap::new();
    let mut corp_recipients_cache: HashMap<i64, Vec<&CharacterRecord>> = HashMap::new();
    let mut explicit_effect_items = Vec::new();
    let mut fuel_reserved_items = Vec::new();
    let char_all_bp_active = cfg.char_all_items
        && (in_dispatch || shadow_outside_dry_run || cfg.char_same_scene_item_outside_rpc);
    let corp_all_bp_active = cfg.corp_all_items
        && (in_dispatch || shadow_outside_dry_run || cfg.char_same_scene_item_outside_rpc);

    for change in &changes.items {
        let before_rec = change.before.as_deref();
        let after_rec = change.after.as_deref();
        let before_is_bp = before_rec.is_some_and(|r| is_blueprint_type(sde, r.type_id));
        let after_is_bp = after_rec.is_some_and(|r| is_blueprint_type(sde, r.type_id));
        let bp_involved = before_is_bp || after_is_bp;
        if bp_involved {
            if let Some(b) = before_rec {
                invalidated_bp_owners.insert(b.owner_id);
            }
            if let Some(a) = after_rec {
                invalidated_bp_owners.insert(a.owner_id);
            }
        }

        // Leave Cargo fuel items held under an active rollback reserve untouched here so silent
        // actor-validation rollbacks emit zero notifications and deferred diffs stay authoritative (#1486).
        if active_fuel_reserves.is_item_reserved(db_key, change.key) {
            fuel_reserved_items.push(change.key);
            continue;
        }

        if (char_all_bp_active || corp_all_bp_active) && bp_involved {
            let bp_changed = match (before_rec, after_rec) {
                (None, Some(_)) | (Some(_), None) => true,
                (Some(old_item), Some(new_item)) => {
                    !compute_item_change_entries(old_item, new_item).is_empty()
                        || old_item.type_id != new_item.type_id
                        || old_item.is_blueprint_copy != new_item.is_blueprint_copy
                        || old_item.blueprint_runs_remaining != new_item.blueprint_runs_remaining
                        || old_item.dynamic_attributes != new_item.dynamic_attributes
                }
                (None, None) => false,
            };
            if bp_changed {
                let mut bp_owners = [None, None];
                match (before_rec, after_rec) {
                    (None, Some(new_item)) => bp_owners[0] = Some(new_item.owner_id),
                    (Some(old_item), None) => bp_owners[0] = Some(old_item.owner_id),
                    (Some(old_item), Some(new_item)) => {
                        bp_owners[0] = Some(new_item.owner_id);
                        if old_item.owner_id != new_item.owner_id {
                            bp_owners[1] = Some(old_item.owner_id);
                        }
                    }
                    (None, None) => {}
                }
                for owner_id in bp_owners.into_iter().flatten() {
                    if char_all_bp_active {
                        if let Some(ch) = online_non_transitioned_owner(
                            owner_id,
                            owner_id,
                            snap_after,
                            &changed_chars,
                            online,
                            sde,
                        ) {
                            let active_ship_suppressed = prior_transitioned_chars
                                .contains(&owner_id)
                                && ch.active_ship_id > 0
                                && change.key == ch.active_ship_id;
                            if !active_ship_suppressed
                                && !suppressed_effects.contains_blueprint(owner_id, owner_id)
                            {
                                blueprint_tokens_set.insert((owner_id, owner_id));
                                if !shadow_outside_dry_run {
                                    blueprint_specs_map
                                        .entry((owner_id, owner_id))
                                        .or_insert(ch.user_id);
                                }
                            }
                            continue;
                        }
                    }
                    if corp_all_bp_active
                        && is_corp_blueprint_owner_eligible(owner_id, snap_after, sde)
                    {
                        let members = corp_recipients_cache.entry(owner_id).or_insert_with(|| {
                            resolve_online_non_transitioned_corp_recipients(
                                owner_id,
                                snap_after,
                                &changed_chars,
                                online,
                            )
                        });
                        for &ch in members.iter() {
                            let cid = ch.character_id;
                            let active_ship_suppressed = prior_transitioned_chars.contains(&cid)
                                && ch.active_ship_id > 0
                                && change.key == ch.active_ship_id;
                            if active_ship_suppressed
                                || suppressed_effects.contains_blueprint(cid, owner_id)
                            {
                                continue;
                            }
                            blueprint_tokens_set.insert((cid, owner_id));
                            if !shadow_outside_dry_run {
                                blueprint_specs_map
                                    .entry((cid, owner_id))
                                    .or_insert(ch.user_id);
                            }
                        }
                    }
                }
            }
        }

        let classified_recipients = collect_switched_item_recipients_for_change(
            in_dispatch,
            shadow_outside_dry_run,
            cfg,
            change,
            snap_after,
            &changed_chars,
            online,
            sde,
            &mut corp_recipients_cache,
        );
        let mut eligible_recipients = Vec::with_capacity(classified_recipients.len());
        for (ch, op_kind, fp_rec, id_type) in classified_recipients {
            let recipient_char_id = ch.character_id;
            if prior_transitioned_chars.contains(&recipient_char_id)
                && ch.active_ship_id > 0
                && change.key == ch.active_ship_id
            {
                continue;
            }
            if suppressed_effects.contains_item(recipient_char_id, change.key) {
                explicit_effect_items.push((recipient_char_id, change.key));
                continue;
            }
            eligible_recipients.push((ch, op_kind, fp_rec, id_type));
        }
        if eligible_recipients.is_empty() {
            continue;
        }

        let Some(item_payload) = build_projected_item_change_args(change, changes, snap_after, sde) else {
            continue;
        };
        let (_, _, first_fp_rec, _) = eligible_recipients[0];
        let fp = ItemWireFingerprint::from_record(first_fp_rec);
        let change_dict = match item_payload.as_slice() {
            [_, dict_rep, ..] => CanonicalChangeDict::from_pyrep(dict_rep),
            _ => None,
        };
        // Only non-singleton endpoints affect available material counts in OnIndustryMaterials
        // (matching collect_available_monitor_materials; locking a singleton BPO must not inject {tid: 0}).
        let mut industry_scopes = Vec::with_capacity(2);
        if let Some(old_item) = before_rec.filter(|r| !r.singleton) {
            industry_scopes.push((
                old_item.owner_id,
                old_item.location_id,
                old_item.flag as i64,
                old_item.type_id,
            ));
        }
        if let Some(new_item) = after_rec.filter(|r| !r.singleton) {
            let new_scope = (
                new_item.owner_id,
                new_item.location_id,
                new_item.flag as i64,
                new_item.type_id,
            );
            if industry_scopes.first() != Some(&new_scope) {
                industry_scopes.push(new_scope);
            }
        }
        let (after_loc, after_flag, after_qty) = match after_rec {
            Some(after_r) => {
                let after_fp = ItemWireFingerprint::from_record(after_r);
                (after_fp.location_id, after_fp.flag_id, after_fp.wire_qty)
            }
            None => (6_i64, 0_i64, 0_i64),
        };
        let class_key = classify_item_row_kind_key(
            before_rec.map(|r| r.location_id),
            before_rec.map(|r| r.flag as i64),
            fp.owner_id,
            after_loc,
            after_flag,
            after_qty,
            snap_after,
            changes,
            &item_changes_by_id,
            sde,
        );

        let eligible_count = eligible_recipients.len();
        for (idx, (ch, op_kind, _, id_type)) in eligible_recipients.into_iter().enumerate() {
            let recipient_char_id = ch.character_id;
            let is_last = idx + 1 == eligible_count;
            tokens.push(CommitProjectedToken {
                char_id: recipient_char_id,
                item_id: change.key,
                is_corp_target: id_type == "corpid",
                kind: op_kind,
                fingerprint: fp,
                change_dict: change_dict.clone(),
                class_key: class_key.clone(),
                rpc_method: rpc_method.clone(),
            });
            if !shadow_outside_dry_run {
                if is_last {
                    specs.push(SwitchedProjectedItemSpec {
                        recipient_char_id,
                        recipient_user_id: ch.user_id,
                        id_type,
                        item_id: change.key,
                        args: item_payload,
                        industry_scopes,
                    });
                    break;
                }
                specs.push(SwitchedProjectedItemSpec {
                    recipient_char_id,
                    recipient_user_id: ch.user_id,
                    id_type,
                    item_id: change.key,
                    args: item_payload.clone(),
                    industry_scopes: industry_scopes.clone(),
                });
            }
        }
    }

    let bp_tokens_vec: Vec<(i64, i64)> = blueprint_tokens_set.into_iter().collect();
    let blueprint_specs: Vec<SwitchedProjectedBlueprintSpec> = blueprint_specs_map
        .into_iter()
        .map(|((recipient_char_id, owner_id), recipient_user_id)| SwitchedProjectedBlueprintSpec {
            recipient_char_id,
            recipient_user_id,
            owner_id,
        })
        .collect();

    switches.apply_commit_dedupe_updates_with_mode(
        scope_id,
        effective_origin_scope,
        cfg.char_same_scene_item_outside_rpc,
        changes,
        &transitioned_chars,
        tokens,
        &invalidated_bp_owners,
        &bp_tokens_vec,
        &explicit_effect_items,
        &fuel_reserved_items,
    );
    SwitchedCommitBatch {
        item_specs: specs,
        blueprint_specs,
    }
}

pub(crate) fn collect_item_change_scopes(
    item: &InventoryItemRecord,
    changes: &[(i32, PyRep)],
) -> Vec<(i64, i64, i64, u32)> {
    let mut scopes = vec![(
        item.owner_id,
        item.location_id,
        item.flag as i64,
        item.type_id,
    )];
    let old_owner = changes
        .iter()
        .find_map(|(k, v)| (*k == 2).then(|| v.as_i64()).flatten())
        .unwrap_or(item.owner_id);
    let old_loc = changes
        .iter()
        .find_map(|(k, v)| (*k == 3).then(|| v.as_i64()).flatten())
        .unwrap_or(item.location_id);
    let old_flag = changes
        .iter()
        .find_map(|(k, v)| (*k == 4).then(|| v.as_i64()).flatten())
        .unwrap_or(item.flag as i64);
    if old_loc > 0
        && (old_owner != item.owner_id
            || old_loc != item.location_id
            || old_flag != (item.flag as i64))
    {
        scopes.push((old_owner, old_loc, old_flag, item.type_id));
    }
    scopes
}

#[inline]
pub(crate) fn build_item_change_dict(changes: Vec<(i32, PyRep)>) -> PyRep {
    PyRep::Dict(
        changes
            .into_iter()
            .map(|(col_ix, old_val)| (PyRep::Integer(col_ix as i64), old_val))
            .collect(),
    )
}

/// Returns the switched item-projection telemetry snapshot for `ctx`.
pub fn projected_item_switch_summary(ctx: &ServerContext) -> ProjectedItemSwitchSummary {
    ctx.projected_item_switches.summary()
}

pub(crate) use super::delta_item_switch_ctx::{
    build_fitted_item_trashed_args, has_active_switch_for_item_event,
};
