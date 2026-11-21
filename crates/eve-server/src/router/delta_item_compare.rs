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
    pub hand_written_only: u64,
    pub projected_only: u64,
    pub pending: usize,
    pub by_helper: BTreeMap<String, HelperCompareCounts>,
    pub sample_projected_only: Vec<String>,
    pub sample_hand_written_only: Vec<String>,
}

/// Combined compare-only diagnostics for `#931` Track 2 (`OnItemChange` & `OnBlueprintsUpdated`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemCompareSummary {
    pub item_change: ItemCompareEventSummary,
    pub blueprints_updated: ItemCompareEventSummary,
    pub explicit_in_tx_effects: u64,
}

impl ItemCompareSummary {
    fn total_activity(&self) -> u64 {
        self.item_change.matched
            + self.item_change.hand_written_only
            + self.item_change.projected_only
            + self.blueprints_updated.matched
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
}

#[derive(Debug, Default)]
struct CompareCounters {
    matched: u64,
    hand_written_only: u64,
    projected_only: u64,
    by_helper: BTreeMap<String, HelperCompareCounts>,
    sample_projected_only: Vec<String>,
    sample_hand_written_only: Vec<String>,
}

impl CompareCounters {
    fn record_matched(&mut self, helper: &str) {
        self.matched += 1;
        self.by_helper.entry(helper.to_string()).or_default().matched += 1;
    }

    fn record_hand_written_only(&mut self, helper: &str, detail: impl FnOnce() -> String) {
        self.hand_written_only += 1;
        self.by_helper
            .entry(helper.to_string())
            .or_default()
            .hand_written_only += 1;
        if self.sample_hand_written_only.len() < MAX_COMPARE_SAMPLES {
            let msg = detail();
            tracing::debug!(target: "delta_item_compare", "hand_written_only: {msg}");
            self.sample_hand_written_only.push(msg);
        }
    }

    fn record_projected_only(
        &mut self,
        mismatched_helper: Option<&str>,
        detail: impl FnOnce() -> String,
    ) {
        self.projected_only += 1;
        let bucket = mismatched_helper.unwrap_or("<no_helper>");
        self.by_helper
            .entry(bucket.to_string())
            .or_default()
            .projected_only += 1;
        if self.sample_projected_only.len() < MAX_COMPARE_SAMPLES {
            let msg = detail();
            tracing::debug!(target: "delta_item_compare", "projected_only: {msg}");
            self.sample_projected_only.push(msg);
        }
    }

    fn to_summary(&self, pending: usize) -> ItemCompareEventSummary {
        ItemCompareEventSummary {
            matched: self.matched,
            hand_written_only: self.hand_written_only,
            projected_only: self.projected_only,
            pending,
            by_helper: self.by_helper.clone(),
            sample_projected_only: self.sample_projected_only.clone(),
            sample_hand_written_only: self.sample_hand_written_only.clone(),
        }
    }
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
    mismatched_helper: Option<String>,
}

#[derive(Debug, Clone)]
struct PendingBlueprintCompare {
    char_id: i64,
    owner_id: i64,
    mismatched_helper: Option<String>,
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
    pending_items: VecDeque<PendingItemCompare>,
    pending_bps: VecDeque<PendingBlueprintCompare>,
    item_counts: CompareCounters,
    bp_counts: CompareCounters,
    explicit_in_tx_effects: u64,
    // Synchronous call-stack helper labels keyed by ThreadId so fixed-signature `ServerContext::notify`
    // retains the single ratcheted `pending_notifications.write()` call site (`::notify::pending_notifications`)
    // and never alters notification delivery.
    active_sources_by_thread: HashMap<ThreadId, Vec<&'static str>>,
    enabled_override: Option<bool>,
    report_path_override: Option<Option<PathBuf>>,
    reported_to_file: bool,
    logged_summary: bool,
}

impl ItemCompareLedger {
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
        let helper = entry.mismatched_helper.clone();
        self.item_counts.record_projected_only(helper.as_deref(), || {
            format!(
                "cid={} id_type={} {} mismatched_helper={:?}",
                entry.char_id,
                entry.id_type,
                format_item_fields(&entry.fields),
                helper
            )
        });
    }

    fn retire_bp(&mut self, entry: PendingBlueprintCompare) {
        let helper = entry.mismatched_helper.clone();
        self.bp_counts.record_projected_only(helper.as_deref(), || {
            format!(
                "cid={} owner={} mismatched_helper={:?}",
                entry.char_id, entry.owner_id, helper
            )
        });
    }

