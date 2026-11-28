//! Corporation & Upwell structure item / blueprint / `OnIndustryMaterials` projection helpers
//! and commit-time notification materialization (`#931` Track 5C Step 1).
//!
//! Extracted from [`super::delta_item_switch`] to keep both modules cohesive and under the
//! megafile cap while adding lock-free `corp_all_items` qualification, recipient resolution,
//! and `CorpActor::resolve_for_corp`-gated `OnIndustryMaterials` delivery.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, HashMap};

use eve_db::{
    ChangeSet, CharacterRecord, InventoryFlag, InventoryItemRecord, RowChange, WorldStateSnapshot,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;

use super::cache_notifications::{
    assemble_pending_notification, is_corp_hangar_flag, PendingMachoNotification,
};
use super::delta_item_recipients::{append_extended_rules_and_dedup, SwitchedItemRecipient};
use super::delta_item_switch::{
    is_character_scene_or_ship_transition, online_non_transitioned_owner, ItemBeforeAfter,
};
use super::delta_projection::{is_blueprint_type, OnlineCharacterLookup};
use crate::services::notify_emitters_character_services::{
    collect_available_monitor_materials, load_industry_material_monitors,
    IndustryMaterialMonitorSub,
};

pub(crate) struct SwitchedProjectedItemSpec {
    pub recipient_char_id: i64,
    pub recipient_user_id: u64,
    pub id_type: &'static str,
    pub args: Vec<PyRep>,
    pub industry_scopes: Vec<(i64, i64, i64, u32)>,
}

pub(crate) struct SwitchedProjectedBlueprintSpec {
    pub recipient_char_id: i64,
    pub recipient_user_id: u64,
    pub owner_id: i64,
}

#[derive(Default)]
pub(crate) struct SwitchedCommitBatch {
    pub item_specs: Vec<SwitchedProjectedItemSpec>,
    pub blueprint_specs: Vec<SwitchedProjectedBlueprintSpec>,
}

/// Result of materializing commit-time switched item notifications for one `WorldRepository`
/// transaction (`#931` Track 3C-1 / 5B / 5C).
pub(crate) struct SwitchedCommitNotifications {
    pub notifs: Vec<PendingMachoNotification>,
    pub covered_industry_monitors: HashMap<i64, (i64, i64, Vec<u32>)>,
}

#[inline]
pub(crate) fn is_npc_corp_owner_id(owner_id: i64, sde: &SdeRegistry) -> bool {
    owner_id > 0 && eve_db::is_npc_corporation(sde, owner_id)
}

/// Exact corporation & Upwell structure ownership qualification (`#931` Track 5C):
/// - `(a)` Materialized/player corporation (`snap_after.corporations`): all inventory flags qualify
///   (hangar divisions, `CorpDeliveries`, `Locked`, `OfficeImpound`, Upwell structure slots/fuel/deed,
///   corp ships, containers, corp blueprints).
/// - `(b)` NPC corporation (`eve_db::is_npc_corporation`, `sde.npc_corporations` or `1_000_000..2_000_000`):
///   restricted to corp hangar / locked / blueprint items so non-hangar NPC space objects never broadcast to
///   starter-corp pilots.
#[inline]
pub(crate) fn is_corp_item_eligible_coords(
    owner_id: i64,
    flag_id: i64,
    type_id: u32,
    snap_after: &WorldStateSnapshot,
    sde: &SdeRegistry,
) -> bool {
    if owner_id <= 0 || snap_after.characters.contains_key(&owner_id) {
        return false;
    }
    if snap_after.corporations.contains_key(&owner_id) {
        return true;
    }
    if !is_npc_corp_owner_id(owner_id, sde) {
        return false;
    }
    let hangar_flag = u16::try_from(flag_id)
        .ok()
        .map(InventoryFlag::from_u16)
        .is_some_and(is_corp_hangar_flag);
    hangar_flag || flag_id == InventoryFlag::Locked as i64 || is_blueprint_type(sde, type_id)
}

#[inline]
pub(crate) fn is_corp_item_eligible(
    item: &InventoryItemRecord,
    snap_after: &WorldStateSnapshot,
    sde: &SdeRegistry,
) -> bool {
    is_corp_item_eligible_coords(item.owner_id, item.flag as i64, item.type_id, snap_after, sde)
}

#[inline]
pub(crate) fn is_corp_blueprint_owner_eligible(
    owner_id: i64,
    snap_after: &WorldStateSnapshot,
    sde: &SdeRegistry,
) -> bool {
    owner_id > 0
        && !snap_after.characters.contains_key(&owner_id)
        && (snap_after.corporations.contains_key(&owner_id) || is_npc_corp_owner_id(owner_id, sde))
}

/// Lock-free online non-transitioned corporation member resolution from `snap_after` + `online`,
/// sorted and deduplicated by `character_id`.
pub(crate) fn resolve_online_non_transitioned_corp_recipients<'a>(
    corp_id: i64,
    snap_after: &'a WorldStateSnapshot,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
    online: &impl OnlineCharacterLookup,
) -> Vec<&'a CharacterRecord> {
    let mut members: Vec<&'a CharacterRecord> = snap_after
        .characters
        .values()
        .filter(|ch| {
            ch.corporation_id == corp_id
                && online.is_character_online(ch.character_id, ch.user_id)
                && !is_character_scene_or_ship_transition(ch.character_id, changed_chars)
        })
        .collect();
    members.sort_by_key(|ch| ch.character_id);
    members.dedup_by_key(|ch| ch.character_id);
    members
}

