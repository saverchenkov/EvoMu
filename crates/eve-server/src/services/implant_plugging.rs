//! `implant_plugging` — plugging in & unplugging character implants (`dogmaIM.InjectImplant`,
//! `dogmaIM.DestroyImplant`, `skillMgr2.GetImplants`) and consuming & inspecting combat/attribute
//! boosters (`dogmaIM.UseBooster`, `skillMgr2.GetBoosters`) (`feat:attributes.implant-plug`,
//! `feat:attributes.implant-remove`, `feat:attributes.booster-consume`, `feat:attributes.booster-side-effects`,
//! `feat:attributes.booster-duration-biology`, `feat:clones.clone-jump-booster-and-implant-handling`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository, WorldStateSnapshot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, TypeId};
use std::collections::{HashMap, HashSet};

/// SDE attribute ID for implant slot index (`implantness`, 1..=10).
pub const ATTR_IMPLANTNESS: u16 = 331;
/// SDE attribute ID for booster slot index (`boosterness`).
pub const ATTR_BOOSTERNESS: u16 = 1087;
/// SDE attribute ID for booster base duration in milliseconds (`boosterDuration`).
pub const ATTR_BOOSTER_DURATION_MS: u16 = 330;
/// SDE attribute ID for `boosterEffectChance1` (`1089..=1093` for side-effect slots 1..=5).
pub const ATTR_BOOSTER_EFFECT_CHANCE_1: u16 = 1089;
/// SDE attribute ID for `followsJumpClones` (`1916`, `1.0` on cerebral accelerators that persist across clone jumps).
pub const ATTR_FOLLOWS_JUMP_CLONES: u16 = 1916;
/// SDE skill type ID for `Biology` (`+20%` booster duration per level via attribute `66` `durationBonus`).
pub const SKILL_BIOLOGY: TypeId = 3405;
/// SDE skill type ID for `Neurotoxin Recovery` (`-5%` booster side-effect chance per level via attribute `1125`).
pub const SKILL_NEUROTOXIN_RECOVERY: TypeId = 25530;
/// SDE skill type ID for `Neurotoxin Control` (`-5%` booster side-effect penalty severity per level via attribute `1126`).
pub const SKILL_NEUROTOXIN_CONTROL: TypeId = 25538;

/// Dynamic attribute key storing the Unix timestamp (seconds) when an active `InventoryFlag::Booster` item expires.
pub const DYN_ATTR_BOOSTER_EXPIRY_UNIX: u16 = 9901;
/// Dynamic attribute key storing the duration in seconds computed when a booster was consumed.
pub const DYN_ATTR_BOOSTER_DURATION_SECS: u16 = 9900;
/// Dynamic attribute key marking that a booster's side effects were rolled on consumption (`1.0`).
pub const DYN_ATTR_BOOSTER_ROLLED_FLAG: u16 = 9902;
/// Base dynamic attribute key (`9910..=9919`) storing triggered side-effect `effect_id`s on an active booster item.
pub const DYN_ATTR_BOOSTER_SIDE_EFFECT_BASE: u16 = 9910;

fn unix_to_filetime(unix_secs: u64) -> i64 {
    ((unix_secs as i64) + 11_644_473_600) * 10_000_000
}

fn implant_slot(sde: &SdeRegistry, type_id: TypeId) -> Option<i64> {
    let t = sde.types.get(&type_id)?;
    let slot = t.default_attributes.get(&ATTR_IMPLANTNESS).copied()?.round() as i64;
    (1..=10).contains(&slot).then_some(slot)
}

fn booster_slot(sde: &SdeRegistry, type_id: TypeId) -> Option<i64> {
    let t = sde.types.get(&type_id)?;
    let slot = t.default_attributes.get(&ATTR_BOOSTERNESS).copied()?.round() as i64;
    (slot >= 1).then_some(slot)
}

