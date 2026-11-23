//! Per-class default-on projection switches for character-owned same-scene item mutations
//! and deletions (`#931` Track 3B / 3C-1).
//!
//! Guardrails enforced by this module:
//! 1. **On by default with env kill switches:** `char_same_scene_item_mutation`
//!    (`EVOMU_PROJECTED_ITEM_MUTATION`) and `char_same_scene_item_deleted`
//!    (`EVOMU_PROJECTED_ITEM_DELETED`) both default to `true` (unset/empty/unrecognized stays ON;
//!    `0`/`false`/`no`/`off` disable).
//! 2. **First-set rule only:** Commit must occur inside an RPC dispatch (`dispatch_scope_info`
//!    present — never tick or unscoped), for an online human character with no scene or
//!    `active_ship_id` change in the commit, where before and after the change the row sits directly
//!    in either the hangar (`flag 4`) of the station/structure the pilot is docked in or the cargo
//!    hold (`flag 5`) of the pilot's active ship.
//! 3. **Character-owned only:** Only human character owners present in `snap.characters` and
//!    absent from player/NPC corporation tables (with unchanged owner) are eligible; corporation,
//!    NPC-corp, and cross-owner item changes stay compare-only.
//! 4. **Dispatch-scoped zero-eviction dedupe + change-dict comparison:** Tokens are stored per
//!    `dispatch_scope_id` with no artificial 4,096 eviction cap, consumed only inside the same
//!    dispatch, invalidated on subsequent commits touching `item_id`, and expired automatically when
//!    the dispatch scope drops. Matches also compare the canonicalized 2nd-arg change dictionary.
//! 5. **Send helpers without a token and split no-token telemetry (`#931` Check #39):** First-set
//!    helper calls that find no matching token still send their notification and are split into
//!    `helper_without_token_with_projected_event` (the same item had a projected event in this
//!    dispatch — an intermediate/superseded state such as `marketProxy.PlaceMultiSellOrder`) vs
//!    `helper_without_token_no_projected_event` (the same item had no projected event in this
//!    dispatch — nothing changed behind the call, such as no-op `invbroker.Add` /
//!    `invbroker.MultiAdd`). Switched-run pass criterion: zero evictions, both split counters are
//!    reported, every RPC in `helper_without_token_no_projected_event` is explained, every
//!    `dictionary_differs` pattern is explained, and no new client error occurs.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use eve_db::{
    ChangeSet, CharacterRecord, InventoryFlag, InventoryItemRecord, RowChange, WorldStateSnapshot,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;

use super::cache_notifications::{
    assemble_pending_notification, register_dispatch_switch_handle, PendingMachoNotification,
};
use super::delta_item_compare::TxItemEffectKeys;
use super::delta_projection::{build_projected_item_change_args, OnlineCharacterLookup};
use super::ServerContext;

const MAX_CONTAINER_WALK_DEPTH: usize = 8;
const MAX_SWITCH_TELEMETRY_BUCKETS: usize = 512;
const MAX_CALL_SITE_TELEMETRY_BUCKETS: usize = 2048;
const MAX_SWITCH_TELEMETRY_SAMPLES: usize = 16;
const OTHER_CLASS_BUCKET: &str = "<other_class>";
const OTHER_RPC_BUCKET: &str = "<other_rpc>";
const OTHER_KEY_BUCKET: &str = "<other_key>";

/// Explicit per-class projection toggles stored on [`ServerContext`] (`#931` Track 3B / 3C-1).
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
}

impl Default for ProjectedItemSwitchConfig {
    #[inline]
    fn default() -> Self {
        Self::enabled()
    }
}

impl ProjectedItemSwitchConfig {
    /// Returns both first-set item projection switches enabled (the default since Track 3C-1).
    #[inline]
    pub const fn enabled() -> Self {
        Self {
            char_same_scene_item_mutation: true,
            char_same_scene_item_deleted: true,
        }
    }

