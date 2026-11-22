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
use eve_db::{
    ChangeSet, CharacterRecord, InventoryFlag, InventoryItemRecord, WorldStateSnapshot,
};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectedItemOpKind {
    CreatedOrUpdated,
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ItemWireFingerprint {
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i64,
    pub wire_qty: i64,
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

    #[cfg(test)]
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

pub(crate) type ItemBeforeAfter<'a> = (
    Option<&'a InventoryItemRecord>,
    Option<&'a InventoryItemRecord>,
);

pub(crate) fn index_changed_characters(
    changes: &ChangeSet,
) -> HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)> {
    changes
        .characters
        .iter()
        .map(|c| (c.key, (c.before.as_deref(), c.after.as_deref())))
        .collect()
}

pub(crate) fn is_character_scene_or_ship_transition(
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
        || before.active_ship_id != after.active_ship_id
}

pub(crate) fn is_known_human_character_owner(
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

#[inline]
pub(crate) fn is_first_set_direct_slot(item: &InventoryItemRecord, ch: &CharacterRecord) -> bool {
    is_first_set_direct_slot_coords(item.location_id, item.flag as i64, ch)
}

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
    let (owner_id, op_kind) = match (before_opt, after_opt) {
        (None, Some(new_item)) => (new_item.owner_id, ProjectedItemOpKind::CreatedOrUpdated),
        (Some(old_item), Some(new_item)) => {
            if old_item.owner_id != new_item.owner_id {
                return None;
            }
            (new_item.owner_id, ProjectedItemOpKind::CreatedOrUpdated)
        }
        (Some(old_item), None) => (old_item.owner_id, ProjectedItemOpKind::Deleted),
        (None, None) => return None,
    };
    if owner_id != recipient_char_id
        || !is_known_human_character_owner(owner_id, snap_after, changed_chars, sde)
        || is_character_scene_or_ship_transition(owner_id, changed_chars)
    {
        return None;
    }
    let ch = snap_after.characters.get(&owner_id)?;
    if !online.is_character_online(owner_id, ch.user_id) {
        return None;
    }
    if let Some(old_item) = before_opt {
        if !is_first_set_direct_slot(old_item, ch) {
            return None;
        }
    }
    if let Some(new_item) = after_opt {
        if !is_first_set_direct_slot(new_item, ch) {
            return None;
        }
    }
    Some(op_kind)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_hand_written_first_set_in_snap(
    in_dispatch: bool,
    char_id: i64,
    recipient_count: usize,
    kind: ProjectedItemOpKind,
    fp: ItemWireFingerprint,
    change_dict: Option<&CanonicalChangeDict>,
    snap: &WorldStateSnapshot,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> Option<String> {
    if !in_dispatch
        || recipient_count != 1
        || char_id <= 0
        || fp.owner_id != char_id
        || snap.corporations.contains_key(&char_id)
        || sde.npc_corporations.contains_key(&char_id)
    {
        return None;
    }
    let ch = snap.characters.get(&char_id)?;
    if !online.is_character_online(char_id, ch.user_id) {
        return None;
    }

    let col3 = change_dict.and_then(|d| d.col_i64(3));
    let col4 = change_dict.and_then(|d| d.col_i64(4));

    let (before_loc, before_flag) = match kind {
        ProjectedItemOpKind::Deleted => {
            let (Some(old_loc), Some(old_flag)) = (col3, col4) else {
                return None;
            };
            if !is_first_set_direct_slot_coords(old_loc, old_flag, ch) {
                return None;
            }
            (Some(old_loc), Some(old_flag))
        }
        ProjectedItemOpKind::CreatedOrUpdated => {
            if !is_first_set_direct_slot_coords(fp.location_id, fp.flag_id, ch) {
                return None;
            }
            if col3 == Some(0) {
                (None, None)
            } else {
                let old_loc = col3.unwrap_or(fp.location_id);
                let old_flag = col4.unwrap_or(fp.flag_id);
                if !is_first_set_direct_slot_coords(old_loc, old_flag, ch) {
                    return None;
                }
                (Some(old_loc), Some(old_flag))
            }
        }
    };

    Some(classify_item_row_kind_key(
        before_loc,
        before_flag,
        fp.owner_id,
        fp.location_id,
        fp.flag_id,
        fp.wire_qty,
        snap,
        &ChangeSet::default(),
        &HashMap::new(),
        sde,
    ))
}

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
        && seg != crate::flight_recorder::UNKNOWN_RPC_LABEL
        && seg != OTHER_RPC_BUCKET
        && seg != OTHER_CLASS_BUCKET
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

/// Per-helper change-dictionary parity counts (`#931` Check #28–#30 Fix #3).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HelperDictionaryCounts {
    pub same_dictionary: u64,
    pub different_dictionary: u64,
}

/// First-set-only compare summary inside `ItemCompareEventSummary` (`#931` Section 4).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompareFirstSetSummary {
    pub matched: u64,
    pub hand_written_only: u64,
    pub projected_only: u64,
    pub dictionary_same: u64,
    pub dictionary_differs: u64,
    pub matched_by_class: BTreeMap<String, u64>,
    pub matched_by_dispatch_method: BTreeMap<String, u64>,
    pub hand_written_only_by_class: BTreeMap<String, u64>,
    pub hand_written_only_by_dispatch_method: BTreeMap<String, u64>,
    pub projected_only_by_class: BTreeMap<String, u64>,
    pub projected_only_by_dispatch_method: BTreeMap<String, u64>,
    pub contexts_by_class: BTreeMap<String, u64>,
}

/// Compare counts for one [`CompareOriginScope`] bucket (`#931` Track 2.5 / 3A / 3B).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ItemCompareScopeCounts {
    pub matched: u64,
    pub hand_written_only: u64,
    pub projected_only: u64,
    pub pending: usize,
    pub by_helper: BTreeMap<String, HelperCompareCounts>,
    pub dictionary_same: u64,
    pub dictionary_differs: u64,
    pub dictionary_by_helper: BTreeMap<String, HelperDictionaryCounts>,
    pub matched_by_class: BTreeMap<String, u64>,
    pub matched_by_dispatch_method: BTreeMap<String, u64>,
    pub hand_written_only_by_class: BTreeMap<String, u64>,
    pub hand_written_only_by_dispatch_method: BTreeMap<String, u64>,
    pub projected_only_by_class: BTreeMap<String, u64>,
    pub projected_only_by_dispatch_method: BTreeMap<String, u64>,
    pub contexts_by_class: BTreeMap<String, u64>,
    pub sample_projected_only: Vec<String>,
    pub sample_hand_written_only: Vec<String>,
    pub sample_dictionary_differs: Vec<String>,
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
    pub dictionary_same: u64,
    pub dictionary_differs: u64,
    pub dictionary_by_helper: BTreeMap<String, HelperDictionaryCounts>,
    pub matched_by_class: BTreeMap<String, u64>,
    pub matched_by_dispatch_method: BTreeMap<String, u64>,
    pub hand_written_only_by_class: BTreeMap<String, u64>,
    pub hand_written_only_by_dispatch_method: BTreeMap<String, u64>,
    pub projected_only_by_class: BTreeMap<String, u64>,
    pub projected_only_by_dispatch_method: BTreeMap<String, u64>,
    pub contexts_by_class: BTreeMap<String, u64>,
    pub sample_projected_only: Vec<String>,
    pub sample_hand_written_only: Vec<String>,
    pub sample_dictionary_differs: Vec<String>,
    pub first_set: CompareFirstSetSummary,
    pub by_scope: ItemCompareByScope<ItemCompareScopeCounts>,
}

