//! Step 1 outside-RPC (`"tick"` class) first-set projection state, helpers, and dry-run
//! outcome counters (`#931`).
//!
//! Covers commits occurring outside an RPC dispatch (`dispatch_scope_info.is_none()`), without
//! gating emission on telemetry tick markers:
//! 1. While [`ProjectedItemSwitchConfig::char_same_scene_item_outside_rpc`] is `false` (Step 1
//!    default), eligible outside-RPC commits populate shadow dry-run facts and per-scope outcome
//!    counts (`by_scope.{tick, neither}`) without altering live notification delivery.
//! 2. When enabled, outside-dispatch single facts persist unbounded and untimed per
//!    `(pilot, item)` until invalidated by any subsequent commit touching that item or cleared when
//!    the pilot goes offline.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use eve_db::{ChangeSet, CharacterRecord, InventoryItemRecord, RowChange, WorldStateSnapshot};
use eve_sde::SdeRegistry;

use super::delta_item_compare::CompareOriginScope;
use super::delta_item_switch::{
    classify_item_row_kind_key, entry_matches_wire_fp, evaluate_fitted_coords_pair,
    is_first_set_direct_slot, is_first_set_direct_slot_coords, is_first_set_direct_slot_location,
    online_non_transitioned_owner, pair_owner_and_op_kind, remove_item_from_facts,
    remove_items_from_facts, CanonicalChangeDict, CanonicalDictKey, CommitProjectedToken,
    DedupeLookupOutcome, ItemWireFingerprint, NotCoveredReason, ProjectedDedupeEntry,
    ProjectedItemOpKind, ProjectedItemSwitchConfig, WouldQualifyFlags,
};
use super::delta_item_switch_report::SwitchTelemetryState;
use super::delta_projection::OnlineCharacterLookup;

/// Per-execution-scope announcement & §3 dedupe outcome counts (`dispatch`, `tick`, `neither`).
///
/// When `char_same_scene_item_outside_rpc` is `false`, `tick` and `neither` report shadow dry-run
/// counts for class `"tick"` without emitting or suppressing real outside-RPC notifications.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SwitchScopeOutcomeCounts {
    /// Committed first-set rows announced (or eligible to announce in Step 1 dry-run) in this scope.
    pub would_announce: u64,
    /// Hand-written calls where `(pilot, item)` fact was set with matching row (`not sent`).
    pub replaced: u64,
    /// Hand-written calls where `(pilot, item)` fact was set with different row (`not sent`).
    pub superseded: u64,
    /// First-set hand-written calls where `(pilot, item)` fact was not set (`sent`).
    pub no_projected_event: u64,
}

impl SwitchScopeOutcomeCounts {
    /// Returns `true` when any counter in this execution scope is non-zero.
    #[inline]
    pub const fn has_activity(&self) -> bool {
        self.would_announce > 0
            || self.replaced > 0
            || self.superseded > 0
            || self.no_projected_event > 0
    }
}

/// Appends the `by_scope` (`dispatch`, `tick`, `neither`) markdown summary table when active.
pub(super) fn format_by_scope_markdown(
    out: &mut String,
    dispatch: &SwitchScopeOutcomeCounts,
    tick: &SwitchScopeOutcomeCounts,
    neither: &SwitchScopeOutcomeCounts,
) {
    if !dispatch.has_activity() && !tick.has_activity() && !neither.has_activity() {
        return;
    }
    out.push_str(
        "\n#### Per-Scope Projection & Dedupe Outcomes (`by_scope`)\n| Scope | `would_announce` | `replaced` | `superseded` | `no_projected_event` |\n|---|---:|---:|---:|---:|\n",
    );
    for (name, c) in [
        ("dispatch", dispatch),
        ("tick", tick),
        ("neither", neither),
    ] {
        out.push_str(&format!(
            "| `{name}` | {} | {} | {} | {} |\n",
            c.would_announce, c.replaced, c.superseded, c.no_projected_event
        ));
    }
}

/// Shared outside-dispatch single-fact & commit-skip bucket used for live outside-RPC facts and
/// Step 1 dry-run shadow facts.
#[derive(Debug, Default)]
pub(super) struct OutsideRpcScopeFacts {
    pub(super) facts: HashMap<(i64, i64, bool), ProjectedDedupeEntry>,
    pub(super) blueprint_facts: HashSet<(i64, i64)>,
    pub(super) explicit_effect_items: HashSet<(i64, i64)>,
    pub(super) fuel_reserved_items: HashSet<i64>,
}

