//! `ServerContext` item-switch dedupe, suppression, and telemetry helpers (`#931` Track 3B / 3C-1).

#![forbid(unsafe_code)]

use eve_db::{Effects, InventoryItemRecord};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;

use super::cache_notifications::{current_dispatch_staging_scope_info, NotificationTarget};
use super::delta_item_switch::{
    evaluate_hand_written_first_set_in_snap, CanonicalChangeDict, ItemWireFingerprint,
    ProjectedItemOpKind, ProjectedItemSwitchConfig,
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

fn unwrap_multi_row_entry<'a>(
    entry: &'a PyRep,
    shared_dict: Option<&CanonicalChangeDict>,
) -> (&'a PyRep, Option<CanonicalChangeDict>) {
    match entry {
        PyRep::Tuple(pair) | PyRep::List(pair) => {
            if let [first, second, ..] = pair.as_slice() {
                if ItemWireFingerprint::from_pyrep_row(first).is_some() {
                    return (
                        first,
                        CanonicalChangeDict::from_pyrep(second).or_else(|| shared_dict.cloned()),
                    );
                }
            }
            (entry, shared_dict.cloned())
        }
        other => (other, shared_dict.cloned()),
    }
}

/// Returns `true` when at least one row (or an unparseable payload fallback) corresponds to an
/// enabled item projection switch (`#931` P2 #2).
pub(crate) fn has_active_switch_for_item_event(
    cfg: ProjectedItemSwitchConfig,
    event: &str,
    payload: &[PyRep],
) -> bool {
    if !cfg.is_any_enabled() {
        return false;
    }
    let switch_for_fp = |fp: ItemWireFingerprint| match op_kind_for_wire_fp(fp) {
        ProjectedItemOpKind::CreatedOrUpdated => cfg.char_same_scene_item_mutation,
        ProjectedItemOpKind::Deleted => cfg.char_same_scene_item_deleted,
    };
    match event {
        "OnItemChange" => payload
            .first()
            .and_then(ItemWireFingerprint::from_pyrep_row)
            .map_or(cfg.is_any_enabled(), |(_, fp)| switch_for_fp(fp)),
        "OnMultipleItemChange" => match payload.first() {
            Some(PyRep::List(rows) | PyRep::Tuple(rows)) if !rows.is_empty() => {
                let mut parsed_any = false;
                for entry in rows {
                    let (row_ref, _) = unwrap_multi_row_entry(entry, None);
                    if let Some((_, fp)) = ItemWireFingerprint::from_pyrep_row(row_ref) {
                        parsed_any = true;
                        if switch_for_fp(fp) {
                            return true;
                        }
                    }
                }
                if parsed_any {
                    false
                } else {
                    cfg.is_any_enabled()
                }
            }
            _ => cfg.is_any_enabled(),
        },
        _ => cfg.is_any_enabled(),
    }
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

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    fn record_unmatched_first_set_or_check(
        &self,
        scope_info: Option<&(u64, Option<String>)>,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        actual_dict: Option<&CanonicalChangeDict>,
        pre_loc_flag: Option<(i64, i64)>,
        helper: &str,
    ) -> bool {
        let Some(&(sid, ref rpc_opt)) = scope_info else {
            return false;
        };
        if self.projected_item_switches.has_transitioned_char(sid, char_id) {
            return false;
        }
        let first_set_class = self.db.with_snapshot(|snap| {
            evaluate_hand_written_first_set_in_snap(
                true,
                char_id,
                1,
                kind,
                fp,
                actual_dict,
                pre_loc_flag,
                snap,
                &*self.db,
                &self.sde,
            )
        });
        let Some(class_key) = first_set_class else {
            return false;
        };
        let rpc_method = rpc_opt
            .clone()
            .unwrap_or_else(|| crate::flight_recorder::UNKNOWN_RPC_LABEL.to_string());
        self.projected_item_switches.record_helper_without_token(
            sid,
            &class_key,
            &rpc_method,
            helper,
            char_id,
            item_id,
            kind,
            actual_dict,
        );
        true
    }

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    fn consume_or_record_switch_helper(
        &self,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        helper: &'static str,
        actual_dict: Option<&CanonicalChangeDict>,
        pre_loc_flag: Option<(i64, i64)>,
    ) -> bool {
        if !self.projected_item_switches.is_kind_enabled(kind) {
            return false;
        }
        if !self.has_online_character_recipient(char_id) {
            return false;
        }
        if fp.owner_id != char_id {
            self.projected_item_switches.record_non_first_set_emitted();
            return false;
        }
        let scope_info = current_dispatch_staging_scope_info(
            Some(&*self.pending_notifications),
            Some(&self.notify_tx),
        );
        let scope_id = scope_info.as_ref().map(|(id, _)| *id);
        let (matched, _) = self.projected_item_switches.try_take_in_dispatch(
            scope_id,
            char_id,
            item_id,
            kind,
            fp,
            tombstone_wildcard_deleted,
            helper,
            actual_dict,
        );
        if matched {
            return true;
        }
        if !self.record_unmatched_first_set_or_check(
            scope_info.as_ref(),
            char_id,
            item_id,
            kind,
            fp,
            actual_dict,
            pre_loc_flag,
            helper,
        ) {
            self.projected_item_switches.record_non_first_set_emitted();
        }
        false
    }

    #[track_caller]
    fn consume_projected_after_wire_fp(
        &self,
        char_id: i64,
        item_id: i64,
        fp: ItemWireFingerprint,
        helper: &'static str,
        actual_dict: Option<&CanonicalChangeDict>,
    ) -> bool {
        let kind = op_kind_for_wire_fp(fp);
        self.consume_or_record_switch_helper(
            char_id,
            item_id,
            kind,
            fp,
            kind == ProjectedItemOpKind::Deleted,
            helper,
            actual_dict,
            None,
        )
    }

    #[track_caller]
    pub(crate) fn try_consume_projected_item_mutation_with_changes(
        &self,
        char_id: i64,
        item: &InventoryItemRecord,
        changes: &[(i32, PyRep)],
        helper: &'static str,
    ) -> bool {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return false;
        }
        let dict = CanonicalChangeDict::from_entries(changes);
        self.consume_projected_after_wire_fp(
            char_id,
            item.item_id,
            ItemWireFingerprint::from_record(item),
            helper,
            Some(&dict),
        )
    }

    /// Returns `true` and consumes the matching commit-time projection token when `char_id`/`item`
    /// was already projected as a character-owned first-set mutation (`OnItemChange`).
    #[cfg(test)]
    #[inline]
    #[track_caller]
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

    #[track_caller]
    pub(crate) fn try_consume_projected_item_change_row_with_changes(
        &self,
        char_id: i64,
        row: &PyRep,
        changes: &[(i32, PyRep)],
        helper: &'static str,
    ) -> bool {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return false;
        }
        let Some((item_id, fp)) = ItemWireFingerprint::from_pyrep_row(row) else {
            if self.has_online_character_recipient(char_id) {
                self.projected_item_switches.record_non_first_set_emitted();
            }
            return false;
        };
        let dict = CanonicalChangeDict::from_entries(changes);
        self.consume_projected_after_wire_fp(char_id, item_id, fp, helper, Some(&dict))
    }

    #[track_caller]
    pub(crate) fn try_consume_projected_item_deleted_with_changes(
        &self,
        char_id: i64,
        old_item: &InventoryItemRecord,
        changes: &[(i32, PyRep)],
        helper: &'static str,
    ) -> bool {
        let dict = CanonicalChangeDict::from_entries(changes);
        self.consume_or_record_switch_helper(
            char_id,
            old_item.item_id,
            ProjectedItemOpKind::Deleted,
            ItemWireFingerprint::from_record(old_item),
            false,
            helper,
            Some(&dict),
            Some((old_item.location_id, old_item.flag as i64)),
        )
    }

    #[track_caller]
    pub(crate) fn try_consume_projected_on_item_change_args(
        &self,
        char_id: i64,
        payload: &[PyRep],
        helper: &'static str,
    ) -> bool {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return false;
        }
        let (row, actual_dict) = match payload {
            [row, dict_rep, ..] => (row, CanonicalChangeDict::from_pyrep(dict_rep)),
            [row] => (row, None),
            [] => {
                if self.has_online_character_recipient(char_id) {
                    self.projected_item_switches.record_non_first_set_emitted();
                }
                return false;
            }
        };
        let Some((item_id, fp)) = ItemWireFingerprint::from_pyrep_row(row) else {
            if self.has_online_character_recipient(char_id) {
                self.projected_item_switches.record_non_first_set_emitted();
            }
            return false;
        };
        self.consume_projected_after_wire_fp(char_id, item_id, fp, helper, actual_dict.as_ref())
    }

    /// Strips already commit-projected first-set rows from a single-recipient `OnMultipleItemChange`
    /// payload inside `notify_inner`, returning `true` when the notification becomes empty and
    /// must be dropped (`#931` `BF-931-3C1-03`).
    #[track_caller]
    pub(crate) fn filter_switched_on_multiple_item_change_args(
        &self,
        recipients: &[(i64, Option<u64>)],
        payload: &mut Vec<PyRep>,
        helper: &'static str,
    ) -> bool {
        let cfg = self.projected_item_switches.config();
        if recipients.is_empty() || !cfg.is_any_enabled() {
            return false;
        }
        if recipients.len() > 1 {
            if has_active_switch_for_item_event(cfg, "OnMultipleItemChange", payload) {
                self.projected_item_switches.record_non_first_set_emitted();
            }
            return false;
        }
        let char_id = recipients[0].0;
        let shared_dict = payload.get(1).and_then(CanonicalChangeDict::from_pyrep);
        let Some(PyRep::List(rows) | PyRep::Tuple(rows)) = payload.first_mut() else {
            self.projected_item_switches.record_non_first_set_emitted();
            return false;
        };
        if rows.is_empty() {
            return false;
        }
        let scope_info = current_dispatch_staging_scope_info(
            Some(&*self.pending_notifications),
            Some(&self.notify_tx),
        );
        let scope_id = scope_info.as_ref().map(|(id, _)| *id);
        let old_rows = std::mem::take(rows);
        let mut kept_rows = Vec::with_capacity(old_rows.len());
        let mut needs_non_first_set = false;
        for entry in old_rows {
            let (parsed, row_dict) = {
                let (row_ref, row_dict) = unwrap_multi_row_entry(&entry, shared_dict.as_ref());
                (ItemWireFingerprint::from_pyrep_row(row_ref), row_dict)
            };
            let Some((item_id, fp)) = parsed else {
                kept_rows.push(entry);
                needs_non_first_set = true;
                continue;
            };
            let kind = op_kind_for_wire_fp(fp);
            if !self.projected_item_switches.is_kind_enabled(kind) {
                kept_rows.push(entry);
                continue;
            }
            let (matched, _) = self.projected_item_switches.try_take_multi_in_dispatch(
                scope_id,
                char_id,
                item_id,
                kind,
                fp,
                kind == ProjectedItemOpKind::Deleted,
                helper,
                row_dict.as_ref(),
            );
            if matched {
                continue;
            }
            kept_rows.push(entry);
            if fp.owner_id != char_id
                || !self.record_unmatched_first_set_or_check(
                    scope_info.as_ref(),
                    char_id,
                    item_id,
                    kind,
                    fp,
                    row_dict.as_ref(),
                    None,
                    helper,
                )
            {
                needs_non_first_set = true;
            }
        }
        if kept_rows.is_empty() {
            return true;
        }
        *rows = kept_rows;
        if needs_non_first_set {
            self.projected_item_switches.record_non_first_set_emitted();
        }
        false
    }

    #[inline]
    #[track_caller]
    pub(crate) fn should_skip_projected_item_change_for_target(
        &self,
        target: &NotificationTarget,
        item: &InventoryItemRecord,
        changes: &[(i32, PyRep)],
        helper: &'static str,
    ) -> bool {
        if let NotificationTarget::Character(cid) = target {
            return self.try_consume_projected_item_mutation_with_changes(*cid, item, changes, helper);
        }
        let cfg = self.projected_item_switches.config();
        let switch_on = if item.location_id == 6 {
            cfg.char_same_scene_item_deleted
        } else {
            cfg.char_same_scene_item_mutation
        };
        if switch_on && !self.resolve_notification_recipients(target).is_empty() {
            self.projected_item_switches.record_non_first_set_emitted();
        }
        false
    }

    #[inline]
    #[track_caller]
    pub(crate) fn should_skip_projected_item_deleted_for_target(
        &self,
        target: &NotificationTarget,
        old_item: &InventoryItemRecord,
        changes: &[(i32, PyRep)],
        helper: &'static str,
    ) -> bool {
        if let NotificationTarget::Character(cid) = target {
            return self.try_consume_projected_item_deleted_with_changes(*cid, old_item, changes, helper);
        }
        if self.projected_item_switches.config().char_same_scene_item_deleted
            && !self.resolve_notification_recipients(target).is_empty()
        {
            self.projected_item_switches.record_non_first_set_emitted();
        }
        false
    }

    #[inline]
    #[track_caller]
    pub(crate) fn should_skip_projected_item_update(
        &self,
        targets: &[(NotificationTarget, &'static str)],
        old_item: &InventoryItemRecord,
        new_item: &InventoryItemRecord,
        changes: &[(i32, PyRep)],
        helper: &'static str,
    ) -> bool {
        if !self.projected_item_switches.config().char_same_scene_item_mutation {
            return false;
        }
        let has_any_online_recipient = || {
            targets
                .iter()
                .any(|(t, _)| !self.resolve_notification_recipients(t).is_empty())
        };
        if old_item.owner_id != new_item.owner_id || targets.len() != 1 {
            if has_any_online_recipient() {
                self.projected_item_switches.record_non_first_set_emitted();
            }
            return false;
        }
        let NotificationTarget::Character(cid) = targets[0].0 else {
            if has_any_online_recipient() {
                self.projected_item_switches.record_non_first_set_emitted();
            }
            return false;
        };
        let dict = CanonicalChangeDict::from_entries(changes);
        self.consume_or_record_switch_helper(
            cid,
            new_item.item_id,
            ProjectedItemOpKind::CreatedOrUpdated,
            ItemWireFingerprint::from_record(new_item),
            false,
            helper,
            Some(&dict),
            Some((old_item.location_id, old_item.flag as i64)),
        )
    }

    /// Declares a trashed fitted module/rig `OnItemChange` (`locationID = 10`, `flagID = 147`) inside
    /// an active transaction's [`eve_db::Effects`] (`#931` Guardrail 4).
    ///
    /// Declaring this intentional V24 `OnModuleUnfitted` wire divergence inside `Effects` ensures
    /// both `delta_item_compare` and enabled commit-time projection suppress a conflicting
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
