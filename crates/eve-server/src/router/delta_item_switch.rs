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
use super::delta_commit_listener::TxItemEffectKeys;
use super::delta_item_corp::{
    collect_switched_item_recipients_for_change, is_corp_blueprint_owner_eligible,
    resolve_online_non_transitioned_corp_recipients,
};
use super::delta_projection::{
    build_projected_item_change_args, compute_item_change_entries, is_blueprint_type,
    OnlineCharacterLookup,
};

pub(crate) use super::delta_item_corp::{
    build_switched_item_commit_notifications, SwitchedCommitBatch,
    SwitchedProjectedBlueprintSpec, SwitchedProjectedItemSpec,
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

#[derive(Debug, Clone)]
pub(super) struct ProjectedDedupeEntry {
    pub(super) kind: ProjectedItemOpKind,
    pub(super) fingerprint: ItemWireFingerprint,
}

impl ProjectedDedupeEntry {
    #[inline]
    pub(super) fn from_commit_token(tok: CommitProjectedToken) -> Self {
        Self {
            kind: tok.kind,
            fingerprint: tok.fingerprint,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DedupeLookupOutcome {
    /// Fact is set for `(char_id, item_id)` with matching row -> suppress (`replaced`).
    Replaced,
    /// Fact is set for `(char_id, item_id)` with differing row -> suppress (`superseded`).
    Superseded,
    /// Fact is not set for `(char_id, item_id)` -> send normally.
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
    /// `monitor_id -> (char_id, owner_id, set of type_ids)` already covered by coalesced commit-time
    /// `OnIndustryMaterials` emissions in this dispatch scope (`#931` `BF-931-3C1-02` / `BF-01`).
    covered_industry_monitors: HashMap<i64, (i64, i64, HashSet<u32>)>,
}

#[derive(Debug, Default)]
struct OutsideScopeDedupeState {
    facts: HashMap<(i64, i64, bool), ProjectedDedupeEntry>,
    blueprint_facts: HashSet<(i64, i64)>,
    covered_industry_monitors: HashMap<i64, (i64, i64, HashSet<u32>)>,
}

impl OutsideScopeDedupeState {
    fn clear_all(&mut self) {
        self.facts.clear();
        self.blueprint_facts.clear();
        self.covered_industry_monitors.clear();
    }
}

fn purge_transitioned_chars_in_scope(
    facts: &mut HashMap<(i64, i64, bool), ProjectedDedupeEntry>,
    blueprint_facts: &mut HashSet<(i64, i64)>,
    covered_industry_monitors: &mut HashMap<i64, (i64, i64, HashSet<u32>)>,
    transitioned_chars: &[i64],
) {
    if transitioned_chars.is_empty() {
        return;
    }
    facts.retain(|&(cid, _, _), _| !transitioned_chars.contains(&cid));
    blueprint_facts.retain(|&(cid, _)| !transitioned_chars.contains(&cid));
    retain_monitors_excluding_chars(covered_industry_monitors, transitioned_chars);
}

#[derive(Debug, Default)]
struct ProjectedItemDedupeLedger {
    dispatch_buckets: HashMap<u64, DispatchTokenBucket>,
    outside: OutsideScopeDedupeState,
}

pub(crate) struct CommitProjectedToken {
    pub char_id: i64,
    pub item_id: i64,
    pub is_corp_target: bool,
    pub kind: ProjectedItemOpKind,
    pub fingerprint: ItemWireFingerprint,
}

fn merge_covered_industry_monitors(
    dst: &mut HashMap<i64, (i64, i64, HashSet<u32>)>,
    covered: &HashMap<i64, (i64, i64, Vec<u32>)>,
) {
    for (&monitor_id, (char_id, owner_id, tids)) in covered {
        let entry = dst
            .entry(monitor_id)
            .or_insert_with(|| (*char_id, *owner_id, HashSet::new()));
        if entry.0 != *char_id || entry.1 != *owner_id {
            entry.0 = *char_id;
            entry.1 = *owner_id;
            entry.2.clear();
        }
        entry.2.extend(tids.iter().copied());
    }
}

fn invalidate_covered_monitors_for_change(
    monitors: &mut HashMap<i64, (i64, i64, HashSet<u32>)>,
    change: &RowChange<i64, InventoryItemRecord>,
) {
    if monitors.is_empty() {
        return;
    }
    monitors.retain(|_, (_, oid, tids)| {
        for rec in [change.before.as_deref(), change.after.as_deref()]
            .into_iter()
            .flatten()
        {
            if *oid == rec.owner_id {
                tids.remove(&rec.type_id);
            }
        }
        !tids.is_empty()
    });
}

fn collect_touched_monitor_types(changes: &ChangeSet) -> HashMap<i64, HashSet<u32>> {
    let mut touched_types: HashMap<i64, HashSet<u32>> = HashMap::new();
    for change in &changes.items {
        for rec in [change.before.as_deref(), change.after.as_deref()]
            .into_iter()
            .flatten()
        {
            touched_types
                .entry(rec.owner_id)
                .or_default()
                .insert(rec.type_id);
        }
    }
    touched_types
}

fn invalidate_covered_monitors_for_types(
    monitors: &mut HashMap<i64, (i64, i64, HashSet<u32>)>,
    touched_types: &HashMap<i64, HashSet<u32>>,
) {
    if monitors.is_empty() || touched_types.is_empty() {
        return;
    }
    monitors.retain(|_, (_, oid, tids)| {
        if let Some(removed) = touched_types.get(oid) {
            tids.retain(|tid| !removed.contains(tid));
        }
        !tids.is_empty()
    });
}

fn retain_monitors_excluding_chars(
    monitors: &mut HashMap<i64, (i64, i64, HashSet<u32>)>,
    chars: &[i64],
) {
    if !chars.is_empty() && !monitors.is_empty() {
        monitors.retain(|_, (cid, _, _)| !chars.contains(cid));
    }
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

fn remove_item_from_facts(facts: &mut HashMap<(i64, i64, bool), ProjectedDedupeEntry>, item_id: i64) {
    if facts.is_empty() {
        return;
    }
    facts.retain(|&(_, eid, _), _| eid != item_id);
}

fn remove_items_from_facts(
    facts: &mut HashMap<(i64, i64, bool), ProjectedDedupeEntry>,
    item_ids: &HashSet<i64>,
) {
    if facts.is_empty() || item_ids.is_empty() {
        return;
    }
    facts.retain(|&(_, eid, _), _| !item_ids.contains(&eid));
}

impl ProjectedItemDedupeLedger {
    fn invalidate_item_everywhere(&mut self, change: &RowChange<i64, InventoryItemRecord>) {
        let item_id = change.key;
        for bucket in self.dispatch_buckets.values_mut() {
            remove_item_from_facts(&mut bucket.facts, item_id);
            invalidate_covered_monitors_for_change(&mut bucket.covered_industry_monitors, change);
        }
        remove_item_from_facts(&mut self.outside.facts, item_id);
        invalidate_covered_monitors_for_change(&mut self.outside.covered_industry_monitors, change);
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
                .any(|b| !b.facts.is_empty());
            let has_any_dispatch_monitors = self
                .dispatch_buckets
                .values()
                .any(|b| !b.covered_industry_monitors.is_empty());
            if has_any_dispatch_facts || !self.outside.facts.is_empty() {
                let item_ids: HashSet<i64> = changes.items.iter().map(|c| c.key).collect();
                for bucket in self.dispatch_buckets.values_mut() {
                    remove_items_from_facts(&mut bucket.facts, &item_ids);
                }
                remove_items_from_facts(&mut self.outside.facts, &item_ids);
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
            if !self.outside.blueprint_facts.is_empty() {
                self.outside
                    .blueprint_facts
                    .retain(|&(_cid, oid)| !invalidated_bp_owners.contains(&oid));
            }
        }
    }

    fn apply_commit_updates(
        &mut self,
        scope_id: Option<u64>,
        outside_rpc_enabled: bool,
        changes: &ChangeSet,
        transitioned_chars: &[i64],
        tokens: Vec<CommitProjectedToken>,
        invalidated_bp_owners: &HashSet<i64>,
        blueprint_tokens: &[(i64, i64)],
    ) {
        self.invalidate_changes_everywhere(changes, invalidated_bp_owners);
        if let Some(sid) = scope_id {
            if !self.dispatch_buckets.contains_key(&sid)
                && transitioned_chars.is_empty()
                && tokens.is_empty()
                && blueprint_tokens.is_empty()
            {
                return;
            }
            let bucket = self.dispatch_buckets.entry(sid).or_default();
            for &cid in transitioned_chars {
                bucket.transitioned_chars.insert(cid);
            }
            for tok in tokens {
                let key = (tok.char_id, tok.item_id, tok.is_corp_target);
                bucket
                    .facts
                    .insert(key, ProjectedDedupeEntry::from_commit_token(tok));
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

        if !transitioned_chars.is_empty() {
            for bucket in self.dispatch_buckets.values_mut() {
                purge_transitioned_chars_in_scope(
                    &mut bucket.facts,
                    &mut bucket.blueprint_facts,
                    &mut bucket.covered_industry_monitors,
                    transitioned_chars,
                );
            }
            purge_transitioned_chars_in_scope(
                &mut self.outside.facts,
                &mut self.outside.blueprint_facts,
                &mut self.outside.covered_industry_monitors,
                transitioned_chars,
            );
        }
        if !outside_rpc_enabled {
            return;
        }
        for tok in tokens {
            let key = (tok.char_id, tok.item_id, tok.is_corp_target);
            self.outside
                .facts
                .insert(key, ProjectedDedupeEntry::from_commit_token(tok));
        }
        for &bp_key in blueprint_tokens {
            self.outside.blueprint_facts.insert(bp_key);
        }
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
            merge_covered_industry_monitors(&mut self.outside.covered_industry_monitors, covered);
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
        outside_rpc_enabled
            && self
                .outside
                .covered_industry_monitors
                .get(&monitor_id)
                .is_some_and(|(_, _, covered)| tids.iter().all(|tid| covered.contains(tid)))
    }

    fn expire_dispatch_scope(&mut self, scope_id: u64) {
        self.dispatch_buckets.remove(&scope_id);
    }

    fn clear_outside_dispatch_chars(&mut self, offline_chars: &[i64]) {
        if offline_chars.is_empty() {
            return;
        }
        self.outside
            .facts
            .retain(|&(cid, _, _), _| !offline_chars.contains(&cid));
        self.outside
            .blueprint_facts
            .retain(|&(cid, _)| !offline_chars.contains(&cid));
        retain_monitors_excluding_chars(&mut self.outside.covered_industry_monitors, offline_chars);
    }

    #[allow(clippy::too_many_arguments)]
    fn check_in_scope(
        &self,
        scope_id: Option<u64>,
        char_id: i64,
        item_id: i64,
        is_corp_target: bool,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
    ) -> DedupeLookupOutcome {
        let facts = match scope_id {
            Some(sid) => {
                let Some(bucket) = self.dispatch_buckets.get(&sid) else {
                    return DedupeLookupOutcome::NotSet;
                };
                &bucket.facts
            }
            None => &self.outside.facts,
        };
        let key = (char_id, item_id, is_corp_target);
        let Some(entry) = facts.get(&key) else {
            return DedupeLookupOutcome::NotSet;
        };
        if entry_matches_wire_fp(entry, kind, fp, tombstone_wildcard_deleted) {
            DedupeLookupOutcome::Replaced
        } else {
            DedupeLookupOutcome::Superseded
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
            None => self.outside.blueprint_facts.contains(&(char_id, owner_id)),
        }
    }

    fn transitioned_chars_in_scope(&self, scope_id: u64) -> HashSet<i64> {
        self.dispatch_buckets
            .get(&scope_id)
            .map(|b| b.transitioned_chars.clone())
            .unwrap_or_default()
    }

    fn clear_active_buckets(&mut self) {
        self.dispatch_buckets.clear();
        self.outside.clear_all();
    }
}

/// Thread-safe, per-[`ServerContext`] switch state and per-scope fact dedupe ledger.
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

    /// Updates switch flags (clearing active dispatch/outside facts).
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
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_commit_dedupe_updates_with_mode(
        &self,
        scope_id: Option<u64>,
        outside_rpc_enabled: bool,
        changes: &ChangeSet,
        transitioned_chars: &[i64],
        tokens: Vec<CommitProjectedToken>,
        invalidated_bp_owners: &HashSet<i64>,
        blueprint_tokens: &[(i64, i64)],
    ) {
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.apply_commit_updates(
            scope_id,
            outside_rpc_enabled,
            changes,
            transitioned_chars,
            tokens,
            invalidated_bp_owners,
            blueprint_tokens,
        );
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

    pub(crate) fn transitioned_chars_in_scope(&self, scope_id: u64) -> HashSet<i64> {
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.transitioned_chars_in_scope(scope_id)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_check_in_scope(
        &self,
        scope_id: Option<u64>,
        char_id: i64,
        item_id: i64,
        is_corp_target: bool,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
    ) -> DedupeLookupOutcome {
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.check_in_scope(
            scope_id,
            char_id,
            item_id,
            is_corp_target,
            kind,
            fp,
            tombstone_wildcard_deleted,
        )
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

fn collect_invalidated_bp_owners(changes: &ChangeSet, sde: &SdeRegistry) -> HashSet<i64> {
    let mut owners = HashSet::new();
    for change in &changes.items {
        let before_rec = change.before.as_deref();
        let after_rec = change.after.as_deref();
        if before_rec.is_some_and(|r| is_blueprint_type(sde, r.type_id))
            || after_rec.is_some_and(|r| is_blueprint_type(sde, r.type_id))
        {
            if let Some(b) = before_rec {
                owners.insert(b.owner_id);
            }
            if let Some(a) = after_rec {
                owners.insert(a.owner_id);
            }
        }
    }
    owners
}

/// Evaluates `changes.items` under active [`ProjectedItemSwitchConfig`], updating dispatch/outside
/// item & blueprint facts and returning `OnItemChange` and `OnBlueprintsUpdated` specs.
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
    dispatch_scope_id: Option<u64>,
    explicit_acting_char_id: Option<i64>,
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
    let in_dispatch = dispatch_scope_id.is_some();
    if !in_dispatch && !cfg.char_same_scene_item_outside_rpc {
        let invalidated_bp_owners = collect_invalidated_bp_owners(changes, sde);
        if !changes.items.is_empty()
            || !transitioned_chars.is_empty()
            || !invalidated_bp_owners.is_empty()
        {
            switches.apply_commit_dedupe_updates_with_mode(
                None,
                false,
                changes,
                &transitioned_chars,
                Vec::new(),
                &invalidated_bp_owners,
                &[],
            );
        }
        return SwitchedCommitBatch::default();
    }

    if dispatch_scope_id.is_some() {
        let _ = register_dispatch_switch_handle(pend, notify_tx, switches);
    }
    let empty_bp = HashSet::new();
    if changes.items.is_empty() {
        if !transitioned_chars.is_empty() {
            switches.apply_commit_dedupe_updates_with_mode(
                dispatch_scope_id,
                cfg.char_same_scene_item_outside_rpc,
                changes,
                &transitioned_chars,
                Vec::new(),
                &empty_bp,
                &[],
            );
        }
        return SwitchedCommitBatch::default();
    }

    // Snapshot characters already transitioned earlier in this dispatch so subsequent commits in
    // the same dispatch that omit `changes.characters` still stay excluded under Guardrail 2.
    let prior_transitioned_chars = dispatch_scope_id
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
    let invalidated_bp_owners = collect_invalidated_bp_owners(changes, sde);
    let mut blueprint_tokens_set: BTreeSet<(i64, i64)> = BTreeSet::new();
    let mut blueprint_specs_map: BTreeMap<(i64, i64), u64> = BTreeMap::new();
    let mut corp_recipients_cache: HashMap<i64, Vec<&CharacterRecord>> = HashMap::new();
    let char_all_bp_active = cfg.is_char_all_items_enabled_in_scope(in_dispatch);
    let corp_all_bp_active = cfg.is_corp_all_items_enabled_in_scope(in_dispatch);

    for change in &changes.items {
        let before_rec = change.before.as_deref();
        let after_rec = change.after.as_deref();
        let before_is_bp = before_rec.is_some_and(|r| is_blueprint_type(sde, r.type_id));
        let after_is_bp = after_rec.is_some_and(|r| is_blueprint_type(sde, r.type_id));
        let bp_involved = before_is_bp || after_is_bp;

        // Leave Cargo fuel items held under an active rollback reserve untouched here so silent
        // actor-validation rollbacks emit zero notifications and deferred diffs stay authoritative (#1486).
        if active_fuel_reserves.is_item_reserved(db_key, change.key) {
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
                                blueprint_specs_map
                                    .entry((owner_id, owner_id))
                                    .or_insert(ch.user_id);
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
                            blueprint_specs_map
                                .entry((cid, owner_id))
                                .or_insert(ch.user_id);
                        }
                    }
                }
            }
        }

        let acting_char_id = explicit_acting_char_id.or_else(|| {
            in_dispatch
                .then(super::dispatch_active_char::active_dispatch_char)
                .flatten()
        });
        let classified_recipients = collect_switched_item_recipients_for_change(
            in_dispatch,
            cfg,
            acting_char_id,
            change,
            snap_after,
            changes,
            &item_changes_by_id,
            &changed_chars,
            online,
            sde,
            &mut corp_recipients_cache,
        );
        let mut eligible_recipients = Vec::with_capacity(classified_recipients.len());
        for recip in classified_recipients {
            let recipient_char_id = recip.character.character_id;
            if prior_transitioned_chars.contains(&recipient_char_id)
                && recip.character.active_ship_id > 0
                && change.key == recip.character.active_ship_id
            {
                continue;
            }
            if suppressed_effects.contains_item(recipient_char_id, change.key) {
                continue;
            }
            eligible_recipients.push(recip);
        }
        if eligible_recipients.is_empty() {
            continue;
        }

        let Some(item_payload) = build_projected_item_change_args(change, changes, snap_after, sde) else {
            continue;
        };
        let first_fp_rec = eligible_recipients[0].fp_rec;
        let fp = ItemWireFingerprint::from_record(first_fp_rec);
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

        let eligible_count = eligible_recipients.len();
        for (idx, recip) in eligible_recipients.into_iter().enumerate() {
            let recipient_char_id = recip.character.character_id;
            let is_last = idx + 1 == eligible_count;
            tokens.push(CommitProjectedToken {
                char_id: recipient_char_id,
                item_id: change.key,
                is_corp_target: recip.id_type == "corpid",
                kind: recip.op_kind,
                fingerprint: fp,
            });
            if is_last {
                specs.push(SwitchedProjectedItemSpec {
                    recipient_char_id,
                    recipient_user_id: recip.character.user_id,
                    id_type: recip.id_type,
                    args: item_payload,
                    industry_scopes,
                });
                break;
            }
            specs.push(SwitchedProjectedItemSpec {
                recipient_char_id,
                recipient_user_id: recip.character.user_id,
                id_type: recip.id_type,
                args: item_payload.clone(),
                industry_scopes: industry_scopes.clone(),
            });
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
        dispatch_scope_id,
        cfg.char_same_scene_item_outside_rpc,
        changes,
        &transitioned_chars,
        tokens,
        &invalidated_bp_owners,
        &bp_tokens_vec,
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

pub(crate) use super::delta_item_switch_ctx::build_fitted_item_trashed_args;