impl OutsideRpcScopeFacts {
    fn invalidate_item(
        &mut self,
        item_id: i64,
        telemetry: Option<&mut SwitchTelemetryState>,
    ) {
        remove_item_from_facts(telemetry, &mut self.facts, item_id);
        if !self.explicit_effect_items.is_empty() {
            self.explicit_effect_items
                .retain(|&(_, eid)| eid != item_id);
        }
        self.fuel_reserved_items.remove(&item_id);
    }

    fn invalidate_items_batch(
        &mut self,
        item_ids: &HashSet<i64>,
        telemetry: Option<&mut SwitchTelemetryState>,
    ) {
        remove_items_from_facts(telemetry, &mut self.facts, item_ids);
        if !self.explicit_effect_items.is_empty() {
            self.explicit_effect_items
                .retain(|&(_, eid)| !item_ids.contains(&eid));
        }
        if !self.fuel_reserved_items.is_empty() {
            self.fuel_reserved_items.retain(|eid| !item_ids.contains(eid));
        }
    }

    fn invalidate_blueprints(&mut self, bp_owners: &HashSet<i64>) {
        if !self.blueprint_facts.is_empty() && !bp_owners.is_empty() {
            self.blueprint_facts
                .retain(|&(_cid, oid)| !bp_owners.contains(&oid));
        }
    }

    fn record_commit_skips(
        &mut self,
        explicit_effect_items: &[(i64, i64)],
        fuel_reserved_items: &[i64],
    ) {
        for &key in explicit_effect_items {
            self.explicit_effect_items.insert(key);
        }
        for &item_id in fuel_reserved_items {
            self.fuel_reserved_items.insert(item_id);
        }
    }

    fn apply_commit_tokens(
        &mut self,
        origin_scope: CompareOriginScope,
        emit_live: bool,
        tokens: Vec<CommitProjectedToken>,
        blueprint_tokens: &[(i64, i64)],
        telemetry: &mut SwitchTelemetryState,
    ) {
        for tok in tokens {
            let key = (tok.char_id, tok.item_id, tok.is_corp_target);
            let entry = ProjectedDedupeEntry::from_commit_token(tok);
            telemetry.record_emitted(
                origin_scope,
                emit_live,
                &entry.class_key,
                &entry.rpc_method,
            );
            if let Some(old) = self.facts.insert(key, entry) {
                if emit_live && !old.replaced_claimed {
                    telemetry.record_new(key.1, &old);
                }
            }
        }
        for &bp_key in blueprint_tokens {
            self.blueprint_facts.insert(bp_key);
        }
    }

    fn clear_offline_chars(
        &mut self,
        offline_chars: &[i64],
        telemetry: Option<&mut SwitchTelemetryState>,
    ) {
        if !self.facts.is_empty() {
            if let Some(telem) = telemetry {
                let offline_keys: Vec<(i64, i64, bool)> = self
                    .facts
                    .keys()
                    .copied()
                    .filter(|(cid, _, _)| offline_chars.contains(cid))
                    .collect();
                for key in offline_keys {
                    if let Some(entry) = self.facts.remove(&key) {
                        if !entry.replaced_claimed {
                            telem.record_new(key.1, &entry);
                        }
                    }
                }
            } else {
                self.facts
                    .retain(|(cid, _, _), _| !offline_chars.contains(cid));
            }
        }
        if !self.blueprint_facts.is_empty() {
            self.blueprint_facts
                .retain(|(cid, _)| !offline_chars.contains(cid));
        }
        if !self.explicit_effect_items.is_empty() {
            self.explicit_effect_items
                .retain(|(cid, _)| !offline_chars.contains(cid));
        }
    }

    #[inline]
    fn explicit_or_fuel_flags(&self, char_id: i64, item_id: i64) -> (bool, bool) {
        (
            self.explicit_effect_items.contains(&(char_id, item_id)),
            self.fuel_reserved_items.contains(&item_id),
        )
    }

    fn clear(&mut self) {
        self.facts.clear();
        self.blueprint_facts.clear();
        self.explicit_effect_items.clear();
        self.fuel_reserved_items.clear();
    }
}