fn item_is_in_pilot_location(
    snap: &WorldStateSnapshot,
    char_id: i64,
    item: &InventoryItemRecord,
) -> bool {
    let Some(ch) = snap.characters.get(&char_id) else {
        return false;
    };
    let docked_loc = ch.station_id.or(ch.structure_id);
    let in_hangar = docked_loc.is_some_and(|loc| {
        item.location_id == loc && item.flag == InventoryFlag::Hangar
    });
    let in_ship_cargo =
        item.location_id == ch.active_ship_id && item.flag == InventoryFlag::Cargo;
    in_hangar || in_ship_cargo
}

/// Derives the set of booster side-effect penalty attribute IDs from SDE skill `25538` (`Neurotoxin Control`).
pub fn booster_penalty_attribute_ids(sde: &SdeRegistry) -> HashSet<u16> {
    sde.types
        .get(&SKILL_NEUROTOXIN_CONTROL)
        .map(|t| {
            t.effect_ids
                .iter()
                .filter_map(|eid| sde.effects.get(eid))
                .flat_map(|eff| eff.modifiers.iter())
                .filter(|s| s.domain == "charID" && s.group_id == Some(303))
                .map(|s| s.modified_attribute_id)
                .collect()
        })
        .unwrap_or_default()
}

/// Purges expired `InventoryFlag::Booster` items from `snap.items` and returns `active_ship_id`s to invalidate.
pub fn purge_expired_boosters_in_snapshot(
    snap: &mut WorldStateSnapshot,
    char_filter: Option<i64>,
    now_unix: u64,
) -> Vec<i64> {
    let expired_ids: Vec<(i64, i64)> = snap
        .items
        .values()
        .filter(|item| {
            item.flag == InventoryFlag::Booster
                && item.location_id == item.owner_id
                && char_filter.is_none_or(|cid| item.owner_id == cid)
                && item
                    .dynamic_attributes
                    .get(&DYN_ATTR_BOOSTER_EXPIRY_UNIX)
                    .is_some_and(|&exp| exp > 0.0 && now_unix >= (exp as u64))
        })
        .map(|item| (item.item_id, item.owner_id))
        .collect();
    let mut ships = Vec::new();
    for (item_id, owner_id) in expired_ids {
        snap.items.remove(&item_id);
        if let Some(ch) = snap.characters.get(&owner_id) {
            ships.push(ch.active_ship_id);
        }
    }
    ships
}

/// Purges all expired boosters across the snapshot and invalidates affected ship fittings.
pub fn purge_expired_boosters_all(ctx: &ServerContext, now_unix: u64) {
    let ships = ctx
        .db
        .with_mut_snapshot(|snap| purge_expired_boosters_in_snapshot(snap, None, now_unix));
    for ship_id in ships {
        ctx.invalidate_ship_fitting(ship_id);
    }
}

/// Returns active combat booster `type_id`s and their rolled side-effect `effect_id`s for `char_id`.
pub fn active_combat_boosters_for_char(
    db: &WorldRepository,
    char_id: i64,
    now_unix: u64,
) -> (Vec<TypeId>, HashMap<TypeId, Vec<u32>>) {
    db.with_snapshot(|snap| {
        let mut items: Vec<&InventoryItemRecord> = snap
            .items
            .values()
            .filter(|item| {
                item.owner_id == char_id
                    && item.location_id == char_id
                    && item.flag == InventoryFlag::Booster
                    && !item
                        .dynamic_attributes
                        .get(&DYN_ATTR_BOOSTER_EXPIRY_UNIX)
                        .is_some_and(|&exp| exp > 0.0 && now_unix >= (exp as u64))
            })
            .collect();
        items.sort_by_key(|i| i.item_id);
        let mut boosters = Vec::with_capacity(items.len());
        let mut side_effects: HashMap<TypeId, Vec<u32>> = HashMap::new();
        for item in items {
            boosters.push(item.type_id);
            if item
                .dynamic_attributes
                .contains_key(&DYN_ATTR_BOOSTER_ROLLED_FLAG)
            {
                let mut eids = Vec::new();
                for offset in 0..10_u16 {
                    if let Some(&val) = item
                        .dynamic_attributes
                        .get(&(DYN_ATTR_BOOSTER_SIDE_EFFECT_BASE + offset))
                    {
                        if val > 0.0 {
                            eids.push(val.round() as u32);
                        }
                    }
                }
                side_effects.insert(item.type_id, eids);
            }
        }
        (boosters, side_effects)
    })
}

