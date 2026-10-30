//! `crateService` loot, fitting, and skill bundle handlers (`OnCrateLootSpawned`, `OnSkillBundleInjected`).
#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::cosmetics_ui_ops::{load_cosmetics_ui_state, save_cosmetics_ui_state};
use crate::services::notify_emitters_batch3::emit_crate_loot_spawned;
use crate::services::notify_emitters_batch9::{emit_server_brain_updated, emit_skill_bundle_injected};
use eve_db::{
    InventoryFlag, InventoryItemRecord, SkillHistoryEntry,
    TrainedSkillRecord, WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, TypeId};
use std::collections::HashMap;

fn err(code: &str, msg: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: msg.into() }
}

/// Extracted `crateService` dispatcher emitting `OnCrateLootSpawned` on crate open / loot claim.
// parity: notify:OnCrateLootSpawned
pub fn handle_crate_service(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match method {
        "OpenCrate" => Some(handle_crate_open(ctx, session, args)),
        "ClaimLoot" => Some(handle_crate_claim_loot(ctx, session, args)),
        "ClaimSkillBundle" => Some(handle_crate_claim_skill_bundle(ctx, session, args)),
        "ClaimFitting" => Some(handle_crate_claim_fitting(ctx, session, args)),
        "ClaimLootFromCrateStack" => Some(handle_crate_claim_loot_stack(ctx, session, args)),
        "GetCrateLocation" => Some(handle_crate_get_location(ctx, session, args)),
        "PeekInsideFixedCrate" => Some(handle_crate_peek(ctx, session, args)),
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct CrateLootSpec {
    items: Vec<(u32, i32)>,
    skill_bundle: Option<(i64, f64)>,
    fitting_ship_type_id: Option<u32>,
    fittings: Vec<u32>,
}

fn get_crate_specs(ctx: &ServerContext) -> &'static HashMap<u32, CrateLootSpec> {
    static CRATE_SPECS: std::sync::OnceLock<HashMap<u32, CrateLootSpec>> = std::sync::OnceLock::new();
    CRATE_SPECS.get_or_init(|| {
        let mut map = HashMap::new();
        if let Ok(cfsd) = ctx.sde.load_cfsd_nested("res:/staticdata/crates.fsdbinary") {
            for (_k, v) in cfsd {
                let Some(type_id) = v.get("typeID").and_then(|t| t.as_u64()).map(|t| t as u32) else {
                    continue;
                };
                let mut items = Vec::new();
                if let Some(list) = v.get("fixedLootItems").and_then(|l| l.as_array()) {
                    for item_val in list {
                        if let Some(tid) = item_val.as_u64() {
                            items.push((tid as u32, 1));
                        }
                    }
                }
                let skill_bundle = v
                    .get("skillBundleID")
                    .and_then(|b| b.as_i64())
                    .map(|bid| (bid, 25_000.0));
                let mut fitting_ship_type_id = None;
                let mut fittings = Vec::new();
                if let Some(fits) = v.get("fittings").and_then(|f| f.as_array()) {
                    for fit_val in fits {
                        if let Some(fid) = fit_val.as_u64() {
                            fittings.push(fid as u32);
                        }
                    }
                    if !fits.is_empty() {
                        if let Some(crate_type) = ctx.sde.types.get(&type_id) {
                            let name_lower = crate_type.name.to_lowercase();
                            let mut matches: Vec<_> = ctx
                                .sde
                                .types
                                .values()
                                .filter(|t| t.category_id == 6 && t.published && name_lower.contains(&t.name.to_lowercase()))
                                .collect();
                            matches.sort_by_key(|t| std::cmp::Reverse(t.name.len()));
                            if let Some(ship) = matches.first() {
                                fitting_ship_type_id = Some(ship.type_id);
                            }
                        }
                    }
                }
                map.insert(type_id, CrateLootSpec { items, skill_bundle, fitting_ship_type_id, fittings });
            }
        }
        map
    })
}

fn crate_loot_spec(ctx: &ServerContext, type_id: u32) -> Option<CrateLootSpec> {
    if let Some(spec) = get_crate_specs(ctx).get(&type_id) {
        if !spec.items.is_empty() || spec.skill_bundle.is_some() || spec.fitting_ship_type_id.is_some() || !spec.fittings.is_empty() {
            return Some(spec.clone());
        }
    }
    let t = ctx.sde.types.get(&type_id)?;
    let is_crate = (t.category_id == 17 && t.group_id == 1818)
        || (t.category_id == 63 && (t.group_id == 1194 || t.group_id == 4093 || t.group_id == 4139))
        || (t.name.contains("Crate") || t.name.contains("Box") || t.name.contains("Cache") || t.name.contains("Bundle") || t.name.contains("Package"));
    if !is_crate {
        return None;
    }
    if let Some(mats) = ctx.sde.type_materials.get(&type_id).filter(|m| !m.is_empty()) {
        let items = mats.iter().map(|m| (m.material_type_id, m.quantity.max(1) as i32)).collect();
        return Some(CrateLootSpec { items, skill_bundle: None, fitting_ship_type_id: None, fittings: Vec::new() });
    }
    None
}

fn require_owned_crate(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    crate_id: i64,
) -> Result<(InventoryItemRecord, CrateLootSpec), BridgeRpcError> {
    let Some(item) = snap.items.get(&crate_id).cloned() else {
        return Err(err("CrateNotFound", format!("Crate {crate_id} not found in inventory")));
    };
    if item.owner_id != char_id {
        return Err(err("CrateNotFound", format!("Crate {crate_id} not owned by character")));
    }
    let current_dock = snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id)).or_else(|| session.station_id.or(session.structure_id));
    let current_ship = session.ship_id.or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id).filter(|&sid| sid > 0));
    if !snap.is_item_consumable_by(&item, char_id, current_dock, current_ship) {
        return Err(err("InvalidCrateLocation", format!("Crate {crate_id} is not reachable in your current hangar/cargo or is locked in a contract")));
    }
    let Some(spec) = crate_loot_spec(ctx, item.type_id) else {
        return Err(err("InvalidCrateType", format!("Item {crate_id} (type {}) is not an openable crate", item.type_id)));
    };
    Ok((item, spec))
}

