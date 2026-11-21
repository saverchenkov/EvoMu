//! Temporary compare-only telemetry for item & blueprint delta projection (`#931` Track 2).
//!
//! Runs [`DefaultDeltaProjection::items_only`] after commits that mutate `changes.items`
//! without emitting any projected notifications, comparing projected expectations against live
//! hand-written `OnItemChange`, standalone `OnMultipleItemChange`, and `OnBlueprintsUpdated`
//! notifications. Designed as a single self-contained module so compare mode can be removed by
//! deleting this file once per-helper `hand_written_only == 0` parity is verified.
//!
//! Environment controls:
//! - `EVOMU_DELTA_ITEM_COMPARE`: enabled by default when unset; set `0` / `false` / `off` / `no`
//!   to bypass compare projection and legacy telemetry recording cleanly.
//! - `EVOMU_ITEM_COMPARE_REPORT_PATH`: optional path for append-only NDJSON summary records
//!   emitted on shutdown/bridge drop (aggregating across multi-binary tests and playtester runs).

#![forbid(unsafe_code)]

use crate::router::context::ServerContext;
use crate::router::delta_projection::{
    DefaultDeltaProjection, DeltaProjectionInput, OnlineCharacterLookup,
};
use eve_db::{ChangeSet, WorldStateSnapshot};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;

const MAX_COMPARE_SAMPLES: usize = 16;
const MAX_NDJSON_SAMPLES: usize = 4;
const MAX_RECENT_COMPARE_RECORDS: usize = 4096;
const MAX_COMPARE_BREAKDOWN_BUCKETS: usize = 512;
const OTHER_CLASS_BUCKET: &str = "<other_class>";
const OTHER_RPC_BUCKET: &str = "<other_rpc>";

pub(crate) const PAIRED_MULTI_ITEM_SKIP_SOURCE: &str = "__paired_multi_item_skip__";

/// Parses `EVOMU_DELTA_ITEM_COMPARE` value (`true` when unset or non-disable value).
pub(crate) fn item_compare_enabled_from_str(raw: Option<&str>) -> bool {
    !matches!(
        raw.map(|s| s.trim().to_ascii_lowercase()).as_deref(),
        Some("0" | "false" | "off" | "no")
    )
}

/// Returns whether item/blueprint compare-only telemetry is enabled (`EVOMU_DELTA_ITEM_COMPARE`, default `true`).
pub fn is_item_compare_enabled() -> bool {
    let raw = std::env::var_os("EVOMU_DELTA_ITEM_COMPARE");
    let raw_str = raw.as_deref().map(|s| s.to_string_lossy());
    item_compare_enabled_from_str(raw_str.as_deref())
}

fn env_report_path() -> Option<PathBuf> {
    std::env::var_os("EVOMU_ITEM_COMPARE_REPORT_PATH").and_then(|p| {
        if p.is_empty() {
            None
        } else {
            Some(PathBuf::from(p))
        }
    })
}

/// Execution context where a projected commit or hand-written notify occurred (`#931` Track 2.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompareOriginScope {
    Dispatch,
    Tick,
    Neither,
}

impl CompareOriginScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dispatch => "dispatch",
            Self::Tick => "tick",
            Self::Neither => "neither",
        }
    }
}

impl std::fmt::Display for CompareOriginScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

fn is_valid_rpc_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg.len() <= 64
        && !seg
            .bytes()
            .any(|b| b.is_ascii_control() || b.is_ascii_whitespace() || b == b'.')
}

pub(crate) fn normalize_dispatch_method_label(service: &str, method: &str) -> String {
    if is_valid_rpc_segment(method) {
        if is_valid_rpc_segment(service) {
            let label = format!("{service}.{method}");
            if label.len() <= 128 {
                return label;
            }
        } else if service.is_empty() {
            return method.to_string();
        }
    }
    crate::flight_recorder::UNKNOWN_RPC_LABEL.to_string()
}

pub(crate) struct DispatchRpcMethodGuard {
    pend_ptr: Option<usize>,
}

impl Drop for DispatchRpcMethodGuard {
    fn drop(&mut self) {
        if let Some(pend_ptr) = self.pend_ptr {
            super::cache_notifications::pop_dispatch_rpc_method(pend_ptr);
        }
    }
}

pub(crate) fn push_dispatch_rpc_method_guard(
    pend: &dyn eve_db::lock_rank::AnyRwLock<
        HashMap<i64, Vec<super::cache_notifications::PendingMachoNotification>>,
    >,
    notify_tx: &tokio::sync::broadcast::Sender<(
        i64,
        super::cache_notifications::PendingMachoNotification,
    )>,
    rpc_method: String,
) -> DispatchRpcMethodGuard {
    DispatchRpcMethodGuard {
        pend_ptr: super::cache_notifications::push_dispatch_rpc_method(
            pend, notify_tx, rpc_method,
        ),
    }
}

/// Three-way execution-scope breakdown (`dispatch`, `tick`, `neither`) (`#931` Track 2.5).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemCompareByScope<T> {
    pub dispatch: T,
    pub tick: T,
    pub neither: T,
}

impl<T> ItemCompareByScope<T> {
    fn get_mut(&mut self, scope: CompareOriginScope) -> &mut T {
        match scope {
            CompareOriginScope::Dispatch => &mut self.dispatch,
            CompareOriginScope::Tick => &mut self.tick,
            CompareOriginScope::Neither => &mut self.neither,
        }
    }
}

/// Compare counts for one [`CompareOriginScope`] bucket (`#931` Track 2.5 / 3A).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemCompareScopeCounts {
    pub matched: u64,
    pub hand_written_only: u64,
    pub projected_only: u64,
    pub pending: usize,
    pub by_helper: BTreeMap<String, HelperCompareCounts>,
    pub projected_only_by_class: BTreeMap<String, u64>,
    pub projected_only_by_dispatch_method: BTreeMap<String, u64>,
    pub sample_projected_only: Vec<String>,
    pub sample_hand_written_only: Vec<String>,
}

/// Per-helper compare counts (`matched`, `hand_written_only`, `projected_only`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HelperCompareCounts {
    pub matched: u64,
    pub hand_written_only: u64,
    pub projected_only: u64,
}

/// Compare counts for one item/blueprint event (`OnItemChange` or `OnBlueprintsUpdated`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemCompareEventSummary {
    pub matched: u64,
    pub matched_across_scopes: u64,
    pub matched_across_scopes_by_helper: BTreeMap<String, u64>,
    pub matched_across_scopes_by_scope_pair: BTreeMap<String, u64>,
    pub hand_written_only: u64,
    pub projected_only: u64,
    pub pending: usize,
    pub by_helper: BTreeMap<String, HelperCompareCounts>,
    pub projected_only_by_class: BTreeMap<String, u64>,
    pub projected_only_by_dispatch_method: BTreeMap<String, u64>,
    pub sample_projected_only: Vec<String>,
    pub sample_hand_written_only: Vec<String>,
    pub by_scope: ItemCompareByScope<ItemCompareScopeCounts>,
}

