//! Always-on commit-time item, blueprint, and `OnIndustryMaterials` delta projection (`#931`).
//!
//! Guardrails enforced by this module:
//! 1. **Same-scene / transition scoping:** Online human character owners and qualifying corp
//!    members with no scene transition or in-space `active_ship_id` change on the transition commit
//!    itself; subsequent commits in the same dispatch only suppress the transitioned character's
//!    active ship (`change.key == ch.active_ship_id`).
//! 2. **Character & Corp/Structure scope:** Human character owners require `snap.characters` and
//!    absence from player/NPC corporation tables; corporation items cover materialized player
//!    corporations (`snap.corporations`, all flags) and NPC corp hangar/locked/blueprint items.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap, HashSet};

use eve_db::{ChangeSet, CharacterRecord, InventoryItemRecord, WorldStateSnapshot};
#[cfg(test)]
use eve_proto::PyRep;
use eve_sde::SdeRegistry;

use super::delta_commit_listener::TxItemEffectKeys;
use super::delta_item_corp::{
    collect_switched_item_recipients_for_change, is_corp_blueprint_owner_eligible,
    resolve_online_non_transitioned_corp_recipients,
};
use super::delta_projection::{
    build_projected_item_change_args, compute_item_change_entries, is_blueprint_type,
    OnlineCharacterLookup,
};

pub(crate) use super::delta_item_corp::{
    SwitchedCommitBatch, SwitchedProjectedBlueprintSpec, SwitchedProjectedItemSpec,
};

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ItemWireFingerprint {
    pub type_id: u32,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag_id: i64,
    pub wire_qty: i64,
}

#[cfg(test)]
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

#[cfg(test)]
impl ItemWireFingerprint {
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
/// `station_id`, `structure_id`), or switched `active_ship_id` while in space in this commit.
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
        || (before.active_ship_id != after.active_ship_id
            && before.station_id.is_none()
            && before.structure_id.is_none())
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

pub(crate) type ItemBeforeAfter<'a> = (
    Option<&'a InventoryItemRecord>,
    Option<&'a InventoryItemRecord>,
);

pub(crate) fn online_non_transitioned_owner<'a>(
    owner_id: i64,
    recipient_char_id: i64,
    snap_after: &'a WorldStateSnapshot,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
) -> Option<&'a CharacterRecord> {
    if owner_id != recipient_char_id
        || !is_known_human_character_owner(owner_id, snap_after, changed_chars, sde)
        || is_character_scene_or_ship_transition(owner_id, changed_chars)
    {
        return None;
    }
    let ch = snap_after.characters.get(&owner_id)?;
    online.is_character_online(owner_id, ch.user_id).then_some(ch)
}

