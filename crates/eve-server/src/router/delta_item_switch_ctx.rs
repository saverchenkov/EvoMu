//! `ServerContext` item-switch dedupe and suppression helpers (`#931` Track 3B / 3C-1).

#![forbid(unsafe_code)]

use eve_db::{Effects, InventoryItemRecord};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;

use super::cache_notifications::{current_dispatch_staging_scope_id, NotificationTarget};
use super::delta_item_switch::{
    DedupeLookupOutcome, ItemWireFingerprint, ProjectedItemOpKind, ProjectedItemSwitchConfig,
};
use super::ServerContext;

#[inline]
pub(crate) fn op_kind_for_wire_fp(fp: ItemWireFingerprint) -> ProjectedItemOpKind {
    if fp.location_id == 6 {
        ProjectedItemOpKind::Deleted
    } else {
        ProjectedItemOpKind::CreatedOrUpdated
    }
}

fn unwrap_multi_row_entry<'a>(entry: &'a PyRep) -> &'a PyRep {
    match entry {
        PyRep::Tuple(pair) | PyRep::List(pair) => {
            if let [first, _, ..] = pair.as_slice() {
                if ItemWireFingerprint::from_pyrep_row(first).is_some() {
                    return first;
                }
            }
            entry
        }
        other => other,
    }
}

pub(crate) enum MultiItemSwitchFilterOutcome {
    Keep,
    Drop,
    SplitPerRecipient(Vec<(i64, Vec<PyRep>)>),
}

impl ServerContext {
    /// Returns the active [`ProjectedItemSwitchConfig`] for this server context.
    #[inline]
    pub fn projected_item_switches(&self) -> ProjectedItemSwitchConfig {
        self.projected_item_switches.config()
    }

    /// Sets [`ProjectedItemSwitchConfig`] on this server context (primarily for unit/integration tests
    /// without mutating process-wide environment variables).
    #[inline]
    pub fn set_projected_item_switches(&self, config: ProjectedItemSwitchConfig) {
        self.projected_item_switches.set_config(config);
    }

    #[inline]
    fn has_online_character_recipient(&self, char_id: i64) -> bool {
        char_id > 0
            && !self
                .resolve_notification_recipients(&NotificationTarget::Character(char_id))
                .is_empty()
    }

    /// Consumes a matching commit-time projection fact for `(char_id, item_id)` across either ID
    /// type (`is_corp_target` first, then `!is_corp_target`), returning `true` when suppressed.
    fn consume_or_record_switch_inner(
        &self,
        char_id: i64,
        item_id: i64,
        is_corp_target: bool,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
    ) -> bool {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return false;
        }
        let scope_id = current_dispatch_staging_scope_id(
            Some(&*self.pending_notifications),
            Some(&self.notify_tx),
        );
        let in_dispatch = scope_id.is_some();
        let online_recipient = self.has_online_character_recipient(char_id);
        let live_ledger_active = in_dispatch || cfg.char_same_scene_item_outside_rpc;