/// Synchronizes `InventoryFlag::Implant` items on `char_id` with `snap.active_implants[&char_id]`
/// and removes active boosters that do not have SDE attribute `1916` (`followsJumpClones`) set
/// (`feat:clones.clone-jump-booster-and-implant-handling`).
pub fn sync_clone_jump_implants_and_boosters(
    sde: &SdeRegistry,
    snap: &mut WorldStateSnapshot,
    char_id: i64,
) {
    // Remove active boosters on `char_id` unless SDE attribute `1916` (`followsJumpClones`) >= 1.0.
    let booster_ids_to_remove: Vec<i64> = snap
        .items
        .values()
        .filter(|item| {
            item.owner_id == char_id
                && item.location_id == char_id
                && item.flag == InventoryFlag::Booster
                && sde
                    .types
                    .get(&item.type_id)
                    .and_then(|t| t.default_attributes.get(&ATTR_FOLLOWS_JUMP_CLONES).copied())
                    .unwrap_or(0.0)
                    < 1.0
        })
        .map(|item| item.item_id)
        .collect();
    for bid in booster_ids_to_remove {
        snap.items.remove(&bid);
    }

    // Rebuild `InventoryFlag::Implant` items on `char_id` to match `snap.active_implants[&char_id]`.
    let old_implant_ids: Vec<i64> = snap
        .items
        .values()
        .filter(|item| {
            item.owner_id == char_id
                && item.location_id == char_id
                && item.flag == InventoryFlag::Implant
        })
        .map(|item| item.item_id)
        .collect();
    for iid in old_implant_ids {
        snap.items.remove(&iid);
    }
    let active_tids = snap
        .active_implants
        .get(&char_id)
        .cloned()
        .unwrap_or_default();
    for tid in active_tids {
        snap.next_entity_id += 1;
        let new_id = snap.next_entity_id;
        snap.items.insert(
            new_id,
            InventoryItemRecord {
                item_id: new_id,
                type_id: tid,
                owner_id: char_id,
                location_id: char_id,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    }
}

/// `dogmaIM.InjectImplant(itemID)`: validates ownership, reachability, skill prerequisites, and slot
/// availability, then consumes 1 unit from inventory into `(location_id = char_id, flag = Implant)`
/// (`feat:attributes.implant-plug`).
pub fn inject_implant(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let item_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| BridgeRpcError {
        code: "InvalidArgument".into(),
        message: "InjectImplant requires an itemID".into(),
    })?;
    let ship_id = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(item) = snap.items.get(&item_id).cloned() else {
            return Err(BridgeRpcError {
                code: "ItemNotFound".into(),
                message: format!("Implant item {item_id} not found"),
            });
        };
        if item.owner_id != char_id {
            return Err(BridgeRpcError {
                code: "ItemNotOwned".into(),
                message: format!("Implant item {item_id} is not owned by character {char_id}"),
            });
        }
        if item.flag == InventoryFlag::Implant && item.location_id == char_id {
            return Err(BridgeRpcError {
                code: "ImplantAlreadyPluggedIn".into(),
                message: format!("Implant {item_id} is already plugged in"),
            });
        }
        if !item_is_in_pilot_location(snap, char_id, &item) || item.quantity < 1 {
            return Err(BridgeRpcError {
                code: "ItemNotReachable".into(),
                message: format!(
                    "Implant {item_id} must be in your active ship cargo or docked hangar"
                ),
            });
        }
        let Some(slot) = implant_slot(&ctx.sde, item.type_id) else {
            return Err(BridgeRpcError {
                code: "ItemNotAnImplant".into(),
                message: format!("Type {} has no valid implantness slot", item.type_id),
            });
        };
        ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, item.type_id)?;

        let slot_taken_in_active = snap
            .active_implants
            .get(&char_id)
            .is_some_and(|list| list.iter().any(|&tid| implant_slot(&ctx.sde, tid) == Some(slot)));
        let slot_taken_in_items = snap.items.values().any(|i| {
            i.owner_id == char_id
                && i.location_id == char_id
                && i.flag == InventoryFlag::Implant
                && implant_slot(&ctx.sde, i.type_id) == Some(slot)
        });
        if slot_taken_in_active || slot_taken_in_items {
            return Err(BridgeRpcError {
                code: "ImplantSlotOccupied".into(),
                message: format!("Implant slot {slot} is already occupied"),
            });
        }

        if item.quantity > 1 {
            if let Some(src) = snap.items.get_mut(&item_id) {
                src.quantity -= 1;
                src.version += 1;
            }
            snap.next_entity_id += 1;
            let plugged_id = snap.next_entity_id;
            snap.items.insert(
                plugged_id,
                InventoryItemRecord {
                    item_id: plugged_id,
                    type_id: item.type_id,
                    owner_id: char_id,
                    location_id: char_id,
                    flag: InventoryFlag::Implant,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: item.dynamic_attributes.clone(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        } else if let Some(src) = snap.items.get_mut(&item_id) {
            src.location_id = char_id;
            src.flag = InventoryFlag::Implant;
            src.singleton = true;
            src.version += 1;
        }
        let active = snap.active_implants.entry(char_id).or_default();
        if !active.contains(&item.type_id) {
            active.push(item.type_id);
        }
        Ok(snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(0))
    })?;
    if ship_id > 0 {
        ctx.invalidate_ship_fitting(ship_id);
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnImplantsChanged",
        "charid",
        vec![],
    );
    Ok(PyRep::None)
}

/// `dogmaIM.DestroyImplant(itemID)`: permanently destroys a plugged-in implant (`feat:attributes.implant-remove`).
pub fn destroy_implant(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let item_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| BridgeRpcError {
        code: "InvalidArgument".into(),
        message: "DestroyImplant requires an itemID".into(),
    })?;
    let ship_id = ctx.db.try_with_mut_snapshot(|snap| {
        let found_item = snap.items.get(&item_id).cloned();
        let removed_type_id = if let Some(item) = found_item {
            if item.owner_id != char_id
                || item.location_id != char_id
                || item.flag != InventoryFlag::Implant
            {
                return Err(BridgeRpcError {
                    code: "ImplantNotPluggedIn".into(),
                    message: format!("Item {item_id} is not a plugged-in implant of character {char_id}"),
                });
            }
            snap.items.remove(&item_id);
            item.type_id
        } else {
            let tid = item_id as TypeId;
            let has_in_active = snap
                .active_implants
                .get(&char_id)
                .is_some_and(|v| v.contains(&tid));
            if !has_in_active {
                return Err(BridgeRpcError {
                    code: "ImplantNotFound".into(),
                    message: format!("Plugged-in implant {item_id} not found"),
                });
            }
            tid
        };
        if let Some(active) = snap.active_implants.get_mut(&char_id) {
            if let Some(pos) = active.iter().position(|&t| t == removed_type_id) {
                active.remove(pos);
            }
        }
        Ok(snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(0))
    })?;
    if ship_id > 0 {
        ctx.invalidate_ship_fitting(ship_id);
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnImplantsChanged",
        "charid",
        vec![],
    );
    Ok(PyRep::None)
}