fn handle_crate_open(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 {
        return Err(err("CrateNotFound", "Valid crate item ID required"));
    }
    let now_unix = ctx.now_unix();
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let (_item, spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        let mut ext = load_cosmetics_ui_state(snap);
        ext.opened_crates.insert(crate_id, now_unix);
        save_cosmetics_ui_state(snap, &ext);
        let loot_items = spec.items.iter().map(|&(tid, q)| PyRep::keyval([("typeID", PyRep::Integer(i64::from(tid))), ("quantity", PyRep::Integer(i64::from(q)))])).collect();
        let bundle_rep = spec.skill_bundle.map(|(bid, _)| PyRep::Integer(bid)).unwrap_or(PyRep::None);
        Ok(PyRep::keyval([("crateID", PyRep::Integer(crate_id)), ("opened", PyRep::Bool(true)), ("lootItems", PyRep::List(loot_items)), ("skillBundleID", bundle_rep)]))
    })?;
    emit_crate_loot_spawned(ctx, char_id);
    Ok(res)
}

fn handle_crate_claim_loot(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 {
        return Err(err("CrateNotFound", "Valid crate item ID required"));
    }
    let session_dock = session.station_id.or(session.structure_id);
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        if !ext.opened_crates.contains_key(&crate_id) {
            return Err(err("CrateNotOpened", format!("Crate {crate_id} has not been opened")));
        }
        let allowed_dest_id = snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id)).or(session_dock)
            .ok_or_else(|| err("DockingRequired", "Must be docked in a station or structure to claim crate loot"))?;
        let (crate_item, spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        if args.get(1).and_then(|a| a.as_i64()).is_some_and(|d| d != allowed_dest_id) {
            return Err(err("InvalidCrateLocation", "Crate loot can only be delivered to the character's current station hangar"));
        }
        if args.get(2).and_then(|a| a.as_i64()).is_some_and(|f| f != i64::from(InventoryFlag::Hangar as u16)) {
            return Err(err("InvalidCrateLocation", "Crate loot can only be delivered into the station hangar"));
        }
        let claimed = ext.claimed_crate_loot.entry(char_id).or_default();
        if claimed.contains(&crate_id) {
            return Err(err("LootAlreadyClaimed", format!("Loot from crate {crate_id} has already been claimed")));
        }
        ext.opened_crates.remove(&crate_id);
        if crate_item.quantity > 1 {
            if let Some(it_mut) = snap.items.get_mut(&crate_id) {
                it_mut.quantity -= 1;
                it_mut.version += 1;
            }
        } else {
            claimed.push(crate_id);
            snap.items.remove(&crate_id);
        }
        let mut first_delivered = (0_i64, 0_u32, 0_i32);
        for &(item_type, qty) in &spec.items {
            snap.next_entity_id += 1;
            let new_item_id = snap.next_entity_id;
            snap.items.insert(new_item_id, InventoryItemRecord {
                item_id: new_item_id, type_id: item_type, owner_id: char_id, location_id: allowed_dest_id,
                flag: InventoryFlag::Hangar, quantity: qty, singleton: false, ..Default::default()
            });
            if first_delivered.0 == 0 {
                first_delivered = (new_item_id, item_type, qty);
            }
        }
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::keyval([("crateID", PyRep::Integer(crate_id)), ("deliveredItemID", PyRep::Integer(first_delivered.0)), ("typeID", PyRep::Integer(i64::from(first_delivered.1))), ("quantity", PyRep::Integer(i64::from(first_delivered.2)))]))
    })?;
    emit_crate_loot_spawned(ctx, char_id);
    Ok(res)
}

