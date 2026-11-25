//! Extended recipient rules (Rules 3–5), container/ship holder resolution, and per-pilot
//! notification deduplication (`#931` Track 5C Step 3a).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use eve_db::{
    ChangeSet, CharacterRecord, InventoryItemRecord, RowChange, WorldStateSnapshot,
};
use eve_sde::SdeRegistry;

use super::delta_item_switch::{
    online_non_transitioned_owner, ItemBeforeAfter, ProjectedItemOpKind,
};
use super::delta_projection::OnlineCharacterLookup;

pub(crate) const RULE_OWNER: &str = "owner";
pub(crate) const RULE_CORPORATION: &str = "corporation";
pub(crate) const RULE_FORMER_MEMBER: &str = "former member";
pub(crate) const RULE_OWNERLESS: &str = "ownerless";
pub(crate) const RULE_HOLDER_ACTING_PILOT: &str = "holder acting pilot";
pub(crate) const RULE_HOLDER_OWNER: &str = "holder owner";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ItemHolderKind {
    Ship,
    Space,
    None,
}

impl ItemHolderKind {
    #[inline]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ship => "ship",
            Self::Space => "space",
            Self::None => "none",
        }
    }
}

pub(crate) struct SwitchedItemRecipient<'a> {
    pub character: &'a CharacterRecord,
    pub op_kind: ProjectedItemOpKind,
    pub fp_rec: &'a InventoryItemRecord,
    pub id_type: &'static str,
    pub rule: &'static str,
}

#[inline]
fn is_station_id(id: i64, sde: &SdeRegistry) -> bool {
    sde.stations.contains_key(&id) || (60_000_000..=64_000_000).contains(&id)
}

#[inline]
fn is_solar_system_id(id: i64, sde: &SdeRegistry) -> bool {
    u32::try_from(id)
        .ok()
        .is_some_and(|sid| sde.solar_systems.contains_key(&sid))
        || (30_000_000..=39_999_999).contains(&id)
}

#[inline]
fn is_upwell_structure_id(id: i64, snap_after: &WorldStateSnapshot, changes: Option<&ChangeSet>) -> bool {
    snap_after.structures.contains_key(&id)
        || changes.is_some_and(|c| c.structures.iter().any(|sc| sc.key == id))
}

#[inline]
fn is_created_in_commit(
    item_id: i64,
    item_changes_by_id: Option<&HashMap<i64, ItemBeforeAfter<'_>>>,
) -> bool {
    item_changes_by_id
        .and_then(|m| m.get(&item_id))
        .is_some_and(|(before, after)| before.is_none() && after.is_some())
}

/// Resolves Rule 5 holder for an item whose direct `location_id` is `start_loc_id`:
/// - Nearest `category_id == 6` ship enclosing it (directly or via nested containers);
/// - Otherwise outermost non-ship, non-Upwell, non-office container whose `location_id` is a
///   solar system (floating in space).
///
/// Looks up parent containers in `snap_after.items`, falling back to `before` images from
/// `item_changes_by_id` when a container was deleted in the same commit (`after.is_none()`).
/// Rejects holders created in the same commit.
pub(crate) fn resolve_item_holder<'a>(
    item_id: i64,
    start_loc_id: i64,
    snap_after: &'a WorldStateSnapshot,
    changes: Option<&ChangeSet>,
    item_changes_by_id: Option<&HashMap<i64, ItemBeforeAfter<'a>>>,
    sde: &SdeRegistry,
) -> Option<(ItemHolderKind, &'a InventoryItemRecord)> {
    let mut cur_loc = start_loc_id;
    let mut outermost_container: Option<&'a InventoryItemRecord> = None;
    let mut visited = [0_i64; 16];

    for depth in 0..visited.len() {
        if cur_loc <= 0
            || cur_loc == 6
            || cur_loc == item_id
            || visited[..depth].contains(&cur_loc)
        {
            return None;
        }
        visited[depth] = cur_loc;

        if snap_after.characters.contains_key(&cur_loc)
            || is_station_id(cur_loc, sde)
            || is_upwell_structure_id(cur_loc, snap_after, changes)
        {
            return None;
        }
        if is_solar_system_id(cur_loc, sde) {
            let outer = outermost_container?;
            if is_created_in_commit(outer.item_id, item_changes_by_id) {
                return None;
            }
            return Some((ItemHolderKind::Space, outer));
        }

        let node = snap_after.items.get(&cur_loc).or_else(|| {
            item_changes_by_id
                .and_then(|m| m.get(&cur_loc))
                .and_then(|(before, after)| after.is_none().then_some(*before).flatten())
        })?;
        let node_cat = sde.types.get(&node.type_id).map(|t| t.category_id);
        if node_cat == Some(65) || node.type_id == 27 {
            return None;
        }
        if node_cat == Some(6) {
            if is_created_in_commit(node.item_id, item_changes_by_id) {
                return None;
            }
            return Some((ItemHolderKind::Ship, node));
        }
        outermost_container = Some(node);
        cur_loc = node.location_id;
    }
    None
}

