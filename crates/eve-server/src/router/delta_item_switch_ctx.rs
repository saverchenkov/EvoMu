//! `ServerContext` item-switch dedupe, suppression, and telemetry helpers (`#931` Track 3B / 3C-1).

#![forbid(unsafe_code)]

use eve_db::{Effects, InventoryItemRecord};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;

use super::cache_notifications::{current_dispatch_staging_scope_info, NotificationTarget};
use super::delta_item_compare::{current_origin_scope_telemetry, CompareOriginScope};
use super::delta_item_switch::{
    classify_hand_written_not_covered_reason_in_snap, evaluate_hand_written_first_set_in_snap,
    evaluate_hand_written_outside_first_set_in_snap, CanonicalChangeDict, DedupeLookupOutcome,
    ItemWireFingerprint, NotCoveredReason, ProjectedItemOpKind, ProjectedItemSwitchConfig,
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
/// active item switch in `(in_dispatch, kind)` (preserving real legacy telemetry when outside RPC
/// and `char_same_scene_item_outside_rpc` is `false`).
pub(crate) fn has_active_switch_for_item_event(
    cfg: ProjectedItemSwitchConfig,
    in_dispatch: bool,
    event: &str,
    payload: &[PyRep],
) -> bool {
    if !cfg.is_any_enabled() {
        return false;
    }
    let switch_for_fp = |fp: ItemWireFingerprint| {
        let kind = op_kind_for_wire_fp(fp);
        cfg.is_kind_enabled_in_scope(in_dispatch, kind) || (!in_dispatch && cfg.is_kind_enabled(kind))
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
    fn record_offline_or_non_char_not_covered(
        &self,
        char_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        actual_dict: Option<&CanonicalChangeDict>,
        pre_loc_flag: Option<(i64, i64)>,
    ) {
        let reason = self.db.with_snapshot(|snap| {
            classify_hand_written_not_covered_reason_in_snap(
                false,
                false,
                false,
                false,
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
        self.projected_item_switches.record_not_covered(reason);
    }

    #[allow(clippy::too_many_arguments)]
    fn record_shadow_outside_helper(
        &self,
        origin_scope: CompareOriginScope,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        actual_dict: Option<&CanonicalChangeDict>,
        pre_loc_flag: Option<(i64, i64)>,
    ) {
        if !self.has_online_character_recipient(char_id) || fp.owner_id != char_id {
            return;
        }
        let outcome = self.projected_item_switches.check_shadow_outside_outcome(
            origin_scope,
            char_id,
            item_id,
            kind,
            fp,
            tombstone_wildcard_deleted,
        );
        if outcome == DedupeLookupOutcome::NotSet {
            let (is_explicit, is_fuel) = self
                .projected_item_switches
                .is_outside_explicit_or_fuel_skipped(false, char_id, item_id);
            if !is_explicit && !is_fuel {
                let is_first_set = self.db.with_snapshot(|snap| {
                    evaluate_hand_written_outside_first_set_in_snap(
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
                    .is_some()
                });
                if is_first_set {
                    self.projected_item_switches
                        .record_shadow_outside_no_projected_event(origin_scope);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    fn record_unmatched_first_set_or_check(
        &self,
        scope_info: Option<&(u64, Option<String>)>,
        origin_scope: CompareOriginScope,
        outside_rpc_enabled: bool,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        actual_dict: Option<&CanonicalChangeDict>,
        pre_loc_flag: Option<(i64, i64)>,
        helper: &str,
    ) -> bool {
        let sid_opt = scope_info.map(|(sid, _)| *sid);
        let pilot_moved = sid_opt
            .is_some_and(|sid| self.projected_item_switches.has_transitioned_char(sid, char_id));
        let (is_explicit, is_fuel) = match sid_opt {
            Some(sid) => (
                self.projected_item_switches
                    .is_explicit_effect_in_scope(sid, char_id, item_id),
                self.projected_item_switches
                    .is_fuel_reserved_in_scope(sid, item_id),
            ),
            None => self
                .projected_item_switches
                .is_outside_explicit_or_fuel_skipped(outside_rpc_enabled, char_id, item_id),
        };
        let scope_covered = scope_info.is_some() || outside_rpc_enabled;
        if scope_covered && !pilot_moved && !is_explicit && !is_fuel {
            let first_set_class = self.db.with_snapshot(|snap| {
                if scope_info.is_some() {
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
                } else {
                    evaluate_hand_written_outside_first_set_in_snap(
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
                }
            });
            if let Some(class_key) = first_set_class {
                let rpc_method = match scope_info {
                    Some((_, rpc_opt)) => rpc_opt
                        .clone()
                        .unwrap_or_else(|| crate::flight_recorder::UNKNOWN_RPC_LABEL.to_string()),
                    None => match origin_scope {
                        CompareOriginScope::Tick => "<tick>".to_string(),
                        _ => "<outside_rpc>".to_string(),
                    },
                };
                self.projected_item_switches.record_helper_without_token(
                    origin_scope,
                    true,
                    &class_key,
                    &rpc_method,
                    helper,
                    char_id,
                    item_id,
                    kind,
                    actual_dict,
                );
                return true;
            }
        }
        let reason = self.db.with_snapshot(|snap| {
            classify_hand_written_not_covered_reason_in_snap(
                scope_covered,
                pilot_moved,
                is_explicit,
                is_fuel,
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
        self.projected_item_switches.record_not_covered(reason);
        false
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
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return false;
        }
        let scope_info = current_dispatch_staging_scope_info(
            Some(&*self.pending_notifications),
            Some(&self.notify_tx),
        );
        let in_dispatch = scope_info.is_some();
        let active_in_scope = cfg.is_kind_enabled_in_scope(in_dispatch, kind);
        if !active_in_scope && !(!in_dispatch && cfg.is_kind_enabled(kind)) {
            return false;
        }
        if !self.has_online_character_recipient(char_id) {
            self.record_offline_or_non_char_not_covered(
                char_id,
                kind,
                fp,
                actual_dict,
                pre_loc_flag,
            );
            return false;
        }
        if fp.owner_id != char_id {
            self.projected_item_switches
                .record_not_covered(NotCoveredReason::NotCharacterOwned);
            self.projected_item_switches.record_non_first_set_emitted();
            return false;
        }
        let origin_scope = current_origin_scope_telemetry(self, in_dispatch);
        if !active_in_scope {
            self.record_shadow_outside_helper(
                origin_scope,
                char_id,
                item_id,
                kind,
                fp,
                tombstone_wildcard_deleted,
                actual_dict,
                pre_loc_flag,
            );
            self.projected_item_switches
                .record_not_covered(NotCoveredReason::NotInAnRpc);
            self.projected_item_switches.record_non_first_set_emitted();
            return false;
        }
        let scope_id = scope_info.as_ref().map(|(id, _)| *id);
        match self.projected_item_switches.try_check_in_scope(
            scope_id,
            origin_scope,
            char_id,
            item_id,
            kind,
            fp,
            tombstone_wildcard_deleted,
            helper,
            actual_dict,
        ) {
            DedupeLookupOutcome::Replaced | DedupeLookupOutcome::Superseded => return true,
            DedupeLookupOutcome::NotSet => {}
        }
        if !self.record_unmatched_first_set_or_check(
            scope_info.as_ref(),
            origin_scope,
            cfg.char_same_scene_item_outside_rpc,
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
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::NotCharacterOwned);
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
                    self.projected_item_switches
                        .record_not_covered(NotCoveredReason::NotCharacterOwned);
                    self.projected_item_switches.record_non_first_set_emitted();
                }
                return false;
            }
        };
        let Some((item_id, fp)) = ItemWireFingerprint::from_pyrep_row(row) else {
            if self.has_online_character_recipient(char_id) {
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::NotCharacterOwned);
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
        let scope_info = current_dispatch_staging_scope_info(
            Some(&*self.pending_notifications),
            Some(&self.notify_tx),
        );
        let in_dispatch = scope_info.is_some();
        if recipients.len() > 1 {
            if has_active_switch_for_item_event(cfg, in_dispatch, "OnMultipleItemChange", payload) {
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::NotCharacterOwned);
                self.projected_item_switches.record_non_first_set_emitted();
            }
            return false;
        }
        let char_id = recipients[0].0;
        let shared_dict = payload.get(1).and_then(CanonicalChangeDict::from_pyrep);
        let Some(PyRep::List(rows) | PyRep::Tuple(rows)) = payload.first_mut() else {
            self.projected_item_switches
                .record_not_covered(NotCoveredReason::NotCharacterOwned);
            self.projected_item_switches.record_non_first_set_emitted();
            return false;
        };
        if rows.is_empty() {
            return false;
        }
        let recipient_online = self.has_online_character_recipient(char_id);
        let origin_scope = current_origin_scope_telemetry(self, in_dispatch);
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
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::NotCharacterOwned);
                needs_non_first_set = true;
                continue;
            };
            let kind = op_kind_for_wire_fp(fp);
            let active_in_scope = cfg.is_kind_enabled_in_scope(in_dispatch, kind);
            if !active_in_scope && !(!in_dispatch && cfg.is_kind_enabled(kind)) {
                kept_rows.push(entry);
                continue;
            }
            if !recipient_online {
                kept_rows.push(entry);
                self.record_offline_or_non_char_not_covered(
                    char_id,
                    kind,
                    fp,
                    row_dict.as_ref(),
                    None,
                );
                continue;
            }
            if fp.owner_id != char_id {
                kept_rows.push(entry);
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::NotCharacterOwned);
                needs_non_first_set = true;
                continue;
            }
            if !active_in_scope {
                kept_rows.push(entry);
                self.record_shadow_outside_helper(
                    origin_scope,
                    char_id,
                    item_id,
                    kind,
                    fp,
                    kind == ProjectedItemOpKind::Deleted,
                    row_dict.as_ref(),
                    None,
                );
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::NotInAnRpc);
                needs_non_first_set = true;
                continue;
            }
            match self.projected_item_switches.try_check_in_scope(
                scope_id,
                origin_scope,
                char_id,
                item_id,
                kind,
                fp,
                kind == ProjectedItemOpKind::Deleted,
                helper,
                row_dict.as_ref(),
            ) {
                DedupeLookupOutcome::Replaced | DedupeLookupOutcome::Superseded => continue,
                DedupeLookupOutcome::NotSet => {}
            }
            kept_rows.push(entry);
            if !self.record_unmatched_first_set_or_check(
                scope_info.as_ref(),
                origin_scope,
                cfg.char_same_scene_item_outside_rpc,
                char_id,
                item_id,
                kind,
                fp,
                row_dict.as_ref(),
                None,
                helper,
            ) {
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
            self.projected_item_switches
                .record_not_covered(NotCoveredReason::NotCharacterOwned);
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
            self.projected_item_switches
                .record_not_covered(NotCoveredReason::NotCharacterOwned);
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
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return false;
        }
        let has_any_online_recipient = || {
            targets
                .iter()
                .any(|(t, _)| !self.resolve_notification_recipients(t).is_empty())
        };
        if old_item.owner_id != new_item.owner_id || targets.len() != 1 {
            if cfg.char_same_scene_item_mutation && has_any_online_recipient() {
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::NotCharacterOwned);
                self.projected_item_switches.record_non_first_set_emitted();
            }
            return false;
        }
        let NotificationTarget::Character(cid) = targets[0].0 else {
            if cfg.char_same_scene_item_mutation && has_any_online_recipient() {
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::NotCharacterOwned);
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

#[cfg(test)]
mod t3c2_tests {
    use super::*;
    use crate::router::cache_notifications::{
        flush_staged_dispatch_notifications, with_staged_dispatch_notifications,
    };
    use crate::router::delta_item_switch::{projected_item_switch_summary, NotCoveredReason};
    use crate::services::tactical_modules::db_ptr_key;
    use eve_db::{InventoryFlag, InventoryItemRecord};

    #[tokio::test]
    async fn unswitched_item_helpers_record_all_eight_not_covered_reasons_and_call_sites() {
        let ctx = ServerContext::new();
        let (pilot_id, offline_id, corp_id, station_id, other_station, ship_id) = (
            90_000_001_i64,
            90_000_002_i64,
            98_000_001_i64,
            60_003_760_i64,
            60_003_761_i64,
            993_100_010_i64,
        );
        let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
        ctx.db.acquire_online_lock(uid, 93_204, Some(pilot_id), true, 1_700_000_500).expect("lock");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&pilot_id) {
                c.station_id = Some(station_id);
                c.structure_id = None;
                c.active_ship_id = ship_id;
            }
            snap.corporations.entry(corp_id).or_default();
        });
        let mk = |item_id, owner_id, location_id, flag| InventoryItemRecord {
            item_id, type_id: 34, owner_id, location_id, flag, quantity: 10, version: 1,
            ..Default::default()
        };

        // 1. Offline owner + 2. Corp owner (outside RPC/offline too -> NotCharacterOwned wins priority).
        let offline_item = mk(993_100_021, offline_id, station_id, InventoryFlag::Hangar);
        let corp_item = mk(993_100_022, corp_id, station_id, InventoryFlag::Hangar);
        // 3. Online char item outside RPC -> NotInAnRpc.
        let rpc_miss_item = mk(993_100_023, pilot_id, station_id, InventoryFlag::Hangar);
        ctx.db.with_mut_snapshot(|snap| {
            for r in [&offline_item, &corp_item, &rpc_miss_item] {
                snap.items.insert(r.item_id, r.clone());
            }
        });
        ctx.notify_item_mutation(offline_id, None, Some(&offline_item));
        ctx.notify_item_mutation(pilot_id, None, Some(&corp_item));
        ctx.notify_item_mutation(pilot_id, None, Some(&rpc_miss_item));

        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            // 4. Explicit effect tx notification -> ExplicitEffect.
            let exp_item = mk(993_100_024, pilot_id, station_id, InventoryFlag::Hangar);
            ctx.db.tx("t3c2_exp", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(exp_item.item_id, exp_item.clone());
                let row = crate::router::delta_projection::build_inv_item_dbrow(&exp_item, &ctx.sde, "");
                tx.effects.notify_character(pilot_id, "OnItemChange", "charid", vec![row, PyRep::Dict(vec![])]);
                Ok(())
            }).expect("exp tx");
            ctx.notify_item_mutation(pilot_id, None, Some(&exp_item));

            // 5. Fuel reservation guard -> FuelReservation.
            let fuel_item = mk(993_100_025, pilot_id, station_id, InventoryFlag::Hangar);
            let _fuel_guard = ctx.active_fuel_reserves.begin_reserve(db_ptr_key(&ctx.db), [fuel_item.item_id]);
            ctx.db.tx("t3c2_fuel", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(fuel_item.item_id, fuel_item.clone());
                Ok(())
            }).expect("fuel tx");
            drop(_fuel_guard);
            ctx.notify_item_mutation(pilot_id, None, Some(&fuel_item));

            // 6. Location not docked hangar/active ship & 7. Flag not 4 or 5.
            let bad_loc = mk(993_100_026, pilot_id, other_station, InventoryFlag::Hangar);
            let bad_flag = mk(993_100_027, pilot_id, ship_id, InventoryFlag::HiSlot0);
            ctx.db.tx("t3c2_loc_flag", |tx| -> Result<(), eve_db::TxError> {
                tx.snap.items.insert(bad_loc.item_id, bad_loc.clone());
                tx.snap.items.insert(bad_flag.item_id, bad_flag.clone());
                Ok(())
            }).expect("loc/flag tx");
            ctx.notify_item_mutation(pilot_id, None, Some(&bad_loc));
            ctx.notify_item_mutation(pilot_id, None, Some(&bad_flag));

            // 8. Pilot moved in this dispatch commit -> PilotMovedInThisCommit.
            let moved_item = mk(993_100_028, pilot_id, other_station, InventoryFlag::Hangar);
            ctx.db.tx("t3c2_move", |tx| -> Result<(), eve_db::TxError> {
                if let Some(c) = tx.snap.characters.get_mut(&pilot_id) {
                    c.station_id = Some(other_station);
                }
                tx.snap.items.insert(moved_item.item_id, moved_item.clone());
                Ok(())
            }).expect("move tx");
            ctx.notify_item_mutation(pilot_id, None, Some(&moved_item));
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        }).await;

        let sw = projected_item_switch_summary(&ctx);
        for reason in [
            NotCoveredReason::OwnerOffline,
            NotCoveredReason::NotCharacterOwned,
            NotCoveredReason::NotInAnRpc,
            NotCoveredReason::ExplicitEffect,
            NotCoveredReason::FuelReservation,
            NotCoveredReason::LocationNotDockedHangarOrActiveShip,
            NotCoveredReason::FlagNot4Or5,
            NotCoveredReason::PilotMovedInThisCommit,
        ] {
            assert!(sw.not_covered_by_reason.get(reason.as_str()).copied().unwrap_or(0) >= 1, "missing reason {:?} in {:?}", reason, sw.not_covered_by_reason);
        }
        assert!(!sw.not_covered_by_call_site_reason.is_empty());
        let md = sw.format_markdown_report();
        assert!(md.contains("#### Not Covered by Reason"), "{md}");
        assert!(md.contains("#### Not Covered by Call Site & Reason"), "{md}");
    }
}