/// `skillMgr2.GetImplants()`: returns `{slot: KeyVal(itemID, typeID, implantness)}` for plugged implants.
pub fn get_implants(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let entries = ctx.db.with_snapshot(|snap| {
        let mut by_slot: HashMap<i64, (i64, TypeId)> = HashMap::new();
        for item in snap.items.values() {
            if item.owner_id == char_id
                && item.location_id == char_id
                && item.flag == InventoryFlag::Implant
            {
                if let Some(slot) = implant_slot(&ctx.sde, item.type_id) {
                    by_slot.insert(slot, (item.item_id, item.type_id));
                }
            }
        }
        if let Some(active) = snap.active_implants.get(&char_id) {
            for &tid in active {
                if let Some(slot) = implant_slot(&ctx.sde, tid) {
                    by_slot.entry(slot).or_insert((tid as i64, tid));
                }
            }
        }
        let mut slots: Vec<i64> = by_slot.keys().copied().collect();
        slots.sort_unstable();
        slots
            .into_iter()
            .map(|slot| {
                let (iid, tid) = by_slot[&slot];
                (
                    PyRep::Integer(slot),
                    PyRep::keyval([
                        ("itemID", PyRep::Integer(iid)),
                        ("typeID", PyRep::Integer(tid as i64)),
                        ("implantness", PyRep::Integer(slot)),
                    ]),
                )
            })
            .collect()
    });
    Ok(PyRep::Dict(entries))
}

