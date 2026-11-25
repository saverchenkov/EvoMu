//! `crateService` loot, fitting, and skill bundle handlers (`OnCrateLootSpawned`, `OnSkillBundleInjected`).
#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::cosmetics_ui_ops::{load_cosmetics_ui_state, save_cosmetics_ui_state};
use crate::services::notify_emitters_character_services::emit_crate_loot_spawned;
use crate::services::notify_emitters_skills_trade_godma::emit_skill_bundle_injected;
use eve_db::{
    InventoryFlag, InventoryItemRecord, SkillHistoryEntry,
    TrainedSkillRecord, WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, ItemQty, PyRep};
use eve_sde::{attr_ids, TypeId};
use std::collections::HashMap;

fn err(code: &str, msg: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: msg.into(), client_error: None }
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

struct CachedCrateSpecs(std::sync::Arc<HashMap<u32, CrateLootSpec>>);

fn get_crate_specs(ctx: &ServerContext) -> std::sync::Arc<HashMap<u32, CrateLootSpec>> {
    ctx.sde
        .get_or_init_derived::<CachedCrateSpecs>(|| {
            let mut map = HashMap::new();
            if let Ok(cfsd) = ctx.sde.load_cfsd_nested("res:/staticdata/crates.fsdbinary") {
                for (_k, v) in cfsd {
                    let Some(type_id) = v
                        .get("typeID")
                        .and_then(|t| t.as_u64())
                        .and_then(|t| u32::try_from(t).ok())
                    else {
                        continue;
                    };
                    let mut items = Vec::new();
                    if let Some(list) = v.get("fixedLootItems").and_then(|l| l.as_array()) {
                        for item_val in list {
                            if let Some(tid) = item_val.as_u64().and_then(|t| u32::try_from(t).ok()) {
                                items.push((tid, 1));
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
                            if let Some(fid) = fit_val.as_u64().and_then(|f| u32::try_from(f).ok()) {
                                fittings.push(fid);
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
            CachedCrateSpecs(std::sync::Arc::new(map))
        })
        .0
        .clone()
}

fn crate_loot_spec(ctx: &ServerContext, type_id: u32) -> Option<CrateLootSpec> {
    let specs = get_crate_specs(ctx);
    let spec = specs.get(&type_id)?;
    if !spec.items.is_empty()
        || spec.skill_bundle.is_some()
        || spec.fitting_ship_type_id.is_some()
        || !spec.fittings.is_empty()
    {
        return Some(spec.clone());
    }
    None
}

/// Verifies crate ownership/reachability in `snap` and resolves its [`CrateLootSpec`] via `resolve_spec`.
fn require_owned_crate_with_spec<F>(
    snap: &WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    crate_id: i64,
    resolve_spec: F,
) -> Result<(InventoryItemRecord, CrateLootSpec), BridgeRpcError>
where
    F: FnOnce(u32) -> Option<CrateLootSpec>,
{
    let Some(item) = snap.items.get(&crate_id).cloned() else {
        return Err(err("CrateNotFound", format!("Crate {crate_id} not found in inventory")));
    };
    if item.owner_id != char_id {
        return Err(err("CrateNotFound", format!("Crate {crate_id} not owned by character")));
    }
    let current_dock = session
        .station_id
        .or(session.structure_id)
        .or_else(|| snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id)));
    let current_ship = session
        .ship_id
        .or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id).filter(|&sid| sid > 0));
    if !snap.is_item_consumable_by(&item, char_id, current_dock, current_ship) {
        return Err(err("InvalidCrateLocation", format!("Crate {crate_id} is not reachable in your current hangar/cargo or is locked in a contract")));
    }
    let Some(spec) = resolve_spec(item.type_id) else {
        return Err(err("InvalidCrateType", format!("Item {crate_id} (type {}) is not an openable crate", item.type_id)));
    };
    Ok((item, spec))
}

fn require_owned_crate(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    crate_id: i64,
) -> Result<(InventoryItemRecord, CrateLootSpec), BridgeRpcError> {
    require_owned_crate_with_spec(snap, session, char_id, crate_id, |type_id| crate_loot_spec(ctx, type_id))
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
        let active_ship = session.ship_id.or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id));
        if crate_item.quantity == 1 {
            claimed.push(crate_id);
        }
        snap.consume_exact_item_quantity(
            crate_id,
            1,
            eve_db::ConsumeExactOptions {
                active_ship_id: active_ship,
                ..eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), crate_item.owner_id)
            },
        )
        .map_err(|e| err("ItemLocked", e.to_string()))?;
        let mut first_delivered = (0_i64, 0_u32, 0_i32);
        for &(item_type, qty) in &spec.items {
            let granted = crate::domain::items::grant_item_in_snap(
                None,
                snap,
                None,
                crate::domain::items::GrantItemRequest::explicit(
                    char_id,
                    allowed_dest_id,
                    InventoryFlag::Hangar,
                    item_type,
                    qty,
                ),
            )
            .map_err(crate::services::domain_err_to_rpc)?;
            if first_delivered.0 == 0 {
                first_delivered = (granted.item.item_id, item_type, qty);
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
    let rpc = crate::router::RpcArgs::positional(args);
    let crate_id = rpc.req_positive_i64(0, "crateID").map_err(|_| err("CrateNotFound", "Valid crate item ID required"))?;
    let fitting_id = rpc.req_positive_i64(1, "fittingID").map_err(|_| err("InvalidFittingClaim", "Valid opened crate ID and fitting ID required"))?;
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
        if !u32::try_from(fitting_id).is_ok_and(|fid| spec.fittings.contains(&fid)) {
            return Err(err("InvalidCrateFitting", format!("Fitting {fitting_id} is not valid for crate type {}", crate_item.type_id)));
        }
        let claimed = ext.claimed_crate_loot.entry(char_id).or_default();
        ext.opened_crates.remove(&crate_id);
        let active_ship = session.ship_id.or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id));
        if crate_item.quantity <= 1 {
            claimed.push(key);
            claimed.push(crate_id);
        }
        snap.consume_exact_item_quantity(
            crate_id,
            1,
            eve_db::ConsumeExactOptions {
                active_ship_id: active_ship,
                ..eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), crate_item.owner_id)
            },
        )
        .map_err(|e| err("ItemLocked", e.to_string()))?;
        let ship_item_id = crate::domain::items::grant_item_in_snap(
            None,
            snap,
            None,
            crate::domain::items::GrantItemRequest::explicit(
                char_id,
                dest_id,
                InventoryFlag::Hangar,
                ship_type_id,
                1,
            ),
        )
        .map_err(crate::services::domain_err_to_rpc)?
        .item
        .item_id;
        save_cosmetics_ui_state(snap, &ext);
        Ok(PyRep::keyval([("crateID", PyRep::Integer(crate_id)), ("fittingID", PyRep::Integer(fitting_id)), ("deliveredItemID", PyRep::Integer(ship_item_id)), ("claimed", PyRep::Bool(true))]))
    })
}