    /// Returns both first-set item projection switches disabled (compare-only / kill-switched).
    #[inline]
    pub const fn disabled() -> Self {
        Self {
            char_same_scene_item_mutation: false,
            char_same_scene_item_deleted: false,
        }
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

    pub(crate) fn from_env_lookup<F: Fn(&str) -> Option<String>>(get: F) -> Self {
        let mutation = get("EVOMU_PROJECTED_ITEM_MUTATION");
        let deleted = get("EVOMU_PROJECTED_ITEM_DELETED");
        Self {
            char_same_scene_item_mutation: Self::parse_env_flag_default(mutation.as_deref(), true),
            char_same_scene_item_deleted: Self::parse_env_flag_default(deleted.as_deref(), true),
        }
    }

    /// Reads per-class switches from process environment (`true`/`true` when unset or empty;
    /// `0`/`false`/`no`/`off` disable each class independently).
    pub fn from_env() -> Self {
        Self::from_env_lookup(|key| std::env::var(key).ok())
    }

    /// Returns `true` when at least one projection switch is enabled.
    #[inline]
    pub const fn is_any_enabled(self) -> bool {
        self.char_same_scene_item_mutation || self.char_same_scene_item_deleted
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

/// Per-call-site (`file:line`) telemetry counters collected via `#[track_caller]` (`#931` Track 3C-1).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SwitchCallSiteCounts {
    /// First-set helper calls replaced/suppressed by a commit-time projection token.
    pub replaced: u64,
    /// Non-first-set (or non-dispatch/transitioned/corp) helper calls emitted normally.
    pub non_first_set_emitted: u64,
    /// First-set helper calls without a token where the same item had a projected event in dispatch.
    pub helper_without_token_with_projected_event: u64,
    /// First-set helper calls without a token where the same item had no projected event in dispatch.
    pub helper_without_token_no_projected_event: u64,
}

/// Per-bucket telemetry counters for active item projection switches (`#931` Check #28–#30, #39).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SwitchTelemetryCounts {
    pub emitted: u64,
    pub replaced: u64,
    pub new: u64,
    /// Total first-set helper calls without a matching commit token
    /// (`helper_without_token_with_projected_event + helper_without_token_no_projected_event`).
    pub helper_without_token: u64,
    /// First-set helper calls without a matching token where the same item had a projected event
    /// in this dispatch (intermediate state superseded by the committed state or later commit).
    pub helper_without_token_with_projected_event: u64,
    /// First-set helper calls without a matching token where the same item had no projected event
    /// in this dispatch (nothing changed behind the call).
    pub helper_without_token_no_projected_event: u64,
    pub dictionary_same: u64,
    pub dictionary_differs: u64,
}

/// Observability summary across all switched-on item projection runs (`#931` Track 3B).
///
/// Switched-run pass criterion (`#931` Check #39):
/// - `evictions == 0`
/// - Both `helper_without_token_with_projected_event` and
///   `helper_without_token_no_projected_event` are reported, and every RPC appearing in
///   `no_projected_event_rpcs()` (`helper_without_token_no_projected_event > 0`) is explained
/// - Every `dictionary_differs` pattern is explained, and no new client error occurs
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectedItemSwitchSummary {
    pub config: ProjectedItemSwitchConfig,
    pub emitted: u64,
    pub replaced: u64,
    pub new: u64,
    pub non_first_set_emitted: u64,
    /// Total first-set helper calls without a matching commit token
    /// (`helper_without_token_with_projected_event + helper_without_token_no_projected_event`).
    pub helper_without_token: u64,
    /// First-set helper calls without a matching token where the same item had a projected event
    /// in this dispatch (intermediate/superseded state).
    pub helper_without_token_with_projected_event: u64,
    /// First-set helper calls without a matching token where the same item had no projected event
    /// in this dispatch (nothing changed behind the call).
    pub helper_without_token_no_projected_event: u64,
    pub dictionary_same: u64,
    pub dictionary_differs: u64,
    pub evictions: u64,
    pub by_class: BTreeMap<String, SwitchTelemetryCounts>,
    pub by_dispatch_method: BTreeMap<String, SwitchTelemetryCounts>,
    pub by_helper: BTreeMap<String, SwitchTelemetryCounts>,
    pub by_class_rpc_helper: BTreeMap<String, SwitchTelemetryCounts>,
    pub by_call_site: BTreeMap<String, SwitchCallSiteCounts>,
    pub sample_new: Vec<String>,
    pub sample_helper_without_token: Vec<String>,
    pub sample_helper_without_token_with_projected_event: Vec<String>,
    pub sample_helper_without_token_no_projected_event: Vec<String>,
    pub sample_dictionary_differs: Vec<String>,
}

impl ProjectedItemSwitchSummary {
    /// Returns `true` when any switch telemetry counter or call-site bucket recorded activity.
    #[inline]
    pub fn has_activity(&self) -> bool {
        self.emitted > 0
            || self.replaced > 0
            || self.new > 0
            || self.non_first_set_emitted > 0
            || self.helper_without_token > 0
            || self.helper_without_token_with_projected_event > 0
            || self.helper_without_token_no_projected_event > 0
            || self.dictionary_same > 0
            || self.dictionary_differs > 0
            || self.evictions > 0
            || !self.by_call_site.is_empty()
    }

    /// Returns dispatch RPC methods that triggered `helper_without_token_no_projected_event`.
    pub fn no_projected_event_rpcs(&self) -> BTreeMap<String, u64> {
        self.by_dispatch_method
            .iter()
            .filter_map(|(rpc, counts)| {
                (counts.helper_without_token_no_projected_event > 0)
                    .then_some((rpc.clone(), counts.helper_without_token_no_projected_event))
            })
            .collect()
    }

    /// Renders a markdown report detailing both split `helper_without_token` counters, every RPC
    /// in `no_projected_event`, per-call-site `#[track_caller]` attribution, and
    /// `first_set`-tagged dictionary differences (`#931` Check #39 / Track 3C-1).
    pub fn format_markdown_report(&self) -> String {
        let mut out = String::new();
        out.push_str("### Item Projection Switch Telemetry Summary\n");
        out.push_str(&format!(
            "- Switches: `char_same_scene_item_mutation={}`, `char_same_scene_item_deleted={}`\n",
            self.config.char_same_scene_item_mutation,
            self.config.char_same_scene_item_deleted
        ));
        out.push_str(&format!(
            "- Totals: `emitted={}`, `replaced={}`, `new={}`, `non_first_set_emitted={}`, `evictions={}`, `dictionary_same={}`, `dictionary_differs={}`\n",
            self.emitted,
            self.replaced,
            self.new,
            self.non_first_set_emitted,
            self.evictions,
            self.dictionary_same,
            self.dictionary_differs
        ));
        out.push_str(&format!(
            "- Helper without token (`helper_without_token={}`): `helper_without_token_with_projected_event={}` (intermediate/superseded), `helper_without_token_no_projected_event={}` (no projected event in dispatch)\n",
            self.helper_without_token,
            self.helper_without_token_with_projected_event,
            self.helper_without_token_no_projected_event
        ));
        let no_token_rpcs: Vec<(&String, &SwitchTelemetryCounts)> = self
            .by_dispatch_method
            .iter()
            .filter(|(_, c)| c.helper_without_token > 0)
            .collect();
        if !no_token_rpcs.is_empty() {
            out.push_str("\n| Dispatch RPC | `with_projected_event` | `no_projected_event` | Total `helper_without_token` |\n");
            out.push_str("|---|---:|---:|---:|\n");
            for (rpc, c) in no_token_rpcs {
                out.push_str(&format!(
                    "| `{rpc}` | {} | {} | {} |\n",
                    c.helper_without_token_with_projected_event,
                    c.helper_without_token_no_projected_event,
                    c.helper_without_token
                ));
            }
        }
        if !self.by_call_site.is_empty() {
            out.push_str("\n| Call Site | `replaced` | `non_first_set_emitted` | `with_projected_event` | `no_projected_event` |\n");
            out.push_str("|---|---:|---:|---:|---:|\n");
            for (call_site, c) in &self.by_call_site {
                out.push_str(&format!(
                    "| `{call_site}` | {} | {} | {} | {} |\n",
                    c.replaced,
                    c.non_first_set_emitted,
                    c.helper_without_token_with_projected_event,
                    c.helper_without_token_no_projected_event,
                ));
            }
        }
        if !self.sample_dictionary_differs.is_empty() {
            out.push_str("\nDictionary-difference samples:\n");
            for sample in &self.sample_dictionary_differs {
                out.push_str(&format!("- `{sample}`\n"));
            }
        }
        out
    }
}

fn bucket_mut<'a>(
    map: &'a mut BTreeMap<String, SwitchTelemetryCounts>,
    key: &str,
    overflow_key: &str,
) -> &'a mut SwitchTelemetryCounts {
    let target = if map.contains_key(key) || map.len() < MAX_SWITCH_TELEMETRY_BUCKETS {
        key
    } else {
        overflow_key
    };
    map.entry(target.to_string()).or_default()
}