fn handle_crate_get_location(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 {
        return Err(err("CrateNotFound", "Valid crate item ID required"));
    }
    ctx.db.with_snapshot(|snap| {
        let (crate_item, _spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        let dest_id = snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id)).unwrap_or(crate_item.location_id);
        Ok(PyRep::Tuple(vec![PyRep::Integer(dest_id), PyRep::Integer(4)]))
    })
}

fn handle_crate_peek(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 {
        return Err(err("CrateNotFound", "Valid crate item ID required"));
    }
    ctx.db.with_snapshot(|snap| {
        let (_crate_item, spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        let entries = spec.items.into_iter().map(|(tid, qty)| PyRep::Tuple(vec![PyRep::Integer(i64::from(tid)), PyRep::Integer(i64::from(qty)), PyRep::None])).collect();
        Ok(PyRep::List(entries))
    })
}

fn handle_crate_claim_fitting(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let fitting_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 || fitting_id <= 0 {
        return Err(err("InvalidFittingClaim", "Valid opened crate ID and fitting ID required"));
    }
    let session_dock = session.station_id.or(session.structure_id);
    ctx.db.try_with_mut_snapshot(|snap| {
        let dest_id = snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id)).or(session_dock)
            .ok_or_else(|| err("DockingRequired", "Must be docked in a station or structure to claim crate fitting"))?;
        let mut ext = load_cosmetics_ui_state(snap);
        let key = crate_id.saturating_mul(10_000).saturating_add(fitting_id);
        if ext.claimed_crate_loot.get(&char_id).map(|v| v.contains(&key)).unwrap_or(false) {
            return Err(err("FittingAlreadyClaimed", format!("Fitting {fitting_id} from crate {crate_id} already claimed")));
        }
        if !ext.opened_crates.contains_key(&crate_id) {
            return Err(err("CrateNotOpened", format!("Crate {crate_id} has not been opened")));
        }
        let (crate_item, spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        let ship_type_id = spec.fitting_ship_type_id.ok_or_else(|| err("InvalidCrateFitting", "This crate does not contain a ship fitting"))?;
        if !spec.fittings.contains(&(fitting_id as u32)) {
            return Err(err("InvalidCrateFitting", format!("Fitting {fitting_id} is not valid for crate type {}", crate_item.type_id)));
        }
        let claimed = ext.claimed_crate_loot.entry(char_id).or_default();
        ext.opened_crates.remove(&crate_id);
        if crate_item.quantity > 1 {
            if let Some(it_mut) = snap.items.get_mut(&crate_id) {
                it_mut.quantity -= 1;
                it_mut.version += 1;
            }
        } else {
            claimed.push(key);
            claimed.push(crate_id);
            snap.items.remove(&crate_id);
        }
        snap.next_entity_id += 1;
        let ship_item_id = snap.next_entity_id;
        snap.items.insert(ship_item_id, InventoryItemRecord {
            item_id: ship_item_id, type_id: ship_type_id, owner_id: char_id, location_id: dest_id,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: false, ..Default::default()
        });
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::keyval([("crateID", PyRep::Integer(crate_id)), ("fittingID", PyRep::Integer(fitting_id)), ("deliveredItemID", PyRep::Integer(ship_item_id)), ("claimed", PyRep::Bool(true))]))
    })
}

fn handle_crate_claim_loot_stack(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let qty = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1).clamp(1, i64::from(i32::MAX)) as i32;
    if crate_id <= 0 {
        return Err(err("CrateNotFound", "Valid crate item ID required"));
    }
    let session_dock = session.station_id.or(session.structure_id);
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let dest_id = snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id)).or(session_dock)
            .ok_or_else(|| err("DockingRequired", "Must be docked in a station or structure to claim crate loot"))?;
        let (crate_item, spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        if spec.items.is_empty() {
            return Err(err("InvalidCrateType", format!("Crate {crate_id} (type {}) does not contain fixed loot items", crate_item.type_id)));
        }
        let actual_to_open = qty.min(crate_item.quantity);
        if actual_to_open <= 0 {
            return Err(err("NoCratesToOpen", "No crates available in stack"));
        }
        if crate_item.quantity <= actual_to_open {
            snap.items.remove(&crate_id);
        } else if let Some(item_mut) = snap.items.get_mut(&crate_id) {
            item_mut.quantity -= actual_to_open;
            item_mut.version += 1;
        }
        for (item_type, base_qty) in spec.items {
            snap.next_entity_id += 1;
            let new_item_id = snap.next_entity_id;
            snap.items.insert(new_item_id, InventoryItemRecord {
                item_id: new_item_id, type_id: item_type, owner_id: char_id, location_id: dest_id,
                flag: InventoryFlag::Hangar, quantity: base_qty.saturating_mul(actual_to_open), singleton: false, ..Default::default()
            });
        }
        Ok(PyRep::Tuple(vec![PyRep::Integer(actual_to_open as i64), PyRep::Integer(qty as i64)]))
    })?;
    emit_crate_loot_spawned(ctx, char_id);
    Ok(res)
}