fn classify_char_item_owners<'a>(
    change: &'a RowChange<i64, InventoryItemRecord>,
    snap_after: &WorldStateSnapshot,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> [Option<i64>; 2] {
    let before_opt = change.before.as_deref();
    let after_opt = change.after.as_deref();
    let qualify_char = |cid: i64| {
        online_non_transitioned_owner(cid, cid, snap_after, changed_chars, online, sde)
            .map(|_| cid)
    };
    match (before_opt, after_opt) {
        (None, Some(new_item)) => [qualify_char(new_item.owner_id), None],
        (Some(old_item), None) => [qualify_char(old_item.owner_id), None],
        (Some(old_item), Some(new_item)) if old_item.owner_id == new_item.owner_id => {
            [qualify_char(new_item.owner_id), None]
        }
        (Some(old_item), Some(new_item)) => {
            [qualify_char(new_item.owner_id), qualify_char(old_item.owner_id)]
        }
        (None, None) => [None, None],
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_switched_item_recipients_for_change<'a>(
    acting_char_id: Option<i64>,
    change: &'a RowChange<i64, InventoryItemRecord>,
    snap_after: &'a WorldStateSnapshot,
    changes: &ChangeSet,
    item_changes_by_id: &HashMap<i64, ItemBeforeAfter<'a>>,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
    corp_recipients_cache: &mut HashMap<i64, Vec<&'a CharacterRecord>>,
) -> Vec<SwitchedItemRecipient<'a>> {
    let char_entries =
        classify_char_item_owners(change, snap_after, changed_chars, online, sde);

    let before_opt = change.before.as_deref();
    let after_opt = change.after.as_deref();
    if before_opt.is_none() && after_opt.is_none() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(6);

    let push_char = |out: &mut Vec<SwitchedItemRecipient<'a>>, cid: i64| {
        if let Some(ch) = snap_after.characters.get(&cid) {
            out.push(SwitchedItemRecipient {
                character: ch,
                id_type: "charid",
            });
        }
    };

    let push_corp = |out: &mut Vec<SwitchedItemRecipient<'a>>,
                     corp_recipients_cache: &mut HashMap<i64, Vec<&'a CharacterRecord>>,
                     corp_id: i64| {
        let members = corp_recipients_cache.entry(corp_id).or_insert_with(|| {
            resolve_online_non_transitioned_corp_recipients(
                corp_id,
                snap_after,
                changed_chars,
                online,
            )
        });
        for &ch in members.iter() {
            out.push(SwitchedItemRecipient {
                character: ch,
                id_type: "corpid",
            });
        }
    };

    match (before_opt, after_opt) {
        (None, Some(new_item)) => {
            if let Some(cid) = char_entries[0] {
                push_char(&mut out, cid);
            } else if is_corp_item_eligible(new_item, snap_after, sde) {
                push_corp(&mut out, corp_recipients_cache, new_item.owner_id);
            }
        }
        (Some(old_item), None) => {
            if let Some(cid) = char_entries[0] {
                push_char(&mut out, cid);
            } else if is_corp_item_eligible(old_item, snap_after, sde) {
                push_corp(&mut out, corp_recipients_cache, old_item.owner_id);
            }
        }
        (Some(old_item), Some(new_item)) if old_item.owner_id == new_item.owner_id => {
            if let Some(cid) = char_entries[0] {
                push_char(&mut out, cid);
            } else if is_corp_item_eligible(new_item, snap_after, sde)
                || is_corp_item_eligible(old_item, snap_after, sde)
            {
                push_corp(&mut out, corp_recipients_cache, new_item.owner_id);
            }
        }
        (Some(old_item), Some(new_item)) => {
            // Preserve exact new-owner-first then old-owner-second ordering across Char/Corp pairs.
            let mut char_iter = char_entries.into_iter().flatten();
            let first_char = char_iter.next();
            let second_char = char_iter.next();
            if let Some(cid) = first_char.filter(|&cid| cid == new_item.owner_id) {
                push_char(&mut out, cid);
            } else if is_corp_item_eligible(new_item, snap_after, sde) {
                push_corp(&mut out, corp_recipients_cache, new_item.owner_id);
            }

            let old_char_opt = second_char
                .filter(|&cid| cid == old_item.owner_id)
                .or_else(|| first_char.filter(|&cid| cid == old_item.owner_id));
            if let Some(cid) = old_char_opt {
                push_char(&mut out, cid);
            } else if is_corp_item_eligible(old_item, snap_after, sde) {
                push_corp(&mut out, corp_recipients_cache, old_item.owner_id);
            }
        }
        (None, None) => {}
    }

    append_extended_rules_and_dedup(
        acting_char_id,
        change,
        snap_after,
        changes,
        item_changes_by_id,
        changed_chars,
        online,
        sde,
        &mut out,
    );

    out
}