fn call_site_bucket_mut<'a>(
    map: &'a mut BTreeMap<String, SwitchCallSiteCounts>,
    call_site: &str,
) -> &'a mut SwitchCallSiteCounts {
    let target = if map.contains_key(call_site) || map.len() < MAX_CALL_SITE_TELEMETRY_BUCKETS {
        call_site
    } else {
        OTHER_KEY_BUCKET
    };
    map.entry(target.to_string()).or_default()
}

#[inline]
#[track_caller]
fn caller_call_site() -> String {
    let loc = std::panic::Location::caller();
    format!("{}:{}", loc.file().replace('\\', "/"), loc.line())
}

#[derive(Debug, Clone)]
struct ProjectedDedupeEntry {
    kind: ProjectedItemOpKind,
    fingerprint: ItemWireFingerprint,
    change_dict: Option<CanonicalChangeDict>,
    class_key: String,
    rpc_method: String,
}

#[derive(Debug, Default)]
struct DispatchTokenBucket {
    entries: HashMap<(i64, i64), ProjectedDedupeEntry>,
    /// Tokens available for stripping already-projected rows from hand-written
    /// `OnMultipleItemChange` batches within this dispatch scope (`#931` `BF-931-3C1-03`).
    multi_entries: HashMap<(i64, i64), ProjectedDedupeEntry>,
    /// `(char_id, item_id)` pairs that received a switched projected `OnItemChange` event in this
    /// dispatch scope (preserved until dispatch scope expiry so intermediate/superseded helper
    /// calls without tokens are distinguished from calls where nothing changed).
    projected_items: HashSet<(i64, i64)>,
    transitioned_chars: HashSet<i64>,
    /// `monitor_id -> set of type_ids` already covered by coalesced commit-time
    /// `OnIndustryMaterials` emissions in this dispatch scope (`#931` `BF-931-3C1-02`).
    covered_industry_monitors: HashMap<i64, HashSet<u32>>,
}

#[derive(Debug, Default)]
struct SwitchTelemetryState {
    emitted: u64,
    replaced: u64,
    new: u64,
    non_first_set_emitted: u64,
    helper_without_token: u64,
    helper_without_token_with_projected_event: u64,
    helper_without_token_no_projected_event: u64,
    dictionary_same: u64,
    dictionary_differs: u64,
    evictions: u64,
    by_class: BTreeMap<String, SwitchTelemetryCounts>,
    by_dispatch_method: BTreeMap<String, SwitchTelemetryCounts>,
    by_helper: BTreeMap<String, SwitchTelemetryCounts>,
    by_class_rpc_helper: BTreeMap<String, SwitchTelemetryCounts>,
    by_call_site: BTreeMap<String, SwitchCallSiteCounts>,
    sample_new: Vec<String>,
    sample_helper_without_token: Vec<String>,
    sample_helper_without_token_with_projected_event: Vec<String>,
    sample_helper_without_token_no_projected_event: Vec<String>,
    sample_dictionary_differs: Vec<String>,
}

impl SwitchTelemetryState {
    fn bump(
        &mut self,
        class_key: &str,
        rpc_method: &str,
        helper: Option<&str>,
        f: impl Fn(&mut SwitchTelemetryCounts),
    ) {
        let helper_str = helper.unwrap_or("<none>");
        f(bucket_mut(&mut self.by_class, class_key, OTHER_CLASS_BUCKET));
        f(bucket_mut(&mut self.by_dispatch_method, rpc_method, OTHER_RPC_BUCKET));
        if let Some(h) = helper {
            f(bucket_mut(&mut self.by_helper, h, OTHER_KEY_BUCKET));
        }
        let compound = format!("class={class_key}|rpc={rpc_method}|helper={helper_str}");
        f(bucket_mut(&mut self.by_class_rpc_helper, &compound, OTHER_KEY_BUCKET));
    }

    fn record_emitted(&mut self, class_key: &str, rpc_method: &str) {
        self.emitted += 1;
        self.bump(class_key, rpc_method, None, |c| c.emitted += 1);
    }

    #[allow(clippy::too_many_arguments)]
    fn record_replaced(
        &mut self,
        class_key: &str,
        rpc_method: &str,
        helper: &str,
        call_site: &str,
        item_id: i64,
        expected_dict: Option<&CanonicalChangeDict>,
        actual_dict: Option<&CanonicalChangeDict>,
    ) {
        self.replaced += 1;
        call_site_bucket_mut(&mut self.by_call_site, call_site).replaced += 1;
        let same_dict = matches!((expected_dict, actual_dict), (Some(a), Some(b)) if a == b);
        if same_dict {
            self.dictionary_same += 1;
        } else {
            self.dictionary_differs += 1;
            if self.sample_dictionary_differs.len() < MAX_SWITCH_TELEMETRY_SAMPLES {
                let exp_s = expected_dict.map_or_else(|| "None".to_string(), CanonicalChangeDict::format_compact);
                let act_s = actual_dict.map_or_else(|| "None".to_string(), CanonicalChangeDict::format_compact);
                self.sample_dictionary_differs.push(format!(
                    "item={item_id} first_set=true class={class_key} rpc={rpc_method} helper={helper} site={call_site} projected_dict={exp_s} actual_dict={act_s}"
                ));
            }
        }
        self.bump(class_key, rpc_method, Some(helper), |c| {
            c.replaced += 1;
            if same_dict {
                c.dictionary_same += 1;
            } else {
                c.dictionary_differs += 1;
            }
        });
    }