/// Computes per-entry loot stack spawn `(type_id, quantity)` pairs for opening `actual_qty` crates.
fn calculate_crate_stack_spawns(
    items: &[(u32, i32)],
    actual_qty: ItemQty,
) -> Result<Vec<(u32, i32)>, BridgeRpcError> {
    items
        .iter()
        .map(|&(item_type, base_qty)| {
            let spawn_qty = ItemQty::try_from_i32(base_qty)
                .and_then(|base| base.checked_mul(actual_qty))
                .and_then(ItemQty::try_into_stack_i32)
                .map_err(|e| e.into_rpc_error("InvalidQuantity"))?;
            Ok((item_type, spawn_qty))
        })
        .collect()
}

/// Core transactional handler for `ClaimLootFromCrateStack` parameterized by crate spec lookup.
fn handle_crate_claim_loot_stack_with_spec<F>(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    resolve_spec: F,
) -> Result<PyRep, BridgeRpcError>
where
    F: FnOnce(u32) -> Option<CrateLootSpec>,
{
    let (_, char_id) = ServerContext::require_character(session)?;
    let rpc = crate::router::RpcArgs::positional(args);
    let crate_id = rpc.req_positive_i64(0, "crateID").map_err(|_| err("CrateNotFound", "Valid crate item ID required"))?;
    let qty_raw = rpc.opt_positive_i64(1, "quantity")?.unwrap_or(1);
    let requested_qty = ItemQty::try_from_i64(qty_raw).map_err(|e| e.into_rpc_error("InvalidQuantity"))?;
    let qty = requested_qty
        .try_into_stack_i32()
        .map_err(|e| e.into_rpc_error("InvalidQuantity"))?;
    let session_dock = session.station_id.or(session.structure_id);
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let dest_id = snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id)).or(session_dock)
            .ok_or_else(|| err("DockingRequired", "Must be docked in a station or structure to claim crate loot"))?;
        let (crate_item, spec) = require_owned_crate_with_spec(snap, session, char_id, crate_id, resolve_spec)?;
        if spec.items.is_empty() {
            return Err(err("InvalidCrateType", format!("Crate {crate_id} (type {}) does not contain fixed loot items", crate_item.type_id)));
        }
        let actual_to_open = qty.min(crate_item.quantity);
        if actual_to_open <= 0 {
            return Err(err("NoCratesToOpen", "No crates available in stack"));
        }
        let actual_qty = ItemQty::try_from_i32(actual_to_open)
            .map_err(|e| e.into_rpc_error("InvalidQuantity"))?;
        let active_ship = session.ship_id.or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id));
        snap.consume_exact_item_quantity(
            crate_id,
            actual_to_open,
            eve_db::ConsumeExactOptions {
                active_ship_id: active_ship,
                ..eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), crate_item.owner_id)
            },
        )
        .map_err(|e| err("ItemLocked", e.to_string()))?;
        for (item_type, spawn_qty) in calculate_crate_stack_spawns(&spec.items, actual_qty)? {
            crate::domain::items::grant_item_in_snap(
                None,
                snap,
                None,
                crate::domain::items::GrantItemRequest::explicit(
                    char_id,
                    dest_id,
                    InventoryFlag::Hangar,
                    item_type,
                    spawn_qty,
                ),
            )
            .map_err(crate::services::domain_err_to_rpc)?;
        }
        Ok(PyRep::Tuple(vec![PyRep::Integer(i64::from(actual_to_open)), PyRep::Integer(requested_qty.as_i64())]))
    })?;
    emit_crate_loot_spawned(ctx, char_id);
    Ok(res)
}

