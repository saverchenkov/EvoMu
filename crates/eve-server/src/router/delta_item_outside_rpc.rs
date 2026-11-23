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
    classify_item_row_kind_key, entry_matches_wire_fp, evaluate_hand_written_row_details,
    is_first_set_direct_slot_pair, remove_item_from_facts, CanonicalChangeDict,
    CommitProjectedToken, DedupeLookupOutcome, ItemWireFingerprint, ProjectedDedupeEntry,
    ProjectedItemOpKind, ProjectedItemSwitchConfig,
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
    pub(super) facts: HashMap<(i64, i64), ProjectedDedupeEntry>,
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
        telemetry: &mut SwitchTelemetryState,
    ) {
        for tok in tokens {
            let key = (tok.char_id, tok.item_id);
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
    }

    fn clear_offline_chars(
        &mut self,
        offline_chars: &[i64],
        telemetry: Option<&mut SwitchTelemetryState>,
    ) {
        if !self.facts.is_empty() {
            if let Some(telem) = telemetry {
                let offline_keys: Vec<(i64, i64)> = self
                    .facts
                    .keys()
                    .copied()
                    .filter(|(cid, _)| offline_chars.contains(cid))
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
                    .retain(|(cid, _), _| !offline_chars.contains(cid));
            }
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
    pub(super) covered_industry_monitors: HashMap<i64, (i64, HashSet<u32>)>,
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
        if !self.covered_industry_monitors.is_empty() {
            self.covered_industry_monitors.retain(|_, (cid, tids)| {
                for rec in [change.before.as_deref(), change.after.as_deref()]
                    .into_iter()
                    .flatten()
                {
                    if *cid == rec.owner_id {
                        tids.remove(&rec.type_id);
                    }
                }
                !tids.is_empty()
            });
        }
    }

    pub(super) fn invalidate_transitioned_chars(&mut self, transitioned_chars: &[i64]) {
        if !transitioned_chars.is_empty() && !self.covered_industry_monitors.is_empty() {
            self.covered_industry_monitors
                .retain(|_, (cid, _)| !transitioned_chars.contains(cid));
        }
    }

    pub(super) fn apply_commit(
        &mut self,
        origin_scope: CompareOriginScope,
        outside_rpc_enabled: bool,
        tokens: Vec<CommitProjectedToken>,
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
        target.apply_commit_tokens(origin_scope, emit_live, tokens, telemetry);
    }

    pub(super) fn record_covered_industry_monitors(
        &mut self,
        covered: &HashMap<i64, (i64, Vec<u32>)>,
    ) {
        for (&monitor_id, (char_id, tids)) in covered {
            let entry = self
                .covered_industry_monitors
                .entry(monitor_id)
                .or_insert_with(|| (*char_id, HashSet::new()));
            if entry.0 != *char_id {
                entry.0 = *char_id;
                entry.1.clear();
            }
            entry.1.extend(tids.iter().copied());
        }
    }

    pub(super) fn is_industry_monitor_covered(&self, monitor_id: i64, tids: &[u32]) -> bool {
        self.covered_industry_monitors
            .get(&monitor_id)
            .is_some_and(|(_, covered)| tids.iter().all(|tid| covered.contains(tid)))
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
        if !self.covered_industry_monitors.is_empty() {
            self.covered_industry_monitors
                .retain(|_, (cid, _)| !offline_chars.contains(cid));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_shadow_outcome(
        &self,
        origin_scope: CompareOriginScope,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        telemetry: &mut SwitchTelemetryState,
    ) -> DedupeLookupOutcome {
        let Some(entry) = self.shadow.facts.get(&(char_id, item_id)) else {
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
/// shadow outside-RPC telemetry when `shadow_outside_dry_run` is `true`).
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
) -> Option<(i64, ProjectedItemOpKind, &'a InventoryItemRecord)> {
    let before_opt = change.before.as_deref();
    let after_opt = change.after.as_deref();
    let fp_rec = after_opt.or(before_opt)?;
    let owner_id = fp_rec.owner_id;
    let op_kind = is_first_set_direct_slot_pair(
        owner_id,
        before_opt,
        after_opt,
        snap_after,
        changed_chars,
        online,
        sde,
    )?;
    let active = if in_dispatch || shadow_outside_dry_run {
        config.is_kind_enabled(op_kind)
    } else {
        config.is_kind_enabled_in_scope(false, op_kind)
    };
    active.then_some((owner_id, op_kind, fp_rec))
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