fn handle_crate_claim_skill_bundle(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let crate_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let bundle_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
    if crate_id <= 0 || bundle_id <= 0 {
        return Err(err("InvalidSkillBundle", "Valid opened crate and skill bundle ID required"));
    }
    let session_dock = session.station_id.or(session.structure_id);
    let now_ft = ctx.now_blueos_filetime_ticks();
    let (sp_grant, injected_skills, new_sp, rep) = ctx.db.try_with_mut_snapshot(|snap| {
        let _dock_loc = snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id)).or(session_dock)
            .ok_or_else(|| err("DockingRequired", "Must be docked in a station or structure to claim crate skill bundle"))?;
        let mut ext = load_cosmetics_ui_state(snap);
        if !ext.opened_crates.contains_key(&crate_id) {
            return Err(err("CrateNotOpened", format!("Crate {crate_id} has not been opened")));
        }
        let (crate_item, spec) = require_owned_crate(ctx, snap, session, char_id, crate_id)?;
        let Some((expected_bundle_id, sp_grant)) = spec.skill_bundle else {
            return Err(err("InvalidSkillBundle", format!("Crate {crate_id} does not contain a skill bundle")));
        };
        if bundle_id != expected_bundle_id {
            return Err(err("InvalidSkillBundle", format!("Skill bundle {bundle_id} does not match crate bundle {expected_bundle_id}")));
        }
        let claimed = ext.claimed_crate_loot.entry(char_id).or_default();
        let key = crate_id.saturating_mul(1000).saturating_add(bundle_id);
        if claimed.contains(&key) || claimed.contains(&crate_id) {
            return Err(err("SkillBundleAlreadyClaimed", format!("Skill bundle {bundle_id} from crate {crate_id} already claimed")));
        }
        ext.opened_crates.remove(&crate_id);
        if crate_item.quantity > 1 {
            if let Some(it_mut) = snap.items.get_mut(&crate_id) {
                it_mut.quantity -= 1;
                it_mut.version += 1;
            }
        } else {
            claimed.push(key);
            claimed.push(crate_id);
            snap.items.remove(&crate_id);
        }
        // DATA GAP: V24 crates.fsdbinary defines skillBundleID but not per-bundle skill lists.
        let bundle_skills: Vec<TypeId> = spec
            .items
            .iter()
            .filter_map(|&(tid, _)| {
                ctx.sde
                    .types
                    .get(&tid)
                    .filter(|t| t.category_id == 16 && t.published)
                    .map(|_| tid)
            })
            .collect();
        let mut injected = 0_i64;
        if !bundle_skills.is_empty() {
            let skills_map = snap.skills.entry(char_id).or_default();
            let hist = snap.skill_history.entry(char_id).or_default();
            for tid in bundle_skills {
                if !skills_map.contains_key(&tid) {
                    let rank = ctx
                        .sde
                        .types
                        .get(&tid)
                        .and_then(|t| t.default_attributes.get(&attr_ids::SKILL_TIME_CONSTANT).copied())
                        .unwrap_or(1.0);
                    let sp_lvl1 = crate::formulas::sp_for_skill_level(rank, 1);
                    skills_map.insert(tid, TrainedSkillRecord { type_id: tid, level: 1, skill_points: sp_lvl1 });
                    hist.push(SkillHistoryEntry { log_date_ft: now_ft, event_type_id: 36, skill_type_id: tid, skill_level: 1, absolute_points: sp_lvl1 });
                    injected += 1;
                }
            }
        }
        let ch = snap.characters.get_mut(&char_id).ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        ch.free_skill_points += sp_grant;
        let new_sp = ch.free_skill_points;
        save_cosmetics_ui_state(snap, &ext);
        Ok((
            sp_grant,
            injected,
            new_sp,
            PyRep::keyval([
                ("crateID", PyRep::Integer(crate_id)),
                ("skillBundleID", PyRep::Integer(bundle_id)),
                ("spGranted", PyRep::Real(sp_grant)),
                ("freeSkillPoints", PyRep::Real(new_sp)),
            ]),
        ))
    })?;
    if injected_skills > 0 {
        emit_skill_bundle_injected(ctx, char_id, injected_skills, sp_grant.round() as i64);
        emit_server_brain_updated(ctx, char_id);
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnFreeSkillPointsChanged",
            "charid",
            vec![PyRep::Real(new_sp)],
        );
    }
    Ok(rep)
}