/// Combined compare-only diagnostics for `#931` Tracks 2 & 3A (`OnItemChange` & `OnBlueprintsUpdated`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemCompareSummary {
    pub item_change: ItemCompareEventSummary,
    pub blueprints_updated: ItemCompareEventSummary,
    pub explicit_in_tx_effects: u64,
    pub explicit_in_tx_effects_by_scope: ItemCompareByScope<u64>,
}

impl ItemCompareSummary {
    fn total_activity(&self) -> u64 {
        self.item_change.matched
            + self.item_change.matched_across_scopes
            + self.item_change.hand_written_only
            + self.item_change.projected_only
            + self.blueprints_updated.matched
            + self.blueprints_updated.matched_across_scopes
            + self.blueprints_updated.hand_written_only
            + self.blueprints_updated.projected_only
            + self.explicit_in_tx_effects
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ItemCompareNdjsonRecord {
    pid: u32,
    bridge_id: u64,
    item_change: ItemCompareEventSummary,
    blueprints_updated: ItemCompareEventSummary,
    explicit_in_tx_effects: u64,
    explicit_in_tx_effects_by_scope: ItemCompareByScope<u64>,
}

fn increment_breakdown_bucket(
    map: &mut BTreeMap<String, u64>,
    key: &str,
    overflow_key: &str,
) -> String {
    let target = if map.contains_key(key) || map.len() < MAX_COMPARE_BREAKDOWN_BUCKETS {
        key
    } else {
        overflow_key
    };
    *map.entry(target.to_string()).or_insert(0) += 1;
    target.to_string()
}

fn decrement_breakdown_bucket(map: &mut BTreeMap<String, u64>, key: &str, overflow_key: &str) {
    let target = if map.contains_key(key) {
        key
    } else if map.contains_key(overflow_key) {
        overflow_key
    } else {
        return;
    };
    if let Some(cnt) = map.get_mut(target) {
        *cnt = cnt.saturating_sub(1);
        if *cnt == 0 {
            map.remove(target);
        }
    }
}

fn merge_breakdown_counts(dst: &mut BTreeMap<String, u64>, src: &BTreeMap<String, u64>) {
    for (k, v) in src {
        *dst.entry(k.clone()).or_insert(0) += v;
    }
}

#[derive(Debug, Default, Clone)]
struct CrossScopeCompareCounters {
    matched_across_scopes: u64,
    by_helper: BTreeMap<String, u64>,
    by_scope_pair: BTreeMap<String, u64>,
}

impl CrossScopeCompareCounters {
    fn record_pair(
        &mut self,
        helper: &str,
        proj_scope: CompareOriginScope,
        hw_scope: CompareOriginScope,
    ) {
        self.matched_across_scopes += 1;
        *self.by_helper.entry(helper.to_string()).or_insert(0) += 1;
        let pair_key = format!(
            "projected:{}->hand_written:{}",
            proj_scope.as_str(),
            hw_scope.as_str()
        );
        *self.by_scope_pair.entry(pair_key).or_insert(0) += 1;
    }
}

#[derive(Debug, Default)]
struct CompareCounters {
    matched: u64,
    hand_written_only: u64,
    projected_only: u64,
    by_helper: BTreeMap<String, HelperCompareCounts>,
    projected_only_by_class: BTreeMap<String, u64>,
    projected_only_by_dispatch_method: BTreeMap<String, u64>,
    sample_projected_only: Vec<(u64, String)>,
    sample_hand_written_only: Vec<(u64, String)>,
}

impl CompareCounters {
    fn record_matched(&mut self, helper: &str) {
        self.matched += 1;
        self.by_helper.entry(helper.to_string()).or_default().matched += 1;
    }

    fn record_hand_written_only(
        &mut self,
        seq: u64,
        helper: &str,
        detail: impl FnOnce() -> String,
    ) {
        self.hand_written_only += 1;
        self.by_helper
            .entry(helper.to_string())
            .or_default()
            .hand_written_only += 1;
        if self.sample_hand_written_only.len() < MAX_COMPARE_SAMPLES {
            let msg = detail();
            tracing::debug!(target: "delta_item_compare", "hand_written_only: {msg}");
            self.sample_hand_written_only.push((seq, msg));
        }
    }

    fn undo_hand_written_only(&mut self, seq: u64, helper: &str) {
        self.hand_written_only = self.hand_written_only.saturating_sub(1);
        if let Some(counts) = self.by_helper.get_mut(helper) {
            counts.hand_written_only = counts.hand_written_only.saturating_sub(1);
            if counts.matched == 0 && counts.hand_written_only == 0 && counts.projected_only == 0 {
                self.by_helper.remove(helper);
            }
        }
        self.sample_hand_written_only.retain(|(s, _)| *s != seq);
    }

    fn record_projected_only(
        &mut self,
        seq: u64,
        mismatched_helper: Option<&str>,
        class_key: Option<&str>,
        dispatch_method: Option<&str>,
        detail: impl FnOnce() -> String,
    ) -> (Option<String>, Option<String>) {
        self.projected_only += 1;
        let bucket = mismatched_helper.unwrap_or("<no_helper>");
        self.by_helper
            .entry(bucket.to_string())
            .or_default()
            .projected_only += 1;
        let recorded_class = class_key.map(|ck| {
            increment_breakdown_bucket(&mut self.projected_only_by_class, ck, OTHER_CLASS_BUCKET)
        });
        let recorded_method = dispatch_method.map(|dm| {
            increment_breakdown_bucket(
                &mut self.projected_only_by_dispatch_method,
                dm,
                OTHER_RPC_BUCKET,
            )
        });
        if self.sample_projected_only.len() < MAX_COMPARE_SAMPLES {
            let msg = detail();
            tracing::debug!(target: "delta_item_compare", "projected_only: {msg}");
            self.sample_projected_only.push((seq, msg));
        }
        (recorded_class, recorded_method)
    }

    fn undo_projected_only(
        &mut self,
        seq: u64,
        mismatched_helper: Option<&str>,
        class_bucket: Option<&str>,
        method_bucket: Option<&str>,
    ) {
        self.projected_only = self.projected_only.saturating_sub(1);
        let bucket = mismatched_helper.unwrap_or("<no_helper>");
        if let Some(counts) = self.by_helper.get_mut(bucket) {
            counts.projected_only = counts.projected_only.saturating_sub(1);
            if counts.matched == 0 && counts.hand_written_only == 0 && counts.projected_only == 0 {
                self.by_helper.remove(bucket);
            }
        }
        if let Some(cb) = class_bucket {
            decrement_breakdown_bucket(&mut self.projected_only_by_class, cb, OTHER_CLASS_BUCKET);
        }
        if let Some(mb) = method_bucket {
            decrement_breakdown_bucket(
                &mut self.projected_only_by_dispatch_method,
                mb,
                OTHER_RPC_BUCKET,
            );
        }
        self.sample_projected_only.retain(|(s, _)| *s != seq);
    }

    fn to_scope_counts(&self, pending: usize) -> ItemCompareScopeCounts {
        ItemCompareScopeCounts {
            matched: self.matched,
            hand_written_only: self.hand_written_only,
            projected_only: self.projected_only,
            pending,
            by_helper: self.by_helper.clone(),
            projected_only_by_class: self.projected_only_by_class.clone(),
            projected_only_by_dispatch_method: self.projected_only_by_dispatch_method.clone(),
            sample_projected_only: self
                .sample_projected_only
                .iter()
                .map(|(_, s)| s.clone())
                .collect(),
            sample_hand_written_only: self
                .sample_hand_written_only
                .iter()
                .map(|(_, s)| s.clone())
                .collect(),
        }
    }
}

impl ItemCompareByScope<CompareCounters> {
    fn to_summary(
        &self,
        pending: ItemCompareByScope<usize>,
        cross_scope: &CrossScopeCompareCounters,
    ) -> ItemCompareEventSummary {
        let mut total = CompareCounters::default();
        let push_samples = |dst: &mut Vec<(u64, String)>, src: &[(u64, String)]| {
            dst.extend(
                src.iter()
                    .take(MAX_COMPARE_SAMPLES.saturating_sub(dst.len()))
                    .cloned(),
            );
        };
        for sc in [&self.dispatch, &self.tick, &self.neither] {
            total.matched += sc.matched;
            total.hand_written_only += sc.hand_written_only;
            total.projected_only += sc.projected_only;
            for (k, v) in &sc.by_helper {
                let d = total.by_helper.entry(k.clone()).or_default();
                d.matched += v.matched;
                d.hand_written_only += v.hand_written_only;
                d.projected_only += v.projected_only;
            }
            merge_breakdown_counts(&mut total.projected_only_by_class, &sc.projected_only_by_class);
            merge_breakdown_counts(
                &mut total.projected_only_by_dispatch_method,
                &sc.projected_only_by_dispatch_method,
            );
            push_samples(&mut total.sample_projected_only, &sc.sample_projected_only);
            push_samples(
                &mut total.sample_hand_written_only,
                &sc.sample_hand_written_only,
            );
        }
        ItemCompareEventSummary {
            matched: total.matched,
            matched_across_scopes: cross_scope.matched_across_scopes,
            matched_across_scopes_by_helper: cross_scope.by_helper.clone(),
            matched_across_scopes_by_scope_pair: cross_scope.by_scope_pair.clone(),
            hand_written_only: total.hand_written_only,
            projected_only: total.projected_only,
            pending: pending.dispatch + pending.tick + pending.neither,
            by_helper: total.by_helper,
            projected_only_by_class: total.projected_only_by_class,
            projected_only_by_dispatch_method: total.projected_only_by_dispatch_method,
            sample_projected_only: total
                .sample_projected_only
                .into_iter()
                .map(|(_, s)| s)
                .collect(),
            sample_hand_written_only: total
                .sample_hand_written_only
                .into_iter()
                .map(|(_, s)| s)
                .collect(),
            by_scope: ItemCompareByScope {
                dispatch: self.dispatch.to_scope_counts(pending.dispatch),
                tick: self.tick.to_scope_counts(pending.tick),
                neither: self.neither.to_scope_counts(pending.neither),
            },
        }
    }
}

fn pending_counts_by_scope<T>(q: &VecDeque<T>, f: impl Fn(&T) -> CompareOriginScope) -> ItemCompareByScope<usize> {
    let mut out = ItemCompareByScope::default();
    q.iter().for_each(|entry| *out.get_mut(f(entry)) += 1);
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ItemChangeFields {
    item_id: i64,
    type_id: Option<i64>,
    owner_id: i64,
    location_id: i64,
    flag_id: i64,
    wire_qty: i64,
}

impl ItemChangeFields {
    fn matches(&self, other: &Self) -> bool {
        self.item_id == other.item_id
            && self.owner_id == other.owner_id
            && self.location_id == other.location_id
            && self.flag_id == other.flag_id
            && self.wire_qty == other.wire_qty
            && (self.type_id.is_none()
                || other.type_id.is_none()
                || self.type_id == other.type_id)
    }
}

#[derive(Debug, Clone)]
struct PendingItemCompare {
    char_id: i64,
    id_type: &'static str,
    fields: ItemChangeFields,
    class_key: String,
    scope: CompareOriginScope,
    dispatch_method: Option<String>,
    mismatched_helper: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingBlueprintCompare {
    char_id: i64,
    owner_id: i64,
    scope: CompareOriginScope,
    dispatch_method: Option<String>,
    mismatched_helper: Option<String>,
}

#[derive(Debug, Clone)]
struct UnmatchedProjectedItem {
    seq: u64,
    char_id: i64,
    id_type: &'static str,
    fields: ItemChangeFields,
    recorded_class_key: String,
    scope: CompareOriginScope,
    recorded_dispatch_method: Option<String>,
}

#[derive(Debug, Clone)]
struct UnmatchedHandWrittenItem {
    seq: u64,
    char_id: i64,
    id_type: String,
    fields: ItemChangeFields,
    scope: CompareOriginScope,
    helper: String,
}

#[derive(Debug, Clone)]
struct UnmatchedProjectedBp {
    seq: u64,
    char_id: i64,
    owner_id: i64,
    scope: CompareOriginScope,
    recorded_dispatch_method: Option<String>,
}

#[derive(Debug, Clone)]
struct UnmatchedHandWrittenBp {
    seq: u64,
    char_id: i64,
    owner_id: i64,
    scope: CompareOriginScope,
    helper: String,
}

fn push_window_capped<T>(queue: &mut VecDeque<T>, entry: T) {
    while queue.len() >= MAX_RECENT_COMPARE_RECORDS {
        let _ = queue.pop_front();
    }
    queue.push_back(entry);
}

fn select_best_cross_scope_index<T>(
    items: &VecDeque<T>,
    mut candidate: impl FnMut(&T) -> Option<(bool, bool)>,
) -> Option<usize> {
    let mut field_match_idx = None;
    let mut any_match_idx = None;
    for (idx, item) in items.iter().enumerate() {
        if let Some((fields_match, id_type_match)) = candidate(item) {
            if fields_match && id_type_match {
                return Some(idx);
            }
            if fields_match && field_match_idx.is_none() {
                field_match_idx = Some(idx);
            } else if any_match_idx.is_none() {
                any_match_idx = Some(idx);
            }
        }
    }
    field_match_idx.or(any_match_idx)
}

type ItemBeforeAfter<'a> = (
    Option<&'a eve_db::InventoryItemRecord>,
    Option<&'a eve_db::InventoryItemRecord>,
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

fn classify_location_kind(
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

fn classify_owner_kind(
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

fn classify_projected_item(
    before_rec: Option<&eve_db::InventoryItemRecord>,
    fields: &ItemChangeFields,
    snap: &WorldStateSnapshot,
    changes: &ChangeSet,
    item_changes_by_id: &HashMap<i64, ItemBeforeAfter<'_>>,
    sde: &SdeRegistry,
) -> String {
    let loc_before = classify_location_kind(
        before_rec.map(|r| r.location_id),
        snap,
        changes,
        item_changes_by_id,
        sde,
    );
    let loc_after = classify_location_kind(
        Some(fields.location_id),
        snap,
        changes,
        item_changes_by_id,
        sde,
    );
    let owner_kind = classify_owner_kind(fields.owner_id, snap, changes, sde);
    let singleton = fields.wire_qty < 0;
    format!(
        "before={},after={},flag={},owner={},singleton={}",
        loc_before, loc_after, fields.flag_id, owner_kind, singleton
    )
}

fn row_col_i64(row: &PyRep, col: &str) -> Option<i64> {
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

fn extract_item_change_fields_from_row(row: &PyRep) -> Option<ItemChangeFields> {
    Some(ItemChangeFields {
        item_id: row_col_i64(row, "itemID")?,
        type_id: row_col_i64(row, "typeID"),
        owner_id: row_col_i64(row, "ownerID")?,
        location_id: row_col_i64(row, "locationID")?,
        flag_id: row_col_i64(row, "flagID")?,
        wire_qty: row_col_i64(row, "quantity")?,
    })
}

fn extract_item_change_fields(payload_args: &[PyRep]) -> Option<ItemChangeFields> {
    extract_item_change_fields_from_row(payload_args.first()?)
}

fn extract_blueprint_owner_id(payload_args: &[PyRep]) -> Option<i64> {
    payload_args.first().and_then(PyRep::as_i64)
}

fn format_item_fields(f: &ItemChangeFields) -> String {
    format!(
        "item={} type={:?} owner={} loc={} flag={} qty={}",
        f.item_id, f.type_id, f.owner_id, f.location_id, f.flag_id, f.wire_qty
    )
}

fn push_pending_capped<T>(
    queue: &mut VecDeque<T>,
    item: T,
    mut matches_same_key: impl FnMut(&T) -> bool,
    mut on_retire: impl FnMut(T),
) {
    let mut idx = 0;
    while idx < queue.len() {
        if matches_same_key(&queue[idx]) {
            if let Some(prev) = queue.remove(idx) {
                on_retire(prev);
            }
        } else {
            idx += 1;
        }
    }
    while queue.len() >= MAX_RECENT_COMPARE_RECORDS {
        if let Some(evicted) = queue.pop_front() {
            on_retire(evicted);
        }
    }
    queue.push_back(item);
}

/// Tracks explicit in-transaction item/blueprint `TxNotificationEffect`s emitted during `WorldRepository::tx`.
#[derive(Debug, Default)]
pub(crate) struct TxItemEffectKeys {
    item_keys: HashSet<(i64, i64)>,
    blueprint_keys: HashSet<(i64, i64)>,
    explicit_count: u64,
}

impl TxItemEffectKeys {
    pub(crate) fn observe(&mut self, char_id: i64, event: &str, payload_args: &[PyRep]) {
        if !is_item_compare_enabled() {
            return;
        }
        match event {
            "OnItemChange" => {
                if let Some(fields) = extract_item_change_fields(payload_args) {
                    self.item_keys.insert((char_id, fields.item_id));
                }
                self.explicit_count += 1;
            }
            "OnMultipleItemChange" => {
                if let Some(PyRep::List(rows) | PyRep::Tuple(rows)) = payload_args.first() {
                    for row in rows {
                        if let Some(fields) = extract_item_change_fields_from_row(row) {
                            self.item_keys.insert((char_id, fields.item_id));
                        }
                    }
                    self.explicit_count += rows.len().max(1) as u64;
                } else {
                    self.explicit_count += 1;
                }
            }
            "OnBlueprintsUpdated" => {
                if let Some(owner_id) = extract_blueprint_owner_id(payload_args) {
                    self.blueprint_keys.insert((char_id, owner_id));
                }
                self.explicit_count += 1;
            }
            _ => {}
        }
    }
}

#[derive(Debug, Default)]
struct ItemCompareLedger {
    next_entry_seq: u64,
    pending_items: VecDeque<PendingItemCompare>,
    pending_bps: VecDeque<PendingBlueprintCompare>,
    unmatched_projected_items: VecDeque<UnmatchedProjectedItem>,
    unmatched_hand_written_items: VecDeque<UnmatchedHandWrittenItem>,
    unmatched_projected_bps: VecDeque<UnmatchedProjectedBp>,
    unmatched_hand_written_bps: VecDeque<UnmatchedHandWrittenBp>,
    item_counts: ItemCompareByScope<CompareCounters>,
    bp_counts: ItemCompareByScope<CompareCounters>,
    item_cross_scope: CrossScopeCompareCounters,
    bp_cross_scope: CrossScopeCompareCounters,
    explicit_in_tx_effects: u64,
    explicit_in_tx_effects_by_scope: ItemCompareByScope<u64>,
    // Synchronous call-stack helper labels keyed by ThreadId so fixed-signature `ServerContext::notify`
    // retains the single ratcheted `pending_notifications.write()` call site (`::notify::pending_notifications`)
    // and never alters notification delivery.
    active_sources_by_thread: HashMap<ThreadId, Vec<&'static str>>,
    tick_sync_depths_by_thread: HashMap<ThreadId, u32>,
    enabled_override: Option<bool>,
    report_path_override: Option<Option<PathBuf>>,
    reported_to_file: bool,
    logged_summary: bool,
}

impl ItemCompareLedger {
    fn next_seq(&mut self) -> u64 {
        self.next_entry_seq += 1;
        self.next_entry_seq
    }

    fn is_enabled(&self) -> bool {
        self.enabled_override
            .unwrap_or_else(is_item_compare_enabled)
    }

    fn resolved_report_path(&self) -> Option<PathBuf> {
        match &self.report_path_override {
            Some(path_opt) => path_opt.clone(),
            None => env_report_path(),
        }
    }

    fn retire_item(&mut self, entry: PendingItemCompare) {
        if entry.mismatched_helper.is_none() {
            if let Some(pos) = select_best_cross_scope_index(
                &self.unmatched_hand_written_items,
                |hw| {
                    (hw.scope != entry.scope
                        && hw.char_id == entry.char_id
                        && hw.fields.item_id == entry.fields.item_id)
                        .then(|| {
                            (
                                entry.fields.matches(&hw.fields),
                                hw.id_type == entry.id_type,
                            )
                        })
                },
            ) {
                let hw = self
                    .unmatched_hand_written_items
                    .remove(pos)
                    .expect("cross-scope hand-written item index valid");
                self.item_counts
                    .get_mut(hw.scope)
                    .undo_hand_written_only(hw.seq, &hw.helper);
                self.item_cross_scope
                    .record_pair(&hw.helper, entry.scope, hw.scope);
                return;
            }
        }
        let seq = self.next_seq();
        let helper = entry.mismatched_helper.clone();
        let class_key = entry.class_key.clone();
        let dispatch_method = entry.dispatch_method.clone();
        let fields_desc = format_item_fields(&entry.fields);
        let (recorded_class, recorded_method) = self
            .item_counts
            .get_mut(entry.scope)
            .record_projected_only(
                seq,
                helper.as_deref(),
                Some(&class_key),
                dispatch_method.as_deref(),
                || {
                    format!(
                        "cid={} id_type={} {} class={} rpc={:?} mismatched_helper={:?}",
                        entry.char_id,
                        entry.id_type,
                        fields_desc,
                        class_key,
                        dispatch_method,
                        helper
                    )
                },
            );
        if entry.mismatched_helper.is_none() {
            push_window_capped(
                &mut self.unmatched_projected_items,
                UnmatchedProjectedItem {
                    seq,
                    char_id: entry.char_id,
                    id_type: entry.id_type,
                    fields: entry.fields,
                    recorded_class_key: recorded_class.unwrap_or(entry.class_key),
                    scope: entry.scope,
                    recorded_dispatch_method: recorded_method,
                },
            );
        }
    }

    fn retire_bp(&mut self, entry: PendingBlueprintCompare) {
        if entry.mismatched_helper.is_none() {
            if let Some(pos) = self.unmatched_hand_written_bps.iter().position(|hw| {
                hw.scope != entry.scope
                    && hw.char_id == entry.char_id
                    && hw.owner_id == entry.owner_id
            }) {
                let hw = self
                    .unmatched_hand_written_bps
                    .remove(pos)
                    .expect("cross-scope hand-written bp index valid");
                self.bp_counts
                    .get_mut(hw.scope)
                    .undo_hand_written_only(hw.seq, &hw.helper);
                self.bp_cross_scope
                    .record_pair(&hw.helper, entry.scope, hw.scope);
                return;
            }
        }
        let seq = self.next_seq();
        let helper = entry.mismatched_helper.clone();
        let dispatch_method = entry.dispatch_method.clone();
        let (_, recorded_method) = self.bp_counts.get_mut(entry.scope).record_projected_only(
            seq,
            helper.as_deref(),
            None,
            dispatch_method.as_deref(),
            || {
                format!(
                    "cid={} owner={} rpc={:?} mismatched_helper={:?}",
                    entry.char_id, entry.owner_id, dispatch_method, helper
                )
            },
        );
        if entry.mismatched_helper.is_none() {
            push_window_capped(
                &mut self.unmatched_projected_bps,
                UnmatchedProjectedBp {
                    seq,
                    char_id: entry.char_id,
                    owner_id: entry.owner_id,
                    scope: entry.scope,
                    recorded_dispatch_method: recorded_method,
                },
            );
        }
    }

    fn record_projected_batch(
        &mut self,
        scope: CompareOriginScope,
        dispatch_method: Option<String>,
        projected_items: Vec<(i64, &'static str, ItemChangeFields, String)>,
        projected_bps: Vec<(i64, i64)>,
        explicit_effects: u64,
    ) {
        if !projected_items.is_empty() || !projected_bps.is_empty() || explicit_effects > 0 {
            self.reported_to_file = false;
            self.logged_summary = false;
        }
        self.explicit_in_tx_effects += explicit_effects;
        *self.explicit_in_tx_effects_by_scope.get_mut(scope) += explicit_effects;
        let effective_dispatch_method = if scope == CompareOriginScope::Dispatch {
            Some(
                dispatch_method
                    .unwrap_or_else(|| crate::flight_recorder::UNKNOWN_RPC_LABEL.to_string()),
            )
        } else {
            None
        };
        let mut retired_items = Vec::new();
        for (char_id, id_type, fields, class_key) in projected_items {
            let item_id = fields.item_id;
            push_pending_capped(
                &mut self.pending_items,
                PendingItemCompare {
                    char_id,
                    id_type,
                    fields,
                    class_key,
                    scope,
                    dispatch_method: effective_dispatch_method.clone(),
                    mismatched_helper: None,
                },
                |p| p.char_id == char_id && p.id_type == id_type && p.fields.item_id == item_id,
                |old| retired_items.push(old),
            );
        }
        for old in retired_items {
            self.retire_item(old);
        }

        let mut retired_bps = Vec::new();
        for (char_id, owner_id) in projected_bps {
            push_pending_capped(
                &mut self.pending_bps,
                PendingBlueprintCompare {
                    char_id,
                    owner_id,
                    scope,
                    dispatch_method: effective_dispatch_method.clone(),
                    mismatched_helper: None,
                },
                |p| p.char_id == char_id && p.owner_id == owner_id,
                |old| retired_bps.push(old),
            );
        }
        for old in retired_bps {
            self.retire_bp(old);
        }
    }

    fn match_or_record_item_row(
        &mut self,
        scope: CompareOriginScope,
        helper: &str,
        recipients: &[(i64, Option<u64>)],
        id_type: &str,
        actual_opt: Option<&ItemChangeFields>,
    ) {
        for &(cid, _) in recipients {
            let Some(actual) = actual_opt else {
                let seq = self.next_seq();
                self.item_counts
                    .get_mut(scope)
                    .record_hand_written_only(seq, helper, || {
                        format!("helper={helper} cid={cid} id_type={id_type} unparsed_args")
                    });
                continue;
            };
            if let Some(pos) = self.pending_items.iter().position(|p| {
                p.scope == scope
                    && p.char_id == cid
                    && p.id_type == id_type
                    && p.fields.matches(actual)
            }) {
                let _ = self.pending_items.remove(pos);
                self.item_counts.get_mut(scope).record_matched(helper);
            } else if let Some(p) = self.pending_items.iter_mut().rev().find(|p| {
                p.scope == scope && p.char_id == cid && p.fields.item_id == actual.item_id
            }) {
                p.mismatched_helper = Some(helper.to_string());
                let near_desc = format!(
                    " vs_projected(id_type={} {})",
                    p.id_type,
                    format_item_fields(&p.fields)
                );
                let seq = self.next_seq();
                self.item_counts
                    .get_mut(scope)
                    .record_hand_written_only(seq, helper, || {
                        format!(
                            "helper={helper} cid={cid} id_type={id_type} {}{near_desc}",
                            format_item_fields(actual)
                        )
                    });
            } else if let Some(pos) = select_best_cross_scope_index(&self.pending_items, |p| {
                (p.scope != scope
                    && p.mismatched_helper.is_none()
                    && p.char_id == cid
                    && p.fields.item_id == actual.item_id)
                    .then(|| (p.fields.matches(actual), p.id_type == id_type))
            }) {
                let matched_proj = self
                    .pending_items
                    .remove(pos)
                    .expect("cross-scope pending item index valid");
                self.item_cross_scope
                    .record_pair(helper, matched_proj.scope, scope);
            } else if let Some(pos) =
                select_best_cross_scope_index(&self.unmatched_projected_items, |u| {
                    (u.scope != scope
                        && u.char_id == cid
                        && u.fields.item_id == actual.item_id)
                        .then(|| (u.fields.matches(actual), u.id_type == id_type))
                })
            {
                let unmatched_proj = self
                    .unmatched_projected_items
                    .remove(pos)
                    .expect("cross-scope unmatched projected item index valid");
                self.item_counts
                    .get_mut(unmatched_proj.scope)
                    .undo_projected_only(
                        unmatched_proj.seq,
                        None,
                        Some(&unmatched_proj.recorded_class_key),
                        unmatched_proj.recorded_dispatch_method.as_deref(),
                    );
                self.item_cross_scope
                    .record_pair(helper, unmatched_proj.scope, scope);
            } else {
                let seq = self.next_seq();
                self.item_counts
                    .get_mut(scope)
                    .record_hand_written_only(seq, helper, || {
                        format!(
                            "helper={helper} cid={cid} id_type={id_type} {}",
                            format_item_fields(actual)
                        )
                    });
                push_window_capped(
                    &mut self.unmatched_hand_written_items,
                    UnmatchedHandWrittenItem {
                        seq,
                        char_id: cid,
                        id_type: id_type.to_string(),
                        fields: actual.clone(),
                        scope,
                        helper: helper.to_string(),
                    },
                );
            }
        }
    }

    fn record_hand_written(
        &mut self,
        scope: CompareOriginScope,
        default_helper: &str,
        recipients: &[(i64, Option<u64>)],
        id_type: &str,
        event: &str,
        payload_args: &[PyRep],
    ) {
        let tid = std::thread::current().id();
        let active_source = self
            .active_sources_by_thread
            .get(&tid)
            .and_then(|stack| stack.last().copied());
        if event == "OnMultipleItemChange" && active_source == Some(PAIRED_MULTI_ITEM_SKIP_SOURCE) {
            return;
        }
        let helper = match (active_source, event, default_helper) {
            (Some(src), _, _) => src,
            (None, "OnMultipleItemChange", "notify") => "OnMultipleItemChange",
            _ => default_helper,
        };
        self.reported_to_file = false;
        self.logged_summary = false;
        match event {
            "OnItemChange" => {
                let actual_opt = extract_item_change_fields(payload_args);
                self.match_or_record_item_row(scope, helper, recipients, id_type, actual_opt.as_ref());
            }
            "OnMultipleItemChange" => match payload_args.first() {
                Some(PyRep::List(rows) | PyRep::Tuple(rows)) if !rows.is_empty() => {
                    for row in rows {
                        let actual_opt = extract_item_change_fields_from_row(row);
                        self.match_or_record_item_row(scope, helper, recipients, id_type, actual_opt.as_ref());
                    }
                }
                _ => {
                    self.match_or_record_item_row(scope, helper, recipients, id_type, None);
                }
            },
            "OnBlueprintsUpdated" => {
                let owner_opt = extract_blueprint_owner_id(payload_args);
                for &(cid, _) in recipients {
                    let Some(owner_id) = owner_opt else {
                        let seq = self.next_seq();
                        self.bp_counts.get_mut(scope).record_hand_written_only(seq, helper, || {
                            format!("helper={helper} cid={cid} unparsed_owner")
                        });
                        continue;
                    };
                    if let Some(pos) = self.pending_bps.iter().position(|p| {
                        p.scope == scope && p.char_id == cid && p.owner_id == owner_id
                    }) {
                        let _ = self.pending_bps.remove(pos);
                        self.bp_counts.get_mut(scope).record_matched(helper);
                    } else if let Some(pos) = self.pending_bps.iter().position(|p| {
                        p.scope != scope
                            && p.mismatched_helper.is_none()
                            && p.char_id == cid
                            && p.owner_id == owner_id
                    }) {
                        let matched_bp = self
                            .pending_bps
                            .remove(pos)
                            .expect("cross-scope pending bp index valid");
                        self.bp_cross_scope
                            .record_pair(helper, matched_bp.scope, scope);
                    } else if let Some(pos) = self.unmatched_projected_bps.iter().position(|u| {
                        u.scope != scope && u.char_id == cid && u.owner_id == owner_id
                    }) {
                        let unmatched_bp = self
                            .unmatched_projected_bps
                            .remove(pos)
                            .expect("cross-scope unmatched projected bp index valid");
                        self.bp_counts.get_mut(unmatched_bp.scope).undo_projected_only(
                            unmatched_bp.seq,
                            None,
                            None,
                            unmatched_bp.recorded_dispatch_method.as_deref(),
                        );
                        self.bp_cross_scope
                            .record_pair(helper, unmatched_bp.scope, scope);
                    } else {
                        let seq = self.next_seq();
                        self.bp_counts.get_mut(scope).record_hand_written_only(seq, helper, || {
                            format!("helper={helper} cid={cid} owner={owner_id}")
                        });
                        push_window_capped(
                            &mut self.unmatched_hand_written_bps,
                            UnmatchedHandWrittenBp {
                                seq,
                                char_id: cid,
                                owner_id,
                                scope,
                                helper: helper.to_string(),
                            },
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn flush_pending(&mut self) -> ItemCompareSummary {
        while let Some(item) = self.pending_items.pop_front() {
            self.retire_item(item);
        }
        while let Some(bp) = self.pending_bps.pop_front() {
            self.retire_bp(bp);
        }
        self.unmatched_projected_items.clear();
        self.unmatched_hand_written_items.clear();
        self.unmatched_projected_bps.clear();
        self.unmatched_hand_written_bps.clear();
        self.summary()
    }

    fn summary(&self) -> ItemCompareSummary {
        ItemCompareSummary {
            item_change: self.item_counts.to_summary(
                pending_counts_by_scope(&self.pending_items, |p| p.scope),
                &self.item_cross_scope,
            ),
            blueprints_updated: self.bp_counts.to_summary(
                pending_counts_by_scope(&self.pending_bps, |p| p.scope),
                &self.bp_cross_scope,
            ),
            explicit_in_tx_effects: self.explicit_in_tx_effects,
            explicit_in_tx_effects_by_scope: self.explicit_in_tx_effects_by_scope.clone(),
        }
    }
}

fn collect_compare_item_expectations(
    item_projector: &DefaultDeltaProjection,
    snap: &WorldStateSnapshot,
    changes: &ChangeSet,
    sde: &SdeRegistry,
    now_unix: u64,
    online: &impl OnlineCharacterLookup,
    emitted_effects: &TxItemEffectKeys,
) -> (Vec<(i64, &'static str, ItemChangeFields, String)>, Vec<(i64, i64)>) {
    let item_input = DeltaProjectionInput::new(None, snap, changes, sde, now_unix);
    let resolved_items = item_projector.project_and_resolve(&item_input, online);
    let item_changes_by_id = {
        let mut map: HashMap<i64, ItemBeforeAfter<'_>> = HashMap::new();
        for ch in &changes.items {
            map.entry(ch.key)
                .and_modify(|entry| entry.1 = ch.after.as_deref())
                .or_insert((ch.before.as_deref(), ch.after.as_deref()));
        }
        map
    };
    let mut projected_items: Vec<(i64, &'static str, ItemChangeFields, String)> = Vec::new();
    let mut seen_items: HashMap<(i64, &'static str, i64), usize> = HashMap::new();
    let mut projected_bps: Vec<(i64, i64)> = Vec::new();
    let mut seen_bps: HashSet<(i64, i64)> = HashSet::new();

    for r in resolved_items {
        match r.event {
            "OnItemChange" => {
                let Some(fields) = extract_item_change_fields(&r.args) else {
                    continue;
                };
                if emitted_effects
                    .item_keys
                    .contains(&(r.recipient_char_id, fields.item_id))
                {
                    continue;
                }
                let before_rec = item_changes_by_id
                    .get(&fields.item_id)
                    .and_then(|(before, _)| *before);
                let class_key = classify_projected_item(
                    before_rec,
                    &fields,
                    snap,
                    changes,
                    &item_changes_by_id,
                    sde,
                );
                let key = (r.recipient_char_id, r.id_type, fields.item_id);
                if let Some(&idx) = seen_items.get(&key) {
                    projected_items[idx] = (r.recipient_char_id, r.id_type, fields, class_key);
                } else {
                    seen_items.insert(key, projected_items.len());
                    projected_items.push((r.recipient_char_id, r.id_type, fields, class_key));
                }
            }
            "OnBlueprintsUpdated" => {
                let Some(owner_id) = extract_blueprint_owner_id(&r.args) else {
                    continue;
                };
                if emitted_effects
                    .blueprint_keys
                    .contains(&(r.recipient_char_id, owner_id))
                {
                    continue;
                }
                if seen_bps.insert((r.recipient_char_id, owner_id)) {
                    projected_bps.push((r.recipient_char_id, owner_id));
                }
            }
            _ => {}
        }
    }
    (projected_items, projected_bps)
}

fn trim_event_samples_for_ndjson(mut ev: ItemCompareEventSummary) -> ItemCompareEventSummary {
    ev.sample_projected_only.truncate(MAX_NDJSON_SAMPLES);
    ev.sample_hand_written_only.truncate(MAX_NDJSON_SAMPLES);
    for sc in [&mut ev.by_scope.dispatch, &mut ev.by_scope.tick, &mut ev.by_scope.neither] {
        sc.sample_projected_only.truncate(MAX_NDJSON_SAMPLES);
        sc.sample_hand_written_only.truncate(MAX_NDJSON_SAMPLES);
    }
    ev
}

fn append_ndjson_record(path: &Path, bridge_id: u64, summary: &ItemCompareSummary) {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let record = ItemCompareNdjsonRecord {
        pid: std::process::id(),
        bridge_id,
        item_change: trim_event_samples_for_ndjson(summary.item_change.clone()),
        blueprints_updated: trim_event_samples_for_ndjson(summary.blueprints_updated.clone()),
        explicit_in_tx_effects: summary.explicit_in_tx_effects,
        explicit_in_tx_effects_by_scope: summary.explicit_in_tx_effects_by_scope.clone(),
    };
    let Ok(mut line) = serde_json::to_string(&record) else {
        return;
    };
    line.push('\n');
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

fn emit_summary_log(summary: &ItemCompareSummary, msg: &str) {
    tracing::info!(
        target: "delta_item_compare",
        item_matched = summary.item_change.matched,
        item_matched_across_scopes = summary.item_change.matched_across_scopes,
        item_hand_written_only = summary.item_change.hand_written_only,
        item_projected_only = summary.item_change.projected_only,
        item_pending = summary.item_change.pending,
        bp_matched = summary.blueprints_updated.matched,
        bp_matched_across_scopes = summary.blueprints_updated.matched_across_scopes,
        bp_hand_written_only = summary.blueprints_updated.hand_written_only,
        bp_projected_only = summary.blueprints_updated.projected_only,
        bp_pending = summary.blueprints_updated.pending,
        explicit_in_tx = summary.explicit_in_tx_effects,
        item_helpers = ?summary.item_change.by_helper,
        item_cross_scope_helpers = ?summary.item_change.matched_across_scopes_by_helper,
        item_cross_scope_pairs = ?summary.item_change.matched_across_scopes_by_scope_pair,
        item_projected_only_by_class = ?summary.item_change.projected_only_by_class,
        item_projected_only_by_dispatch_method = ?summary.item_change.projected_only_by_dispatch_method,
        bp_helpers = ?summary.blueprints_updated.by_helper,
        "{msg}"
    );
}

/// Per-`ServerContext` compare-only state wired into `DeltaCommitBridgeState`.
pub(crate) struct ItemCompareBridge {
    bridge_id: u64,
    projector: DefaultDeltaProjection,
    ledger: Mutex<ItemCompareLedger>,
}

impl ItemCompareBridge {
    pub(crate) fn new(bridge_id: u64) -> Self {
        Self {
            bridge_id,
            projector: DefaultDeltaProjection::items_only(),
            ledger: Mutex::new(ItemCompareLedger::default()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_commit(
        &self,
        snap: &WorldStateSnapshot,
        changes: &ChangeSet,
        sde: &SdeRegistry,
        now_unix: u64,
        online: &impl OnlineCharacterLookup,
        effects: &TxItemEffectKeys,
        dispatch_scope_info: Option<(u64, Option<String>)>,
    ) {
        {
            let ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            if !ledger.is_enabled() {
                return;
            }
        }
        if changes.items.is_empty() && effects.explicit_count == 0 {
            return;
        }
        let (projected_items, projected_bps) = if !changes.items.is_empty() {
            collect_compare_item_expectations(
                &self.projector,
                snap,
                changes,
                sde,
                now_unix,
                online,
                effects,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        if !projected_items.is_empty() || !projected_bps.is_empty() || effects.explicit_count > 0 {
            let tid = std::thread::current().id();
            let in_dispatch = dispatch_scope_info.is_some();
            let dispatch_method = dispatch_scope_info.and_then(|(_, method)| method);
            let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            let scope = resolve_compare_origin_scope(in_dispatch, ledger.tick_sync_depths_by_thread.contains_key(&tid));
            ledger.record_projected_batch(
                scope,
                dispatch_method,
                projected_items,
                projected_bps,
                effects.explicit_count,
            );
        }
    }

    fn flush_and_maybe_report(
        &self,
        write_ndjson: bool,
        log_msg: Option<&str>,
    ) -> ItemCompareSummary {
        let (summary, should_log, report_target) = {
            let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            let summary = ledger.flush_pending();
            let total = summary.total_activity();
            let should_log = if log_msg.is_some() && !ledger.logged_summary && total > 0 {
                ledger.logged_summary = true;
                true
            } else {
                false
            };
            let target = if write_ndjson && !ledger.reported_to_file && total > 0 {
                let path = ledger.resolved_report_path();
                if path.is_some() {
                    ledger.reported_to_file = true;
                }
                path
            } else {
                None
            };
            (summary, should_log, target)
        };
        if summary.total_activity() > 0 {
            if should_log {
                if let Some(msg) = log_msg {
                    emit_summary_log(&summary, msg);
                }
            }
            if let Some(ref path) = report_target {
                append_ndjson_record(path, self.bridge_id, &summary);
            }
        }
        summary
    }
}

impl Drop for ItemCompareBridge {
    fn drop(&mut self) {
        let _ = self.flush_and_maybe_report(true, Some("DeltaItemCompare summary on bridge drop"));
    }
}

/// Synchronous RAII guard attributing nested [`ServerContext::notify`] item/blueprint emits
/// to a specific helper name without splitting `notify`'s ratcheted lock site.
pub(crate) struct CompareSourceGuard {
    bridge: Option<Arc<ItemCompareBridge>>,
    tid: ThreadId,
}

impl Drop for CompareSourceGuard {
    fn drop(&mut self) {
        if let Some(ref bridge) = self.bridge {
            let mut ledger = bridge.ledger.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(stack) = ledger.active_sources_by_thread.get_mut(&self.tid) {
                stack.pop();
                if stack.is_empty() {
                    ledger.active_sources_by_thread.remove(&self.tid);
                }
            }
        }
    }
}

pub(crate) fn push_compare_source_scope(
    ctx: &ServerContext,
    source: &'static str,
) -> CompareSourceGuard {
    let tid = std::thread::current().id();
    let Some(bridge) = super::delta_commit_listener::item_compare_bridge(ctx) else {
        return CompareSourceGuard { bridge: None, tid };
    };
    {
        let mut ledger = bridge.ledger.lock().unwrap_or_else(|e| e.into_inner());
        if !ledger.is_enabled() {
            return CompareSourceGuard { bridge: None, tid };
        }
        ledger
            .active_sources_by_thread
            .entry(tid)
            .or_default()
            .push(source);
    }
    CompareSourceGuard {
        bridge: Some(bridge),
        tid,
    }
}

pub(crate) fn push_paired_multi_item_skip_scope(ctx: &ServerContext) -> CompareSourceGuard {
    push_compare_source_scope(ctx, PAIRED_MULTI_ITEM_SKIP_SOURCE)
}

tokio::task_local! { static COMPARE_TICK_SCOPE: (); }

/// Executes an async server-tick future within compare tick scope (`#931` Track 2.5).
pub(crate) async fn with_compare_tick_scope<F: std::future::Future<Output = R>, R>(fut: F) -> R {
    COMPARE_TICK_SCOPE.scope((), fut).await
}

/// Non-`Send` RAII guard marking a synchronous block on this thread as running inside a server tick.
pub(crate) struct CompareTickSyncGuard {
    bridges: Vec<Arc<ItemCompareBridge>>,
    tid: ThreadId,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl Drop for CompareTickSyncGuard {
    fn drop(&mut self) {
        for bridge in &self.bridges {
            let mut ledger = bridge.ledger.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(d) = ledger.tick_sync_depths_by_thread.get_mut(&self.tid) {
                *d -= 1;
                if *d == 0 {
                    ledger.tick_sync_depths_by_thread.remove(&self.tid);
                }
            }
        }
    }
}

pub(crate) fn enter_compare_tick_sync_scope(
    db: Option<&eve_db::WorldRepository>,
) -> CompareTickSyncGuard {
    let tid = std::thread::current().id();
    let bridges = db
        .map(|r| super::delta_commit_listener::item_compare_bridges_for_db(r.instance_id()))
        .unwrap_or_default()
        .into_iter()
        .filter(|bridge| {
            let mut ledger = bridge.ledger.lock().unwrap_or_else(|e| e.into_inner());
            if !ledger.is_enabled() {
                return false;
            }
            *ledger.tick_sync_depths_by_thread.entry(tid).or_insert(0) += 1;
            true
        })
        .collect();
    CompareTickSyncGuard {
        bridges,
        tid,
        _not_send: std::marker::PhantomData,
    }
}

fn resolve_compare_origin_scope(in_dispatch: bool, in_tick_sync: bool) -> CompareOriginScope {
    if in_dispatch { CompareOriginScope::Dispatch }
    else if in_tick_sync || COMPARE_TICK_SCOPE.try_with(|_| ()).is_ok() { CompareOriginScope::Tick }
    else { CompareOriginScope::Neither }
}

/// Records a hand-written `OnItemChange`, `OnMultipleItemChange`, or `OnBlueprintsUpdated`
/// emission against compare-only commit expectations without mutating queued/live notifications.
pub(crate) fn record_legacy_item_compare(
    ctx: &ServerContext,
    default_helper: &str,
    recipients: &[(i64, Option<u64>)],
    id_type: &str,
    event: &str,
    payload_args: &[PyRep],
) {
    if recipients.is_empty()
        || !matches!(
            event,
            "OnItemChange" | "OnMultipleItemChange" | "OnBlueprintsUpdated"
        )
    {
        return;
    }
    let Some(bridge) = super::delta_commit_listener::item_compare_bridge(ctx) else {
        return;
    };
    let in_dispatch = super::cache_notifications::current_dispatch_staging_scope_info(
        Some(&*ctx.pending_notifications),
        Some(&ctx.notify_tx),
    )
    .is_some();
    let tid = std::thread::current().id();
    let mut ledger = bridge.ledger.lock().unwrap_or_else(|e| e.into_inner());
    if !ledger.is_enabled() {
        return;
    }
    let scope = resolve_compare_origin_scope(in_dispatch, ledger.tick_sync_depths_by_thread.contains_key(&tid));
    ledger.record_hand_written(scope, default_helper, recipients, id_type, event, payload_args);
}

/// Returns current compare-only counters (including unmatched `pending` entries) for `ctx`.
pub fn item_compare_summary(ctx: &ServerContext) -> ItemCompareSummary {
    let Some(bridge) = super::delta_commit_listener::item_compare_bridge(ctx) else {
        return ItemCompareSummary::default();
    };
    let summary = bridge
        .ledger
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .summary();
    summary
}

/// Retires remaining pending compare expectations into `projected_only` and returns the summary.
pub fn flush_item_compare_summary(ctx: &ServerContext) -> ItemCompareSummary {
    let Some(bridge) = super::delta_commit_listener::item_compare_bridge(ctx) else {
        return ItemCompareSummary::default();
    };
    bridge.flush_and_maybe_report(false, None)
}

/// Flushes pending compare expectations, logs the live summary via `tracing::info!`, and appends
/// an NDJSON record if `EVOMU_ITEM_COMPARE_REPORT_PATH` is configured.
pub fn log_item_compare_summary(ctx: &ServerContext) {
    let Some(bridge) = super::delta_commit_listener::item_compare_bridge(ctx) else {
        return;
    };
    let _ = bridge.flush_and_maybe_report(true, Some("DeltaItemCompare live summary"));
}

#[cfg(test)]
pub(crate) fn set_item_compare_override_for_test(
    ctx: &ServerContext,
    enabled: Option<bool>,
    report_path: Option<Option<PathBuf>>,
) {
    let Some(bridge) = super::delta_commit_listener::item_compare_bridge(ctx) else {
        return;
    };
    let mut ledger = bridge.ledger.lock().unwrap_or_else(|e| e.into_inner());
    ledger.enabled_override = enabled;
    ledger.report_path_override = report_path;
}

#[cfg(test)]
#[path = "delta_item_compare_tests.rs"]
mod tests;