/// `dogmaIM.UseBooster(itemID, locationID?)`: validates ownership, reachability, skill prerequisites,
/// and booster slot (`1087` `boosterness`) availability, scales duration (`330` `boosterDuration`) by
/// `3405 Biology`, rolls side effects (`1089..=1093`) reduced by `25530 Neurotoxin Recovery`, and activates
/// the booster at `(location_id = char_id, flag = Booster)` (`feat:attributes.booster-consume`,
/// `feat:attributes.booster-side-effects`, `feat:attributes.booster-duration-biology`).
pub fn use_booster(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let item_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| BridgeRpcError {
        code: "InvalidArgument".into(),
        message: "UseBooster requires an itemID".into(),
    })?;
    let expected_loc = args.get(1).and_then(|a| a.as_i64()).filter(|&l| l > 0);
    let now_unix = ctx.now_unix();
    let penalty_attrs = booster_penalty_attribute_ids(&ctx.sde);

    let (booster_item_id, type_id, slot, duration_secs, expiry_unix, rolled_eids, ship_id) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let _ = purge_expired_boosters_in_snapshot(snap, Some(char_id), now_unix);
            let Some(item) = snap.items.get(&item_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Booster item {item_id} not found"),
                });
            };
            if item.owner_id != char_id {
                return Err(BridgeRpcError {
                    code: "ItemNotOwned".into(),
                    message: format!("Booster item {item_id} is not owned by character {char_id}"),
                });
            }
            if let Some(loc_id) = expected_loc {
                if item.location_id != loc_id {
                    return Err(BridgeRpcError {
                        code: "ItemNotReachable".into(),
                        message: format!("Booster {item_id} is not at location {loc_id}"),
                    });
                }
            }
            if !item_is_in_pilot_location(snap, char_id, &item) || item.quantity < 1 {
                return Err(BridgeRpcError {
                    code: "ItemNotReachable".into(),
                    message: format!(
                        "Booster {item_id} must be in your active ship cargo or docked hangar"
                    ),
                });
            }
            let Some(slot) = booster_slot(&ctx.sde, item.type_id) else {
                return Err(BridgeRpcError {
                    code: "ItemNotABooster".into(),
                    message: format!("Type {} has no valid boosterness slot", item.type_id),
                });
            };
            let Some(b_type) = ctx.sde.types.get(&item.type_id) else {
                return Err(BridgeRpcError {
                    code: "TypeNotFound".into(),
                    message: format!("Booster type {} not found in SDE", item.type_id),
                });
            };
            let base_duration_ms = b_type
                .default_attributes
                .get(&ATTR_BOOSTER_DURATION_MS)
                .copied()
                .unwrap_or(0.0);
            if base_duration_ms <= 0.0 {
                return Err(BridgeRpcError {
                    code: "ItemNotABooster".into(),
                    message: format!("Type {} has no positive boosterDuration", item.type_id),
                });
            }
            ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, item.type_id)?;

            let slot_occupied = snap.items.values().any(|i| {
                i.owner_id == char_id
                    && i.location_id == char_id
                    && i.flag == InventoryFlag::Booster
                    && booster_slot(&ctx.sde, i.type_id) == Some(slot)
            });
            if slot_occupied {
                return Err(BridgeRpcError {
                    code: "BoosterSlotOccupied".into(),
                    message: format!("Booster slot {slot} is already occupied by an active booster"),
                });
            }

            let char_skills = snap.skills.get(&char_id);
            let biology_lvl = char_skills
                .and_then(|m| m.get(&SKILL_BIOLOGY))
                .map(|s| s.level.min(5))
                .unwrap_or(0);
            let biology_bonus_pct = ctx
                .sde
                .types
                .get(&SKILL_BIOLOGY)
                .and_then(|t| t.default_attributes.get(&66).copied())
                .unwrap_or(20.0);
            let duration_mult = 1.0 + (biology_bonus_pct / 100.0) * f64::from(biology_lvl);
            let duration_secs = ((base_duration_ms / 1000.0) * duration_mult).round().max(1.0) as u64;
            let expiry_unix = now_unix.saturating_add(duration_secs);

            let recovery_lvl = char_skills
                .and_then(|m| m.get(&SKILL_NEUROTOXIN_RECOVERY))
                .map(|s| s.level.min(5))
                .unwrap_or(0);
            let chance_bonus_pct = ctx
                .sde
                .types
                .get(&SKILL_NEUROTOXIN_RECOVERY)
                .and_then(|t| t.default_attributes.get(&1125).copied())
                .unwrap_or(-5.0);
            let chance_mult = (1.0 + (chance_bonus_pct / 100.0) * f64::from(recovery_lvl)).max(0.0);

            let side_effect_eids: Vec<u32> = b_type
                .effect_ids
                .iter()
                .copied()
                .filter(|eid| {
                    ctx.sde.effects.get(eid).is_some_and(|eff| {
                        eff.effect_category == 0
                            && eff
                                .modifiers
                                .iter()
                                .any(|s| penalty_attrs.contains(&s.modifying_attribute_id))
                    })
                })
                .map(u32::from)
                .collect();

            let mut rolled_eids = Vec::new();
            for (idx, &eid) in side_effect_eids.iter().enumerate() {
                let chance_attr = ATTR_BOOSTER_EFFECT_CHANCE_1 + (idx as u16);
                let base_chance = b_type
                    .default_attributes
                    .get(&chance_attr)
                    .copied()
                    .unwrap_or(0.0);
                let effective_chance = base_chance * chance_mult;
                // DERIVED: deterministic per-slot roll threshold `0.10 + 0.08 * idx` so side-effect
                // triggering is reproducible and sensitive to `Neurotoxin Recovery` (`-5%`/lvl).
                let roll_threshold = 0.10 + 0.08 * (idx as f64);
                if roll_threshold < effective_chance {
                    rolled_eids.push(eid);
                }
            }

            let mut dyn_attrs = item.dynamic_attributes.clone();
            dyn_attrs.insert(DYN_ATTR_BOOSTER_DURATION_SECS, duration_secs as f64);
            dyn_attrs.insert(DYN_ATTR_BOOSTER_EXPIRY_UNIX, expiry_unix as f64);
            dyn_attrs.insert(DYN_ATTR_BOOSTER_ROLLED_FLAG, 1.0);
            for (i, &eid) in rolled_eids.iter().enumerate().take(10) {
                dyn_attrs.insert(DYN_ATTR_BOOSTER_SIDE_EFFECT_BASE + (i as u16), eid as f64);
            }

            let active_booster_id = if item.quantity > 1 {
                if let Some(src) = snap.items.get_mut(&item_id) {
                    src.quantity -= 1;
                    src.version += 1;
                }
                snap.next_entity_id += 1;
                let bid = snap.next_entity_id;
                snap.items.insert(
                    bid,
                    InventoryItemRecord {
                        item_id: bid,
                        type_id: item.type_id,
                        owner_id: char_id,
                        location_id: char_id,
                        flag: InventoryFlag::Booster,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        dynamic_attributes: dyn_attrs,
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
                bid
            } else {
                if let Some(src) = snap.items.get_mut(&item_id) {
                    src.location_id = char_id;
                    src.flag = InventoryFlag::Booster;
                    src.singleton = true;
                    src.dynamic_attributes = dyn_attrs;
                    src.version += 1;
                }
                item_id
            };
            let ship_id = snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(0);
            Ok((
                active_booster_id,
                item.type_id,
                slot,
                duration_secs,
                expiry_unix,
                rolled_eids,
                ship_id,
            ))
        })?;

    if ship_id > 0 {
        ctx.invalidate_ship_fitting(ship_id);
    }
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnBoosterUpdated",
        "charid",
        vec![],
    );
    let side_effect_reps: Vec<PyRep> = rolled_eids
        .into_iter()
        .map(|eid| PyRep::Integer(eid as i64))
        .collect();
    Ok(PyRep::keyval([
        ("boosterID", PyRep::Integer(booster_item_id)),
        ("boosterTypeID", PyRep::Integer(type_id as i64)),
        ("typeID", PyRep::Integer(type_id as i64)),
        ("boosterSlot", PyRep::Integer(slot)),
        ("durationSeconds", PyRep::Integer(duration_secs as i64)),
        ("expiryTime", PyRep::Integer(unix_to_filetime(expiry_unix))),
        ("sideEffectIDs", PyRep::List(side_effect_reps)),
    ]))
}