/// Combined compare-only diagnostics for `#931` Tracks 2 & 3A/3B (`OnItemChange` & `OnBlueprintsUpdated`).
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

fn merge_dictionary_counts(
    dst: &mut BTreeMap<String, HelperDictionaryCounts>,
    src: &BTreeMap<String, HelperDictionaryCounts>,
) {
    for (k, v) in src {
        let entry = dst.entry(k.clone()).or_default();
        entry.same_dictionary += v.same_dictionary;
        entry.different_dictionary += v.different_dictionary;
    }
}

fn populate_contexts_by_class(
    matched_by_class: &BTreeMap<String, u64>,
    hand_written_only_by_class: &BTreeMap<String, u64>,
    projected_only_by_class: &BTreeMap<String, u64>,
) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for map in [
        matched_by_class,
        hand_written_only_by_class,
        projected_only_by_class,
    ] {
        for (k, &v) in map {
            if v > 0 {
                out.insert(k.clone(), 1);
            }
        }
    }
    out
}

#[derive(Debug, Default, Clone)]
struct CrossScopeCompareCounters {
    matched_across_scopes: u64,
    by_helper: BTreeMap<String, u64>,
    by_scope_pair: BTreeMap<String, u64>,
    dictionary_same: u64,
    dictionary_differs: u64,
    dictionary_by_helper: BTreeMap<String, HelperDictionaryCounts>,
    sample_dictionary_differs: Vec<(u64, String)>,
}

