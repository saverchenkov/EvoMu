//! `ServerContext` item-switch dedupe, suppression, and telemetry helpers (`#931` Track 3B / 3C-1).

#![forbid(unsafe_code)]

use eve_db::{Effects, InventoryItemRecord};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;

use super::cache_notifications::{current_dispatch_staging_scope_info, NotificationTarget};
use super::delta_item_compare::{current_origin_scope_telemetry, CompareOriginScope};
use super::delta_item_corp::is_corp_item_eligible_coords;
use super::delta_item_recipients::classify_item_holder_kind_in_snap;
use super::delta_item_switch::{
    classify_hand_written_not_covered_reason_in_snap,
    classify_hand_written_not_covered_reason_with_fitted_in_snap, classify_item_row_kind_key,
    evaluate_hand_written_covered_class_in_snap,
    evaluate_hand_written_outside_first_set_in_snap, CanonicalChangeDict, DedupeLookupOutcome,
    ItemWireFingerprint, NotCoveredReason, ProjectedItemOpKind, ProjectedItemSwitchConfig,
};
use super::dispatch_active_char::active_dispatch_char;
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
        cfg.is_kind_enabled_in_scope(in_dispatch, kind)
            || (!in_dispatch
                && (cfg.is_kind_enabled(kind) || cfg.char_all_items || cfg.corp_all_items))
            || cfg.is_fitted_enabled_in_scope(in_dispatch)
            || cfg.is_char_all_items_enabled_in_scope(in_dispatch)
            || cfg.is_corp_all_items_enabled_in_scope(in_dispatch)
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

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    fn record_offline_or_non_char_not_covered(
        &self,
        char_id: i64,
        char_all_enabled: bool,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        actual_dict: Option<&CanonicalChangeDict>,
        pre_loc_flag: Option<(i64, i64)>,
    ) {
        let reason = self.db.with_snapshot(|snap| {
            classify_hand_written_not_covered_reason_in_snap(
                char_all_enabled,
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

    #[inline]
    fn has_corp_owner_match_for_recipient(
        &self,
        char_id: i64,
        fp: ItemWireFingerprint,
        actual_dict: Option<&CanonicalChangeDict>,
        pre_loc_flag: Option<(i64, i64)>,
    ) -> bool {
        self.db.with_snapshot(|snap| {
            let Some(ch) = snap.characters.get(&char_id) else {
                return false;
            };
            let my_corp = ch.corporation_id;
            if my_corp <= 0 || snap.characters.contains_key(&my_corp) {
                return false;
            }
            let after_matches = fp.owner_id == my_corp
                && is_corp_item_eligible_coords(my_corp, fp.flag_id, fp.type_id, snap, &self.sde);
            if after_matches {
                return true;
            }
            let before_owner = actual_dict
                .and_then(|d| d.col_i64(2))
                .unwrap_or(fp.owner_id);
            if before_owner != my_corp {
                return false;
            }
            let before_flag = pre_loc_flag
                .map(|(_, f)| f)
                .or_else(|| actual_dict.and_then(|d| d.col_i64(4)))
                .unwrap_or(fp.flag_id);
            is_corp_item_eligible_coords(my_corp, before_flag, fp.type_id, snap, &self.sde)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn record_shadow_outside_helper(
        &self,
        origin_scope: CompareOriginScope,
        char_all_enabled: bool,
        is_corp_target: bool,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        actual_dict: Option<&CanonicalChangeDict>,
        pre_loc_flag: Option<(i64, i64)>,
    ) {
        let owner_ok = if is_corp_target {
            self.projected_item_switches
                .has_fact_in_scope(None, char_id, item_id, true)
                || self.has_corp_owner_match_for_recipient(char_id, fp, actual_dict, pre_loc_flag)
        } else {
            fp.owner_id == char_id
                || (char_all_enabled
                    && (actual_dict.is_some_and(|d| d.col_i64(2) == Some(char_id))
                        || self
                            .projected_item_switches
                            .has_fact_in_scope(None, char_id, item_id, false)))
        };
        if !self.has_online_character_recipient(char_id) || !owner_ok {
            return;
        }
        let outcome = self.projected_item_switches.check_shadow_outside_outcome(
            origin_scope,
            char_id,
            item_id,
            is_corp_target,
            kind,
            fp,
            tombstone_wildcard_deleted,
        );
        if outcome == DedupeLookupOutcome::NotSet {
            let (is_explicit, is_fuel) = self
                .projected_item_switches
                .is_outside_explicit_or_fuel_skipped(false, char_id, item_id);
            if !is_explicit && !is_fuel {
                let is_first_set = if is_corp_target {
                    true
                } else {
                    self.db.with_snapshot(|snap| {
                        if char_all_enabled {
                            evaluate_hand_written_covered_class_in_snap(
                                false,
                                false,
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
                            .is_some()
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
                            .is_some()
                        }
                    })
                };
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
        cfg: ProjectedItemSwitchConfig,
        is_corp_target: bool,
        char_id: i64,
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        actual_dict: Option<&CanonicalChangeDict>,
        pre_loc_flag: Option<(i64, i64)>,
        helper: &str,
    ) -> bool {
        let in_dispatch = scope_info.is_some();
        let outside_rpc_enabled = cfg.char_same_scene_item_outside_rpc;
        let sid_opt = scope_info.map(|(sid, _)| *sid);
        let pilot_moved = sid_opt.is_some_and(|sid| {
            let active_ship_id = self.db.with_snapshot(|snap| {
                snap.characters
                    .get(&char_id)
                    .map(|ch| ch.active_ship_id)
                    .unwrap_or(0)
            });
            self.projected_item_switches
                .is_item_transition_suppressed_in_scope(sid, char_id, item_id, active_ship_id)
        });
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
        if is_corp_target {
            let corp_covered = cfg.is_corp_all_items_enabled_in_scope(in_dispatch);
            let reason = if pilot_moved {
                Some(NotCoveredReason::PilotMovedInThisCommit)
            } else if !corp_covered {
                Some(NotCoveredReason::NotInAnRpc)
            } else if is_explicit {
                Some(NotCoveredReason::ExplicitEffect)
            } else if is_fuel {
                Some(NotCoveredReason::FuelReservation)
            } else {
                None
            };
            if let Some(reason) = reason {
                self.projected_item_switches.record_not_covered(reason);
                return false;
            }
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
                "corp",
                &rpc_method,
                helper,
                char_id,
                item_id,
                kind,
                actual_dict,
            );
            return true;
        }
        let direct_covered = cfg.is_kind_enabled_in_scope(in_dispatch, kind);
        let fitted_covered = cfg.is_fitted_enabled_in_scope(in_dispatch);
        let char_all_covered = cfg.is_char_all_items_enabled_in_scope(in_dispatch);
        let scope_covered = direct_covered || fitted_covered || char_all_covered;
        if scope_covered && !pilot_moved && !is_explicit && !is_fuel {
            let covered_class = self.db.with_snapshot(|snap| {
                evaluate_hand_written_covered_class_in_snap(
                    direct_covered,
                    fitted_covered,
                    char_all_covered,
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
            if let Some(class_key) = covered_class {
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
            classify_hand_written_not_covered_reason_with_fitted_in_snap(
                fitted_covered,
                char_all_covered,
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

    fn resolve_hand_sent_rpc_and_actor(
        &self,
        scope_info: Option<&(u64, Option<String>)>,
        origin_scope: CompareOriginScope,
        char_id: i64,
    ) -> (String, bool) {
        let rpc_method = match scope_info {
            Some((_, rpc_opt)) => rpc_opt
                .clone()
                .unwrap_or_else(|| crate::flight_recorder::UNKNOWN_RPC_LABEL.to_string()),
            None => match origin_scope {
                CompareOriginScope::Tick => "<tick>".to_string(),
                _ => "<outside_rpc>".to_string(),
            },
        };
        let is_actor = scope_info.is_some() && active_dispatch_char() == Some(char_id);
        (rpc_method, is_actor)
    }

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    fn record_hand_written_item_sent_for_fp(
        &self,
        scope_info: Option<&(u64, Option<String>)>,
        origin_scope: CompareOriginScope,
        char_id: i64,
        item_id: i64,
        fp: ItemWireFingerprint,
        actual_dict: Option<&CanonicalChangeDict>,
        pre_loc_flag: Option<(i64, i64)>,
    ) {
        let (rpc_method, is_actor) =
            self.resolve_hand_sent_rpc_and_actor(scope_info, origin_scope, char_id);
        let before_loc = pre_loc_flag
            .map(|(l, _)| l)
            .or_else(|| actual_dict.and_then(|d| d.col_i64(3)));
        let before_flag = pre_loc_flag
            .map(|(_, f)| f)
            .or_else(|| actual_dict.and_then(|d| d.col_i64(4)));
        let (class_key, holder_kind) = self.db.with_snapshot(|snap| {
            let empty_changes = eve_db::ChangeSet::default();
            let empty_item_map = std::collections::HashMap::new();
            let class_key = classify_item_row_kind_key(
                before_loc,
                before_flag,
                fp.owner_id,
                fp.location_id,
                fp.flag_id,
                fp.wire_qty,
                snap,
                &empty_changes,
                &empty_item_map,
                &self.sde,
            );
            let holder_kind = classify_item_holder_kind_in_snap(
                item_id,
                fp.location_id,
                before_loc,
                snap,
                &self.sde,
            )
            .as_str();
            (class_key, holder_kind)
        });
        self.projected_item_switches.record_hand_sent(
            &rpc_method,
            origin_scope,
            &class_key,
            holder_kind,
            is_actor,
        );
    }

    #[track_caller]
    fn record_hand_written_item_sent_unparseable(
        &self,
        scope_info: Option<&(u64, Option<String>)>,
        origin_scope: CompareOriginScope,
        char_id: i64,
    ) {
        let (rpc_method, is_actor) =
            self.resolve_hand_sent_rpc_and_actor(scope_info, origin_scope, char_id);
        self.projected_item_switches.record_hand_sent(
            &rpc_method,
            origin_scope,
            "unparseable",
            "none",
            is_actor,
        );
    }

    /// Evaluates one candidate recipient against the switch ledger, returning
    /// `(consumed, unmatched_non_first_set)`.
    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    fn consume_or_record_switch_inner(
        &self,
        char_id: i64,
        item_id: i64,
        is_corp_target: bool,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        helper: &'static str,
        actual_dict: Option<&CanonicalChangeDict>,
        pre_loc_flag: Option<(i64, i64)>,
    ) -> (bool, bool) {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return (false, false);
        }
        let scope_info = current_dispatch_staging_scope_info(
            Some(&*self.pending_notifications),
            Some(&self.notify_tx),
        );
        let in_dispatch = scope_info.is_some();
        let scope_id = scope_info.as_ref().map(|(id, _)| *id);
        let origin_scope = current_origin_scope_telemetry(self, in_dispatch);
        let online_recipient = self.has_online_character_recipient(char_id);
        let live_ledger_active = in_dispatch || cfg.char_same_scene_item_outside_rpc;

        // Section 4D: suppress a hand-written call for `(char_id, item_id)` whenever the live
        // ledger holds a fact of either id type (`is_corp_target` first, then `!is_corp_target`).
        if live_ledger_active && online_recipient {
            for check_corp in [is_corp_target, !is_corp_target] {
                match self.projected_item_switches.try_check_in_scope(
                    scope_id,
                    origin_scope,
                    char_id,
                    item_id,
                    check_corp,
                    kind,
                    fp,
                    tombstone_wildcard_deleted,
                    helper,
                    actual_dict,
                ) {
                    DedupeLookupOutcome::Replaced | DedupeLookupOutcome::Superseded => {
                        return (true, false);
                    }
                    DedupeLookupOutcome::NotSet => {}
                }
            }
        }

        let (sent, non_first_set) = if is_corp_target {
            let active_in_scope = cfg.is_corp_all_items_enabled_in_scope(in_dispatch);
            let shadow_active = !in_dispatch && cfg.corp_all_items;
            if !active_in_scope && !shadow_active {
                let kind_switch_active = cfg.char_all_items
                    || cfg.is_kind_enabled_in_scope(in_dispatch, kind)
                    || cfg.is_fitted_enabled_in_scope(in_dispatch)
                    || (!in_dispatch && cfg.is_kind_enabled(kind));
                if kind_switch_active && online_recipient {
                    self.projected_item_switches
                        .record_not_covered(NotCoveredReason::NotCharacterOwned);
                    (true, true)
                } else {
                    (online_recipient, false)
                }
            } else if !online_recipient {
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::OwnerOffline);
                (false, false)
            } else {
                let corp_owner_ok = self
                    .projected_item_switches
                    .has_fact_in_scope(scope_id, char_id, item_id, true)
                    || self.has_corp_owner_match_for_recipient(
                        char_id,
                        fp,
                        actual_dict,
                        pre_loc_flag,
                    );
                if !corp_owner_ok {
                    self.projected_item_switches
                        .record_not_covered(NotCoveredReason::NotCharacterOwned);
                    (true, true)
                } else if !active_in_scope {
                    self.record_shadow_outside_helper(
                        origin_scope,
                        cfg.char_all_items,
                        true,
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
                    (true, true)
                } else {
                    let unmatched_covered = self.record_unmatched_first_set_or_check(
                        scope_info.as_ref(),
                        origin_scope,
                        cfg,
                        true,
                        char_id,
                        item_id,
                        kind,
                        fp,
                        actual_dict,
                        pre_loc_flag,
                        helper,
                    );
                    (true, !unmatched_covered)
                }
            }
        } else {
            let active_in_scope = cfg.is_kind_enabled_in_scope(in_dispatch, kind)
                || cfg.is_fitted_enabled_in_scope(in_dispatch)
                || cfg.is_char_all_items_enabled_in_scope(in_dispatch);
            if !active_in_scope
                && !(!in_dispatch && (cfg.is_kind_enabled(kind) || cfg.char_all_items))
            {
                (online_recipient, false)
            } else if !online_recipient {
                self.record_offline_or_non_char_not_covered(
                    char_id,
                    cfg.char_all_items,
                    kind,
                    fp,
                    actual_dict,
                    pre_loc_flag,
                );
                (false, false)
            } else {
                let char_all_outgoing_transfer = cfg.char_all_items
                    && (actual_dict.is_some_and(|d| d.col_i64(2) == Some(char_id))
                        || self
                            .projected_item_switches
                            .has_fact_in_scope(scope_id, char_id, item_id, false));
                if fp.owner_id != char_id && !char_all_outgoing_transfer {
                    self.projected_item_switches
                        .record_not_covered(NotCoveredReason::NotCharacterOwned);
                    (true, true)
                } else if !active_in_scope {
                    self.record_shadow_outside_helper(
                        origin_scope,
                        cfg.char_all_items,
                        false,
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
                    (true, true)
                } else {
                    let unmatched_covered = self.record_unmatched_first_set_or_check(
                        scope_info.as_ref(),
                        origin_scope,
                        cfg,
                        false,
                        char_id,
                        item_id,
                        kind,
                        fp,
                        actual_dict,
                        pre_loc_flag,
                        helper,
                    );
                    (true, !unmatched_covered)
                }
            }
        };

        if sent {
            self.record_hand_written_item_sent_for_fp(
                scope_info.as_ref(),
                origin_scope,
                char_id,
                item_id,
                fp,
                actual_dict,
                pre_loc_flag,
            );
        }
        (false, non_first_set)
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
        let (consumed, non_first_set) = self.consume_or_record_switch_inner(
            char_id,
            item_id,
            false,
            kind,
            fp,
            tombstone_wildcard_deleted,
            helper,
            actual_dict,
            pre_loc_flag,
        );
        if non_first_set {
            self.projected_item_switches.record_non_first_set_emitted();
        }
        consumed
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
                let scope_info = current_dispatch_staging_scope_info(
                    Some(&*self.pending_notifications),
                    Some(&self.notify_tx),
                );
                let origin_scope = current_origin_scope_telemetry(self, scope_info.is_some());
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::NotCharacterOwned);
                self.projected_item_switches.record_non_first_set_emitted();
                self.record_hand_written_item_sent_unparseable(
                    scope_info.as_ref(),
                    origin_scope,
                    char_id,
                );
            }
            return false;
        };
        let dict = CanonicalChangeDict::from_entries(changes);
        self.consume_projected_after_wire_fp(char_id, item_id, fp, helper, Some(&dict))
    }

    #[cfg(test)]
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
    pub(crate) fn filter_projected_on_item_change_recipients(
        &self,
        recipients: &mut Vec<(i64, Option<u64>)>,
        id_type: &str,
        payload: &[PyRep],
        helper: &'static str,
    ) -> bool {
        let cfg = self.projected_item_switches.config();
        if recipients.is_empty() || !cfg.is_any_enabled() {
            return false;
        }
        let is_corp = id_type == "corpid";
        if (!is_corp || !cfg.corp_all_items) && recipients.len() > 1 {
            let scope_info = current_dispatch_staging_scope_info(
                Some(&*self.pending_notifications),
                Some(&self.notify_tx),
            );
            let in_dispatch = scope_info.is_some();
            let origin_scope = current_origin_scope_telemetry(self, in_dispatch);
            if super::delta_item_switch::has_active_switch_for_item_event(
                cfg,
                in_dispatch,
                "OnItemChange",
                payload,
            ) {
                if is_corp {
                    self.projected_item_switches
                        .record_not_covered(NotCoveredReason::NotCharacterOwned);
                }
                self.projected_item_switches.record_non_first_set_emitted();
            }
            let parsed_payload = match payload {
                [row, dict_rep, ..] => ItemWireFingerprint::from_pyrep_row(row)
                    .map(|(iid, fp)| (iid, fp, CanonicalChangeDict::from_pyrep(dict_rep))),
                [row] => ItemWireFingerprint::from_pyrep_row(row).map(|(iid, fp)| (iid, fp, None)),
                [] => None,
            };
            for &(cid, _) in recipients.iter() {
                if let Some((item_id, fp, ref actual_dict)) = parsed_payload {
                    self.record_hand_written_item_sent_for_fp(
                        scope_info.as_ref(),
                        origin_scope,
                        cid,
                        item_id,
                        fp,
                        actual_dict.as_ref(),
                        None,
                    );
                } else {
                    self.record_hand_written_item_sent_unparseable(
                        scope_info.as_ref(),
                        origin_scope,
                        cid,
                    );
                }
            }
            return false;
        }
        let (row, actual_dict) = match payload {
            [row, dict_rep, ..] => (row, CanonicalChangeDict::from_pyrep(dict_rep)),
            [row] => (row, None),
            [] => {
                let scope_info = current_dispatch_staging_scope_info(
                    Some(&*self.pending_notifications),
                    Some(&self.notify_tx),
                );
                let origin_scope = current_origin_scope_telemetry(self, scope_info.is_some());
                if recipients.len() == 1 && self.has_online_character_recipient(recipients[0].0) {
                    self.projected_item_switches
                        .record_not_covered(NotCoveredReason::NotCharacterOwned);
                    self.projected_item_switches.record_non_first_set_emitted();
                }
                for &(cid, _) in recipients.iter() {
                    if self.has_online_character_recipient(cid) {
                        self.record_hand_written_item_sent_unparseable(
                            scope_info.as_ref(),
                            origin_scope,
                            cid,
                        );
                    }
                }
                return false;
            }
        };
        let Some((item_id, fp)) = ItemWireFingerprint::from_pyrep_row(row) else {
            let scope_info = current_dispatch_staging_scope_info(
                Some(&*self.pending_notifications),
                Some(&self.notify_tx),
            );
            let origin_scope = current_origin_scope_telemetry(self, scope_info.is_some());
            if recipients.len() == 1 && self.has_online_character_recipient(recipients[0].0) {
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::NotCharacterOwned);
                self.projected_item_switches.record_non_first_set_emitted();
            }
            for &(cid, _) in recipients.iter() {
                if self.has_online_character_recipient(cid) {
                    self.record_hand_written_item_sent_unparseable(
                        scope_info.as_ref(),
                        origin_scope,
                        cid,
                    );
                }
            }
            return false;
        };
        let kind = op_kind_for_wire_fp(fp);
        let tombstone_wildcard_deleted = kind == ProjectedItemOpKind::Deleted;
        let mut any_non_first_set = false;
        recipients.retain(|&(cid, _)| {
            let (consumed, nfs) = self.consume_or_record_switch_inner(
                cid,
                item_id,
                is_corp,
                kind,
                fp,
                tombstone_wildcard_deleted,
                helper,
                actual_dict.as_ref(),
                None,
            );
            if nfs {
                any_non_first_set = true;
            }
            !consumed
        });
        if any_non_first_set {
            self.projected_item_switches.record_non_first_set_emitted();
        }
        recipients.is_empty()
    }

    /// Strips already commit-projected rows and/or already-notified recipients from
    /// `OnMultipleItemChange` inside `notify_inner`, returning [`MultiItemSwitchFilterOutcome::Drop`]
    /// when empty, [`MultiItemSwitchFilterOutcome::SplitPerRecipient`] when multi-row corp
    /// consumption is ragged across surviving recipients (`BF-01`), or
    /// [`MultiItemSwitchFilterOutcome::Keep`] for shared delivery (`#931` `BF-931-3C1-03` &
    /// Track 5C).
    #[track_caller]
    pub(crate) fn filter_switched_on_multiple_item_change_args(
        &self,
        recipients: &mut Vec<(i64, Option<u64>)>,
        id_type: &str,
        payload: &mut Vec<PyRep>,
        helper: &'static str,
    ) -> MultiItemSwitchFilterOutcome {
        let cfg = self.projected_item_switches.config();
        if recipients.is_empty() || !cfg.is_any_enabled() {
            return MultiItemSwitchFilterOutcome::Keep;
        }
        let scope_info = current_dispatch_staging_scope_info(
            Some(&*self.pending_notifications),
            Some(&self.notify_tx),
        );
        let in_dispatch = scope_info.is_some();
        let origin_scope = current_origin_scope_telemetry(self, in_dispatch);
        let is_corp = id_type == "corpid";
        if (!is_corp || !cfg.corp_all_items) && recipients.len() > 1 {
            if has_active_switch_for_item_event(cfg, in_dispatch, "OnMultipleItemChange", payload) {
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::NotCharacterOwned);
                self.projected_item_switches.record_non_first_set_emitted();
            }
            let shared_dict = payload.get(1).and_then(CanonicalChangeDict::from_pyrep);
            match payload.first() {
                Some(PyRep::List(entries) | PyRep::Tuple(entries)) => {
                    for entry in entries {
                        let (row_ref, row_dict) = unwrap_multi_row_entry(entry, shared_dict.as_ref());
                        let parsed = ItemWireFingerprint::from_pyrep_row(row_ref);
                        for &(cid, _) in recipients.iter() {
                            if let Some((item_id, fp)) = parsed {
                                self.record_hand_written_item_sent_for_fp(
                                    scope_info.as_ref(),
                                    origin_scope,
                                    cid,
                                    item_id,
                                    fp,
                                    row_dict.as_ref(),
                                    None,
                                );
                            } else {
                                self.record_hand_written_item_sent_unparseable(
                                    scope_info.as_ref(),
                                    origin_scope,
                                    cid,
                                );
                            }
                        }
                    }
                }
                _ => {
                    for &(cid, _) in recipients.iter() {
                        self.record_hand_written_item_sent_unparseable(
                            scope_info.as_ref(),
                            origin_scope,
                            cid,
                        );
                    }
                }
            }
            return MultiItemSwitchFilterOutcome::Keep;
        }
        let shared_dict = payload.get(1).and_then(CanonicalChangeDict::from_pyrep);
        let is_tuple_wrap = matches!(payload.first(), Some(PyRep::Tuple(_)));
        let Some(PyRep::List(rows) | PyRep::Tuple(rows)) = payload.first_mut() else {
            self.projected_item_switches
                .record_not_covered(NotCoveredReason::NotCharacterOwned);
            self.projected_item_switches.record_non_first_set_emitted();
            for &(cid, _) in recipients.iter() {
                self.record_hand_written_item_sent_unparseable(
                    scope_info.as_ref(),
                    origin_scope,
                    cid,
                );
            }
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
            let mut nfs_matrix = vec![vec![false; rec_count]; row_count];
            let mut row_unparseable = vec![false; row_count];
            let mut has_unparseable_row = false;
            for (r_idx, entry) in old_rows.iter().enumerate() {
                let (parsed, row_dict) = {
                    let (row_ref, row_dict) = unwrap_multi_row_entry(entry, shared_dict.as_ref());
                    (ItemWireFingerprint::from_pyrep_row(row_ref), row_dict)
                };
                let Some((item_id, fp)) = parsed else {
                    has_unparseable_row = true;
                    row_unparseable[r_idx] = true;
                    self.projected_item_switches
                        .record_not_covered(NotCoveredReason::NotCharacterOwned);
                    for &(cid, _) in recipients.iter() {
                        self.record_hand_written_item_sent_unparseable(
                            scope_info.as_ref(),
                            origin_scope,
                            cid,
                        );
                    }
                    continue;
                };
                let kind = op_kind_for_wire_fp(fp);
                let tombstone_wildcard_deleted = kind == ProjectedItemOpKind::Deleted;
                for (m_idx, &(cid, _)) in recipients.iter().enumerate() {
                    let (consumed, nfs) = self.consume_or_record_switch_inner(
                        cid,
                        item_id,
                        true,
                        kind,
                        fp,
                        tombstone_wildcard_deleted,
                        helper,
                        row_dict.as_ref(),
                        None,
                    );
                    consumed_matrix[r_idx][m_idx] = consumed;
                    nfs_matrix[r_idx][m_idx] = nfs;
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
            let needs_non_first_set = has_unparseable_row
                || kept_row_orig_indices.iter().any(|&r_idx| {
                    surviving_indices
                        .iter()
                        .any(|&m_idx| !consumed_matrix[r_idx][m_idx] && nfs_matrix[r_idx][m_idx])
                });
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
                if needs_non_first_set {
                    self.projected_item_switches.record_non_first_set_emitted();
                }
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
            if needs_non_first_set {
                self.projected_item_switches.record_non_first_set_emitted();
            }
            return MultiItemSwitchFilterOutcome::Keep;
        }

        let char_id = recipients[0].0;
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
                self.record_hand_written_item_sent_unparseable(
                    scope_info.as_ref(),
                    origin_scope,
                    char_id,
                );
                needs_non_first_set = true;
                continue;
            };
            let kind = op_kind_for_wire_fp(fp);
            let (consumed, nfs) = self.consume_or_record_switch_inner(
                char_id,
                item_id,
                is_corp,
                kind,
                fp,
                kind == ProjectedItemOpKind::Deleted,
                helper,
                row_dict.as_ref(),
                None,
            );
            if !consumed {
                kept_rows.push(entry);
                if nfs {
                    needs_non_first_set = true;
                }
            }
        }
        if kept_rows.is_empty() {
            return MultiItemSwitchFilterOutcome::Drop;
        }
        *rows = kept_rows;
        if needs_non_first_set {
            self.projected_item_switches.record_non_first_set_emitted();
        }
        MultiItemSwitchFilterOutcome::Keep
    }

    #[allow(clippy::too_many_arguments)]
    #[track_caller]
    fn filter_projected_target_list_for_item(
        &self,
        targets: &[(NotificationTarget, &'static str)],
        item_id: i64,
        kind: ProjectedItemOpKind,
        fp: ItemWireFingerprint,
        tombstone_wildcard_deleted: bool,
        dict: Option<&CanonicalChangeDict>,
        pre_loc_flag: Option<(i64, i64)>,
        helper: &'static str,
    ) -> Vec<(NotificationTarget, &'static str)> {
        let cfg = self.projected_item_switches.config();
        let mut kept = Vec::with_capacity(targets.len());
        let mut recorded_non_char = false;
        let mut any_non_first_set = false;
        for (target, id_type) in targets {
            match target {
                NotificationTarget::Character(cid) => {
                    let (consumed, nfs) = self.consume_or_record_switch_inner(
                        *cid,
                        item_id,
                        false,
                        kind,
                        fp,
                        tombstone_wildcard_deleted,
                        helper,
                        dict,
                        pre_loc_flag,
                    );
                    if nfs {
                        any_non_first_set = true;
                    }
                    if !consumed {
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
                        let (consumed, nfs) = self.consume_or_record_switch_inner(
                            *cid,
                            item_id,
                            true,
                            kind,
                            fp,
                            tombstone_wildcard_deleted,
                            helper,
                            dict,
                            pre_loc_flag,
                        );
                        if nfs {
                            any_non_first_set = true;
                        }
                        if consumed {
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
                _ => {
                    let online_recips = self.resolve_notification_recipients(target);
                    let switch_on = cfg.char_all_items
                        || if kind == ProjectedItemOpKind::Deleted {
                            cfg.char_same_scene_item_deleted
                        } else {
                            cfg.char_same_scene_item_mutation
                        };
                    if switch_on && !online_recips.is_empty() && !recorded_non_char {
                        self.projected_item_switches
                            .record_not_covered(NotCoveredReason::NotCharacterOwned);
                        any_non_first_set = true;
                        recorded_non_char = true;
                    }
                    if !online_recips.is_empty() {
                        let scope_info = current_dispatch_staging_scope_info(
                            Some(&*self.pending_notifications),
                            Some(&self.notify_tx),
                        );
                        let origin_scope =
                            current_origin_scope_telemetry(self, scope_info.is_some());
                        for (cid, _) in online_recips {
                            self.record_hand_written_item_sent_for_fp(
                                scope_info.as_ref(),
                                origin_scope,
                                cid,
                                item_id,
                                fp,
                                dict,
                                pre_loc_flag,
                            );
                        }
                    }
                    kept.push((target.clone(), *id_type));
                }
            }
        }
        if any_non_first_set {
            self.projected_item_switches.record_non_first_set_emitted();
        }
        kept
    }

    #[track_caller]
    pub(crate) fn filter_projected_item_change_targets(
        &self,
        targets: &[(NotificationTarget, &'static str)],
        item: &InventoryItemRecord,
        changes: &[(i32, PyRep)],
        helper: &'static str,
    ) -> Vec<(NotificationTarget, &'static str)> {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return targets.to_vec();
        }
        let dict = CanonicalChangeDict::from_entries(changes);
        let fp = ItemWireFingerprint::from_record(item);
        let kind = op_kind_for_wire_fp(fp);
        self.filter_projected_target_list_for_item(
            targets,
            item.item_id,
            kind,
            fp,
            kind == ProjectedItemOpKind::Deleted,
            Some(&dict),
            None,
            helper,
        )
    }

    #[track_caller]
    pub(crate) fn filter_projected_item_change_target(
        &self,
        target: NotificationTarget,
        item: &InventoryItemRecord,
        changes: &[(i32, PyRep)],
        helper: &'static str,
    ) -> Vec<NotificationTarget> {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return vec![target];
        }
        let dict = CanonicalChangeDict::from_entries(changes);
        let fp = ItemWireFingerprint::from_record(item);
        let kind = op_kind_for_wire_fp(fp);
        self.filter_projected_target_list_for_item(
            &[(target, "")],
            item.item_id,
            kind,
            fp,
            kind == ProjectedItemOpKind::Deleted,
            Some(&dict),
            None,
            helper,
        )
        .into_iter()
        .map(|(t, _)| t)
        .collect()
    }

    #[track_caller]
    pub(crate) fn filter_projected_item_deleted_targets(
        &self,
        targets: &[(NotificationTarget, &'static str)],
        old_item: &InventoryItemRecord,
        changes: &[(i32, PyRep)],
        helper: &'static str,
    ) -> Vec<(NotificationTarget, &'static str)> {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return targets.to_vec();
        }
        let dict = CanonicalChangeDict::from_entries(changes);
        let fp = ItemWireFingerprint::from_record(old_item);
        self.filter_projected_target_list_for_item(
            targets,
            old_item.item_id,
            ProjectedItemOpKind::Deleted,
            fp,
            false,
            Some(&dict),
            Some((old_item.location_id, old_item.flag as i64)),
            helper,
        )
    }

    #[cfg(test)]
    #[inline]
    #[track_caller]
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
    #[track_caller]
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

    #[track_caller]
    pub(crate) fn filter_projected_item_update_targets(
        &self,
        targets: &[(NotificationTarget, &'static str)],
        old_item: &InventoryItemRecord,
        new_item: &InventoryItemRecord,
        changes: &[(i32, PyRep)],
        helper: &'static str,
    ) -> Vec<(NotificationTarget, &'static str)> {
        let cfg = self.projected_item_switches.config();
        if !cfg.is_any_enabled() {
            return targets.to_vec();
        }
        let has_any_online_recipient = || {
            targets
                .iter()
                .any(|(t, _)| !self.resolve_notification_recipients(t).is_empty())
        };
        let dict = CanonicalChangeDict::from_entries(changes);
        let fp = ItemWireFingerprint::from_record(new_item);
        let pre_loc_flag = Some((old_item.location_id, old_item.flag as i64));
        if !cfg.char_all_items
            && !cfg.corp_all_items
            && (old_item.owner_id != new_item.owner_id || targets.len() != 1)
        {
            if cfg.char_same_scene_item_mutation && has_any_online_recipient() {
                self.projected_item_switches
                    .record_not_covered(NotCoveredReason::NotCharacterOwned);
                self.projected_item_switches.record_non_first_set_emitted();
            }
            let scope_info = current_dispatch_staging_scope_info(
                Some(&*self.pending_notifications),
                Some(&self.notify_tx),
            );
            let origin_scope = current_origin_scope_telemetry(self, scope_info.is_some());
            for (target, _) in targets {
                for (cid, _) in self.resolve_notification_recipients(target) {
                    self.record_hand_written_item_sent_for_fp(
                        scope_info.as_ref(),
                        origin_scope,
                        cid,
                        new_item.item_id,
                        fp,
                        Some(&dict),
                        pre_loc_flag,
                    );
                }
            }
            return targets.to_vec();
        }
        self.filter_projected_target_list_for_item(
            targets,
            new_item.item_id,
            ProjectedItemOpKind::CreatedOrUpdated,
            fp,
            false,
            Some(&dict),
            pre_loc_flag,
            helper,
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
        let scope_info = current_dispatch_staging_scope_info(
            Some(&*self.pending_notifications),
            Some(&self.notify_tx),
        );
        let in_dispatch = scope_info.is_some();
        let char_active = cfg.is_char_all_items_enabled_in_scope(in_dispatch);
        let corp_active = cfg.is_corp_all_items_enabled_in_scope(in_dispatch);
        if !char_active && !corp_active {
            return false;
        }
        let Some(PyRep::Integer(owner_id)) = payload.first() else {
            return false;
        };
        let scope_id = scope_info.map(|(id, _)| id);
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
        ctx.set_projected_item_switches(
            ProjectedItemSwitchConfig::enabled()
                .with_outside_rpc(false)
                .with_char_all_items(false),
        );
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

            // 6. Location not docked hangar/active ship & 7. Flag not 4 or 5 (and not Class `"fitted"`).
            let bad_loc = mk(993_100_026, pilot_id, other_station, InventoryFlag::Hangar);
            let bad_flag = mk(993_100_027, pilot_id, ship_id, InventoryFlag::DroneBay);
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