        // Section 4D: suppress a hand-written call for `(char_id, item_id)` whenever the live
        // ledger holds a fact of either id type (`is_corp_target` first, then `!is_corp_target`).
        if live_ledger_active && online_recipient {
            for check_corp in [is_corp_target, !is_corp_target] {
                match self.projected_item_switches.try_check_in_scope(
                    scope_id,
                    char_id,
                    item_id,
                    check_corp,
                    kind,
                    fp,
                    tombstone_wildcard_deleted,
                ) {
                    DedupeLookupOutcome::Replaced | DedupeLookupOutcome::Superseded => {
                        return true;
                    }
                    DedupeLookupOutcome::NotSet => {}
                }
            }
        }
        false
    }

    fn consume_projected_after_wire_fp(
        &self,
        char_id: i64,
        item_id: i64,
        fp: ItemWireFingerprint,
    ) -> bool {
        let kind = op_kind_for_wire_fp(fp);
        self.consume_or_record_switch_inner(
            char_id,
            item_id,
            false,
            kind,
            fp,
            kind == ProjectedItemOpKind::Deleted,
        )
    }

    pub(crate) fn try_consume_projected_item_mutation_with_changes(
        &self,
        char_id: i64,
        item: &InventoryItemRecord,
        _changes: &[(i32, PyRep)],
        _helper: &'static str,
    ) -> bool {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return false;
        }
        self.consume_projected_after_wire_fp(
            char_id,
            item.item_id,
            ItemWireFingerprint::from_record(item),
        )
    }

    /// Returns `true` and consumes the matching commit-time projection token when `char_id`/`item`
    /// was already projected as a character-owned first-set mutation (`OnItemChange`).
    #[cfg(test)]
    #[inline]
    pub(crate) fn try_consume_projected_item_mutation(
        &self,
        char_id: i64,
        item: &InventoryItemRecord,
    ) -> bool {
        self.try_consume_projected_item_mutation_with_changes(
            char_id,
            item,
            &[],
            "try_consume_projected_item_mutation",
        )
    }

    pub(crate) fn try_consume_projected_item_change_row_with_changes(
        &self,
        char_id: i64,
        row: &PyRep,
        _changes: &[(i32, PyRep)],
        _helper: &'static str,
    ) -> bool {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return false;
        }
        let Some((item_id, fp)) = ItemWireFingerprint::from_pyrep_row(row) else {
            return false;
        };
        self.consume_projected_after_wire_fp(char_id, item_id, fp)
    }

    pub(crate) fn filter_projected_on_item_change_recipients(
        &self,
        recipients: &mut Vec<(i64, Option<u64>)>,
        id_type: &str,
        payload: &[PyRep],
        _helper: &'static str,
    ) -> bool {
        let cfg = self.projected_item_switches.config();
        if recipients.is_empty() || !cfg.is_any_enabled() {
            return false;
        }
        let is_corp = id_type == "corpid";
        if (!is_corp || !cfg.corp_all_items) && recipients.len() > 1 {
            return false;
        }
        let Some(row) = payload.first() else {
            return false;
        };
        let Some((item_id, fp)) = ItemWireFingerprint::from_pyrep_row(row) else {
            return false;
        };
        let kind = op_kind_for_wire_fp(fp);
        let tombstone_wildcard_deleted = kind == ProjectedItemOpKind::Deleted;
        recipients.retain(|&(cid, _)| {
            !self.consume_or_record_switch_inner(
                cid,
                item_id,
                is_corp,
                kind,
                fp,
                tombstone_wildcard_deleted,
            )
        });
        recipients.is_empty()
    }

    /// Strips already commit-projected rows and/or already-notified recipients from
    /// `OnMultipleItemChange` inside `notify_inner`, returning [`MultiItemSwitchFilterOutcome::Drop`]
    /// when empty, [`MultiItemSwitchFilterOutcome::SplitPerRecipient`] when multi-row corp
    /// consumption is ragged across surviving recipients (`BF-01`), or
    /// [`MultiItemSwitchFilterOutcome::Keep`] for shared delivery (`#931` `BF-931-3C1-03` &
    /// Track 5C).
    pub(crate) fn filter_switched_on_multiple_item_change_args(
        &self,
        recipients: &mut Vec<(i64, Option<u64>)>,
        id_type: &str,
        payload: &mut Vec<PyRep>,
        _helper: &'static str,
    ) -> MultiItemSwitchFilterOutcome {
        let cfg = self.projected_item_switches.config();
        if recipients.is_empty() || !cfg.is_any_enabled() {
            return MultiItemSwitchFilterOutcome::Keep;
        }
        let is_corp = id_type == "corpid";
        if (!is_corp || !cfg.corp_all_items) && recipients.len() > 1 {
            return MultiItemSwitchFilterOutcome::Keep;
        }
        let is_tuple_wrap = matches!(payload.first(), Some(PyRep::Tuple(_)));
        let Some(PyRep::List(rows) | PyRep::Tuple(rows)) = payload.first_mut() else {
            return MultiItemSwitchFilterOutcome::Keep;
        };
        if rows.is_empty() {
            return MultiItemSwitchFilterOutcome::Keep;
        }
        let old_rows = std::mem::take(rows);
        if is_corp && cfg.corp_all_items {
            let rec_count = recipients.len();
            let row_count = old_rows.len();
            let mut consumed_matrix = vec![vec![false; rec_count]; row_count];
            let mut row_unparseable = vec![false; row_count];
            let mut has_unparseable_row = false;
            for (r_idx, entry) in old_rows.iter().enumerate() {
                let Some((item_id, fp)) =
                    ItemWireFingerprint::from_pyrep_row(unwrap_multi_row_entry(entry))
                else {
                    has_unparseable_row = true;
                    row_unparseable[r_idx] = true;
                    continue;
                };
                let kind = op_kind_for_wire_fp(fp);
                let tombstone_wildcard_deleted = kind == ProjectedItemOpKind::Deleted;
                for (m_idx, &(cid, _)) in recipients.iter().enumerate() {
                    consumed_matrix[r_idx][m_idx] = self.consume_or_record_switch_inner(
                        cid,
                        item_id,
                        true,
                        kind,
                        fp,
                        tombstone_wildcard_deleted,
                    );
                }
            }
            let mut kept_rows = Vec::with_capacity(row_count);
            let mut kept_row_orig_indices = Vec::with_capacity(row_count);
            for (r_idx, entry) in old_rows.into_iter().enumerate() {
                if !consumed_matrix[r_idx].iter().all(|&c| c) {
                    kept_rows.push(entry);
                    kept_row_orig_indices.push(r_idx);
                }
            }
            if kept_rows.is_empty() {
                recipients.clear();
                return MultiItemSwitchFilterOutcome::Drop;
            }
            let mut surviving_indices = Vec::with_capacity(rec_count);
            for m_idx in 0..rec_count {
                if has_unparseable_row
                    || kept_row_orig_indices
                        .iter()
                        .any(|&r_idx| !consumed_matrix[r_idx][m_idx])
                {
                    surviving_indices.push(m_idx);
                }
            }
            if surviving_indices.is_empty() {
                recipients.clear();
                return MultiItemSwitchFilterOutcome::Drop;
            }
            let is_ragged = kept_row_orig_indices.iter().any(|&r_idx| {
                surviving_indices
                    .iter()
                    .any(|&m_idx| consumed_matrix[r_idx][m_idx])
            });
            if is_ragged {
                let shared_tail: Vec<PyRep> = payload[1..].to_vec();
                let mut per_recipient = Vec::with_capacity(surviving_indices.len());
                for m_idx in surviving_indices {
                    let mut rec_rows = Vec::with_capacity(kept_rows.len());
                    for (k_pos, &orig_r) in kept_row_orig_indices.iter().enumerate() {
                        if row_unparseable[orig_r] || !consumed_matrix[orig_r][m_idx] {
                            rec_rows.push(kept_rows[k_pos].clone());
                        }
                    }
                    let mut rec_args = Vec::with_capacity(1 + shared_tail.len());
                    rec_args.push(if is_tuple_wrap {
                        PyRep::Tuple(rec_rows)
                    } else {
                        PyRep::List(rec_rows)
                    });
                    rec_args.extend(shared_tail.iter().cloned());
                    per_recipient.push((recipients[m_idx].0, rec_args));
                }
                recipients.clear();
                return MultiItemSwitchFilterOutcome::SplitPerRecipient(per_recipient);
            }
            if surviving_indices.len() < rec_count {
                let kept_recips = surviving_indices
                    .into_iter()
                    .map(|m_idx| recipients[m_idx])
                    .collect();
                *recipients = kept_recips;
            }
            *rows = kept_rows;
            return MultiItemSwitchFilterOutcome::Keep;
        }

        let char_id = recipients[0].0;
        let mut kept_rows = Vec::with_capacity(old_rows.len());
        for entry in old_rows {
            let Some((item_id, fp)) =
                ItemWireFingerprint::from_pyrep_row(unwrap_multi_row_entry(&entry))
            else {
                kept_rows.push(entry);
                continue;
            };
            let kind = op_kind_for_wire_fp(fp);
            if !self.consume_or_record_switch_inner(
                char_id,
                item_id,
                is_corp,
                kind,
                fp,
                kind == ProjectedItemOpKind::Deleted,
            ) {
                kept_rows.push(entry);
            }
        }
        if kept_rows.is_empty() {
            return MultiItemSwitchFilterOutcome::Drop;
        }
        *rows = kept_rows;
        MultiItemSwitchFilterOutcome::Keep
    }

    fn filter_projected_target_list_for_item(
        &self,
        targets: &[(NotificationTarget, &'static str)],
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
    ) -> Vec<(NotificationTarget, &'static str)> {
        let cfg = self.projected_item_switches.config();
        let mut kept = Vec::with_capacity(targets.len());
        for (target, id_type) in targets {
            match target {
                NotificationTarget::Character(cid) => {
                    if !self.consume_or_record_switch_inner(
                        *cid,
                        item_id,
                        false,
                        kind,
                        fp,
                        tombstone_wildcard_deleted,
                    ) {
                        kept.push((target.clone(), *id_type));
                    }
                }
                NotificationTarget::Corporation(corp_id) if cfg.corp_all_items => {
                    let recipients = self.resolve_notification_recipients(target);
                    if recipients.is_empty() {
                        kept.push((target.clone(), *id_type));
                        continue;
                    }
                    let mut unconsumed = Vec::with_capacity(recipients.len());
                    let mut any_consumed = false;
                    for (cid, _) in &recipients {
                        if self.consume_or_record_switch_inner(
                            *cid,
                            item_id,
                            true,
                            kind,
                            fp,
                            tombstone_wildcard_deleted,
                        ) {
                            any_consumed = true;
                        } else {
                            unconsumed.push(*cid);
                        }
                    }
                    if unconsumed.is_empty() {
                        continue;
                    }
                    if !any_consumed {
                        kept.push((NotificationTarget::Corporation(*corp_id), *id_type));
                    } else {
                        for cid in unconsumed {
                            kept.push((NotificationTarget::Character(cid), *id_type));
                        }
                    }
                }
                _ => kept.push((target.clone(), *id_type)),
            }
        }
        kept
    }

    pub(crate) fn filter_projected_item_change_targets(
        &self,
        targets: &[(NotificationTarget, &'static str)],
        item: &InventoryItemRecord,
        _changes: &[(i32, PyRep)],
        _helper: &'static str,
    ) -> Vec<(NotificationTarget, &'static str)> {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return targets.to_vec();
        }
        let fp = ItemWireFingerprint::from_record(item);
        let kind = op_kind_for_wire_fp(fp);
        self.filter_projected_target_list_for_item(
            targets,
            item.item_id,
            kind,
            fp,
            kind == ProjectedItemOpKind::Deleted,
        )
    }

    pub(crate) fn filter_projected_item_change_target(
        &self,
        target: NotificationTarget,
        item: &InventoryItemRecord,
        _changes: &[(i32, PyRep)],
        _helper: &'static str,
    ) -> Vec<NotificationTarget> {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return vec![target];
        }
        let fp = ItemWireFingerprint::from_record(item);
        let kind = op_kind_for_wire_fp(fp);
        self.filter_projected_target_list_for_item(
            &[(target, "")],
            item.item_id,
            kind,
            fp,
            kind == ProjectedItemOpKind::Deleted,
        )
        .into_iter()
        .map(|(t, _)| t)
        .collect()
    }

    pub(crate) fn filter_projected_item_deleted_targets(
        &self,
        targets: &[(NotificationTarget, &'static str)],
        old_item: &InventoryItemRecord,
        _changes: &[(i32, PyRep)],
        _helper: &'static str,
    ) -> Vec<(NotificationTarget, &'static str)> {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return targets.to_vec();
        }
        let fp = ItemWireFingerprint::from_record(old_item);
        self.filter_projected_target_list_for_item(
            targets,
            old_item.item_id,
            ProjectedItemOpKind::Deleted,
            fp,
            false,
        )
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn should_skip_projected_item_change_for_target(
        &self,
        target: &NotificationTarget,
        item: &InventoryItemRecord,
        changes: &[(i32, PyRep)],
        helper: &'static str,
    ) -> bool {
        self.filter_projected_item_change_target(target.clone(), item, changes, helper)
            .is_empty()
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn should_skip_projected_item_deleted_for_target(
        &self,
        target: &NotificationTarget,
        old_item: &InventoryItemRecord,
        changes: &[(i32, PyRep)],
        helper: &'static str,
    ) -> bool {
        self.filter_projected_item_deleted_targets(
            &[(target.clone(), "")],
            old_item,
            changes,
            helper,
        )
        .is_empty()
    }

    pub(crate) fn filter_projected_item_update_targets(
        &self,
        targets: &[(NotificationTarget, &'static str)],
        old_item: &InventoryItemRecord,
        new_item: &InventoryItemRecord,
        _changes: &[(i32, PyRep)],
        _helper: &'static str,
    ) -> Vec<(NotificationTarget, &'static str)> {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return targets.to_vec();
        }
        if !cfg.char_all_items
            && !cfg.corp_all_items
            && (old_item.owner_id != new_item.owner_id || targets.len() != 1)
        {
            return targets.to_vec();
        }
        let fp = ItemWireFingerprint::from_record(new_item);
        self.filter_projected_target_list_for_item(
            targets,
            new_item.item_id,
            ProjectedItemOpKind::CreatedOrUpdated,
            fp,
            false,
        )
    }

    pub(crate) fn try_consume_projected_on_blueprints_updated(
        &self,
        recipients: &mut Vec<(i64, Option<u64>)>,
        payload: &[PyRep],
    ) -> bool {
        let cfg = self.projected_item_switches.config();
        if recipients.is_empty() || (!cfg.char_all_items && !cfg.corp_all_items) {
            return false;
        }
        let scope_id = current_dispatch_staging_scope_id(
            Some(&*self.pending_notifications),
            Some(&self.notify_tx),
        );
        let in_dispatch = scope_id.is_some();
        let char_active = cfg.is_char_all_items_enabled_in_scope(in_dispatch);
        let corp_active = cfg.is_corp_all_items_enabled_in_scope(in_dispatch);
        if !char_active && !corp_active {
            return false;
        }
        let Some(PyRep::Integer(owner_id)) = payload.first() else {
            return false;
        };
        let orig_multi = recipients.len() > 1;
        recipients.retain(|&(cid, _)| {
            let switch_active = if cid == *owner_id {
                char_active
            } else {
                corp_active
            };
            if !switch_active || (orig_multi && !corp_active) {
                return true;
            }
            !self
                .projected_item_switches
                .has_blueprint_fact_in_scope(scope_id, cid, *owner_id)
        });
        recipients.is_empty()
    }

    /// Declares a trashed fitted module/rig `OnItemChange` (`locationID = 10`, `flagID = 147`) inside
    /// an active transaction's [`eve_db::Effects`] (`#931` Guardrail 4).
    ///
    /// Declares this intentional V24 `OnModuleUnfitted` wire divergence inside `Effects` so
    /// enabled commit-time projection suppresses a conflicting
    /// `loc=6, flag=0, qty=0` tombstone for `(char_id, item_id)`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_tx_fitted_item_trashed(
        effects: &mut Effects,
        sde: &SdeRegistry,
        char_id: i64,
        item_id: i64,
        type_id: i64,
        quantity: i64,
        old_location_id: i64,
        old_flag: i64,
    ) {
        if char_id <= 0 {
            return;
        }
        let args = build_fitted_item_trashed_args(
            sde,
            char_id,
            item_id,
            type_id,
            quantity,
            old_location_id,
            old_flag,
        );
        effects.notify_character(char_id, "OnItemChange", "charid", args);
    }
}

/// Builds the V24 `OnItemChange` payload (`location = 10`, `flag = 147`) expected for fitted
/// module/rig destruction so `invCache` fires `OnModuleUnfitted` (`#931` Guardrail 4).
pub(crate) fn build_fitted_item_trashed_args(
    sde: &SdeRegistry,
    char_id: i64,
    item_id: i64,
    type_id: i64,
    quantity: i64,
    old_location_id: i64,
    old_flag: i64,
) -> Vec<PyRep> {
    let (group_id, category_id) = u32::try_from(type_id)
        .ok()
        .and_then(|tid| sde.types.get(&tid))
        .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
        .unwrap_or((0, 0));
    // Fitted modules/rigs are singleton items, so their packed inv_item quantity is -1 when qty == 1.
    let packed_qty = if quantity == 1 { -1 } else { quantity };
    let row = eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(vec![
        PyRep::Integer(item_id),
        PyRep::Integer(type_id),
        PyRep::Integer(char_id),
        PyRep::Integer(10),
        PyRep::Integer(147),
        PyRep::Integer(packed_qty),
        PyRep::Integer(group_id),
        PyRep::Integer(category_id),
        PyRep::String(String::new()),
    ]);
    vec![
        row,
        PyRep::Dict(vec![
            (PyRep::Integer(3), PyRep::Integer(old_location_id)),
            (PyRep::Integer(4), PyRep::Integer(old_flag)),
        ]),
    ]
}