fn handle_crate_claim_loot_stack(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    handle_crate_claim_loot_stack_with_spec(ctx, session, args, |type_id| crate_loot_spec(ctx, type_id))
}

fn handle_crate_claim_skill_bundle(ctx: &ServerContext, session: &SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let rpc = crate::router::RpcArgs::positional(args);
    let crate_id = rpc.req_positive_i64(0, "crateID").map_err(|_| err("InvalidSkillBundle", "Valid opened crate and skill bundle ID required"))?;
    let bundle_id = rpc.req_positive_i64(1, "bundleID").map_err(|_| err("InvalidSkillBundle", "Valid opened crate and skill bundle ID required"))?;
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
        let active_ship = session.ship_id.or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id));
        if crate_item.quantity <= 1 {
            claimed.push(key);
            claimed.push(crate_id);
        }
        snap.consume_exact_item_quantity(
            crate_id,
            1,
            eve_db::ConsumeExactOptions {
                active_ship_id: active_ship,
                ..eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), crate_item.owner_id)
            },
        )
        .map_err(|e| err("ItemLocked", e.to_string()))?;
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
        ctx.emit_typed(
            NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnFreeSkillPointsChanged {
                new_free_skill_points: new_sp,
            },
        );
    }
    Ok(rep)
}

#[cfg(test)] #[path = "crate_loot_service_tests.rs"] mod crate_loot_service_tests;

#[cfg(test)]
#[path = "crate_loot_service_edges_tests.rs"]
mod crate_loot_service_edges_tests;

#[cfg(test)]
mod qty_tests {
    use super::*;
    use crate::services::account_services::account_services_tests::{auth_session, test_ctx};

    #[test]
    fn calculate_crate_stack_spawns_rejects_overflow_and_accepts_stack_max_boundary() {
        let max_half = ItemQty::try_from_i32(i32::MAX / 2).unwrap();
        assert_eq!(
            calculate_crate_stack_spawns(&[(34, 2)], max_half).unwrap(),
            vec![(34, i32::MAX - 1)]
        );

        let overflow_half = ItemQty::try_from_i32(i32::MAX / 2 + 1).unwrap();
        let err_two = calculate_crate_stack_spawns(&[(34, 1), (35, 2)], overflow_half).unwrap_err();
        assert_eq!(err_two.code, "InvalidQuantity");

        let overflow_three = ItemQty::try_from_i32(i32::MAX / 3 + 1).unwrap();
        let err_three = calculate_crate_stack_spawns(&[(34, 3)], overflow_three).unwrap_err();
        assert_eq!(err_three.code, "InvalidQuantity");

        let err_non_pos = calculate_crate_stack_spawns(&[(34, 0)], ItemQty::ONE).unwrap_err();
        assert_eq!(err_non_pos.code, "InvalidQuantity");
    }