/// Telemetry classifier for hand-written item events (`holder=ship|space|none`).
pub(crate) fn classify_item_holder_kind_in_snap(
    item_id: i64,
    after_loc: i64,
    before_loc: Option<i64>,
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
) -> ItemHolderKind {
    let live_loc = snap.items.get(&item_id).map(|r| r.location_id).unwrap_or(after_loc);
    if let Some((kind, _)) = resolve_item_holder(item_id, live_loc, snap, None, None, sde) {
        return kind;
    }
    if after_loc > 0 && after_loc != live_loc && after_loc != 6 {
        if let Some((kind, _)) = resolve_item_holder(item_id, after_loc, snap, None, None, sde) {
            return kind;
        }
    }
    if let Some(prev_loc) = before_loc {
        if let Some((kind, _)) = resolve_item_holder(item_id, prev_loc, snap, None, None, sde) {
            return kind;
        }
    }
    ItemHolderKind::None
}

#[inline]
fn is_existing_character_or_player_corp(owner_id: i64, snap_after: &WorldStateSnapshot) -> bool {
    snap_after.characters.contains_key(&owner_id) || snap_after.corporations.contains_key(&owner_id)
}

/// Appends Rules 3–5 recipients (when `extend_rules`, i.e. `cfg.char_all_items`, is active) in
/// Section 4C priority order (`holder owner` -> `former member` -> `ownerless` -> `holder acting pilot`),
/// then deduplicates keeping the first entry per pilot.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_extended_rules_and_dedup<'a>(
    extend_rules: bool,
    acting_char_id: Option<i64>,
    change: &'a RowChange<i64, InventoryItemRecord>,
    default_op_kind: ProjectedItemOpKind,
    default_fp_rec: &'a InventoryItemRecord,
    snap_after: &'a WorldStateSnapshot,
    changes: &ChangeSet,
    item_changes_by_id: &HashMap<i64, ItemBeforeAfter<'a>>,
    changed_chars: &HashMap<i64, (Option<&CharacterRecord>, Option<&CharacterRecord>)>,
    online: &impl OnlineCharacterLookup,
    sde: &SdeRegistry,
    out: &mut Vec<SwitchedItemRecipient<'a>>,
) {
    if !extend_rules {
        let mut seen_chars = HashSet::with_capacity(out.len());
        out.retain(|r| seen_chars.insert(r.character.character_id));
        return;
    }
    let before_opt = change.before.as_deref();
    let after_opt = change.after.as_deref();

    let after_holder = after_opt.and_then(|rec| {
        resolve_item_holder(
            rec.item_id,
            rec.location_id,
            snap_after,
            Some(changes),
            Some(item_changes_by_id),
            sde,
        )
    });
    let before_holder = before_opt.and_then(|rec| {
        resolve_item_holder(
            rec.item_id,
            rec.location_id,
            snap_after,
            Some(changes),
            Some(item_changes_by_id),
            sde,
        )
    });

    // Rule 5 (state half): holder owner.
    for (_, holder_rec) in [after_holder, before_holder].into_iter().flatten() {
        if let Some(ch) = online_non_transitioned_owner(
            holder_rec.owner_id,
            holder_rec.owner_id,
            snap_after,
            changed_chars,
            online,
            sde,
        ) {
            out.push(SwitchedItemRecipient {
                character: ch,
                op_kind: default_op_kind,
                fp_rec: default_fp_rec,
                id_type: "charid",
                rule: RULE_HOLDER_OWNER,
            });
        }
    }

    // Rule 3: corporation removed by the commit -> former members who left it in this commit.
    let after_owner = after_opt.map(|r| r.owner_id);
    let before_owner = before_opt.map(|r| r.owner_id).filter(|o| Some(*o) != after_owner);
    for oid in [after_owner, before_owner].into_iter().flatten() {
        if oid <= 0
            || snap_after.characters.contains_key(&oid)
            || snap_after.corporations.contains_key(&oid)
            || eve_db::is_npc_corporation(sde, oid)
        {
            continue;
        }
        let mut former: Vec<&'a CharacterRecord> = Vec::new();
        for (&cid, &(before_ch_opt, after_ch_opt)) in changed_chars {
            let (Some(before_ch), Some(after_ch)) = (before_ch_opt, after_ch_opt) else {
                continue;
            };
            if before_ch.corporation_id == oid && after_ch.corporation_id != oid {
                if let Some(ch) = online_non_transitioned_owner(
                    cid,
                    cid,
                    snap_after,
                    changed_chars,
                    online,
                    sde,
                ) {
                    former.push(ch);
                }
            }
        }
        former.sort_by_key(|ch| ch.character_id);
        for ch in former {
            out.push(SwitchedItemRecipient {
                character: ch,
                op_kind: default_op_kind,
                fp_rec: default_fp_rec,
                id_type: "charid",
                rule: RULE_FORMER_MEMBER,
            });
        }
    }

    // Rules 4 & 5 (RPC-only halves): acting pilot.
    if let Some(aid) = acting_char_id {
        if let Some(actor_ch) =
            online_non_transitioned_owner(aid, aid, snap_after, changed_chars, online, sde)
        {
            let ownerless = before_opt.is_some()
                && [before_opt.map(|r| r.owner_id), after_opt.map(|r| r.owner_id)]
                    .into_iter()
                    .flatten()
                    .all(|oid| !is_existing_character_or_player_corp(oid, snap_after));
            if ownerless {
                out.push(SwitchedItemRecipient {
                    character: actor_ch,
                    op_kind: default_op_kind,
                    fp_rec: default_fp_rec,
                    id_type: "charid",
                    rule: RULE_OWNERLESS,
                });
            }
            // Gate RULE_HOLDER_ACTING_PILOT on a live holder or a surviving item relocated out of a
            // space holder (such as salvage moving loot from a deleted wreck into a new jetcan):
            // - Ship destruction deletes `victim_ship_id` (`ItemHolderKind::Ship`), so `before_holder`
            //   (retained for `RULE_HOLDER_OWNER`) must not leak victim items to the killer.
            // - Space container/deployable destruction deletes enclosed items (`after_opt.is_none()`),
            //   so destroyed container contents must not leak to the attacker.
            let acting_holder_live = [after_holder, before_holder]
                .into_iter()
                .flatten()
                .any(|(kind, h)| {
                    snap_after.items.contains_key(&h.item_id)
                        || (kind == ItemHolderKind::Space && after_opt.is_some())
                });
            if acting_holder_live {
                out.push(SwitchedItemRecipient {
                    character: actor_ch,
                    op_kind: default_op_kind,
                    fp_rec: default_fp_rec,
                    id_type: "charid",
                    rule: RULE_HOLDER_ACTING_PILOT,
                });
            }
        }
    }

    // Section 4C: at most one event per pilot per item change, keeping the first entry.
    let mut seen_chars = HashSet::with_capacity(out.len());
    out.retain(|r| seen_chars.insert(r.character.character_id));
}