impl CrossScopeCompareCounters {
    fn record_pair(
        &mut self,
        seq: u64,
        helper: &str,
        proj_scope: CompareOriginScope,
        hw_scope: CompareOriginScope,
        dict_cmp: Option<(i64, Option<&CanonicalChangeDict>, Option<&CanonicalChangeDict>)>,
    ) {
        self.matched_across_scopes += 1;
        *self.by_helper.entry(helper.to_string()).or_insert(0) += 1;
        let pair_key = format!(
            "projected:{}->hand_written:{}",
            proj_scope.as_str(),
            hw_scope.as_str()
        );
        *self.by_scope_pair.entry(pair_key).or_insert(0) += 1;
        if let Some((item_id, exp_dict, act_dict)) = dict_cmp {
            let same = matches!((exp_dict, act_dict), (Some(a), Some(b)) if a == b);
            let h = self
                .dictionary_by_helper
                .entry(helper.to_string())
                .or_default();
            if same {
                self.dictionary_same += 1;
                h.same_dictionary += 1;
            } else {
                self.dictionary_differs += 1;
                h.different_dictionary += 1;
                if self.sample_dictionary_differs.len() < MAX_COMPARE_SAMPLES {
                    let exp_s = exp_dict.map_or_else(|| "None".to_string(), CanonicalChangeDict::format_compact);
                    let act_s = act_dict.map_or_else(|| "None".to_string(), CanonicalChangeDict::format_compact);
                    self.sample_dictionary_differs.push((
                        seq,
                        format!("helper={helper} item={item_id} cross_scope projected={exp_s} actual={act_s}"),
                    ));
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct CompareFirstSetCounters {
    matched: u64,
    hand_written_only: u64,
    projected_only: u64,
    dictionary_same: u64,
    dictionary_differs: u64,
    matched_by_class: BTreeMap<String, u64>,
    matched_by_dispatch_method: BTreeMap<String, u64>,
    hand_written_only_by_class: BTreeMap<String, u64>,
    hand_written_only_by_dispatch_method: BTreeMap<String, u64>,
    projected_only_by_class: BTreeMap<String, u64>,
    projected_only_by_dispatch_method: BTreeMap<String, u64>,
}

#[derive(Debug, Default)]
struct CompareCounters {
    matched: u64,
    hand_written_only: u64,
    projected_only: u64,
    by_helper: BTreeMap<String, HelperCompareCounts>,
    dictionary_same: u64,
    dictionary_differs: u64,
    dictionary_by_helper: BTreeMap<String, HelperDictionaryCounts>,
    matched_by_class: BTreeMap<String, u64>,
    matched_by_dispatch_method: BTreeMap<String, u64>,
    hand_written_only_by_class: BTreeMap<String, u64>,
    hand_written_only_by_dispatch_method: BTreeMap<String, u64>,
    projected_only_by_class: BTreeMap<String, u64>,
    projected_only_by_dispatch_method: BTreeMap<String, u64>,
    sample_projected_only: Vec<(u64, String)>,
    sample_hand_written_only: Vec<(u64, String)>,
    sample_dictionary_differs: Vec<(u64, String)>,
    first_set: CompareFirstSetCounters,
}

impl CompareCounters {
    #[allow(clippy::too_many_arguments)]
    fn record_matched(
        &mut self,
        seq: u64,
        helper: &str,
        class_key: Option<&str>,
        dispatch_method: Option<&str>,
        first_set: bool,
        dict_cmp: Option<(i64, Option<&CanonicalChangeDict>, Option<&CanonicalChangeDict>)>,
    ) {
        self.matched += 1;
        self.by_helper.entry(helper.to_string()).or_default().matched += 1;
        let rec_class = class_key
            .map(|ck| increment_breakdown_bucket(&mut self.matched_by_class, ck, OTHER_CLASS_BUCKET));
        let rec_method = dispatch_method.map(|dm| {
            increment_breakdown_bucket(&mut self.matched_by_dispatch_method, dm, OTHER_RPC_BUCKET)
        });
        if first_set {
            self.first_set.matched += 1;
            if let Some(ref ck) = rec_class {
                increment_breakdown_bucket(&mut self.first_set.matched_by_class, ck, OTHER_CLASS_BUCKET);
            }
            if let Some(ref dm) = rec_method {
                increment_breakdown_bucket(
                    &mut self.first_set.matched_by_dispatch_method,
                    dm,
                    OTHER_RPC_BUCKET,
                );
            }
        }
        if let Some((item_id, exp_dict, act_dict)) = dict_cmp {
            let same = matches!((exp_dict, act_dict), (Some(a), Some(b)) if a == b);
            let h = self
                .dictionary_by_helper
                .entry(helper.to_string())
                .or_default();
            if same {
                self.dictionary_same += 1;
                h.same_dictionary += 1;
                if first_set {
                    self.first_set.dictionary_same += 1;
                }
            } else {
                self.dictionary_differs += 1;
                h.different_dictionary += 1;
                if first_set {
                    self.first_set.dictionary_differs += 1;
                }
                if self.sample_dictionary_differs.len() < MAX_COMPARE_SAMPLES {
                    let exp_s = exp_dict.map_or_else(|| "None".to_string(), CanonicalChangeDict::format_compact);
                    let act_s = act_dict.map_or_else(|| "None".to_string(), CanonicalChangeDict::format_compact);
                    self.sample_dictionary_differs.push((
                        seq,
                        format!("helper={helper} item={item_id} projected={exp_s} actual={act_s}"),
                    ));
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_hand_written_only(
        &mut self,
        seq: u64,
        helper: &str,
        class_key: Option<&str>,
        dispatch_method: Option<&str>,
        first_set: bool,
        detail: impl FnOnce() -> String,
    ) -> (Option<String>, Option<String>) {
        self.hand_written_only += 1;
        self.by_helper
            .entry(helper.to_string())
            .or_default()
            .hand_written_only += 1;
        let rec_class = class_key.map(|ck| {
            increment_breakdown_bucket(&mut self.hand_written_only_by_class, ck, OTHER_CLASS_BUCKET)
        });
        let rec_method = dispatch_method.map(|dm| {
            increment_breakdown_bucket(
                &mut self.hand_written_only_by_dispatch_method,
                dm,
                OTHER_RPC_BUCKET,
            )
        });
        if first_set {
            self.first_set.hand_written_only += 1;
            if let Some(ref ck) = rec_class {
                increment_breakdown_bucket(
                    &mut self.first_set.hand_written_only_by_class,
                    ck,
                    OTHER_CLASS_BUCKET,
                );
            }
            if let Some(ref dm) = rec_method {
                increment_breakdown_bucket(
                    &mut self.first_set.hand_written_only_by_dispatch_method,
                    dm,
                    OTHER_RPC_BUCKET,
                );
            }
        }
        if self.sample_hand_written_only.len() < MAX_COMPARE_SAMPLES {
            let msg = detail();
            tracing::debug!(target: "delta_item_compare", "hand_written_only: {msg}");
            self.sample_hand_written_only.push((seq, msg));
        }
        (rec_class, rec_method)
    }

    fn undo_hand_written_only(
        &mut self,
        seq: u64,
        helper: &str,
        class_bucket: Option<&str>,
        method_bucket: Option<&str>,
        first_set: bool,
    ) {
        self.hand_written_only = self.hand_written_only.saturating_sub(1);
        if let Some(counts) = self.by_helper.get_mut(helper) {
            counts.hand_written_only = counts.hand_written_only.saturating_sub(1);
            if counts.matched == 0 && counts.hand_written_only == 0 && counts.projected_only == 0 {
                self.by_helper.remove(helper);
            }
        }
        if let Some(cb) = class_bucket {
            decrement_breakdown_bucket(&mut self.hand_written_only_by_class, cb, OTHER_CLASS_BUCKET);
        }
        if let Some(mb) = method_bucket {
            decrement_breakdown_bucket(
                &mut self.hand_written_only_by_dispatch_method,
                mb,
                OTHER_RPC_BUCKET,
            );
        }
        if first_set {
            self.first_set.hand_written_only = self.first_set.hand_written_only.saturating_sub(1);
            if let Some(cb) = class_bucket {
                decrement_breakdown_bucket(
                    &mut self.first_set.hand_written_only_by_class,
                    cb,
                    OTHER_CLASS_BUCKET,
                );
            }
            if let Some(mb) = method_bucket {
                decrement_breakdown_bucket(
                    &mut self.first_set.hand_written_only_by_dispatch_method,
                    mb,
                    OTHER_RPC_BUCKET,
                );
            }
        }
        self.sample_hand_written_only.retain(|(s, _)| *s != seq);
    }

    #[allow(clippy::too_many_arguments)]
    fn record_projected_only(
        &mut self,
        seq: u64,
        mismatched_helper: Option<&str>,
        class_key: Option<&str>,
        dispatch_method: Option<&str>,
        first_set: bool,
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
        if first_set {
            self.first_set.projected_only += 1;
            if let Some(ref ck) = recorded_class {
                increment_breakdown_bucket(
                    &mut self.first_set.projected_only_by_class,
                    ck,
                    OTHER_CLASS_BUCKET,
                );
            }
            if let Some(ref dm) = recorded_method {
                increment_breakdown_bucket(
                    &mut self.first_set.projected_only_by_dispatch_method,
                    dm,
                    OTHER_RPC_BUCKET,
                );
            }
        }
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
        first_set: bool,
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
        if first_set {
            self.first_set.projected_only = self.first_set.projected_only.saturating_sub(1);
            if let Some(cb) = class_bucket {
                decrement_breakdown_bucket(
                    &mut self.first_set.projected_only_by_class,
                    cb,
                    OTHER_CLASS_BUCKET,
                );
            }
            if let Some(mb) = method_bucket {
                decrement_breakdown_bucket(
                    &mut self.first_set.projected_only_by_dispatch_method,
                    mb,
                    OTHER_RPC_BUCKET,
                );
            }
        }
        self.sample_projected_only.retain(|(s, _)| *s != seq);
    }

    fn to_scope_counts(&self, pending: usize) -> ItemCompareScopeCounts {
        let contexts_by_class = populate_contexts_by_class(
            &self.matched_by_class,
            &self.hand_written_only_by_class,
            &self.projected_only_by_class,
        );
        ItemCompareScopeCounts {
            matched: self.matched,
            hand_written_only: self.hand_written_only,
            projected_only: self.projected_only,
            pending,
            by_helper: self.by_helper.clone(),
            dictionary_same: self.dictionary_same,
            dictionary_differs: self.dictionary_differs,
            dictionary_by_helper: self.dictionary_by_helper.clone(),
            matched_by_class: self.matched_by_class.clone(),
            matched_by_dispatch_method: self.matched_by_dispatch_method.clone(),
            hand_written_only_by_class: self.hand_written_only_by_class.clone(),
            hand_written_only_by_dispatch_method: self.hand_written_only_by_dispatch_method.clone(),
            projected_only_by_class: self.projected_only_by_class.clone(),
            projected_only_by_dispatch_method: self.projected_only_by_dispatch_method.clone(),
            contexts_by_class,
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
            sample_dictionary_differs: self
                .sample_dictionary_differs
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
            total.dictionary_same += sc.dictionary_same;
            total.dictionary_differs += sc.dictionary_differs;
            for (k, v) in &sc.by_helper {
                let d = total.by_helper.entry(k.clone()).or_default();
                d.matched += v.matched;
                d.hand_written_only += v.hand_written_only;
                d.projected_only += v.projected_only;
            }
            merge_dictionary_counts(&mut total.dictionary_by_helper, &sc.dictionary_by_helper);
            merge_breakdown_counts(&mut total.matched_by_class, &sc.matched_by_class);
            merge_breakdown_counts(
                &mut total.matched_by_dispatch_method,
                &sc.matched_by_dispatch_method,
            );
            merge_breakdown_counts(
                &mut total.hand_written_only_by_class,
                &sc.hand_written_only_by_class,
            );
            merge_breakdown_counts(
                &mut total.hand_written_only_by_dispatch_method,
                &sc.hand_written_only_by_dispatch_method,
            );
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
            push_samples(
                &mut total.sample_dictionary_differs,
                &sc.sample_dictionary_differs,
            );

            total.first_set.matched += sc.first_set.matched;
            total.first_set.hand_written_only += sc.first_set.hand_written_only;
            total.first_set.projected_only += sc.first_set.projected_only;
            total.first_set.dictionary_same += sc.first_set.dictionary_same;
            total.first_set.dictionary_differs += sc.first_set.dictionary_differs;
            merge_breakdown_counts(
                &mut total.first_set.matched_by_class,
                &sc.first_set.matched_by_class,
            );
            merge_breakdown_counts(
                &mut total.first_set.matched_by_dispatch_method,
                &sc.first_set.matched_by_dispatch_method,
            );
            merge_breakdown_counts(
                &mut total.first_set.hand_written_only_by_class,
                &sc.first_set.hand_written_only_by_class,
            );
            merge_breakdown_counts(
                &mut total.first_set.hand_written_only_by_dispatch_method,
                &sc.first_set.hand_written_only_by_dispatch_method,
            );
            merge_breakdown_counts(
                &mut total.first_set.projected_only_by_class,
                &sc.first_set.projected_only_by_class,
            );
            merge_breakdown_counts(
                &mut total.first_set.projected_only_by_dispatch_method,
                &sc.first_set.projected_only_by_dispatch_method,
            );
        }
        total.dictionary_same += cross_scope.dictionary_same;
        total.dictionary_differs += cross_scope.dictionary_differs;
        merge_dictionary_counts(&mut total.dictionary_by_helper, &cross_scope.dictionary_by_helper);
        push_samples(
            &mut total.sample_dictionary_differs,
            &cross_scope.sample_dictionary_differs,
        );
        let contexts_by_class = populate_contexts_by_class(
            &total.matched_by_class,
            &total.hand_written_only_by_class,
            &total.projected_only_by_class,
        );
        let first_set_contexts = populate_contexts_by_class(
            &total.first_set.matched_by_class,
            &total.first_set.hand_written_only_by_class,
            &total.first_set.projected_only_by_class,
        );
        ItemCompareEventSummary {
            matched: total.matched,
            matched_across_scopes: cross_scope.matched_across_scopes,
            matched_across_scopes_by_helper: cross_scope.by_helper.clone(),
            matched_across_scopes_by_scope_pair: cross_scope.by_scope_pair.clone(),
            hand_written_only: total.hand_written_only,
            projected_only: total.projected_only,
            pending: pending.dispatch + pending.tick + pending.neither,
            by_helper: total.by_helper,
            dictionary_same: total.dictionary_same,
            dictionary_differs: total.dictionary_differs,
            dictionary_by_helper: total.dictionary_by_helper,
            matched_by_class: total.matched_by_class,
            matched_by_dispatch_method: total.matched_by_dispatch_method,
            hand_written_only_by_class: total.hand_written_only_by_class,
            hand_written_only_by_dispatch_method: total.hand_written_only_by_dispatch_method,
            projected_only_by_class: total.projected_only_by_class,
            projected_only_by_dispatch_method: total.projected_only_by_dispatch_method,
            contexts_by_class,
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
            sample_dictionary_differs: total
                .sample_dictionary_differs
                .into_iter()
                .map(|(_, s)| s)
                .collect(),
            first_set: CompareFirstSetSummary {
                matched: total.first_set.matched,
                hand_written_only: total.first_set.hand_written_only,
                projected_only: total.first_set.projected_only,
                dictionary_same: total.first_set.dictionary_same,
                dictionary_differs: total.first_set.dictionary_differs,
                matched_by_class: total.first_set.matched_by_class,
                matched_by_dispatch_method: total.first_set.matched_by_dispatch_method,
                hand_written_only_by_class: total.first_set.hand_written_only_by_class,
                hand_written_only_by_dispatch_method: total.first_set.hand_written_only_by_dispatch_method,
                projected_only_by_class: total.first_set.projected_only_by_class,
                projected_only_by_dispatch_method: total.first_set.projected_only_by_dispatch_method,
                contexts_by_class: first_set_contexts,
            },
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
    change_dict: Option<CanonicalChangeDict>,
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
    first_set: bool,
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
    first_set: bool,
    scope: CompareOriginScope,
    recorded_dispatch_method: Option<String>,
}

#[derive(Debug, Clone)]
struct UnmatchedHandWrittenItem {
    seq: u64,
    char_id: i64,
    id_type: String,
    fields: ItemChangeFields,
    recorded_class_key: Option<String>,
    recorded_dispatch_method: Option<String>,
    first_set: bool,
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
    items
        .iter()
        .position(|item| matches!(candidate(item), Some((true, true))))
}

fn classify_projected_item(
    before_rec: Option<&InventoryItemRecord>,
    fields: &ItemChangeFields,
    snap: &WorldStateSnapshot,
    changes: &ChangeSet,
    item_changes_by_id: &HashMap<i64, ItemBeforeAfter<'_>>,
    sde: &SdeRegistry,
) -> String {
    let before_flag = before_rec
        .map(|r| r.flag as i64)
        .or_else(|| fields.change_dict.as_ref().and_then(|d| d.col_i64(4)));
    classify_item_row_kind_key(
        before_rec.map(|r| r.location_id),
        before_flag,
        fields.owner_id,
        fields.location_id,
        fields.flag_id,
        fields.wire_qty,
        snap,
        changes,
        item_changes_by_id,
        sde,
    )
}

fn classify_hand_written_item_in_snap(
    fields: &ItemChangeFields,
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
) -> String {
    let before_loc = match (
        fields.change_dict.as_ref().and_then(|d| d.col_i64(3)),
        fields.location_id,
    ) {
        (Some(loc), _) => Some(loc),
        (None, 6) => None,
        (None, after_loc) => Some(after_loc),
    };
    let before_flag = fields.change_dict.as_ref().and_then(|d| d.col_i64(4));
    classify_item_row_kind_key(
        before_loc,
        before_flag,
        fields.owner_id,
        fields.location_id,
        fields.flag_id,
        fields.wire_qty,
        snap,
        &ChangeSet::default(),
        &HashMap::new(),
        sde,
    )
}

fn classify_hand_written_fields_fallback(fields: &ItemChangeFields) -> String {
    let loc_kind = |loc: Option<i64>| -> &'static str {
        match loc {
            None | Some(0) => "none",
            Some(6) => "recycling_loc_6",
            Some(id) if (60_000_000..=64_000_000).contains(&id) => "station",
            Some(id) if (30_000_000..=39_999_999).contains(&id) => "solar_system",
            Some(_) => "other",
        }
    };
    let owner_kind = if (90_000_000..98_000_000).contains(&fields.owner_id) {
        "character"
    } else if (98_000_000..99_000_000).contains(&fields.owner_id) {
        "player_corp"
    } else {
        "npc_corp_or_other"
    };
    let before_loc = fields
        .change_dict
        .as_ref()
        .and_then(|d| d.col_i64(3))
        .or_else(|| (fields.location_id != 6).then_some(fields.location_id));
    let loc_before = loc_kind(before_loc);
    let loc_after = loc_kind(Some(fields.location_id));
    let singleton = fields.wire_qty < 0;
    if loc_after == "recycling_loc_6" {
        match fields.change_dict.as_ref().and_then(|d| d.col_i64(4)) {
            Some(bf) => format!(
                "before={loc_before},after={loc_after},flag={},before_flag={bf},owner={owner_kind},singleton={singleton}",
                fields.flag_id
            ),
            None => format!(
                "before={loc_before},after={loc_after},flag={},before_flag=unknown,owner={owner_kind},singleton={singleton}",
                fields.flag_id
            ),
        }
    } else {
        format!(
            "before={loc_before},after={loc_after},flag={},owner={owner_kind},singleton={singleton}",
            fields.flag_id
        )
    }
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

fn extract_item_change_fields_from_row(
    row: &PyRep,
    change_dict: Option<CanonicalChangeDict>,
) -> Option<ItemChangeFields> {
    Some(ItemChangeFields {
        item_id: row_col_i64(row, "itemID")?,
        type_id: row_col_i64(row, "typeID"),
        owner_id: row_col_i64(row, "ownerID")?,
        location_id: row_col_i64(row, "locationID")?,
        flag_id: row_col_i64(row, "flagID")?,
        wire_qty: row_col_i64(row, "quantity")?,
        change_dict,
    })
}

fn extract_multi_item_change_fields_from_entry(
    entry: &PyRep,
    shared_dict: Option<&CanonicalChangeDict>,
) -> Option<ItemChangeFields> {
    if let PyRep::Tuple(pair) = entry {
        if let Some(row) = pair.first() {
            let dict = pair
                .get(1)
                .and_then(CanonicalChangeDict::from_pyrep)
                .or_else(|| shared_dict.cloned());
            return extract_item_change_fields_from_row(row, dict);
        }
    }
    extract_item_change_fields_from_row(entry, shared_dict.cloned())
}

fn extract_item_change_fields(payload_args: &[PyRep]) -> Option<ItemChangeFields> {
    let change_dict = payload_args.get(1).and_then(CanonicalChangeDict::from_pyrep);
    extract_item_change_fields_from_row(payload_args.first()?, change_dict)
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
                let shared_dict = payload_args.get(1).and_then(CanonicalChangeDict::from_pyrep);
                if let Some(PyRep::List(rows) | PyRep::Tuple(rows)) = payload_args.first() {
                    for row in rows {
                        if let Some(fields) =
                            extract_multi_item_change_fields_from_entry(row, shared_dict.as_ref())
                        {
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
    dispatch_transitions: HashMap<u64, HashSet<i64>>,
    dispatch_transition_order: VecDeque<u64>,
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

    fn record_dispatch_transitions(&mut self, scope_id: u64, chars: Vec<i64>) {
        if chars.is_empty() {
            return;
        }
        if !self.dispatch_transitions.contains_key(&scope_id) {
            if self.dispatch_transition_order.len() >= 256 {
                if let Some(old_id) = self.dispatch_transition_order.pop_front() {
                    self.dispatch_transitions.remove(&old_id);
                }
            }
            self.dispatch_transition_order.push_back(scope_id);
        }
        self.dispatch_transitions
            .entry(scope_id)
            .or_default()
            .extend(chars);
    }

    fn is_char_transitioned_in_dispatch(&self, scope_id: u64, char_id: i64) -> bool {
        self.dispatch_transitions
            .get(&scope_id)
            .is_some_and(|set| set.contains(&char_id))
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
                let seq = self.next_seq();
                self.item_counts.get_mut(hw.scope).undo_hand_written_only(
                    hw.seq,
                    &hw.helper,
                    hw.recorded_class_key.as_deref(),
                    hw.recorded_dispatch_method.as_deref(),
                    hw.first_set,
                );
                self.item_cross_scope.record_pair(
                    seq,
                    &hw.helper,
                    entry.scope,
                    hw.scope,
                    Some((
                        entry.fields.item_id,
                        entry.fields.change_dict.as_ref(),
                        hw.fields.change_dict.as_ref(),
                    )),
                );
                return;
            }
        }
        let seq = self.next_seq();
        let helper = entry.mismatched_helper.clone();
        let class_key = entry.class_key.clone();
        let dispatch_method = entry.dispatch_method.clone();
        let first_set = entry.first_set;
        let fields_desc = format_item_fields(&entry.fields);
        let (recorded_class, recorded_method) = self
            .item_counts
            .get_mut(entry.scope)
            .record_projected_only(
                seq,
                helper.as_deref(),
                Some(&class_key),
                dispatch_method.as_deref(),
                first_set,
                || {
                    format!(
                        "cid={} id_type={} {} class={} first_set={} rpc={:?} mismatched_helper={:?}",
                        entry.char_id,
                        entry.id_type,
                        fields_desc,
                        class_key,
                        first_set,
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
                    first_set,
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
                let seq = self.next_seq();
                self.bp_counts.get_mut(hw.scope).undo_hand_written_only(
                    hw.seq,
                    &hw.helper,
                    None,
                    None,
                    false,
                );
                self.bp_cross_scope
                    .record_pair(seq, &hw.helper, entry.scope, hw.scope, None);
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
            false,
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

    #[cfg(test)]
    fn record_projected_batch(
        &mut self,
        scope: CompareOriginScope,
        dispatch_method: Option<String>,
        projected_items: Vec<(i64, &'static str, ItemChangeFields, String)>,
        projected_bps: Vec<(i64, i64)>,
        explicit_effects: u64,
    ) {
        let items = projected_items
            .into_iter()
            .map(|(cid, id_type, fields, class_key)| (cid, id_type, fields, class_key, false))
            .collect();
        self.record_projected_batch_detailed(
            scope,
            dispatch_method,
            None,
            items,
            projected_bps,
            explicit_effects,
        );
    }

    fn record_projected_batch_detailed(
        &mut self,
        scope: CompareOriginScope,
        dispatch_method: Option<String>,
        dispatch_transitions: Option<(u64, Vec<i64>)>,
        projected_items: Vec<(i64, &'static str, ItemChangeFields, String, bool)>,
        projected_bps: Vec<(i64, i64)>,
        explicit_effects: u64,
    ) {
        if let Some((scope_id, chars)) = dispatch_transitions {
            self.record_dispatch_transitions(scope_id, chars);
        }
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
        for (char_id, id_type, fields, class_key, first_set) in projected_items {
            let item_id = fields.item_id;
            push_pending_capped(
                &mut self.pending_items,
                PendingItemCompare {
                    char_id,
                    id_type,
                    fields,
                    class_key,
                    first_set,
                    scope,
                    dispatch_method: effective_dispatch_method.clone(),
                    mismatched_helper: None,
                },
                |p| {
                    p.scope == scope
                        && p.char_id == char_id
                        && p.id_type == id_type
                        && p.fields.item_id == item_id
                },
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
                |p| p.scope == scope && p.char_id == char_id && p.owner_id == owner_id,
                |old| retired_bps.push(old),
            );
        }
        for old in retired_bps {
            self.retire_bp(old);
        }
    }

    #[cfg(test)]
    fn match_or_record_item_row(
        &mut self,
        scope: CompareOriginScope,
        helper: &str,
        recipients: &[(i64, Option<u64>)],
        id_type: &str,
        actual_opt: Option<&ItemChangeFields>,
    ) {
        self.match_or_record_item_row_detailed(scope, None, helper, recipients, id_type, actual_opt, None);
    }

    #[allow(clippy::too_many_arguments)]
    fn match_or_record_item_row_detailed(
        &mut self,
        scope: CompareOriginScope,
        dispatch_method: Option<&str>,
        helper: &str,
        recipients: &[(i64, Option<u64>)],
        id_type: &str,
        actual_opt: Option<&ItemChangeFields>,
        preclassified: Option<(&str, bool)>,
    ) {
        let eff_dispatch_method = if scope == CompareOriginScope::Dispatch {
            Some(dispatch_method.unwrap_or(crate::flight_recorder::UNKNOWN_RPC_LABEL))
        } else {
            None
        };
        for &(cid, _) in recipients {
            let Some(actual) = actual_opt else {
                let seq = self.next_seq();
                self.item_counts.get_mut(scope).record_hand_written_only(
                    seq,
                    helper,
                    None,
                    eff_dispatch_method,
                    false,
                    || {
                        format!(
                            "helper={helper} cid={cid} id_type={id_type} first_set=false unparsed_args"
                        )
                    },
                );
                continue;
            };
            if let Some(pos) = self.pending_items.iter().position(|p| {
                p.scope == scope
                    && p.char_id == cid
                    && p.id_type == id_type
                    && p.fields.matches(actual)
            }) {
                let matched_proj = self
                    .pending_items
                    .remove(pos)
                    .expect("pending item match index valid");
                let seq = self.next_seq();
                let dict_cmp = Some((
                    actual.item_id,
                    matched_proj.fields.change_dict.as_ref(),
                    actual.change_dict.as_ref(),
                ));
                let eff_method = matched_proj
                    .dispatch_method
                    .as_deref()
                    .or(eff_dispatch_method);
                self.item_counts.get_mut(scope).record_matched(
                    seq,
                    helper,
                    Some(&matched_proj.class_key),
                    eff_method,
                    matched_proj.first_set,
                    dict_cmp,
                );
            } else if let Some(p) = self.pending_items.iter_mut().rev().find(|p| {
                p.scope == scope && p.char_id == cid && p.fields.item_id == actual.item_id
            }) {
                p.mismatched_helper = Some(helper.to_string());
                let near_desc = format!(
                    " vs_projected(id_type={} {})",
                    p.id_type,
                    format_item_fields(&p.fields)
                );
                let (hw_class, hw_first_set) = match preclassified {
                    Some((ck, fs)) => (ck.to_string(), fs),
                    None => (classify_hand_written_fields_fallback(actual), false),
                };
                let seq = self.next_seq();
                self.item_counts.get_mut(scope).record_hand_written_only(
                    seq,
                    helper,
                    Some(&hw_class),
                    eff_dispatch_method,
                    hw_first_set,
                    || {
                        format!(
                            "helper={helper} cid={cid} id_type={id_type} class={hw_class} first_set={hw_first_set} rpc={eff_dispatch_method:?} {}{near_desc}",
                            format_item_fields(actual)
                        )
                    },
                );
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
                let seq = self.next_seq();
                self.item_cross_scope.record_pair(
                    seq,
                    helper,
                    matched_proj.scope,
                    scope,
                    Some((
                        actual.item_id,
                        matched_proj.fields.change_dict.as_ref(),
                        actual.change_dict.as_ref(),
                    )),
                );
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
                let seq = self.next_seq();
                self.item_counts
                    .get_mut(unmatched_proj.scope)
                    .undo_projected_only(
                        unmatched_proj.seq,
                        None,
                        Some(&unmatched_proj.recorded_class_key),
                        unmatched_proj.recorded_dispatch_method.as_deref(),
                        unmatched_proj.first_set,
                    );
                self.item_cross_scope.record_pair(
                    seq,
                    helper,
                    unmatched_proj.scope,
                    scope,
                    Some((
                        actual.item_id,
                        unmatched_proj.fields.change_dict.as_ref(),
                        actual.change_dict.as_ref(),
                    )),
                );
            } else {
                let (hw_class, hw_first_set) = match preclassified {
                    Some((ck, fs)) => (ck.to_string(), fs),
                    None => (classify_hand_written_fields_fallback(actual), false),
                };
                let seq = self.next_seq();
                let (recorded_class, recorded_method) =
                    self.item_counts.get_mut(scope).record_hand_written_only(
                        seq,
                        helper,
                        Some(&hw_class),
                        eff_dispatch_method,
                        hw_first_set,
                        || {
                            format!(
                                "helper={helper} cid={cid} id_type={id_type} class={hw_class} first_set={hw_first_set} rpc={eff_dispatch_method:?} {}",
                                format_item_fields(actual)
                            )
                        },
                    );
                push_window_capped(
                    &mut self.unmatched_hand_written_items,
                    UnmatchedHandWrittenItem {
                        seq,
                        char_id: cid,
                        id_type: id_type.to_string(),
                        fields: actual.clone(),
                        recorded_class_key: recorded_class,
                        recorded_dispatch_method: recorded_method,
                        first_set: hw_first_set,
                        scope,
                        helper: helper.to_string(),
                    },
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_hand_written(
        &mut self,
        scope: CompareOriginScope,
        dispatch_method: Option<&str>,
        default_helper: &str,
        recipients: &[(i64, Option<u64>)],
        id_type: &str,
        event: &str,
        payload_args: &[PyRep],
        preclassified_rows: &[Option<(String, bool)>],
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
        let eff_dispatch_method = if scope == CompareOriginScope::Dispatch {
            Some(dispatch_method.unwrap_or(crate::flight_recorder::UNKNOWN_RPC_LABEL))
        } else {
            None
        };
        self.reported_to_file = false;
        self.logged_summary = false;
        match event {
            "OnItemChange" => {
                let actual_opt = extract_item_change_fields(payload_args);
                let pre = preclassified_rows
                    .first()
                    .and_then(|o| o.as_ref())
                    .map(|(ck, fs)| (ck.as_str(), *fs));
                self.match_or_record_item_row_detailed(
                    scope,
                    dispatch_method,
                    helper,
                    recipients,
                    id_type,
                    actual_opt.as_ref(),
                    pre,
                );
            }
            "OnMultipleItemChange" => match payload_args.first() {
                Some(PyRep::List(rows) | PyRep::Tuple(rows)) if !rows.is_empty() => {
                    let shared_dict = payload_args.get(1).and_then(CanonicalChangeDict::from_pyrep);
                    for (idx, row) in rows.iter().enumerate() {
                        let actual_opt =
                            extract_multi_item_change_fields_from_entry(row, shared_dict.as_ref());
                        let pre = preclassified_rows
                            .get(idx)
                            .and_then(|o| o.as_ref())
                            .map(|(ck, fs)| (ck.as_str(), *fs));
                        self.match_or_record_item_row_detailed(
                            scope,
                            dispatch_method,
                            helper,
                            recipients,
                            id_type,
                            actual_opt.as_ref(),
                            pre,
                        );
                    }
                }
                _ => {
                    self.match_or_record_item_row_detailed(
                        scope,
                        dispatch_method,
                        helper,
                        recipients,
                        id_type,
                        None,
                        None,
                    );
                }
            },
            "OnBlueprintsUpdated" => {
                let owner_opt = extract_blueprint_owner_id(payload_args);
                for &(cid, _) in recipients {
                    let Some(owner_id) = owner_opt else {
                        let seq = self.next_seq();
                        self.bp_counts.get_mut(scope).record_hand_written_only(
                            seq,
                            helper,
                            None,
                            eff_dispatch_method,
                            false,
                            || format!("helper={helper} cid={cid} unparsed_owner"),
                        );
                        continue;
                    };
                    if let Some(pos) = self.pending_bps.iter().position(|p| {
                        p.scope == scope && p.char_id == cid && p.owner_id == owner_id
                    }) {
                        let matched_bp = self
                            .pending_bps
                            .remove(pos)
                            .expect("pending bp match index valid");
                        let seq = self.next_seq();
                        self.bp_counts.get_mut(scope).record_matched(
                            seq,
                            helper,
                            None,
                            matched_bp.dispatch_method.as_deref().or(eff_dispatch_method),
                            false,
                            None,
                        );
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
                        let seq = self.next_seq();
                        self.bp_cross_scope.record_pair(
                            seq,
                            helper,
                            matched_bp.scope,
                            scope,
                            None,
                        );
                    } else if let Some(pos) = self.unmatched_projected_bps.iter().position(|u| {
                        u.scope != scope && u.char_id == cid && u.owner_id == owner_id
                    }) {
                        let unmatched_bp = self
                            .unmatched_projected_bps
                            .remove(pos)
                            .expect("cross-scope unmatched projected bp index valid");
                        let seq = self.next_seq();
                        self.bp_counts.get_mut(unmatched_bp.scope).undo_projected_only(
                            unmatched_bp.seq,
                            None,
                            None,
                            unmatched_bp.recorded_dispatch_method.as_deref(),
                            false,
                        );
                        self.bp_cross_scope.record_pair(
                            seq,
                            helper,
                            unmatched_bp.scope,
                            scope,
                            None,
                        );
                    } else {
                        let seq = self.next_seq();
                        self.bp_counts.get_mut(scope).record_hand_written_only(
                            seq,
                            helper,
                            None,
                            eff_dispatch_method,
                            false,
                            || format!("helper={helper} cid={cid} owner={owner_id}"),
                        );
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
    in_dispatch: bool,
) -> (Vec<(i64, &'static str, ItemChangeFields, String, bool)>, Vec<(i64, i64)>) {
    let item_input = DeltaProjectionInput::new(None, snap, changes, sde, now_unix);
    let resolved_items = item_projector.project_and_resolve(&item_input, online);
    let changed_chars = index_changed_characters(changes);
    let item_changes_by_id = {
        let mut map: HashMap<i64, ItemBeforeAfter<'_>> = HashMap::new();
        for ch in &changes.items {
            map.entry(ch.key)
                .and_modify(|entry| entry.1 = ch.after.as_deref())
                .or_insert((ch.before.as_deref(), ch.after.as_deref()));
        }
        map
    };
    let mut projected_items: Vec<(i64, &'static str, ItemChangeFields, String, bool)> = Vec::new();
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
                let (before_rec, after_rec) = item_changes_by_id
                    .get(&fields.item_id)
                    .copied()
                    .unwrap_or((None, None));
                let class_key = classify_projected_item(
                    before_rec,
                    &fields,
                    snap,
                    changes,
                    &item_changes_by_id,
                    sde,
                );
                let first_set = is_first_set_item_change_pair(
                    in_dispatch,
                    r.recipient_char_id,
                    before_rec,
                    after_rec,
                    snap,
                    &changed_chars,
                    online,
                    sde,
                )
                .is_some();
                let key = (r.recipient_char_id, r.id_type, fields.item_id);
                if let Some(&idx) = seen_items.get(&key) {
                    projected_items[idx] =
                        (r.recipient_char_id, r.id_type, fields, class_key, first_set);
                } else {
                    seen_items.insert(key, projected_items.len());
                    projected_items.push((
                        r.recipient_char_id,
                        r.id_type,
                        fields,
                        class_key,
                        first_set,
                    ));
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
    ev.sample_dictionary_differs.truncate(MAX_NDJSON_SAMPLES);
    for sc in [&mut ev.by_scope.dispatch, &mut ev.by_scope.tick, &mut ev.by_scope.neither] {
        sc.sample_projected_only.truncate(MAX_NDJSON_SAMPLES);
        sc.sample_hand_written_only.truncate(MAX_NDJSON_SAMPLES);
        sc.sample_dictionary_differs.truncate(MAX_NDJSON_SAMPLES);
    }
    ev
}

fn append_ndjson_record(
    path: &Path,
    bridge_id: u64,
    summary: &ItemCompareSummary,
) {
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
        item_dictionary_same = summary.item_change.dictionary_same,
        item_dictionary_differs = summary.item_change.dictionary_differs,
        item_first_set_matched = summary.item_change.first_set.matched,
        item_first_set_hand_written_only = summary.item_change.first_set.hand_written_only,
        item_first_set_projected_only = summary.item_change.first_set.projected_only,
        bp_matched = summary.blueprints_updated.matched,
        bp_matched_across_scopes = summary.blueprints_updated.matched_across_scopes,
        bp_hand_written_only = summary.blueprints_updated.hand_written_only,
        bp_projected_only = summary.blueprints_updated.projected_only,
        bp_pending = summary.blueprints_updated.pending,
        explicit_in_tx = summary.explicit_in_tx_effects,
        item_helpers = ?summary.item_change.by_helper,
        item_dictionary_by_helper = ?summary.item_change.dictionary_by_helper,
        item_cross_scope_helpers = ?summary.item_change.matched_across_scopes_by_helper,
        item_cross_scope_pairs = ?summary.item_change.matched_across_scopes_by_scope_pair,
        item_matched_by_class = ?summary.item_change.matched_by_class,
        item_matched_by_dispatch_method = ?summary.item_change.matched_by_dispatch_method,
        item_hand_written_only_by_class = ?summary.item_change.hand_written_only_by_class,
        item_hand_written_only_by_dispatch_method = ?summary.item_change.hand_written_only_by_dispatch_method,
        item_projected_only_by_class = ?summary.item_change.projected_only_by_class,
        item_projected_only_by_dispatch_method = ?summary.item_change.projected_only_by_dispatch_method,
        item_contexts_by_class = ?summary.item_change.contexts_by_class,
        item_first_set = ?summary.item_change.first_set,
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
        let dispatch_scope_id = dispatch_scope_info.as_ref().map(|(id, _)| *id);
        let transitioned_chars: Vec<i64> = if dispatch_scope_id.is_some() && !changes.characters.is_empty() {
            let changed_chars = index_changed_characters(changes);
            changed_chars
                .keys()
                .copied()
                .filter(|&cid| is_character_scene_or_ship_transition(cid, &changed_chars))
                .collect()
        } else {
            Vec::new()
        };
        if changes.items.is_empty() && effects.explicit_count == 0 {
            if let Some(scope_id) = dispatch_scope_id {
                if !transitioned_chars.is_empty() {
                    let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
                    ledger.record_dispatch_transitions(scope_id, transitioned_chars);
                }
            }
            return;
        }
        let in_dispatch = dispatch_scope_info.is_some();
        let (mut projected_items, projected_bps) = if !changes.items.is_empty() {
            collect_compare_item_expectations(
                &self.projector,
                snap,
                changes,
                sde,
                now_unix,
                online,
                effects,
                in_dispatch,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        if !projected_items.is_empty()
            || !projected_bps.is_empty()
            || effects.explicit_count > 0
            || (dispatch_scope_id.is_some() && !transitioned_chars.is_empty())
        {
            let tid = std::thread::current().id();
            let dispatch_method = dispatch_scope_info.and_then(|(_, method)| method);
            let dispatch_transitions = dispatch_scope_id
                .filter(|_| !transitioned_chars.is_empty())
                .map(|sid| (sid, transitioned_chars));
            let mut ledger = self.ledger.lock().unwrap_or_else(|e| e.into_inner());
            let scope = resolve_compare_origin_scope(
                in_dispatch,
                ledger.tick_sync_depths_by_thread.contains_key(&tid),
            );
            if let Some((sid, _)) = &dispatch_transitions {
                ledger.record_dispatch_transitions(*sid, dispatch_transitions.as_ref().map(|t| t.1.clone()).unwrap_or_default());
            }
            if let Some(sid) = dispatch_scope_id {
                for entry in &mut projected_items {
                    if ledger.is_char_transitioned_in_dispatch(sid, entry.0) {
                        entry.4 = false;
                    }
                }
            }
            ledger.record_projected_batch_detailed(
                scope,
                dispatch_method,
                None,
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
            let has_activity = summary.total_activity() > 0;
            let should_log = if log_msg.is_some() && !ledger.logged_summary && has_activity {
                ledger.logged_summary = true;
                true
            } else {
                false
            };
            let target = if write_ndjson && !ledger.reported_to_file && has_activity {
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

fn preclassify_hand_written_row(
    f: &ItemChangeFields,
    in_dispatch: bool,
    any_transitioned: bool,
    recipients: &[(i64, Option<u64>)],
    snap: &WorldStateSnapshot,
    ctx: &ServerContext,
) -> (String, bool) {
    let class_key = classify_hand_written_item_in_snap(f, snap, &ctx.sde);
    let first_set = if in_dispatch && !any_transitioned && recipients.len() == 1 {
        let char_id = recipients[0].0;
        let kind = if f.location_id == 6 {
            ProjectedItemOpKind::Deleted
        } else {
            ProjectedItemOpKind::CreatedOrUpdated
        };
        let fp = ItemWireFingerprint {
            owner_id: f.owner_id,
            location_id: f.location_id,
            flag_id: f.flag_id,
            wire_qty: f.wire_qty,
        };
        evaluate_hand_written_first_set_in_snap(
            true,
            char_id,
            recipients.len(),
            kind,
            fp,
            f.change_dict.as_ref(),
            snap,
            &*ctx.db,
            &ctx.sde,
        )
        .is_some()
    } else {
        false
    };
    (class_key, first_set)
}

fn preclassify_hand_written_event(
    ctx: &ServerContext,
    in_dispatch: bool,
    any_transitioned: bool,
    recipients: &[(i64, Option<u64>)],
    event: &str,
    payload_args: &[PyRep],
) -> Vec<Option<(String, bool)>> {
    match event {
        "OnItemChange" => {
            let Some(fields) = extract_item_change_fields(payload_args) else {
                return Vec::new();
            };
            let classified = ctx.db.with_snapshot(|snap| {
                preclassify_hand_written_row(&fields, in_dispatch, any_transitioned, recipients, snap, ctx)
            });
            vec![Some(classified)]
        }
        "OnMultipleItemChange" => {
            let Some(PyRep::List(rows) | PyRep::Tuple(rows)) = payload_args.first() else {
                return Vec::new();
            };
            if rows.is_empty() {
                return Vec::new();
            }
            let shared_dict = payload_args.get(1).and_then(CanonicalChangeDict::from_pyrep);
            let extracted: Vec<Option<ItemChangeFields>> = rows
                .iter()
                .map(|r| extract_multi_item_change_fields_from_entry(r, shared_dict.as_ref()))
                .collect();
            if extracted.iter().all(Option::is_none) {
                return vec![None; rows.len()];
            }
            ctx.db.with_snapshot(|snap| {
                extracted
                    .iter()
                    .map(|opt| {
                        opt.as_ref().map(|fields| {
                            preclassify_hand_written_row(
                                fields,
                                in_dispatch,
                                any_transitioned,
                                recipients,
                                snap,
                                ctx,
                            )
                        })
                    })
                    .collect()
            })
        }
        _ => Vec::new(),
    }
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
    let scope_info = super::cache_notifications::current_dispatch_staging_scope_info(
        Some(&*ctx.pending_notifications),
        Some(&ctx.notify_tx),
    );
    let in_dispatch = scope_info.is_some();
    let tid = std::thread::current().id();
    let any_transitioned = {
        let ledger = bridge.ledger.lock().unwrap_or_else(|e| e.into_inner());
        if !ledger.is_enabled() {
            return;
        }
        if event == "OnMultipleItemChange"
            && ledger
                .active_sources_by_thread
                .get(&tid)
                .and_then(|stack| stack.last().copied())
                == Some(PAIRED_MULTI_ITEM_SKIP_SOURCE)
        {
            return;
        }
        scope_info.as_ref().is_some_and(|(sid, _)| {
            recipients
                .iter()
                .any(|&(cid, _)| ledger.is_char_transitioned_in_dispatch(*sid, cid))
        })
    };
    let preclassified_rows = preclassify_hand_written_event(
        ctx,
        in_dispatch,
        any_transitioned,
        recipients,
        event,
        payload_args,
    );
    let dispatch_method = scope_info.and_then(|(_, m)| m);
    let mut ledger = bridge.ledger.lock().unwrap_or_else(|e| e.into_inner());
    if !ledger.is_enabled() {
        return;
    }
    let scope = resolve_compare_origin_scope(
        in_dispatch,
        ledger.tick_sync_depths_by_thread.contains_key(&tid),
    );
    ledger.record_hand_written(
        scope,
        dispatch_method.as_deref(),
        default_helper,
        recipients,
        id_type,
        event,
        payload_args,
        &preclassified_rows,
    );
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