/// Evaluates `changes.items` under always-on projection rules, returning `OnItemChange` and
/// `OnBlueprintsUpdated` notification specs for one committed transaction.
#[allow(clippy::too_many_arguments)]
pub(crate) fn collect_switched_char_same_scene_item_specs(
    in_dispatch: bool,
    prior_transitioned_chars: &HashSet<i64>,
    explicit_acting_char_id: Option<i64>,
    online: &impl OnlineCharacterLookup,
    snap_after: &WorldStateSnapshot,
    changes: &ChangeSet,
    sde: &SdeRegistry,
    suppressed_effects: &TxItemEffectKeys,
    active_fuel_reserves: &crate::services::tactical_modules::ActiveFuelReserveTracker,
    db_key: usize,
) -> SwitchedCommitBatch {
    if changes.items.is_empty() {
        return SwitchedCommitBatch::default();
    }

    let changed_chars = index_changed_characters(changes);
    let mut item_changes_by_id: HashMap<i64, ItemBeforeAfter<'_>> =
        HashMap::with_capacity(changes.items.len());
    for ch in &changes.items {
        item_changes_by_id
            .entry(ch.key)
            .and_modify(|entry| entry.1 = ch.after.as_deref())
            .or_insert((ch.before.as_deref(), ch.after.as_deref()));
    }

    let mut specs = Vec::new();
    let mut blueprint_specs_map: BTreeMap<(i64, i64), u64> = BTreeMap::new();
    let mut corp_recipients_cache: HashMap<i64, Vec<&CharacterRecord>> = HashMap::new();
    let acting_char_id = explicit_acting_char_id.or_else(|| {
        in_dispatch
            .then(super::dispatch_active_char::active_dispatch_char)
            .flatten()
    });

    for change in &changes.items {
        let before_rec = change.before.as_deref();
        let after_rec = change.after.as_deref();
        let before_is_bp = before_rec.is_some_and(|r| is_blueprint_type(sde, r.type_id));
        let after_is_bp = after_rec.is_some_and(|r| is_blueprint_type(sde, r.type_id));
        let bp_involved = before_is_bp || after_is_bp;

        // Leave Cargo fuel items held under an active rollback reserve untouched here so silent
        // actor-validation rollbacks emit zero notifications and deferred diffs stay authoritative (#1486).
        if active_fuel_reserves.is_item_reserved(db_key, change.key) {
            continue;
        }

        if bp_involved {
            let bp_changed = match (before_rec, after_rec) {
                (None, Some(_)) | (Some(_), None) => true,
                (Some(old_item), Some(new_item)) => {
                    !compute_item_change_entries(old_item, new_item).is_empty()
                        || old_item.type_id != new_item.type_id
                        || old_item.is_blueprint_copy != new_item.is_blueprint_copy
                        || old_item.blueprint_runs_remaining != new_item.blueprint_runs_remaining
                        || old_item.dynamic_attributes != new_item.dynamic_attributes
                }
                (None, None) => false,
            };
            if bp_changed {
                let mut bp_owners = [None, None];
                match (before_rec, after_rec) {
                    (None, Some(new_item)) => bp_owners[0] = Some(new_item.owner_id),
                    (Some(old_item), None) => bp_owners[0] = Some(old_item.owner_id),
                    (Some(old_item), Some(new_item)) => {
                        bp_owners[0] = Some(new_item.owner_id);
                        if old_item.owner_id != new_item.owner_id {
                            bp_owners[1] = Some(old_item.owner_id);
                        }
                    }
                    (None, None) => {}
                }
                for owner_id in bp_owners.into_iter().flatten() {
                    if let Some(ch) = online_non_transitioned_owner(
                        owner_id,
                        owner_id,
                        snap_after,
                        &changed_chars,
                        online,
                        sde,
                    ) {
                        let active_ship_suppressed = prior_transitioned_chars.contains(&owner_id)
                            && ch.active_ship_id > 0
                            && change.key == ch.active_ship_id;
                        if !active_ship_suppressed
                            && !suppressed_effects.contains_blueprint(owner_id, owner_id)
                        {
                            blueprint_specs_map
                                .entry((owner_id, owner_id))
                                .or_insert(ch.user_id);
                        }
                        continue;
                    }
                    if is_corp_blueprint_owner_eligible(owner_id, snap_after, sde) {
                        let members = corp_recipients_cache.entry(owner_id).or_insert_with(|| {
                            resolve_online_non_transitioned_corp_recipients(
                                owner_id,
                                snap_after,
                                &changed_chars,
                                online,
                            )
                        });
                        for &ch in members.iter() {
                            let cid = ch.character_id;
                            let active_ship_suppressed = prior_transitioned_chars.contains(&cid)
                                && ch.active_ship_id > 0
                                && change.key == ch.active_ship_id;
                            if active_ship_suppressed
                                || suppressed_effects.contains_blueprint(cid, owner_id)
                            {
                                continue;
                            }
                            blueprint_specs_map
                                .entry((cid, owner_id))
                                .or_insert(ch.user_id);
                        }
                    }
                }
            }
        }

        let classified_recipients = collect_switched_item_recipients_for_change(
            acting_char_id,
            change,
            snap_after,
            changes,
            &item_changes_by_id,
            &changed_chars,
            online,
            sde,
            &mut corp_recipients_cache,
        );
        let mut eligible_recipients = Vec::with_capacity(classified_recipients.len());
        for recip in classified_recipients {
            let recipient_char_id = recip.character.character_id;
            if prior_transitioned_chars.contains(&recipient_char_id)
                && recip.character.active_ship_id > 0
                && change.key == recip.character.active_ship_id
            {
                continue;
            }
            if suppressed_effects.contains_item(recipient_char_id, change.key) {
                continue;
            }
            eligible_recipients.push(recip);
        }
        if eligible_recipients.is_empty() {
            continue;
        }

        let Some(item_payload) = build_projected_item_change_args(change, changes, snap_after, sde)
        else {
            continue;
        };
        // Only non-singleton endpoints affect available material counts in OnIndustryMaterials
        // (matching collect_available_monitor_materials; locking a singleton BPO must not inject {tid: 0}).
        let mut industry_scopes = Vec::with_capacity(2);
        if let Some(old_item) = before_rec.filter(|r| !r.singleton) {
            industry_scopes.push((
                old_item.owner_id,
                old_item.location_id,
                old_item.flag as i64,
                old_item.type_id,
            ));
        }
        if let Some(new_item) = after_rec.filter(|r| !r.singleton) {
            let new_scope = (
                new_item.owner_id,
                new_item.location_id,
                new_item.flag as i64,
                new_item.type_id,
            );
            if industry_scopes.first() != Some(&new_scope) {
                industry_scopes.push(new_scope);
            }
        }

        let eligible_count = eligible_recipients.len();
        for (idx, recip) in eligible_recipients.into_iter().enumerate() {
            let recipient_char_id = recip.character.character_id;
            let is_last = idx + 1 == eligible_count;
            if is_last {
                specs.push(SwitchedProjectedItemSpec {
                    recipient_char_id,
                    recipient_user_id: recip.character.user_id,
                    id_type: recip.id_type,
                    args: item_payload,
                    industry_scopes,
                });
                break;
            }
            specs.push(SwitchedProjectedItemSpec {
                recipient_char_id,
                recipient_user_id: recip.character.user_id,
                id_type: recip.id_type,
                args: item_payload.clone(),
                industry_scopes: industry_scopes.clone(),
            });
        }
    }

    let blueprint_specs: Vec<SwitchedProjectedBlueprintSpec> = blueprint_specs_map
        .into_iter()
        .map(
            |((recipient_char_id, owner_id), recipient_user_id)| SwitchedProjectedBlueprintSpec {
                recipient_char_id,
                recipient_user_id,
                owner_id,
            },
        )
        .collect();

    SwitchedCommitBatch {
        item_specs: specs,
        blueprint_specs,
    }
}