    #[test]
    fn claim_loot_stack_rejects_over_stack_max_and_spawn_overflow() {
        let ctx = test_ctx();
        let cid = 90_000_001_i64;
        let station_id = 60_003_760_i64;
        let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
        sess.station_id = Some(station_id);

        let specs = get_crate_specs(&ctx);
        let (&loot_crate_tid, _) = specs
            .iter()
            .find(|(_, s)| !s.items.is_empty())
            .expect("SDE must have a crate with fixedLootItems");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = Some(station_id);
            }
            snap.items.insert(
                939_901,
                InventoryItemRecord {
                    item_id: 939_901,
                    type_id: loot_crate_tid,
                    owner_id: cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 5,
                    ..Default::default()
                },
            );
            snap.items.insert(
                939_902,
                InventoryItemRecord {
                    item_id: 939_902,
                    type_id: loot_crate_tid,
                    owner_id: cid,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: i32::MAX,
                    ..Default::default()
                },
            );
        });

        let char_uid = ctx.db.with_snapshot(|s| s.characters[&cid].user_id);
        ctx.db
            .acquire_online_lock(char_uid, 501, Some(cid), false, ctx.now_unix())
            .unwrap();

        let over_err = handle_crate_service(
            &ctx,
            &sess,
            "ClaimLootFromCrateStack",
            &[
                PyRep::Integer(939_901),
                PyRep::Integer(i64::from(i32::MAX) + 1),
            ],
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(over_err.code, "InvalidQuantity");
        assert_eq!(
            ctx.db.with_snapshot(|s| s.items.get(&939_901).map(|i| i.quantity)),
            Some(5)
        );

        let multi_spec = CrateLootSpec {
            items: vec![(34, 1), (35, 2)],
            skill_bundle: None,
            fitting_ship_type_id: None,
            fittings: Vec::new(),
        };
        let (before_count, before_next_id, before_crate_ver) = ctx.db.with_snapshot(|s| {
            (
                s.items.len(),
                s.next_entity_id,
                s.items.get(&939_902).map(|i| i.version).unwrap(),
            )
        });
        let _ = ctx.drain_notifications_for_char(cid);

        let overflow_open_count = i64::from(i32::MAX / 2) + 1;
        let spawn_err = handle_crate_claim_loot_stack_with_spec(
            &ctx,
            &sess,
            &[PyRep::Integer(939_902), PyRep::Integer(overflow_open_count)],
            |tid| {
                assert_eq!(tid, loot_crate_tid);
                Some(multi_spec.clone())
            },
        )
        .unwrap_err();
        assert_eq!(spawn_err.code, "InvalidQuantity");
        assert_eq!(
            ctx.db.with_snapshot(|s| s.items.get(&939_902).map(|i| (i.quantity, i.version))),
            Some((i32::MAX, before_crate_ver))
        );
        assert_eq!(
            ctx.db.with_snapshot(|s| (s.items.len(), s.next_entity_id)),
            (before_count, before_next_id)
        );
        assert!(ctx.drain_notifications_for_char(cid).is_empty());

        let ok_res = handle_crate_claim_loot_stack_with_spec(
            &ctx,
            &sess,
            &[PyRep::Integer(939_902), PyRep::Integer(3)],
            |tid| {
                assert_eq!(tid, loot_crate_tid);
                Some(multi_spec.clone())
            },
        )
        .unwrap();
        assert_eq!(
            ok_res,
            PyRep::Tuple(vec![PyRep::Integer(3), PyRep::Integer(3)])
        );
        assert_eq!(
            ctx.db.with_snapshot(|s| s.items.get(&939_902).map(|i| i.quantity)),
            Some(i32::MAX - 3)
        );
        let spawned: Vec<(u32, i32, i64, InventoryFlag)> = ctx.db.with_snapshot(|s| {
            let mut entries: Vec<_> = s
                .items
                .values()
                .filter(|i| i.item_id > before_next_id)
                .map(|i| (i.type_id, i.quantity, i.location_id, i.flag))
                .collect();
            entries.sort_by_key(|e| e.0);
            entries
        });
        assert_eq!(
            spawned,
            vec![
                (34, 3, station_id, InventoryFlag::Hangar),
                (35, 6, station_id, InventoryFlag::Hangar),
            ]
        );
        let notifs = ctx.drain_notifications_for_char(cid);
        assert!(notifs.iter().any(|n| n.event == "OnCrateLootSpawned"));
    }
}