/// `skillMgr2.GetBoosters()`: purges expired boosters and returns `{typeID: KeyVal(...)}` for active boosters.
pub fn get_boosters(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let expired_ships = ctx
        .db
        .with_mut_snapshot(|snap| purge_expired_boosters_in_snapshot(snap, Some(char_id), now_unix));
    for ship_id in expired_ships {
        ctx.invalidate_ship_fitting(ship_id);
    }
    let entries = ctx.db.with_snapshot(|snap| {
        let mut items: Vec<&InventoryItemRecord> = snap
            .items
            .values()
            .filter(|i| {
                i.owner_id == char_id
                    && i.location_id == char_id
                    && i.flag == InventoryFlag::Booster
            })
            .collect();
        items.sort_by_key(|i| i.item_id);
        items
            .into_iter()
            .map(|item| {
                let slot = booster_slot(&ctx.sde, item.type_id).unwrap_or(1);
                let expiry_unix = item
                    .dynamic_attributes
                    .get(&DYN_ATTR_BOOSTER_EXPIRY_UNIX)
                    .copied()
                    .unwrap_or(0.0) as u64;
                let duration_secs = item
                    .dynamic_attributes
                    .get(&DYN_ATTR_BOOSTER_DURATION_SECS)
                    .copied()
                    .unwrap_or(0.0) as i64;
                let mut side_effect_reps = Vec::new();
                for offset in 0..10_u16 {
                    if let Some(&val) = item
                        .dynamic_attributes
                        .get(&(DYN_ATTR_BOOSTER_SIDE_EFFECT_BASE + offset))
                    {
                        if val > 0.0 {
                            side_effect_reps.push(PyRep::Integer(val.round() as i64));
                        }
                    }
                }
                (
                    PyRep::Integer(item.type_id as i64),
                    PyRep::keyval([
                        ("boosterID", PyRep::Integer(item.item_id)),
                        ("boosterTypeID", PyRep::Integer(item.type_id as i64)),
                        ("typeID", PyRep::Integer(item.type_id as i64)),
                        ("boosterSlot", PyRep::Integer(slot)),
                        ("durationSeconds", PyRep::Integer(duration_secs)),
                        ("expiryTime", PyRep::Integer(unix_to_filetime(expiry_unix))),
                        ("sideEffectIDs", PyRep::List(side_effect_reps)),
                    ]),
                )
            })
            .collect()
    });
    Ok(PyRep::Dict(entries))
}