    fn record_non_first_set_emitted(&mut self, call_site: &str) {
        self.non_first_set_emitted += 1;
        call_site_bucket_mut(&mut self.by_call_site, call_site).non_first_set_emitted += 1;
    }

    fn record_new(&mut self, item_id: i64, entry: &ProjectedDedupeEntry) {
        self.new += 1;
        if self.sample_new.len() < MAX_SWITCH_TELEMETRY_SAMPLES {
            self.sample_new.push(format!(
                "item={item_id} kind={:?} class={} rpc={}",
                entry.kind, entry.class_key, entry.rpc_method
            ));
        }
        self.bump(&entry.class_key, &entry.rpc_method, None, |c| c.new += 1);
    }

    #[allow(clippy::too_many_arguments)]
    fn record_helper_without_token(
        &mut self,
        had_projected_event: bool,
        class_key: &str,
        rpc_method: &str,
        helper: &str,
        call_site: &str,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        actual_dict: Option<&CanonicalChangeDict>,
    ) {
        self.helper_without_token += 1;
        let site_counts = call_site_bucket_mut(&mut self.by_call_site, call_site);
        if had_projected_event {
            self.helper_without_token_with_projected_event += 1;
            site_counts.helper_without_token_with_projected_event += 1;
        } else {
            self.helper_without_token_no_projected_event += 1;
            site_counts.helper_without_token_no_projected_event += 1;
        }
        let need_total_sample = self.sample_helper_without_token.len() < MAX_SWITCH_TELEMETRY_SAMPLES;
        let need_split_sample = if had_projected_event {
            self.sample_helper_without_token_with_projected_event.len() < MAX_SWITCH_TELEMETRY_SAMPLES
        } else {
            self.sample_helper_without_token_no_projected_event.len() < MAX_SWITCH_TELEMETRY_SAMPLES
        };
        if need_total_sample || need_split_sample {
            let act_s = actual_dict.map_or_else(|| "None".to_string(), CanonicalChangeDict::format_compact);
            let sample = format!(
                "cid={char_id} item={item_id} kind={kind:?} had_projected_event={had_projected_event} class={class_key} rpc={rpc_method} helper={helper} site={call_site} dict={act_s}"
            );
            if need_total_sample {
                self.sample_helper_without_token.push(sample.clone());
            }
            if need_split_sample {
                if had_projected_event {
                    self.sample_helper_without_token_with_projected_event.push(sample);
                } else {
                    self.sample_helper_without_token_no_projected_event.push(sample);
                }
            }
        }
        self.bump(class_key, rpc_method, Some(helper), |c| {
            c.helper_without_token += 1;
            if had_projected_event {
                c.helper_without_token_with_projected_event += 1;
            } else {
                c.helper_without_token_no_projected_event += 1;
            }
        });
    }

    fn to_summary(&self, config: ProjectedItemSwitchConfig) -> ProjectedItemSwitchSummary {
        ProjectedItemSwitchSummary {
            config,
            emitted: self.emitted,
            replaced: self.replaced,
            new: self.new,
            non_first_set_emitted: self.non_first_set_emitted,
            helper_without_token: self.helper_without_token,
            helper_without_token_with_projected_event: self.helper_without_token_with_projected_event,
            helper_without_token_no_projected_event: self.helper_without_token_no_projected_event,
            dictionary_same: self.dictionary_same,
            dictionary_differs: self.dictionary_differs,
            evictions: self.evictions,
            by_class: self.by_class.clone(),
            by_dispatch_method: self.by_dispatch_method.clone(),
            by_helper: self.by_helper.clone(),
            by_class_rpc_helper: self.by_class_rpc_helper.clone(),
            by_call_site: self.by_call_site.clone(),
            sample_new: self.sample_new.clone(),
            sample_helper_without_token: self.sample_helper_without_token.clone(),
            sample_helper_without_token_with_projected_event: self
                .sample_helper_without_token_with_projected_event
                .clone(),
            sample_helper_without_token_no_projected_event: self
                .sample_helper_without_token_no_projected_event
                .clone(),
            sample_dictionary_differs: self.sample_dictionary_differs.clone(),
        }
    }
}

#[derive(Debug, Default)]
struct ProjectedItemDedupeLedger {
    dispatch_buckets: HashMap<u64, DispatchTokenBucket>,
    telemetry: SwitchTelemetryState,
}

pub(crate) struct CommitProjectedToken {
    pub char_id: i64,
    pub item_id: i64,
    pub kind: ProjectedItemOpKind,
    pub fingerprint: ItemWireFingerprint,
    pub change_dict: Option<CanonicalChangeDict>,
    pub class_key: String,
    pub rpc_method: String,
}