    fn record_projected_batch(
        &mut self,
        projected_items: Vec<(i64, &'static str, ItemChangeFields)>,
        projected_bps: Vec<(i64, i64)>,
        explicit_effects: u64,
    ) {
        if !projected_items.is_empty() || !projected_bps.is_empty() || explicit_effects > 0 {
            self.reported_to_file = false;
            self.logged_summary = false;
        }
        self.explicit_in_tx_effects += explicit_effects;
        let mut retired_items = Vec::new();
        for (char_id, id_type, fields) in projected_items {
            let item_id = fields.item_id;
            push_pending_capped(
                &mut self.pending_items,
                PendingItemCompare {
                    char_id,
                    id_type,
                    fields,
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
        helper: &str,
        recipients: &[(i64, Option<u64>)],
        id_type: &str,
        actual_opt: Option<&ItemChangeFields>,
    ) {
        for &(cid, _) in recipients {
            let Some(actual) = actual_opt else {
                self.item_counts.record_hand_written_only(helper, || {
                    format!("helper={helper} cid={cid} id_type={id_type} unparsed_args")
                });
                continue;
            };
            if let Some(pos) = self.pending_items.iter().position(|p| {
                p.char_id == cid && p.id_type == id_type && p.fields.matches(actual)
            }) {
                let _ = self.pending_items.remove(pos);
                self.item_counts.record_matched(helper);
            } else {
                let near = self
                    .pending_items
                    .iter_mut()
                    .rev()
                    .find(|p| p.char_id == cid && p.fields.item_id == actual.item_id);
                let near_desc = if let Some(p) = near {
                    p.mismatched_helper = Some(helper.to_string());
                    format!(
                        " vs_projected(id_type={} {})",
                        p.id_type,
                        format_item_fields(&p.fields)
                    )
                } else {
                    String::new()
                };
                self.item_counts.record_hand_written_only(helper, || {
                    format!(
                        "helper={helper} cid={cid} id_type={id_type} {}{near_desc}",
                        format_item_fields(actual)
                    )
                });
            }
        }
    }

    fn record_hand_written(
        &mut self,
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
                self.match_or_record_item_row(helper, recipients, id_type, actual_opt.as_ref());
            }
            "OnMultipleItemChange" => match payload_args.first() {
                Some(PyRep::List(rows) | PyRep::Tuple(rows)) if !rows.is_empty() => {
                    for row in rows {
                        let actual_opt = extract_item_change_fields_from_row(row);
                        self.match_or_record_item_row(
                            helper,
                            recipients,
                            id_type,
                            actual_opt.as_ref(),
                        );
                    }
                }
                _ => {
                    self.match_or_record_item_row(helper, recipients, id_type, None);
                }
            },
            "OnBlueprintsUpdated" => {
                let owner_opt = extract_blueprint_owner_id(payload_args);
                for &(cid, _) in recipients {
                    let Some(owner_id) = owner_opt else {
                        self.bp_counts.record_hand_written_only(helper, || {
                            format!("helper={helper} cid={cid} unparsed_owner")
                        });
                        continue;
                    };
                    if let Some(pos) = self
                        .pending_bps
                        .iter()
                        .position(|p| p.char_id == cid && p.owner_id == owner_id)
                    {
                        let _ = self.pending_bps.remove(pos);
                        self.bp_counts.record_matched(helper);
                    } else {
                        self.bp_counts.record_hand_written_only(helper, || {
                            format!("helper={helper} cid={cid} owner={owner_id}")
                        });
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
        self.summary()
    }

    fn summary(&self) -> ItemCompareSummary {
        ItemCompareSummary {
            item_change: self.item_counts.to_summary(self.pending_items.len()),
            blueprints_updated: self.bp_counts.to_summary(self.pending_bps.len()),
            explicit_in_tx_effects: self.explicit_in_tx_effects,
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
) -> (Vec<(i64, &'static str, ItemChangeFields)>, Vec<(i64, i64)>) {
    let item_input = DeltaProjectionInput::new(None, snap, changes, sde, now_unix);
    let resolved_items = item_projector.project_and_resolve(&item_input, online);
    let mut projected_items: Vec<(i64, &'static str, ItemChangeFields)> = Vec::new();
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
                let key = (r.recipient_char_id, r.id_type, fields.item_id);
                if let Some(&idx) = seen_items.get(&key) {
                    projected_items[idx] = (r.recipient_char_id, r.id_type, fields);
                } else {
                    seen_items.insert(key, projected_items.len());
                    projected_items.push((r.recipient_char_id, r.id_type, fields));
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
        item_hand_written_only = summary.item_change.hand_written_only,
        item_projected_only = summary.item_change.projected_only,
        item_pending = summary.item_change.pending,
        bp_matched = summary.blueprints_updated.matched,
        bp_hand_written_only = summary.blueprints_updated.hand_written_only,
        bp_projected_only = summary.blueprints_updated.projected_only,
        bp_pending = summary.blueprints_updated.pending,
        explicit_in_tx = summary.explicit_in_tx_effects,
        item_helpers = ?summary.item_change.by_helper,
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

    pub(crate) fn on_commit(
        &self,
        snap: &WorldStateSnapshot,
        changes: &ChangeSet,
        sde: &SdeRegistry,
        now_unix: u64,
        online: &impl OnlineCharacterLookup,
        effects: &TxItemEffectKeys,
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
            self.ledger
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .record_projected_batch(projected_items, projected_bps, effects.explicit_count);
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
    let mut ledger = bridge.ledger.lock().unwrap_or_else(|e| e.into_inner());
    if !ledger.is_enabled() {
        return;
    }
    ledger.record_hand_written(default_helper, recipients, id_type, event, payload_args);
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