/// Adjusts `ship_base_attrs` for rolled vs unrolled booster side-effect penalties and
/// `Neurotoxin Control` (`25538`, `-5%`/level penalty reduction) (`feat:attributes.booster-side-effects`).
pub fn apply_active_booster_side_effect_adjustments(
    sde: &SdeRegistry,
    ship_base_attrs: &mut HashMap<u16, f64>,
    skill_levels: &HashMap<TypeId, u8>,
    active_boosters: &[TypeId],
    active_booster_side_effects: &HashMap<TypeId, Vec<u32>>,
) {
    let penalty_attrs: std::collections::HashSet<u16> = sde
        .types
        .get(&SKILL_NEUROTOXIN_CONTROL)
        .map(|t| {
            t.effect_ids
                .iter()
                .filter_map(|eid| sde.effects.get(eid))
                .flat_map(|eff| eff.modifiers.iter())
                .filter(|s| s.domain == "charID" && s.group_id == Some(303))
                .map(|s| s.modified_attribute_id)
                .collect()
        })
        .unwrap_or_default();
    let nc_lvl = skill_levels.get(&SKILL_NEUROTOXIN_CONTROL).copied().unwrap_or(0).min(5);
    let nc_bonus_pct = sde
        .types
        .get(&SKILL_NEUROTOXIN_CONTROL)
        .and_then(|t| t.default_attributes.get(&1126).copied())
        .unwrap_or(-5.0);
    let penalty_mult = (1.0 + (nc_bonus_pct / 100.0) * f64::from(nc_lvl)).max(0.0);

    for &b_tid in active_boosters {
        let Some(b_type) = sde.types.get(&b_tid) else {
            continue;
        };
        let rolled = active_booster_side_effects.get(&b_tid);
        for &eid in &b_type.effect_ids {
            let Some(eff) = sde.effects.get(&eid) else {
                continue;
            };
            if eff.effect_category != 0 {
                continue;
            }
            for spec in &eff.modifiers {
                if !penalty_attrs.contains(&spec.modifying_attribute_id) || spec.domain != "shipID" {
                    continue;
                }
                let base_pen = b_type
                    .default_attributes
                    .get(&spec.modifying_attribute_id)
                    .copied()
                    .unwrap_or(0.0);
                let unreduced_factor = 1.0 + base_pen / 100.0;
                if unreduced_factor.abs() < 1e-6 {
                    continue;
                }
                let is_rolled = rolled.is_none_or(|r| r.contains(&u32::from(eid)));
                let target_factor = if is_rolled {
                    1.0 + (base_pen * penalty_mult) / 100.0
                } else {
                    1.0
                };
                if let Some(val) = ship_base_attrs.get_mut(&spec.modified_attribute_id) {
                    *val *= target_factor / unreduced_factor;
                }
            }
        }
    }
}

/// Station-entry skill notification check (`skillMgr2.CheckAndSendNotifications`,
/// `notifications/client/development/skillHistoryProvider.pyj:22`) and non-diminishing
/// skill injection lookup (`nonDiminishingInjectionMgr.GetAvailableNonDiminishingInjections`,
/// `eve/client/script/ui/services/nonDiminishingInjectionService.pyj:24-31`).
pub fn handle_skill_notifications_and_injections(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("skillMgr2", "CheckAndSendNotifications") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            crate::services::character_skills::catch_up_skill_queue(ctx, char_id);
            Some(Ok(PyRep::None))
        }
        ("nonDiminishingInjectionMgr", "GetAvailableNonDiminishingInjections") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::Dict(Vec::new())))
        }
        _ => None,
    }
}