/// Lock-free parity check for corporation `OnIndustryMaterials` monitor access from `snap` + `sde` +
/// `now_unix` via [`eve_db::CorpActor::resolve_for_corp`] (`can_query_industry_monitor_div` /
/// `has_corp_monitor_access_in_snap` parity without acquiring any `ServerContext` locks).
pub(crate) fn has_corp_industry_monitor_access(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    online: &impl OnlineCharacterLookup,
    sub: &IndustryMaterialMonitorSub,
    now_unix: u64,
) -> bool {
    if sub.owner_id == sub.char_id {
        return true;
    }
    let Some(ch) = snap.characters.get(&sub.char_id) else {
        return false;
    };
    if !online.is_character_online(sub.char_id, ch.user_id) {
        return false;
    }
    let div_bits = if (116..=122).contains(&sub.flag_id) {
        let div_idx = (sub.flag_id as u16) - 116;
        (1u64 << (13 + div_idx)) | (1u64 << (20 + div_idx))
    } else {
        0
    };
    let required_roles = eve_db::corp_roles::ROLE_DIRECTOR
        | eve_db::corp_roles::ROLE_FACTORY_MANAGER
        | div_bits;
    let actor = eve_db::CorpActor::resolve_for_corp(
        snap,
        sde,
        sub.owner_id,
        sub.char_id,
        Some(sub.location_id).filter(|&id| id > 0),
        now_unix,
    );
    actor.has_any_role(required_roles)
}