impl ProjectedItemDedupeLedger {
    fn entry_matches_wire_fp(
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

    fn invalidate_item_in_bucket(
        telemetry: &mut SwitchTelemetryState,
        bucket: &mut DispatchTokenBucket,
        item_id: i64,
    ) {
        if !bucket.multi_entries.is_empty() {
            bucket.multi_entries.retain(|&(_, eid), _| eid != item_id);
        }
        if bucket.entries.is_empty() {
            return;
        }
        let stale_keys: Vec<(i64, i64)> = bucket
            .entries
            .keys()
            .copied()
            .filter(|&(_, eid)| eid == item_id)
            .collect();
        for key in stale_keys {
            if let Some(old_entry) = bucket.entries.remove(&key) {
                telemetry.record_new(key.1, &old_entry);
            }
        }
    }

    fn apply_commit_updates(
        &mut self,
        scope_id: Option<u64>,
        changes: &ChangeSet,
        transitioned_chars: &[i64],
        tokens: Vec<CommitProjectedToken>,
    ) {
        let Some(sid) = scope_id else {
            return;
        };
        if !self.dispatch_buckets.contains_key(&sid)
            && transitioned_chars.is_empty()
            && tokens.is_empty()
        {
            return;
        }
        let bucket = self.dispatch_buckets.entry(sid).or_default();
        for &cid in transitioned_chars {
            bucket.transitioned_chars.insert(cid);
        }
        for change in &changes.items {
            Self::invalidate_item_in_bucket(&mut self.telemetry, bucket, change.key);
        }
        for tok in tokens {
            self.telemetry.record_emitted(&tok.class_key, &tok.rpc_method);
            bucket.projected_items.insert((tok.char_id, tok.item_id));
            let entry = ProjectedDedupeEntry {
                kind: tok.kind,
                fingerprint: tok.fingerprint,
                change_dict: tok.change_dict,
                class_key: tok.class_key,
                rpc_method: tok.rpc_method,
            };
            bucket
                .multi_entries
                .insert((tok.char_id, tok.item_id), entry.clone());
            if let Some(superseded) = bucket.entries.insert((tok.char_id, tok.item_id), entry) {
                self.telemetry.record_new(tok.item_id, &superseded);
            }
        }
    }

    fn record_covered_industry_monitors(
        &mut self,
        scope_id: Option<u64>,
        covered: &HashMap<i64, Vec<u32>>,
    ) {
        let Some(sid) = scope_id else {
            return;
        };
        if covered.is_empty() {
            return;
        }
        let bucket = self.dispatch_buckets.entry(sid).or_default();
        for (&monitor_id, tids) in covered {
            bucket
                .covered_industry_monitors
                .entry(monitor_id)
                .or_default()
                .extend(tids.iter().copied());
        }
    }

    fn is_industry_monitor_covered(
        &self,
        scope_id: Option<u64>,
        monitor_id: i64,
        tids: &[u32],
    ) -> bool {
        let Some(sid) = scope_id else {
            return false;
        };
        !tids.is_empty()
            && self.dispatch_buckets.get(&sid).is_some_and(|bucket| {
                bucket
                    .covered_industry_monitors
                    .get(&monitor_id)
                    .is_some_and(|covered| tids.iter().all(|tid| covered.contains(tid)))
            })
    }

    fn expire_dispatch_scope(&mut self, scope_id: u64) {
        let Some(bucket) = self.dispatch_buckets.remove(&scope_id) else {
            return;
        };
        for ((_, item_id), entry) in bucket.entries {
            self.telemetry.record_new(item_id, &entry);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn take_if_matching(
        &mut self,
        scope_id: u64,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        helper: &str,
        call_site: &str,
        actual_dict: Option<&CanonicalChangeDict>,
    ) -> bool {
        let Some(bucket) = self.dispatch_buckets.get_mut(&scope_id) else {
            return false;
        };
        let key = (char_id, item_id);
        let matches = bucket.entries.get(&key).is_some_and(|entry| {
            Self::entry_matches_wire_fp(entry, kind, fp, tombstone_wildcard_deleted)
        });
        if matches {
            if let Some(entry) = bucket.entries.remove(&key) {
                self.telemetry.record_replaced(
                    &entry.class_key,
                    &entry.rpc_method,
                    helper,
                    call_site,
                    item_id,
                    entry.change_dict.as_ref(),
                    actual_dict,
                );
            }
        }
        matches
    }

    #[allow(clippy::too_many_arguments)]
    fn take_multi_if_matching(
        &mut self,
        scope_id: u64,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        helper: &str,
        call_site: &str,
        actual_dict: Option<&CanonicalChangeDict>,
    ) -> bool {
        let Some(bucket) = self.dispatch_buckets.get_mut(&scope_id) else {
            return false;
        };
        let key = (char_id, item_id);
        let matches = bucket.multi_entries.get(&key).is_some_and(|entry| {
            Self::entry_matches_wire_fp(entry, kind, fp, tombstone_wildcard_deleted)
        });
        if matches {
            let _ = bucket.multi_entries.remove(&key);
            // Count `replaced` once only if `OnItemChange` did not already claim `entries[key]`;
            // clearing `entries[key]` ensures `expire_dispatch_scope` does not emit spurious `new`.
            if let Some(entry) = bucket.entries.remove(&key) {
                self.telemetry.record_replaced(
                    &entry.class_key,
                    &entry.rpc_method,
                    helper,
                    call_site,
                    item_id,
                    entry.change_dict.as_ref(),
                    actual_dict,
                );
            }
        }
        matches
    }

    fn has_transitioned_char(&self, scope_id: u64, char_id: i64) -> bool {
        self.dispatch_buckets
            .get(&scope_id)
            .is_some_and(|b| b.transitioned_chars.contains(&char_id))
    }

    fn transitioned_chars_in_scope(&self, scope_id: u64) -> HashSet<i64> {
        self.dispatch_buckets
            .get(&scope_id)
            .map(|b| b.transitioned_chars.clone())
            .unwrap_or_default()
    }

    fn has_projected_item(&self, scope_id: u64, char_id: i64, item_id: i64) -> bool {
        self.dispatch_buckets
            .get(&scope_id)
            .is_some_and(|b| b.projected_items.contains(&(char_id, item_id)))
    }

    fn clear_active_buckets(&mut self) {
        self.dispatch_buckets.clear();
    }
}

/// Thread-safe, per-[`ServerContext`] switch state and dispatch-scoped dedupe/telemetry ledger.
#[derive(Debug)]
pub struct ProjectedItemSwitchHandle {
    char_same_scene_item_mutation: AtomicBool,
    char_same_scene_item_deleted: AtomicBool,
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
        }
    }

    /// Updates switch flags (clearing active dispatch token buckets and resetting test telemetry).
    pub fn set_config(&self, config: ProjectedItemSwitchConfig) {
        self.char_same_scene_item_mutation
            .store(config.char_same_scene_item_mutation, Ordering::Release);
        self.char_same_scene_item_deleted
            .store(config.char_same_scene_item_deleted, Ordering::Release);
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.clear_active_buckets();
        if !config.is_any_enabled() {
            guard.telemetry = SwitchTelemetryState::default();
        }
    }

    pub(crate) fn apply_commit_dedupe_updates(
        &self,
        scope_id: Option<u64>,
        changes: &ChangeSet,
        transitioned_chars: &[i64],
        tokens: Vec<CommitProjectedToken>,
    ) {
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.apply_commit_updates(scope_id, changes, transitioned_chars, tokens);
    }

    pub(crate) fn record_covered_industry_monitors(
        &self,
        scope_id: Option<u64>,
        covered: &HashMap<i64, Vec<u32>>,
    ) {
        if scope_id.is_none() || covered.is_empty() {
            return;
        }
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.record_covered_industry_monitors(scope_id, covered);
    }

    pub(crate) fn is_industry_monitor_covered_in_scope(
        &self,
        scope_id: Option<u64>,
        monitor_id: i64,
        tids: &[u32],
    ) -> bool {
        if scope_id.is_none() || tids.is_empty() {
            return false;
        }
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.is_industry_monitor_covered(scope_id, monitor_id, tids)
    }

    #[inline]
    pub(crate) fn is_kind_enabled(&self, kind: ProjectedItemOpKind) -> bool {
        let cfg = self.config();
        match kind {
            ProjectedItemOpKind::CreatedOrUpdated => cfg.char_same_scene_item_mutation,
            ProjectedItemOpKind::Deleted => cfg.char_same_scene_item_deleted,
        }
    }

    /// Expires all unconsumed dedupe tokens belonging to `scope_id` (`DispatchNotificationStage::drop`).
    pub(crate) fn expire_dispatch_scope(&self, scope_id: u64) {
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.expire_dispatch_scope(scope_id);
    }

    pub(crate) fn has_transitioned_char(&self, scope_id: u64, char_id: i64) -> bool {
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.has_transitioned_char(scope_id, char_id)
    }

    pub(crate) fn transitioned_chars_in_scope(&self, scope_id: u64) -> HashSet<i64> {
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.transitioned_chars_in_scope(scope_id)
    }

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub(crate) fn try_take_in_dispatch(
        &self,
        scope_id: Option<u64>,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        helper: &str,
        actual_dict: Option<&CanonicalChangeDict>,
    ) -> (bool, bool) {
        if !self.is_kind_enabled(kind) {
            return (false, false);
        }
        let Some(sid) = scope_id else {
            return (false, true);
        };
        let call_site = caller_call_site();
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        let matched = guard.take_if_matching(
            sid,
            char_id,
            item_id,
            kind,
            fp,
            tombstone_wildcard_deleted,
            helper,
            &call_site,
            actual_dict,
        );
        (matched, true)
    }

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub(crate) fn try_take_multi_in_dispatch(
        &self,
        scope_id: Option<u64>,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        helper: &str,
        actual_dict: Option<&CanonicalChangeDict>,
    ) -> (bool, bool) {
        if !self.is_kind_enabled(kind) {
            return (false, false);
        }
        let Some(sid) = scope_id else {
            return (false, true);
        };
        let call_site = caller_call_site();
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        let matched = guard.take_multi_if_matching(
            sid,
            char_id,
            item_id,
            kind,
            fp,
            tombstone_wildcard_deleted,
            helper,
            &call_site,
            actual_dict,
        );
        (matched, true)
    }

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    pub(crate) fn record_helper_without_token(
        &self,
        scope_id: u64,
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
        let had_projected_event = guard.has_projected_item(scope_id, char_id, item_id);
        guard.telemetry.record_helper_without_token(
            had_projected_event,
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

    #[track_caller]
    pub(crate) fn record_non_first_set_emitted(&self) {
        let call_site = caller_call_site();
        let mut guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.telemetry.record_non_first_set_emitted(&call_site);
    }

    /// Returns a snapshot of switched-run telemetry counters.
    pub fn summary(&self) -> ProjectedItemSwitchSummary {
        let cfg = self.config();
        let guard = self.dedupe.lock().unwrap_or_else(|e| e.into_inner());
        guard.telemetry.to_summary(cfg)
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
            new = summary.new,
            non_first_set_emitted = summary.non_first_set_emitted,
            helper_without_token = summary.helper_without_token,
            helper_without_token_with_projected_event = summary.helper_without_token_with_projected_event,
            helper_without_token_no_projected_event = summary.helper_without_token_no_projected_event,
            dictionary_same = summary.dictionary_same,
            dictionary_differs = summary.dictionary_differs,
            evictions = summary.evictions,
            by_class = ?summary.by_class,
            by_dispatch_method = ?summary.by_dispatch_method,
            by_helper = ?summary.by_helper,
            by_call_site = ?summary.by_call_site,
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
/// `station_id`, `structure_id`), or switched `active_ship_id` in this commit.
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
        || before.active_ship_id != after.active_ship_id
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

/// Returns `true` when `item` sits directly in `ch`'s docked station/structure hangar (`flag 4`)
/// or active ship cargo hold (`flag 5`).
#[inline]
pub(crate) fn is_first_set_direct_slot(item: &InventoryItemRecord, ch: &CharacterRecord) -> bool {
    is_first_set_direct_slot_coords(item.location_id, item.flag as i64, ch)
}

/// Shared first-set qualification check (`#931` Check #28–#30) ignoring per-class switch toggles so
/// both switched projection and compare-only telemetry evaluate the identical predicate:
/// - commit happened inside an RPC dispatch (`in_dispatch == true`)
/// - owner is an online human character with no move or active-ship transition in `changed_chars`
/// - before and after the change, the row sits directly in the docked station/structure hangar
///   (`flag 4`) or active ship's cargo hold (`flag 5`).
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

/// Evaluates whether a committed item [`RowChange`] is eligible for switched projection under the
/// first-set rule and active [`ProjectedItemSwitchConfig`], returning
/// `(owner_char_id, op_kind, fingerprint_record)`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn classify_char_same_scene_item_change<'a>(
    in_dispatch: bool,
    config: ProjectedItemSwitchConfig,
    change: &'a RowChange<i64, InventoryItemRecord>,
    snap_after: &WorldStateSnapshot,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> Option<(i64, ProjectedItemOpKind, &'a InventoryItemRecord)> {
    let before_opt = change.before.as_deref();
    let after_opt = change.after.as_deref();
    let fp_rec = after_opt.or(before_opt)?;
    let owner_id = fp_rec.owner_id;
    let op_kind = is_first_set_item_change_pair(
        in_dispatch,
        owner_id,
        before_opt,
        after_opt,
        snap_after,
        changed_chars,
        online,
        sde,
    )?;
    let switch_enabled = match op_kind {
        ProjectedItemOpKind::CreatedOrUpdated => config.char_same_scene_item_mutation,
        ProjectedItemOpKind::Deleted => config.char_same_scene_item_deleted,
    };
    switch_enabled.then_some((owner_id, op_kind, fp_rec))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_hand_written_first_set_in_snap(
    in_dispatch: bool,
    char_id: i64,
    recipient_count: usize,
    kind: ProjectedItemOpKind,
    fp: ItemWireFingerprint,
    change_dict: Option<&CanonicalChangeDict>,
    pre_loc_flag: Option<(i64, i64)>,
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

    let dict = change_dict?;
    if dict.0.contains_key(&CanonicalDictKey::ColIx(2))
        && dict.col_i64(2) != Some(fp.owner_id)
    {
        return None;
    }

    let col3 = dict.col_i64(3);
    let col4 = dict.col_i64(4);

    let (before_loc, before_flag) = match kind {
        ProjectedItemOpKind::Deleted => {
            let (old_loc, old_flag) = match (pre_loc_flag, col3, col4) {
                (Some((l, f)), _, _) => (l, f),
                (None, Some(l), Some(f)) => (l, f),
                _ => return None,
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
            if let Some((ol, of)) = pre_loc_flag {
                if !is_first_set_direct_slot_coords(ol, of, ch) {
                    return None;
                }
                (Some(ol), Some(of))
            } else if col3 == Some(0) {
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

    let (after_loc, after_flag, after_qty) = match kind {
        ProjectedItemOpKind::Deleted => (6_i64, 0_i64, 0_i64),
        ProjectedItemOpKind::CreatedOrUpdated => (fp.location_id, fp.flag_id, fp.wire_qty),
    };
    Some(classify_item_row_kind_key(
        before_loc,
        before_flag,
        fp.owner_id,
        after_loc,
        after_flag,
        after_qty,
        snap,
        &ChangeSet::default(),
        &HashMap::new(),
        sde,
    ))
}

pub(crate) struct SwitchedProjectedItemSpec {
    pub recipient_char_id: i64,
    pub recipient_user_id: u64,
    pub item_id: i64,
    pub args: Vec<PyRep>,
    pub industry_scopes: Vec<(i64, i64, i64, u32)>,
}

/// Evaluates `changes.items` under active [`ProjectedItemSwitchConfig`] and the first-set rule,
/// updating dispatch-scoped dedupe tokens/telemetry and returning `OnItemChange` specs.
#[allow(clippy::too_many_arguments)]
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
    online: &impl OnlineCharacterLookup,
    snap_after: &WorldStateSnapshot,
    changes: &ChangeSet,
    sde: &SdeRegistry,
    suppressed_effects: &TxItemEffectKeys,
    active_fuel_reserves: &crate::services::tactical_modules::ActiveFuelReserveTracker,
    db_key: usize,
) -> Vec<SwitchedProjectedItemSpec> {
    let cfg = switches.config();
    if !cfg.is_any_enabled() {
        return Vec::new();
    }

    let changed_chars = index_changed_characters(changes);
    let transitioned_chars: Vec<i64> = changed_chars
        .keys()
        .copied()
        .filter(|&cid| is_character_scene_or_ship_transition(cid, &changed_chars))
        .collect();
    let Some(&(scope_id, ref rpc_opt)) = dispatch_scope_info else {
        return Vec::new();
    };
    let _ = register_dispatch_switch_handle(pend, notify_tx, switches);
    let rpc_method = rpc_opt
        .clone()
        .unwrap_or_else(|| crate::flight_recorder::UNKNOWN_RPC_LABEL.to_string());

    if changes.items.is_empty() {
        if !transitioned_chars.is_empty() {
            switches.apply_commit_dedupe_updates(
                Some(scope_id),
                changes,
                &transitioned_chars,
                Vec::new(),
            );
        }
        return Vec::new();
    }

    // Snapshot characters already transitioned earlier in this dispatch so subsequent commits in
    // the same dispatch that omit `changes.characters` still stay excluded under Guardrail 2.
    let prior_transitioned_chars = switches.transitioned_chars_in_scope(scope_id);

    let mut item_changes_by_id: HashMap<i64, ItemBeforeAfter<'_>> = HashMap::new();
    for ch in &changes.items {
        item_changes_by_id
            .entry(ch.key)
            .and_modify(|entry| entry.1 = ch.after.as_deref())
            .or_insert((ch.before.as_deref(), ch.after.as_deref()));
    }

    let mut specs = Vec::new();
    let mut tokens = Vec::new();

    for change in &changes.items {
        // Leave Cargo fuel items held under an active rollback reserve untouched here so silent
        // actor-validation rollbacks emit zero notifications and deferred diffs stay authoritative (#1486).
        if active_fuel_reserves.is_item_reserved(db_key, change.key) {
            continue;
        }
        let Some((owner_id, op_kind, fp_rec)) = classify_char_same_scene_item_change(
            true,
            cfg,
            change,
            snap_after,
            &changed_chars,
            online,
            sde,
        ) else {
            continue;
        };
        if prior_transitioned_chars.contains(&owner_id)
            || suppressed_effects.contains_item(owner_id, change.key)
        {
            continue;
        }
        let Some(ch) = snap_after.characters.get(&owner_id) else {
            continue;
        };
        let Some(item_payload) = build_projected_item_change_args(change, changes, snap_after, sde) else {
            continue;
        };
        let fp = ItemWireFingerprint::from_record(fp_rec);
        let change_dict = match item_payload.as_slice() {
            [_, dict_rep, ..] => CanonicalChangeDict::from_pyrep(dict_rep),
            _ => None,
        };
        let before_rec = change.before.as_deref();
        let industry_scopes = match (before_rec, change.after.as_deref()) {
            (Some(old_item), Some(new_item)) => {
                let old_scope = (
                    old_item.owner_id,
                    old_item.location_id,
                    old_item.flag as i64,
                    old_item.type_id,
                );
                let new_scope = (
                    new_item.owner_id,
                    new_item.location_id,
                    new_item.flag as i64,
                    new_item.type_id,
                );
                if old_scope == new_scope {
                    vec![new_scope]
                } else {
                    vec![old_scope, new_scope]
                }
            }
            (Some(old_item), None) => vec![(
                old_item.owner_id,
                old_item.location_id,
                old_item.flag as i64,
                old_item.type_id,
            )],
            (None, Some(new_item)) => vec![(
                new_item.owner_id,
                new_item.location_id,
                new_item.flag as i64,
                new_item.type_id,
            )],
            (None, None) => Vec::new(),
        };
        let (after_loc, after_flag, after_qty) = match change.after.as_deref() {
            Some(after_rec) => {
                let after_fp = ItemWireFingerprint::from_record(after_rec);
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
        tokens.push(CommitProjectedToken {
            char_id: owner_id,
            item_id: change.key,
            kind: op_kind,
            fingerprint: fp,
            change_dict,
            class_key,
            rpc_method: rpc_method.clone(),
        });
        specs.push(SwitchedProjectedItemSpec {
            recipient_char_id: owner_id,
            recipient_user_id: ch.user_id,
            item_id: change.key,
            args: item_payload,
            industry_scopes,
        });
    }

    switches.apply_commit_dedupe_updates(Some(scope_id), changes, &transitioned_chars, tokens);
    specs
}

/// Result of materializing commit-time switched item notifications for one `WorldRepository`
/// transaction (`#931` Track 3C-1 / `BF-931-3C1-02`).
pub(crate) struct SwitchedCommitNotifications {
    pub notifs: Vec<PendingMachoNotification>,
    pub switched_keys: HashSet<(i64, i64)>,
    pub covered_industry_monitors: HashMap<i64, Vec<u32>>,
}

/// Materializes commit-time switched item notifications in exact V24 causal order
/// (`OnItemChange` -> coalesced matching personal `OnIndustryMaterials` -> paired
/// `OnMultipleItemChange`).
///
/// At most one coalesced `OnIndustryMaterials` notification is emitted per `monitor_id` per
/// commit, merging all covered material types affected in that commit (including `tid -> 0` for
/// depleted/deleted types). Only personal material monitors (`sub.owner_id == sub.char_id`) are
/// queried directly from `snap` so post-commit listeners never re-enter `ServerContext` locks.
pub(crate) fn build_switched_item_commit_notifications(
    snap: &WorldStateSnapshot,
    specs: Vec<SwitchedProjectedItemSpec>,
    now_ft: i64,
    active_fuel_reserves: &crate::services::tactical_modules::ActiveFuelReserveTracker,
    db_key: usize,
) -> SwitchedCommitNotifications {
    use crate::services::notify_emitters_character_services::{
        collect_available_monitor_materials, load_industry_material_monitors,
        IndustryMaterialMonitorSub,
    };

    let monitors = load_industry_material_monitors(snap);
    let mut base_avail_cache: HashMap<(i64, i64, i64), BTreeMap<u32, i64>> = HashMap::new();
    let mut notifs = Vec::with_capacity(specs.len() * 2 + monitors.len());
    let mut switched_keys = HashSet::with_capacity(specs.len());
    let mut coalesced_monitors: BTreeMap<i64, (u64, &IndustryMaterialMonitorSub, BTreeSet<u32>)> =
        BTreeMap::new();

    for spec in &specs {
        switched_keys.insert((spec.recipient_char_id, spec.item_id));
        active_fuel_reserves.observe_pre_emit_event(db_key, "OnItemChange", &spec.args);
        notifs.push(assemble_pending_notification(
            spec.recipient_char_id,
            Some(spec.recipient_user_id),
            "OnItemChange",
            "charid",
            spec.args.clone(),
            now_ft,
        ));

        if !monitors.is_empty() && !spec.industry_scopes.is_empty() {
            for (&monitor_id, sub) in &monitors {
                if sub.owner_id != sub.char_id || sub.char_id != spec.recipient_char_id {
                    continue;
                }
                for &(owner_id, loc_id, flag_id, tid) in &spec.industry_scopes {
                    if owner_id == sub.owner_id
                        && loc_id == sub.location_id
                        && flag_id == sub.flag_id
                    {
                        coalesced_monitors
                            .entry(monitor_id)
                            .or_insert_with(|| (spec.recipient_user_id, sub, BTreeSet::new()))
                            .2
                            .insert(tid);
                    }
                }
            }
        }
    }

    let mut covered_industry_monitors = HashMap::with_capacity(coalesced_monitors.len());
    for (monitor_id, (recipient_user_id, sub, covered_tids)) in coalesced_monitors {
        let mut avail_map = base_avail_cache
            .entry((sub.owner_id, sub.location_id, sub.flag_id))
            .or_insert_with(|| {
                collect_available_monitor_materials(
                    snap,
                    sub.owner_id,
                    sub.location_id,
                    sub.flag_id,
                )
            })
            .clone();
        for &tid in &covered_tids {
            avail_map.entry(tid).or_insert(0);
        }
        let entries: Vec<(PyRep, PyRep)> = avail_map
            .into_iter()
            .map(|(tid, qty)| (PyRep::Integer(i64::from(tid)), PyRep::Integer(qty)))
            .collect();
        covered_industry_monitors.insert(monitor_id, covered_tids.into_iter().collect());
        notifs.push(assemble_pending_notification(
            sub.char_id,
            Some(recipient_user_id),
            "OnIndustryMaterials",
            "charid",
            vec![PyRep::Integer(monitor_id), PyRep::Dict(entries)],
            now_ft,
        ));
    }

    for spec in specs {
        if let [row, change_dict, ..] = spec.args.as_slice() {
            let multi_args = vec![PyRep::List(vec![row.clone()]), change_dict.clone()];
            active_fuel_reserves.observe_pre_emit_event(db_key, "OnMultipleItemChange", &multi_args);
            notifs.push(assemble_pending_notification(
                spec.recipient_char_id,
                Some(spec.recipient_user_id),
                "OnMultipleItemChange",
                "charid",
                multi_args,
                now_ft,
            ));
        }
    }

    SwitchedCommitNotifications {
        notifs,
        switched_keys,
        covered_industry_monitors,
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