/// Unbounded, untimed outside-RPC dedupe facts (`live` and Step 1 dry-run `shadow`) plus
/// outside-scope covered `OnIndustryMaterials` monitor tracking (`#931`).
#[derive(Debug, Default)]
pub(super) struct OutsideRpcLedgerState {
    pub(super) live: OutsideRpcScopeFacts,
    pub(super) shadow: OutsideRpcScopeFacts,
    pub(super) covered_industry_monitors: HashMap<i64, (i64, i64, HashSet<u32>)>,
}

pub(super) fn merge_covered_industry_monitors(
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

pub(super) fn invalidate_covered_monitors_for_change(
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

pub(super) fn collect_touched_monitor_types(changes: &ChangeSet) -> HashMap<i64, HashSet<u32>> {
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

pub(super) fn invalidate_covered_monitors_for_types(
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

pub(super) fn retain_monitors_excluding_chars(
    monitors: &mut HashMap<i64, (i64, i64, HashSet<u32>)>,
    chars: &[i64],
) {
    if !chars.is_empty() && !monitors.is_empty() {
        monitors.retain(|_, (cid, _, _)| !chars.contains(cid));
    }
}

impl OutsideRpcLedgerState {
    pub(super) fn invalidate_item(
        &mut self,
        change: &RowChange<i64, InventoryItemRecord>,
        telemetry: &mut SwitchTelemetryState,
    ) {
        let item_id = change.key;
        self.live.invalidate_item(item_id, Some(&mut *telemetry));
        self.shadow.invalidate_item(item_id, None);
        invalidate_covered_monitors_for_change(&mut self.covered_industry_monitors, change);
    }

    pub(super) fn invalidate_items_batch(
        &mut self,
        item_ids: &HashSet<i64>,
        telemetry: &mut SwitchTelemetryState,
    ) {
        if !item_ids.is_empty() {
            self.live.invalidate_items_batch(item_ids, Some(&mut *telemetry));
            self.shadow.invalidate_items_batch(item_ids, None);
        }
    }

    pub(super) fn invalidate_blueprints(&mut self, bp_owners: &HashSet<i64>) {
        self.live.invalidate_blueprints(bp_owners);
        self.shadow.invalidate_blueprints(bp_owners);
    }

    pub(super) fn invalidate_transitioned_chars(&mut self, transitioned_chars: &[i64]) {
        if transitioned_chars.is_empty() {
            return;
        }
        retain_monitors_excluding_chars(&mut self.covered_industry_monitors, transitioned_chars);
        self.live
            .blueprint_facts
            .retain(|&(cid, _)| !transitioned_chars.contains(&cid));
        self.shadow
            .blueprint_facts
            .retain(|&(cid, _)| !transitioned_chars.contains(&cid));
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_commit(
        &mut self,
        origin_scope: CompareOriginScope,
        outside_rpc_enabled: bool,
        tokens: Vec<CommitProjectedToken>,
        blueprint_tokens: &[(i64, i64)],
        explicit_effect_items: &[(i64, i64)],
        fuel_reserved_items: &[i64],
        telemetry: &mut SwitchTelemetryState,
    ) {
        let (target, emit_live) = if outside_rpc_enabled {
            (&mut self.live, true)
        } else {
            (&mut self.shadow, false)
        };
        target.record_commit_skips(explicit_effect_items, fuel_reserved_items);
        target.apply_commit_tokens(origin_scope, emit_live, tokens, blueprint_tokens, telemetry);
    }

    pub(super) fn record_covered_industry_monitors(
        &mut self,
        covered: &HashMap<i64, (i64, i64, Vec<u32>)>,
    ) {
        merge_covered_industry_monitors(&mut self.covered_industry_monitors, covered);
    }

    pub(super) fn is_industry_monitor_covered(&self, monitor_id: i64, tids: &[u32]) -> bool {
        self.covered_industry_monitors
            .get(&monitor_id)
            .is_some_and(|(_, _, covered)| tids.iter().all(|tid| covered.contains(tid)))
    }

    pub(super) fn clear_offline_chars(
        &mut self,
        offline_chars: &[i64],
        telemetry: &mut SwitchTelemetryState,
    ) {
        if offline_chars.is_empty() {
            return;
        }
        self.live
            .clear_offline_chars(offline_chars, Some(&mut *telemetry));
        self.shadow.clear_offline_chars(offline_chars, None);
        retain_monitors_excluding_chars(&mut self.covered_industry_monitors, offline_chars);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_shadow_outcome(
        &self,
        origin_scope: CompareOriginScope,
        char_id: i64,
        item_id: i64,
        is_corp_target: bool,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        telemetry: &mut SwitchTelemetryState,
    ) -> DedupeLookupOutcome {
        let Some(entry) = self.shadow.facts.get(&(char_id, item_id, is_corp_target)) else {
            return DedupeLookupOutcome::NotSet;
        };
        let is_replaced = entry_matches_wire_fp(entry, kind, fp, tombstone_wildcard_deleted);
        telemetry.record_shadow_outcome(origin_scope, is_replaced);
        if is_replaced {
            DedupeLookupOutcome::Replaced
        } else {
            DedupeLookupOutcome::Superseded
        }
    }

    #[inline]
    pub(super) fn explicit_or_fuel_flags(
        &self,
        outside_rpc_enabled: bool,
        char_id: i64,
        item_id: i64,
    ) -> (bool, bool) {
        if outside_rpc_enabled {
            self.live.explicit_or_fuel_flags(char_id, item_id)
        } else {
            self.shadow.explicit_or_fuel_flags(char_id, item_id)
        }
    }

    pub(super) fn clear_all(&mut self) {
        self.live.clear();
        self.shadow.clear();
        self.covered_industry_monitors.clear();
    }
}

/// Returns the synthetic RPC-bucket label recorded for outside-RPC commits (`tick` vs generic
/// outside-RPC commits; used for telemetry grouping only, never gating emission).
#[inline]
pub(crate) fn outside_rpc_method_label(origin_scope: CompareOriginScope) -> &'static str {
    match origin_scope {
        CompareOriginScope::Tick => "<tick>",
        _ => "<outside_rpc>",
    }
}

/// Evaluates whether a committed item [`RowChange`] qualifies for switched projection (or Step 1
/// shadow outside-RPC telemetry when `shadow_outside_dry_run` is `true`), returning up to two
/// qualifying character recipients (e.g. incoming and outgoing character owners on a cross-owner
/// transfer when `config.char_all_items` is `true`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn classify_char_same_scene_item_change_for_scope<'a>(
    in_dispatch: bool,
    shadow_outside_dry_run: bool,
    config: ProjectedItemSwitchConfig,
    change: &'a RowChange<i64, InventoryItemRecord>,
    snap_after: &WorldStateSnapshot,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> [Option<(i64, ProjectedItemOpKind, &'a InventoryItemRecord)>; 2] {
    let before_opt = change.before.as_deref();
    let after_opt = change.after.as_deref();
    let Some(fp_rec) = after_opt.or(before_opt) else {
        return [None, None];
    };
    if config.char_all_items
        && (in_dispatch || shadow_outside_dry_run || config.char_same_scene_item_outside_rpc)
    {
        let qualify_char = |cid: i64, kind: ProjectedItemOpKind| {
            online_non_transitioned_owner(cid, cid, snap_after, changed_chars, online, sde)
                .map(|_| (cid, kind, fp_rec))
        };
        return match (before_opt, after_opt) {
            (None, Some(new_item)) => [
                qualify_char(new_item.owner_id, ProjectedItemOpKind::CreatedOrUpdated),
                None,
            ],
            (Some(old_item), None) => [
                qualify_char(old_item.owner_id, ProjectedItemOpKind::Deleted),
                None,
            ],
            (Some(old_item), Some(new_item)) if old_item.owner_id == new_item.owner_id => [
                qualify_char(new_item.owner_id, ProjectedItemOpKind::CreatedOrUpdated),
                None,
            ],
            (Some(old_item), Some(new_item)) => [
                qualify_char(new_item.owner_id, ProjectedItemOpKind::CreatedOrUpdated),
                qualify_char(old_item.owner_id, ProjectedItemOpKind::CreatedOrUpdated),
            ],
            (None, None) => [None, None],
        };
    }

    let Some((owner_id, op_kind)) = pair_owner_and_op_kind(before_opt, after_opt) else {
        return [None, None];
    };
    let Some(ch) = online_non_transitioned_owner(
        owner_id,
        owner_id,
        snap_after,
        changed_chars,
        online,
        sde,
    ) else {
        return [None, None];
    };
    let direct_slot_ok = before_opt.is_none_or(|r| is_first_set_direct_slot(r, ch))
        && after_opt.is_none_or(|r| is_first_set_direct_slot(r, ch));
    let direct_active = direct_slot_ok
        && if in_dispatch || shadow_outside_dry_run {
            config.is_kind_enabled(op_kind)
        } else {
            config.is_kind_enabled_in_scope(false, op_kind)
        };
    let fitted_active = config.is_fitted_enabled_in_scope(in_dispatch)
        && evaluate_fitted_coords_pair(
            before_opt.map(|r| (r.location_id, r.flag as i64)),
            after_opt.map(|r| (r.location_id, r.flag as i64)),
            ch,
        );
    [(direct_active || fitted_active).then_some((owner_id, op_kind, fp_rec)), None]
}

/// Evaluates first-set qualification for a hand-written item helper invoked outside an RPC
/// dispatch against `snap` (`#931` Step 1 class `"tick"`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_hand_written_outside_first_set_in_snap(
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
    let (direct_slot_ok, _, before_coords, (after_loc, after_flag, after_qty)) =
        evaluate_hand_written_row_details(
            char_id,
            recipient_count,
            kind,
            fp,
            change_dict,
            pre_loc_flag,
            snap,
            online,
            sde,
        )?;
    direct_slot_ok.then(|| {
        classify_item_row_kind_key(
            before_coords.map(|(l, _)| l),
            before_coords.map(|(_, f)| f),
            fp.owner_id,
            after_loc,
            after_flag,
            after_qty,
            snap,
            &ChangeSet::default(),
            &HashMap::new(),
            sde,
        )
    })
}

fn is_valid_hand_written_char_owner(
    char_id: i64,
    recipient_count: usize,
    fp: ItemWireFingerprint,
    change_dict: Option<&CanonicalChangeDict>,
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
) -> bool {
    recipient_count == 1
        && char_id > 0
        && fp.owner_id == char_id
        && snap.characters.contains_key(&char_id)
        && !snap.corporations.contains_key(&char_id)
        && !sde.npc_corporations.contains_key(&char_id)
        && change_dict.is_none_or(|d| {
            !d.0.contains_key(&CanonicalDictKey::ColIx(2)) || d.col_i64(2) == Some(fp.owner_id)
        })
}

fn is_valid_hand_written_char_owner_for_mode(
    char_all_items: bool,
    char_id: i64,
    recipient_count: usize,
    fp: ItemWireFingerprint,
    change_dict: Option<&CanonicalChangeDict>,
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
) -> bool {
    if !char_all_items {
        return is_valid_hand_written_char_owner(
            char_id,
            recipient_count,
            fp,
            change_dict,
            snap,
            sde,
        );
    }
    let owner_matches =
        fp.owner_id == char_id || change_dict.is_some_and(|d| d.col_i64(2) == Some(char_id));
    recipient_count == 1
        && char_id > 0
        && owner_matches
        && snap.characters.contains_key(&char_id)
        && !snap.corporations.contains_key(&char_id)
        && !sde.npc_corporations.contains_key(&char_id)
}

pub(crate) fn resolve_hand_written_coords(
    kind: ProjectedItemOpKind,
    fp: ItemWireFingerprint,
    dict: &CanonicalChangeDict,
    pre_loc_flag: Option<(i64, i64)>,
) -> Option<(Option<(i64, i64)>, Option<(i64, i64)>, (i64, i64, i64))> {
    let (col3, col4) = (dict.col_i64(3), dict.col_i64(4));
    match kind {
        ProjectedItemOpKind::Deleted => {
            let old = pre_loc_flag.or_else(|| col3.zip(col4))?;
            Some((Some(old), None, (6, 0, 0)))
        }
        ProjectedItemOpKind::CreatedOrUpdated => {
            let before = pre_loc_flag.or_else(|| {
                (col3 != Some(0))
                    .then(|| (col3.unwrap_or(fp.location_id), col4.unwrap_or(fp.flag_id)))
            });
            Some((
                before,
                Some((fp.location_id, fp.flag_id)),
                (fp.location_id, fp.flag_id, fp.wire_qty),
            ))
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_hand_written_row_details(
    char_id: i64,
    recipient_count: usize,
    kind: ProjectedItemOpKind,
    fp: ItemWireFingerprint,
    change_dict: Option<&CanonicalChangeDict>,
    pre_loc_flag: Option<(i64, i64)>,
    snap: &WorldStateSnapshot,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> Option<(bool, bool, Option<(i64, i64)>, (i64, i64, i64))> {
    if !is_valid_hand_written_char_owner(char_id, recipient_count, fp, change_dict, snap, sde) {
        return None;
    }
    let ch = snap
        .characters
        .get(&char_id)
        .filter(|c| online.is_character_online(char_id, c.user_id))?;
    let dict = change_dict?;
    let (before_coords, after_coords, after_vals) =
        resolve_hand_written_coords(kind, fp, dict, pre_loc_flag)?;
    let direct_slot_ok = match kind {
        ProjectedItemOpKind::Deleted => {
            before_coords.is_some_and(|(l, f)| is_first_set_direct_slot_coords(l, f, ch))
        }
        ProjectedItemOpKind::CreatedOrUpdated => {
            is_first_set_direct_slot_coords(fp.location_id, fp.flag_id, ch)
                && before_coords.is_none_or(|(l, f)| is_first_set_direct_slot_coords(l, f, ch))
        }
    };
    let fitted_ok = evaluate_fitted_coords_pair(before_coords, after_coords, ch);
    Some((direct_slot_ok, fitted_ok, before_coords, after_vals))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_hand_written_first_set_and_next_in_snap(
    in_dispatch: bool,
    in_tick: bool,
    char_id: i64,
    recipient_count: usize,
    kind: ProjectedItemOpKind,
    fp: ItemWireFingerprint,
    change_dict: Option<&CanonicalChangeDict>,
    pre_loc_flag: Option<(i64, i64)>,
    snap: &WorldStateSnapshot,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> (Option<String>, WouldQualifyFlags) {
    let Some((direct_slot_ok, fitted_ok, before_coords, (after_loc, after_flag, after_qty))) =
        evaluate_hand_written_row_details(
            char_id,
            recipient_count,
            kind,
            fp,
            change_dict,
            pre_loc_flag,
            snap,
            online,
            sde,
        )
    else {
        return (None, WouldQualifyFlags::default());
    };
    let fitted = (in_dispatch || in_tick) && fitted_ok;
    let tick = !in_dispatch && direct_slot_ok;
    let first_set = (in_dispatch && direct_slot_ok).then(|| {
        classify_item_row_kind_key(
            before_coords.map(|(l, _)| l),
            before_coords.map(|(_, f)| f),
            fp.owner_id,
            after_loc,
            after_flag,
            after_qty,
            snap,
            &ChangeSet::default(),
            &HashMap::new(),
            sde,
        )
    });
    (first_set, WouldQualifyFlags { fitted, tick })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn evaluate_hand_written_covered_class_in_snap(
    direct_enabled: bool,
    fitted_enabled: bool,
    char_all_enabled: bool,
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
    if char_all_enabled {
        if !is_valid_hand_written_char_owner_for_mode(
            true,
            char_id,
            recipient_count,
            fp,
            change_dict,
            snap,
            sde,
        ) {
            return None;
        }
        snap.characters
            .get(&char_id)
            .filter(|c| online.is_character_online(char_id, c.user_id))?;
        let (before_coords, (after_loc, after_flag, after_qty)) = match change_dict
            .and_then(|d| resolve_hand_written_coords(kind, fp, d, pre_loc_flag))
        {
            Some((b, _, after_vals)) => (b, after_vals),
            None => match kind {
                ProjectedItemOpKind::Deleted => (pre_loc_flag, (6, 0, 0)),
                ProjectedItemOpKind::CreatedOrUpdated => {
                    (pre_loc_flag, (fp.location_id, fp.flag_id, fp.wire_qty))
                }
            },
        };
        return Some(classify_item_row_kind_key(
            before_coords.map(|(l, _)| l),
            before_coords.map(|(_, f)| f),
            fp.owner_id,
            after_loc,
            after_flag,
            after_qty,
            snap,
            &ChangeSet::default(),
            &HashMap::new(),
            sde,
        ));
    }

    let (direct_slot_ok, fitted_ok, before_coords, (after_loc, after_flag, after_qty)) =
        evaluate_hand_written_row_details(
            char_id,
            recipient_count,
            kind,
            fp,
            change_dict,
            pre_loc_flag,
            snap,
            online,
            sde,
        )?;
    ((direct_enabled && direct_slot_ok) || (fitted_enabled && fitted_ok)).then(|| {
        classify_item_row_kind_key(
            before_coords.map(|(l, _)| l),
            before_coords.map(|(_, f)| f),
            fp.owner_id,
            after_loc,
            after_flag,
            after_qty,
            snap,
            &ChangeSet::default(),
            &HashMap::new(),
            sde,
        )
    })
}

#[cfg(test)]
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
    evaluate_hand_written_first_set_and_next_in_snap(
        in_dispatch,
        false,
        char_id,
        recipient_count,
        kind,
        fp,
        change_dict,
        pre_loc_flag,
        snap,
        online,
        sde,
    )
    .0
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn classify_hand_written_not_covered_reason_with_fitted_in_snap(
    fitted_enabled: bool,
    char_all_enabled: bool,
    scope_covered: bool,
    pilot_moved: bool,
    is_explicit_effect: bool,
    is_fuel_reserved: bool,
    char_id: i64,
    recipient_count: usize,
    kind: ProjectedItemOpKind,
    fp: ItemWireFingerprint,
    change_dict: Option<&CanonicalChangeDict>,
    pre_loc_flag: Option<(i64, i64)>,
    snap: &WorldStateSnapshot,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> NotCoveredReason {
    if !is_valid_hand_written_char_owner_for_mode(
        char_all_enabled,
        char_id,
        recipient_count,
        fp,
        change_dict,
        snap,
        sde,
    ) {
        return NotCoveredReason::NotCharacterOwned;
    }
    let Some(ch) = snap.characters.get(&char_id) else {
        return NotCoveredReason::NotCharacterOwned;
    };
    if !online.is_character_online(char_id, ch.user_id) {
        return NotCoveredReason::OwnerOffline;
    }
    if pilot_moved {
        return NotCoveredReason::PilotMovedInThisCommit;
    }
    if !scope_covered {
        return NotCoveredReason::NotInAnRpc;
    }
    if char_all_enabled {
        if is_explicit_effect {
            return NotCoveredReason::ExplicitEffect;
        }
        if is_fuel_reserved {
            return NotCoveredReason::FuelReservation;
        }
        return NotCoveredReason::NotCharacterOwned;
    }
    let coords = match change_dict {
        Some(dict) => {
            resolve_hand_written_coords(kind, fp, dict, pre_loc_flag).map(|(b, a, _)| (b, a))
        }
        None => match kind {
            ProjectedItemOpKind::Deleted => pre_loc_flag.map(|b| (Some(b), None)),
            ProjectedItemOpKind::CreatedOrUpdated => {
                Some((pre_loc_flag, Some((fp.location_id, fp.flag_id))))
            }
        },
    };
    let Some((before_coords, after_coords)) = coords else {
        return NotCoveredReason::LocationNotDockedHangarOrActiveShip;
    };
    let loc_ok = before_coords.is_none_or(|(l, _)| is_first_set_direct_slot_location(l, ch))
        && after_coords.is_none_or(|(l, _)| is_first_set_direct_slot_location(l, ch));
    if !loc_ok {
        return NotCoveredReason::LocationNotDockedHangarOrActiveShip;
    }
    let direct_flag_ok =
        before_coords.is_none_or(|(l, f)| is_first_set_direct_slot_coords(l, f, ch))
            && after_coords.is_none_or(|(l, f)| is_first_set_direct_slot_coords(l, f, ch));
    let fitted_flag_ok =
        fitted_enabled && evaluate_fitted_coords_pair(before_coords, after_coords, ch);
    if !(direct_flag_ok || fitted_flag_ok) {
        return NotCoveredReason::FlagNot4Or5;
    }
    if is_explicit_effect {
        return NotCoveredReason::ExplicitEffect;
    }
    if is_fuel_reserved {
        return NotCoveredReason::FuelReservation;
    }
    NotCoveredReason::LocationNotDockedHangarOrActiveShip
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn classify_hand_written_not_covered_reason_in_snap(
    char_all_enabled: bool,
    scope_covered: bool,
    pilot_moved: bool,
    is_explicit_effect: bool,
    is_fuel_reserved: bool,
    char_id: i64,
    recipient_count: usize,
    kind: ProjectedItemOpKind,
    fp: ItemWireFingerprint,
    change_dict: Option<&CanonicalChangeDict>,
    pre_loc_flag: Option<(i64, i64)>,
    snap: &WorldStateSnapshot,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> NotCoveredReason {
    classify_hand_written_not_covered_reason_with_fitted_in_snap(
        false,
        char_all_enabled,
        scope_covered,
        pilot_moved,
        is_explicit_effect,
        is_fuel_reserved,
        char_id,
        recipient_count,
        kind,
        fp,
        change_dict,
        pre_loc_flag,
        snap,
        online,
        sde,
    )
}