/// Materializes commit-time switched notifications in exact V24 causal order
/// (`OnItemChange` -> coalesced matching personal/corp `OnIndustryMaterials` -> paired
/// `OnMultipleItemChange` -> deduplicated `OnBlueprintsUpdated`).
///
/// At most one coalesced `OnIndustryMaterials` notification is emitted per `monitor_id` per
/// commit, merging all covered material types affected in that commit (including `tid -> 0` for
/// depleted/deleted types). Queries DB snapshot state lock-free (`CorpActor::resolve_for_corp`) so
/// post-commit listeners never re-enter `ServerContext` locks.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_switched_item_commit_notifications(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    online: &impl OnlineCharacterLookup,
    batch: SwitchedCommitBatch,
    now_unix: u64,
    now_ft: i64,
    active_fuel_reserves: &crate::services::tactical_modules::ActiveFuelReserveTracker,
    db_key: usize,
    mut seq_alloc: impl FnMut() -> u64,
) -> SwitchedCommitNotifications {
    let SwitchedCommitBatch {
        item_specs: specs,
        blueprint_specs,
    } = batch;
    let monitors = if specs.is_empty() {
        BTreeMap::new()
    } else {
        load_industry_material_monitors(snap)
    };
    let mut base_avail_cache: HashMap<(i64, i64, i64), BTreeMap<u32, i64>> = HashMap::new();
    let mut corp_auth_cache: HashMap<i64, bool> = HashMap::new();
    let mut notifs =
        Vec::with_capacity(specs.len() * 2 + monitors.len() + blueprint_specs.len());
    let mut coalesced_monitors: BTreeMap<i64, (u64, &IndustryMaterialMonitorSub, BTreeSet<u32>)> =
        BTreeMap::new();
    let mut last_spec_idx_by_char: HashMap<i64, usize> = HashMap::new();

    for (idx, spec) in specs.iter().enumerate() {
        last_spec_idx_by_char.insert(spec.recipient_char_id, idx);

        if !monitors.is_empty() && !spec.industry_scopes.is_empty() {
            for (&monitor_id, sub) in &monitors {
                if sub.char_id != spec.recipient_char_id {
                    continue;
                }
                let is_personal = sub.owner_id == sub.char_id;
                if !is_personal {
                    let authorized = *corp_auth_cache.entry(monitor_id).or_insert_with(|| {
                        has_corp_industry_monitor_access(snap, sde, online, sub, now_unix)
                    });
                    if !authorized {
                        continue;
                    }
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
    let mut industry_notifs_by_char: HashMap<i64, Vec<_>> = HashMap::new();
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
        covered_industry_monitors.insert(
            monitor_id,
            (sub.char_id, sub.owner_id, covered_tids.into_iter().collect()),
        );
        industry_notifs_by_char
            .entry(sub.char_id)
            .or_default()
            .push((recipient_user_id, monitor_id, entries));
    }

    // Interleave OnItemChange and OnMultipleItemChange per item (with each pilot's coalesced
    // OnIndustryMaterials immediately before their final OnMultipleItemChange) so large commits
    // exceeding MAX_PENDING_NOTIFICATIONS_PER_CHAR preserve OnItemChange + OnIndustryMaterials.
    for (idx, spec) in specs.into_iter().enumerate() {
        let recipient_char_id = spec.recipient_char_id;
        let recipient_user_id = spec.recipient_user_id;
        let id_type = spec.id_type;
        let multi_args = if let [row, change_dict, ..] = spec.args.as_slice() {
            Some(vec![PyRep::List(vec![row.clone()]), change_dict.clone()])
        } else {
            None
        };
        active_fuel_reserves.observe_pre_emit_event(db_key, "OnItemChange", &spec.args);
        #[cfg(any(test, feature = "testkit"))]
        crate::router::notify_registry::assert_registered_event_shape_if_registered(
            "OnItemChange",
            &spec.args,
        );
        notifs.push(assemble_pending_notification(
            seq_alloc(),
            recipient_char_id,
            Some(recipient_user_id),
            "OnItemChange",
            id_type,
            spec.args,
            now_ft,
        ));
        if last_spec_idx_by_char.get(&recipient_char_id) == Some(&idx) {
            if let Some(ind_notifs) = industry_notifs_by_char.remove(&recipient_char_id) {
                for (ind_user_id, monitor_id, entries) in ind_notifs {
                    let mat_args = vec![PyRep::Integer(monitor_id), PyRep::Dict(entries)];
                    #[cfg(any(test, feature = "testkit"))]
                    crate::router::notify_registry::assert_registered_event_shape_if_registered(
                        "OnIndustryMaterials",
                        &mat_args,
                    );
                    notifs.push(assemble_pending_notification(
                        seq_alloc(),
                        recipient_char_id,
                        Some(ind_user_id),
                        "OnIndustryMaterials",
                        "charid",
                        mat_args,
                        now_ft,
                    ));
                }
            }
        }
        if let Some(multi_args) = multi_args {
            active_fuel_reserves.observe_pre_emit_event(db_key, "OnMultipleItemChange", &multi_args);
            #[cfg(any(test, feature = "testkit"))]
            crate::router::notify_registry::assert_registered_event_shape_if_registered(
                "OnMultipleItemChange",
                &multi_args,
            );
            notifs.push(assemble_pending_notification(
                seq_alloc(),
                recipient_char_id,
                Some(recipient_user_id),
                "OnMultipleItemChange",
                id_type,
                multi_args,
                now_ft,
            ));
        }
    }

    for bp_spec in blueprint_specs {
        let bp_args = vec![PyRep::Integer(bp_spec.owner_id)];
        #[cfg(any(test, feature = "testkit"))]
        crate::router::notify_registry::assert_registered_event_shape_if_registered(
            "OnBlueprintsUpdated",
            &bp_args,
        );
        notifs.push(assemble_pending_notification(
            seq_alloc(),
            bp_spec.recipient_char_id,
            Some(bp_spec.recipient_user_id),
            "OnBlueprintsUpdated",
            "ownerID",
            bp_args,
            now_ft,
        ));
    }

    SwitchedCommitNotifications {
        notifs,
        covered_industry_monitors,
    }
}
