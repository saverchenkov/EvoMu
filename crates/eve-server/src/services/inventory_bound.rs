//! `invbroker` inventory handles requested through the bound location inventory manager
//! (T-6 `journey:launch_login` station entry).
//!
//! The client binds `invbroker` to its location (`eveMoniker.GetInventoryMgr` →
//! `GetStationInventoryMgr(stationID)` = `Moniker('invbroker', (stationID, const.groupStation))`,
//! `eve/common/script/net/eveMoniker.pyj:149-161`) and then asks that moniker for inventories:
//! - `invCache.GetInventoryFromId(itemid, passive, locationID)` (`eve/client/script/environment/invCache.pyj:590`)
//!   calls `inventoryMgr.GetInventoryFromId(itemid, passive)` and wraps the result in an
//!   `invCacheContainer`, whose `List()` / `ListByFlags()` are sent to the returned remote object.
//! - `invCache.GetInventory(containerid, param1)` (`invCache.pyj:553-588`) calls
//!   `inventoryMgr.GetInventory(containerid, param1)` for the special containers
//!   (`inventorycommon/const.pyj`: `containerHangar = 10004`, ...).
//!
//! Both return a bound object reference (`ServerContext::bound_object_rep`); calls on it reach the
//! existing `invbroker` handlers with `bound_entry.object_id` = the inventory's location ID.
//! - `invCacheContainer._SetItemData` (`invCache.pyj:1046-1053`) calls `moniker.GetSelfInvItem()`
//!   on such an object and reads `.itemID` / `.typeID` from the returned inventory row.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use std::collections::HashMap;

/// `inventorycommon/const.pyj: containerGlobal` — global / asset-safety station container.
const CONTAINER_GLOBAL: i64 = 10_002;
/// `inventorycommon/const.pyj: containerHangar` — the character's hangar at the docked station.
const CONTAINER_HANGAR: i64 = 10_004;
/// `inventorycommon/const.pyj: containerCorpMarket` — corporation deliveries at the docked station.
const CONTAINER_CORP_MARKET: i64 = 10_012;

/// Error code for calls that arrive without the location-bound moniker.
const ERR_NOT_BOUND: &str = "InvalidBoundObject";

fn err(code: &str, message: String) -> Option<Result<PyRep, BridgeRpcError>> {
    Some(Err(BridgeRpcError { code: code.into(), message }))
}

pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    caller_trace: Option<&str>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    // rpc-lint: allow devtools svc_dgmattr.pyj AttributeInspector remote call
    if service == "i2" {
        if !session.has_gm_role() {
            return Some(Err(BridgeRpcError {
                code: "GMRoleRequired".into(),
                message: "i2 requires GM role".into(),
            }));
        }
        let _ = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let target_id = args
            .first()
            .and_then(|a| a.as_i64())
            // rpc-lint: allow devtools svc_dgmattr.pyj AttributeInspector remote call
            .or_else(|| bound_entry.filter(|b| b.service == "i2").map(|b| b.object_id));
        return match method {
            "IsPrimed" => Some(handle_i2_is_primed(ctx, session, target_id)),
            "GetItem" => Some(handle_i2_get_item(ctx, session, target_id)),
            _ => None,
        };
    }
    if service != "invbroker"
        || !matches!(
            method,
            "GetInventoryFromId"
                | "GetInventory"
                | "GetSelfInvItem"
                | "GetAvailableTurretSlots"
        )
    {
        return None;
    }
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    // These methods live on bound inventory objects, never on the unbound service.
    let Some(location_id) = bound_entry.filter(|b| b.service == "invbroker").map(|b| b.object_id) else {
        return err(
            ERR_NOT_BOUND,
            format!("{service}.{method} must be called on the location-bound inventory manager"),
        );
    };
    match method {
        "GetSelfInvItem" => Some(self_inv_item(ctx, session, char_id, location_id)),
        "GetAvailableTurretSlots" => {
            // `monolith_task_tracker/client/eventHandler.pyj:38-46` (`OnHighSlotFittingChanged`):
            // calls `sm.GetService('invCache').GetInventoryFromId(shipID).GetAvailableTurretSlots()`
            // and compares `numberOfFreeSlots >= 2`.
            if crate::services::fitting_containers_ships::resolve_missing_dynamic_ship_stub_type_id(
                ctx,
                char_id,
                location_id,
            )
            .is_some()
            {
                return Some(Ok(PyRep::Integer(0)));
            }
            let ship_id = ctx
                .db
                .get_item(location_id)
                .filter(|i| i.owner_id == char_id)
                .map(|i| i.item_id)
                .or(session.ship_id);
            let Some(ship_id) = ship_id else {
                return Some(Ok(PyRep::Integer(0)));
            };
            match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(fit) => {
                    let max_turrets = fit
                        .ship
                        .get_attribute(eve_sde::attr_ids::TURRET_SLOTS_LEFT, &ctx.sde)
                        .max(0.0) as i64;
                    let fitted_turrets = fit
                        .modules
                        .values()
                        .filter(|m| {
                            ctx.sde
                                .types
                                .get(&m.type_id)
                                .is_some_and(|t| t.effect_ids.contains(&42))
                        })
                        .count() as i64;
                    Some(Ok(PyRep::Integer((max_turrets - fitted_turrets).max(0))))
                }
                Err(e) => Some(Err(e)),
            }
        }
        "GetInventoryFromId" => {
            // `passive` (args[1]) only controls client-side lazy priming; the server hands out the
            // same inventory either way.
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return err("InvalidArguments", "invbroker.GetInventoryFromId expects (itemID, passive)".into());
            };
            let docked = session.station_id.or(session.structure_id);
            let at_location = docked == Some(location_id)
                || (docked.is_none() && session.solar_system_id.map(i64::from) == Some(location_id));
            let is_storage_auth = if docked.is_none() && session.solar_system_id.is_some() {
                let sys_id = session.solar_system_id.unwrap();
                let authorized = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.authorized_shipping_storage.contains(&(item_id, char_id))
                });
                if authorized {
                    let (has_inv, live_ok) = ctx.db.with_snapshot(|snap| {
                        let it = snap.items.get(&item_id);
                        let has_cap = it.and_then(|it| ctx.sde.types.get(&it.type_id)).map(|t| t.capacity > 0.0).unwrap_or(false);
                        let owner = it.map(|i| i.owner_id).unwrap_or(0);
                        let auth = session.has_gm_role()
                            || owner == char_id
                            || owner == 0
                            || snap.fleets.values().any(|f| f.members.contains(&char_id) && f.members.contains(&owner));
                        (has_cap, auth)
                    });
                    if has_inv && live_ok {
                        let sys_handle = ctx.systems.get_or_spawn(sys_id);
                        let ship_id = session.ship_id.unwrap_or(0);
                        let ship_pos = sys_handle.get_ball_position_sync(ship_id);
                        let unit_pos = sys_handle.get_ball_position_sync(item_id);
                        ship_pos.zip(unit_pos).map(|(s, u)| s.distance(u) <= 5_000.0).unwrap_or(false)
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };
            let is_missing_dynamic_ship_stub =
                crate::services::fitting_containers_ships::resolve_missing_dynamic_ship_stub_type_id(
                    ctx,
                    char_id,
                    item_id,
                )
                .is_some();
            let item_exists = item_id == char_id
                || is_storage_auth
                || is_missing_dynamic_ship_stub
                || ctx.db.with_snapshot(|snap| snap.items.contains_key(&item_id));
            if !item_exists {
                return Some(Err(BridgeRpcError::user_error("FakeItemNotFound")));
            }
            let crab_allowed = match crate::services::pve_crab_beacons::check_crab_loot_access(
                ctx,
                item_id,
                char_id,
                session.has_gm_role(),
            ) {
                Ok(Some(true)) => true,
                Ok(_) => false,
                Err(e) => return Some(Err(e)),
            };
            let accessible = item_id == char_id
                || is_storage_auth
                || is_missing_dynamic_ship_stub
                || ctx.db.with_snapshot(|snap| {
                    snap.items.get(&item_id).is_some_and(|it| {
                        let has_inventory = crab_allowed
                            || ctx
                                .sde
                                .types
                                .get(&it.type_id)
                                .is_some_and(|t| t.capacity > 0.0 || matches!(t.category_id, 6 | 23));
                        if !has_inventory {
                            return false;
                        }
                        let is_corp_owned = session
                            .corporation_id
                            .is_some_and(|cid| cid > 0 && it.owner_id == cid);
                        let is_open_space_container =
                            docked.is_none() && it.type_id == 23 && it.owner_id == 0;
                        if crab_allowed
                            || it.owner_id == char_id
                            || is_corp_owned
                            || is_open_space_container
                            || session.has_gm_role()
                        {
                            let in_corp_office_here = at_location
                                && snap
                                    .items
                                    .get(&it.location_id)
                                    .is_some_and(|p| p.location_id == location_id);
                            let in_reach = it.item_id == session.ship_id.unwrap_or(0)
                                || (it.location_id == location_id && at_location)
                                || in_corp_office_here
                                || Some(it.location_id) == session.ship_id;
                            in_reach
                        } else {
                            false
                        }
                    })
                });
            if !accessible {
                return err(
                    "InventoryNotAccessible",
                    format!("item {item_id} is not an inventory reachable by character {char_id} at {location_id}"),
                );
            }
            Some(ctx.bind_verified_sub_object(session, "invbroker", item_id).map(|bid| ctx.bound_object_rep(bid, caller_trace)))
        }
        _ => {
            let container_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if !matches!(
                container_id,
                CONTAINER_HANGAR | CONTAINER_CORP_MARKET | CONTAINER_GLOBAL
            ) {
                return err(
                    "UnimplementedRPC",
                    format!("invbroker.GetInventory({container_id}) is not yet implemented"),
                );
            }
            let docked = session.station_id.or(session.structure_id);
            if docked != Some(location_id) {
                return err(
                    "CharacterNotAtStation",
                    format!("character {char_id} is not docked at {location_id}"),
                );
            }
            Some(ctx.bind_verified_sub_object(session, "invbroker", location_id).map(|bid| ctx.bound_object_rep(bid, caller_trace)))
        }
    }
}

/// The inventory row of the bound inventory's own item, in the `invbroker.List` row layout
/// (`DBRowDescriptor::inv_item_header`).
fn self_inv_item(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    object_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let header = DBRowDescriptor::inv_item_header();
    if let Some(item) = ctx.db.get_item(object_id).filter(|i| {
        i.owner_id == char_id
            || session
                .corporation_id
                .is_some_and(|cid| cid > 0 && i.owner_id == cid)
    }) {
        return Ok(header.build_dbrow(item_to_packed_cells(ctx, &item)));
    }
    if object_id == char_id {
        if let Some(row) = ctx.db.with_snapshot(|snap| {
            snap.characters.get(&object_id).map(|ch| {
                let loc = ch
                    .station_id
                    .or(ch.structure_id)
                    .unwrap_or(i64::from(ch.solar_system_id));
                header.build_dbrow(vec![
                    PyRep::Integer(ch.character_id),
                    PyRep::Integer(1373),
                    PyRep::Integer(ch.corporation_id),
                    PyRep::Integer(loc),
                    PyRep::Integer(0),
                    PyRep::Integer(-1),
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::String(ch.name.clone()),
                ])
            })
        }) {
            return Ok(row);
        }
    }
    if let Some(row) = ctx.db.with_snapshot(|snap| {
        snap.structures.get(&object_id).and_then(|st| {
            let same_corp = session
                .corporation_id
                .is_some_and(|cid| cid > 0 && st.owner_corp_id == cid);
            if !same_corp && session.structure_id != Some(object_id) {
                return None;
            }
            let (gid, cid) = ctx
                .sde
                .types
                .get(&st.type_id)
                .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
                .unwrap_or((1657, 65));
            Some(header.build_dbrow(vec![
                PyRep::Integer(st.structure_id),
                PyRep::Integer(i64::from(st.type_id)),
                PyRep::Integer(st.owner_corp_id),
                PyRep::Integer(i64::from(st.solar_system_id)),
                PyRep::Integer(0),
                PyRep::Integer(-1),
                PyRep::Integer(gid),
                PyRep::Integer(cid),
                PyRep::String(st.name.clone()),
            ]))
        })
    }) {
        return Ok(row);
    }
    if session.station_id == Some(object_id) {
        if let Some(st) = ctx.sde.stations.get(&object_id) {
            let (group_id, category_id) = ctx
                .sde
                .types
                .get(&st.station_type_id)
                .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
                .unwrap_or((0, 0));
            // DERIVED: NPC stations are not rows in the world DB; the station item is the SDE
            // station (owner corporation, located in its solar system, flag 0, singleton = -1).
            return Ok(header.build_dbrow(vec![
                PyRep::Integer(st.station_id),
                PyRep::Integer(i64::from(st.station_type_id)),
                PyRep::Integer(st.owner_corporation_id),
                PyRep::Integer(i64::from(st.solar_system_id)),
                PyRep::Integer(0),
                PyRep::Integer(-1),
                PyRep::Integer(group_id),
                PyRep::Integer(category_id),
                PyRep::String(st.name.clone()),
            ]));
        }
    }
    if let Some(stub_type_id) =
        crate::services::fitting_containers_ships::resolve_missing_dynamic_ship_stub_type_id(
            ctx, char_id, object_id,
        )
    {
        let (group_id, category_id, type_name) = ctx
            .sde
            .types
            .get(&stub_type_id)
            .map(|t| (i64::from(t.group_id), i64::from(t.category_id), t.name.clone()))
            .unwrap_or((29, 6, String::new()));
        let loc_id = session
            .station_id
            .or(session.structure_id)
            .or_else(|| session.solar_system_id.map(i64::from))
            .unwrap_or(30_000_142);
        return Ok(header.build_dbrow(vec![
            PyRep::Integer(object_id),
            PyRep::Integer(i64::from(stub_type_id)),
            PyRep::Integer(char_id),
            PyRep::Integer(loc_id),
            PyRep::Integer(0),
            PyRep::Integer(-1),
            PyRep::Integer(group_id),
            PyRep::Integer(category_id),
            PyRep::String(type_name),
        ]));
    }
    Err(BridgeRpcError {
        code: "UnimplementedRPC".into(),
        message: format!("invbroker.GetSelfInvItem for inventory {object_id} is not yet implemented"),
    })
}

/// `i2.IsPrimed(itemID)`: checks whether the requested inventory item or container entity
/// is primed in `WorldStateSnapshot` or SDE static world tables (`svc_dgmattr.pyj`).
pub fn handle_i2_is_primed(
    ctx: &ServerContext,
    session: &SessionState,
    target_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    if !session.has_gm_role() {
        return Err(BridgeRpcError {
            code: "GMRoleRequired".into(),
            message: "i2.IsPrimed requires GM role".into(),
        });
    }
    let Some(item_id) = target_id.filter(|&id| id > 0) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "i2.IsPrimed requires a positive itemID".into(),
        });
    };
    let primed = ctx.db.with_snapshot(|snap| {
        snap.items.contains_key(&item_id)
            || snap.characters.contains_key(&item_id)
            || snap.structures.contains_key(&item_id)
    }) || ctx.sde.stations.contains_key(&item_id)
        || u32::try_from(item_id)
            .ok()
            .is_some_and(|id| ctx.sde.solar_systems.contains_key(&id));
    Ok(PyRep::Bool(primed))
}

/// `i2.GetItem(itemID)`: returns the inventory item representation (`KeyVal` with `.typeID`,
/// `.itemID`, `.ownerID`, `.locationID`, `.flagID`, `.quantity`, `.groupID`, `.categoryID`,
/// `.singleton`, `.stacksize`, `.customInfo`) from `WorldStateSnapshot`, or `None` if not found.
pub fn handle_i2_get_item(
    ctx: &ServerContext,
    session: &SessionState,
    target_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    if !session.has_gm_role() {
        return Err(BridgeRpcError {
            code: "GMRoleRequired".into(),
            message: "i2.GetItem requires GM role".into(),
        });
    }
    let Some(item_id) = target_id.filter(|&id| id > 0) else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "i2.GetItem requires a positive itemID".into(),
        });
    };
    if let Some(item) = ctx.db.get_item(item_id) {
        let (group_id, category_id) = ctx
            .sde
            .types
            .get(&item.type_id)
            .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
            .unwrap_or((0, 0));
        let singleton_val = if item.is_blueprint_copy {
            2
        } else if item.singleton {
            1
        } else {
            0
        };
        let qty_val = if item.is_blueprint_copy {
            -2
        } else if item.singleton {
            -1
        } else {
            i64::from(item.quantity)
        };
        return Ok(PyRep::keyval([
            ("itemID", PyRep::Integer(item.item_id)),
            ("typeID", PyRep::Integer(i64::from(item.type_id))),
            ("ownerID", PyRep::Integer(item.owner_id)),
            ("locationID", PyRep::Integer(item.location_id)),
            ("flagID", PyRep::Integer(item.flag as i64)),
            ("quantity", PyRep::Integer(qty_val)),
            ("stacksize", PyRep::Integer(i64::from(item.quantity))),
            ("singleton", PyRep::Integer(singleton_val)),
            ("groupID", PyRep::Integer(group_id)),
            ("categoryID", PyRep::Integer(category_id)),
            ("customInfo", PyRep::String(String::new())),
        ]));
    }
    if let Some(row) = ctx.db.with_snapshot(|snap| {
        if let Some(ch) = snap.characters.get(&item_id) {
            let loc = ch.station_id.or(ch.structure_id).unwrap_or(i64::from(ch.solar_system_id));
            return Some(PyRep::keyval([
                ("itemID", PyRep::Integer(ch.character_id)),
                ("typeID", PyRep::Integer(1373)),
                ("ownerID", PyRep::Integer(ch.corporation_id)),
                ("locationID", PyRep::Integer(loc)),
                ("flagID", PyRep::Integer(0)),
                ("quantity", PyRep::Integer(-1)),
                ("stacksize", PyRep::Integer(1)),
                ("singleton", PyRep::Integer(1)),
                ("groupID", PyRep::Integer(1)),
                ("categoryID", PyRep::Integer(1)),
                ("customInfo", PyRep::String(ch.name.clone())),
            ]));
        }
        if let Some(st) = snap.structures.get(&item_id) {
            let (gid, cid) = ctx
                .sde
                .types
                .get(&st.type_id)
                .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
                .unwrap_or((1657, 65));
            return Some(PyRep::keyval([
                ("itemID", PyRep::Integer(st.structure_id)),
                ("typeID", PyRep::Integer(i64::from(st.type_id))),
                ("ownerID", PyRep::Integer(st.owner_corp_id)),
                ("locationID", PyRep::Integer(i64::from(st.solar_system_id))),
                ("flagID", PyRep::Integer(0)),
                ("quantity", PyRep::Integer(-1)),
                ("stacksize", PyRep::Integer(1)),
                ("singleton", PyRep::Integer(1)),
                ("groupID", PyRep::Integer(gid)),
                ("categoryID", PyRep::Integer(cid)),
                ("customInfo", PyRep::String(st.name.clone())),
            ]));
        }
        None
    }) {
        return Ok(row);
    }
    if let Some(st) = ctx.sde.stations.get(&item_id) {
        return Ok(PyRep::keyval([
            ("itemID", PyRep::Integer(st.station_id)),
            ("typeID", PyRep::Integer(i64::from(st.station_type_id))),
            ("ownerID", PyRep::Integer(st.owner_corporation_id)),
            ("locationID", PyRep::Integer(i64::from(st.solar_system_id))),
            ("flagID", PyRep::Integer(0)),
            ("quantity", PyRep::Integer(-1)),
            ("stacksize", PyRep::Integer(1)),
            ("singleton", PyRep::Integer(1)),
            ("groupID", PyRep::Integer(15)),
            ("categoryID", PyRep::Integer(3)),
            ("customInfo", PyRep::String(st.name.clone())),
        ]));
    }
    Ok(PyRep::None)
}

pub(crate) fn item_to_packed_cells(ctx: &ServerContext, i: &eve_db::InventoryItemRecord) -> Vec<PyRep> {
    let (group_id, category_id) = ctx
        .sde
        .types
        .get(&i.type_id)
        .map(|t| (t.group_id as i64, t.category_id as i64))
        .unwrap_or((0, 0));
    let qty = if i.is_blueprint_copy {
        -2
    } else if i.singleton {
        -1
    } else {
        i.quantity as i64
    };
    let custom_name = ctx
        .db
        .with_snapshot(|snap| snap.item_custom_names.get(&i.item_id).cloned())
        .unwrap_or_default();
    vec![
        PyRep::Integer(i.item_id),
        PyRep::Integer(i.type_id as i64),
        PyRep::Integer(i.owner_id),
        PyRep::Integer(i.location_id),
        PyRep::Integer(i.flag as i64),
        PyRep::Integer(qty),
        PyRep::Integer(group_id),
        PyRep::Integer(category_id),
        PyRep::String(custom_name),
    ]
}

pub(crate) fn get_effective_corp_roles(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
) -> (bool, u64) {
    get_effective_corp_roles_at_location(ctx, corp_id, char_id, None)
}

pub(crate) fn get_effective_corp_roles_at_location_in_snap_at(
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    location_id: Option<i64>,
    now_unix: u64,
) -> (bool, u64) {
    snap.effective_corp_roles_at(corp_id, char_id, location_id, 0, now_unix)
}

pub(crate) fn get_effective_corp_roles_at_location(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
    location_id: Option<i64>,
) -> (bool, u64) {
    crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
    let now_unix = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        get_effective_corp_roles_at_location_in_snap_at(snap, corp_id, char_id, location_id, now_unix)
    })
}

/// Validates that `char_id` is authorized to take a corporation-owned item `src_item` out of `corp_id`
/// ownership into a non-corporation inventory (`#385` / `R6-T3-02`, updated `#552`).
pub(crate) fn validate_corp_source_item_take_access_in_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    corp_id: i64,
    src_item: &eve_db::InventoryItemRecord,
) -> Result<(), BridgeRpcError> {
    crate::services::fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(
        snap, src_item,
    )?;
    let (mut root_loc, mut root_flag) = (src_item.location_id, src_item.flag);
    let mut inside_corp_container = false;
    for _ in 0..8 {
        if (60_000_000..=69_999_999).contains(&root_loc) || root_loc >= 1_000_000_000_000 {
            break;
        }
        let Some(parent) = snap.items.get(&root_loc) else {
            break;
        };
        if parent.owner_id != corp_id {
            break;
        }
        if parent.type_id == 27 {
            // Stop at corp offices (type_id 27) after resolving station/structure location so
            // root_flag remains the CorpSAG division and an office is not treated as a container.
            root_loc = parent.location_id;
            break;
        }
        inside_corp_container = true;
        root_loc = parent.location_id;
        root_flag = parent.flag;
    }

    let (is_ceo, role_mask) = get_effective_corp_roles_at_location_in_snap_at(
        snap,
        corp_id,
        char_id,
        Some(root_loc),
        ctx.now_unix(),
    );

    if is_ceo || eve_db::corp_roles::has_role(role_mask, eve_db::corp_roles::ROLE_DIRECTOR) {
        return Ok(());
    }

    let flag_num = root_flag as i64;
    if (116..=122).contains(&flag_num) {
        let div_idx = (flag_num - 116) as usize;
        let div_take_role = if inside_corp_container {
            eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[div_idx]
        } else {
            eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[div_idx]
        };
        if !eve_db::corp_roles::has_role(role_mask, div_take_role) {
            return Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: format!(
                    "Character {char_id} lacks corporation {} division {} take role",
                    if inside_corp_container { "container" } else { "hangar" },
                    div_idx + 1
                ),
            });
        }
        return Ok(());
    }

    if matches!(
        root_flag,
        eve_db::InventoryFlag::Deliveries | eve_db::InventoryFlag::CorpDeliveries
    ) {
        let deliveries_roles = eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_TAKE
            | eve_db::corp_roles::ROLE_FACTORY_MANAGER
            | eve_db::corp_roles::ROLE_TRADER;
        if (role_mask & deliveries_roles) == 0 {
            return Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: format!(
                    "Character {char_id} lacks role to take from corporation deliveries"
                ),
            });
        }
        return Ok(());
    }

    Err(BridgeRpcError {
        code: "CorpRoleDenied".into(),
        message: format!(
            "Director or CEO role required to take non-division corporation item from flag {:?}",
            root_flag
        ),
    })
}

/// Re-verifies source ownership, corporation take roles, container locks/passwords, and destination
/// ownership inside `invbroker.Add`'s write transaction (`#885`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn recheck_invbroker_add_auth_in_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    orig_item: &eve_db::InventoryItemRecord,
    orig_source_snapshot: Option<&eve_db::InventoryItemRecord>,
    new_loc: i64,
    flag: InventoryFlag,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<(), BridgeRpcError> {
    if let Some(prev) = orig_source_snapshot {
        if orig_item.owner_id != prev.owner_id || orig_item.flag != prev.flag {
            return Err(BridgeRpcError {
                code: "ItemOwnershipViolation".into(),
                message: format!(
                    "Item {} ownership or flag changed concurrently (expected owner {} flag {:?}, found owner {} flag {:?})",
                    orig_item.item_id, prev.owner_id, prev.flag, orig_item.owner_id, orig_item.flag
                ),
            });
        }
    }
    let st = crate::services::fitting_containers_ships::read_container_ship_state_snap(snap);
    if st.alsc_locked_items.contains(&orig_item.item_id) {
        return Err(BridgeRpcError {
            code: "ALSCItemLocked".into(),
            message: format!("Item {} is locked in an Audit Log Secure Container", orig_item.item_id),
        });
    }
    let live_corp_id = snap
        .characters
        .get(&char_id)
        .map(|c| c.corporation_id)
        .or(session.corporation_id)
        .unwrap_or(0);
    let is_struct_loc = snap.structures.contains_key(&orig_item.location_id);
    let is_core_item = is_struct_loc
        && (ctx
            .sde
            .types
            .get(&orig_item.type_id)
            .is_some_and(|t| t.group_id == 4086 || t.category_id == 66)
            || snap
                .structures
                .get(&orig_item.location_id)
                .and_then(|st_rec| {
                    crate::services::station_structure::structure_lifecycle::structure_profile(
                        ctx, st_rec.type_id,
                    )
                })
                .and_then(|p| p.core_type_id)
                .is_some_and(|c| c == orig_item.type_id));
    let src_is_deed_bay = is_struct_loc
        && (orig_item.flag == InventoryFlag::StructureDeedBay
            || orig_item.flag as u16 == 180
            || ((orig_item.flag == InventoryFlag::StructureFuel || orig_item.flag as u16 == 172)
                && is_core_item));
    let src_is_struct_fuel = is_struct_loc
        && !is_core_item
        && crate::services::structure_acl_defense_rules::is_structure_fuel_flag(orig_item.flag);
    if src_is_deed_bay && !session.has_gm_role() {
        return Err(BridgeRpcError {
            code: "ItemLocked".into(),
            message: "Quantum core cannot be removed from structure deed bay".into(),
        });
    } else if src_is_struct_fuel {
        let st_owner_corp = snap
            .structures
            .get(&orig_item.location_id)
            .map(|s| s.owner_corp_id)
            .unwrap_or(0);
        if st_owner_corp != live_corp_id && !session.has_gm_role() {
            return Err(BridgeRpcError {
                code: "StructureOwnershipViolation".into(),
                message: format!(
                    "Character {char_id} does not belong to structure owning corp {st_owner_corp}"
                ),
            });
        }
        if !session.has_gm_role()
            && !crate::services::structure_acl_defense_rules::has_structure_fuel_bay_role(
                snap,
                session,
                char_id,
                live_corp_id,
            )
        {
            return Err(BridgeRpcError {
                code: "CorpRoleDenied".into(),
                message: "Accessing the structure fuel bay requires Director, Station Manager, or Fuel Technician role".into(),
            });
        }
    } else if orig_item.owner_id == live_corp_id && live_corp_id > 0 {
        validate_corp_source_item_take_access_in_snap(
            ctx,
            snap,
            char_id,
            live_corp_id,
            orig_item,
        )?;
    } else if orig_item.owner_id != char_id
        && snap.corporations.contains_key(&orig_item.owner_id)
        && orig_item.owner_id != live_corp_id
    {
        let src_is_public_wreck_or_free_cont = snap
            .items
            .get(&orig_item.location_id)
            .is_some_and(|p| p.type_id == 23 || p.owner_id == 0);
        if !src_is_public_wreck_or_free_cont && !session.has_gm_role() {
            return Err(BridgeRpcError {
                code: "ItemOwnershipViolation".into(),
                message: format!(
                    "Character {char_id} is no longer in corporation {} owning item {}",
                    orig_item.owner_id, orig_item.item_id
                ),
            });
        }
    }
    if orig_item.location_id != new_loc {
        if let Some(src_cont) = snap.items.get(&orig_item.location_id) {
            let src_grp = ctx.sde.types.get(&src_cont.type_id).map(|t| t.group_id).unwrap_or(0);
            if crate::services::fitting_containers_ships::is_container_group(src_grp, src_cont.type_id) {
                if src_cont.owner_id != char_id && !session.has_gm_role() {
                    if let Some(req_pw) = st.container_passwords.get(&src_cont.item_id) {
                        if !req_pw.is_empty()
                            && !st.unlocked_containers.contains(&(src_cont.item_id, char_id))
                            && !st.unlocked_containers.contains(&(char_id, src_cont.item_id))
                        {
                            let supplied_pw = kwargs
                                .get("password")
                                .and_then(|v| v.as_str())
                                .or_else(|| args.iter().find_map(|a| a.as_str()));
                            if supplied_pw != Some(req_pw.as_str()) {
                                return Err(BridgeRpcError {
                                    code: "ALSCPasswordNeededToOpen".into(),
                                    message: format!(
                                        "Password required to take items from secure container {}",
                                        src_cont.item_id
                                    ),
                                });
                            }
                        }
                    }
                }
                let cont_corp = if src_cont.owner_id != char_id
                    && snap.corporations.contains_key(&src_cont.owner_id)
                {
                    src_cont.owner_id
                } else if (116..=122).contains(&(src_cont.flag as i64))
                    && live_corp_id > 0
                    && orig_item.owner_id == live_corp_id
                {
                    live_corp_id
                } else {
                    0
                };
                let is_auth_space_storage = (30_000_000..=39_999_999).contains(&src_cont.location_id)
                    && crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                        rt.authorized_shipping_storage
                            .contains(&(src_cont.item_id, char_id))
                    });
                if cont_corp > 0 && cont_corp != live_corp_id {
                    if src_cont.type_id != 23 && !is_auth_space_storage && !session.has_gm_role() {
                        return Err(BridgeRpcError {
                            code: "CorpRoleDenied".into(),
                            message: format!(
                                "Character {char_id} is not in corporation {cont_corp} owning container {}",
                                src_cont.item_id
                            ),
                        });
                    }
                } else if cont_corp > 0 && cont_corp == live_corp_id {
                    let (is_ceo, roles) = get_effective_corp_roles_at_location_in_snap_at(
                        snap,
                        cont_corp,
                        char_id,
                        Some(src_cont.location_id),
                        ctx.now_unix(),
                    );
                    let src_cont_flag = src_cont.flag as i64;
                    let div_idx = if (116..=122).contains(&src_cont_flag) {
                        (src_cont_flag - 116) as usize
                    } else {
                        0
                    };
                    let req_container_take_role =
                        eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[div_idx.min(6)];
                    if !is_ceo
                        && (roles & (eve_db::corp_roles::ROLE_DIRECTOR | req_container_take_role)) == 0
                    {
                        return Err(BridgeRpcError {
                            code: "CorpRoleDenied".into(),
                            message: format!(
                                "Corporation container take role denied for division {}",
                                div_idx + 1
                            ),
                        });
                    }
                }
            }
        }
    }
    let dest_is_core_item = ctx
        .sde
        .types
        .get(&orig_item.type_id)
        .is_some_and(|t| t.group_id == 4086 || t.category_id == 66);
    let dest_flag_u16 = flag as u16;
    let dest_is_deed_flag = dest_flag_u16 == 180
        || flag == InventoryFlag::StructureDeedBay
        || ((dest_flag_u16 == 172 || flag == InventoryFlag::StructureFuel) && dest_is_core_item);
    let dest_is_fuel_flag = dest_flag_u16 == 133
        || flag == InventoryFlag::SpecializedFuelBay
        || ((dest_flag_u16 == 172 || flag == InventoryFlag::StructureFuel) && !dest_is_core_item);
    if let Some(st_dest) = snap.structures.get(&new_loc) {
        if st_dest.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "StructureDestroyed".into(),
                message: format!("Structure {new_loc} is destroyed"),
            });
        }
        if dest_is_deed_flag && !session.has_gm_role() {
            if st_dest.owner_corp_id != live_corp_id {
                return Err(BridgeRpcError {
                    code: "StructureOwnershipViolation".into(),
                    message: format!(
                        "Character {char_id} does not belong to structure owning corp {}",
                        st_dest.owner_corp_id
                    ),
                });
            }
            use eve_db::corp_roles::{ROLE_DIRECTOR, ROLE_STATION_MANAGER};
            let (is_ceo, eff_roles) = get_effective_corp_roles_at_location_in_snap_at(
                snap,
                live_corp_id,
                char_id,
                Some(new_loc),
                ctx.now_unix(),
            );
            if !is_ceo && (eff_roles & (ROLE_DIRECTOR | ROLE_STATION_MANAGER)) == 0 {
                return Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Installing a quantum core requires Director or Station Manager role"
                        .into(),
                });
            }
        } else if dest_is_fuel_flag && !session.has_gm_role() {
            let is_player_corp = st_dest.owner_corp_id >= 98_000_000
                || snap.corporations.contains_key(&st_dest.owner_corp_id);
            if is_player_corp && st_dest.owner_corp_id != live_corp_id {
                return Err(BridgeRpcError {
                    code: "StructureOwnershipViolation".into(),
                    message: format!(
                        "Character {char_id} does not belong to structure owning corp {}",
                        st_dest.owner_corp_id
                    ),
                });
            }
            if is_player_corp
                && !crate::services::structure_acl_defense_rules::has_structure_fuel_bay_role(
                    snap,
                    session,
                    char_id,
                    live_corp_id,
                )
            {
                return Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: "Accessing the structure fuel bay requires Director, Station Manager, or Fuel Technician role".into(),
                });
            }
        } else if st_dest.owner_corp_id != live_corp_id
            && !session.has_gm_role()
            && !crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access(
                ctx, snap, new_loc, char_id,
            )
        {
            return Err(BridgeRpcError {
                code: "DestinationAccessDenied".into(),
                message: format!("Character {char_id} lacks structure access at {new_loc}"),
            });
        }
    } else if let Some(dest) = snap.items.get(&new_loc) {
        let d_grp = ctx.sde.types.get(&dest.type_id).map(|t| t.group_id).unwrap_or(0);
        let d_cat = ctx.sde.types.get(&dest.type_id).map(|t| t.category_id).unwrap_or(0);
        if dest.owner_id != char_id && !session.has_gm_role() {
            if let Some(req_pw) = st.container_passwords.get(&dest.item_id) {
                if !req_pw.is_empty()
                    && !st.unlocked_containers.contains(&(dest.item_id, char_id))
                    && !st.unlocked_containers.contains(&(char_id, dest.item_id))
                {
                    let supplied_pw = kwargs
                        .get("password")
                        .and_then(|v| v.as_str())
                        .or_else(|| args.iter().find_map(|a| a.as_str()));
                    if supplied_pw != Some(req_pw.as_str()) {
                        return Err(BridgeRpcError {
                            code: "ALSCPasswordRequired".into(),
                            message: format!(
                                "Password required to move items into container {}",
                                dest.item_id
                            ),
                        });
                    }
                }
            }
            let in_space_container = (30_000_000..=39_999_999).contains(&dest.location_id);
            let dest_owner_ok = session.has_gm_role()
                || dest.owner_id == char_id
                || dest.owner_id == 0
                || dest.type_id == 23
                || (live_corp_id > 0 && dest.owner_id == live_corp_id)
                || (crate::services::fitting_containers_ships::is_container_group(
                    d_grp,
                    dest.type_id,
                ) && in_space_container)
                || d_grp == 186
                || (d_cat == 6 && matches!(flag as u16, 5 | 90 | 138 | 155));
            if !dest_owner_ok {
                return Err(BridgeRpcError {
                    code: "DestinationAccessDenied".into(),
                    message: format!(
                        "Character {char_id} is not authorized to move items into {new_loc}"
                    ),
                });
            }
        }
    }
    Ok(())
}

/// Helper checking if an item or location is reachable and colocated with the character.
pub(crate) fn is_inventory_location_colocated(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    ch: &eve_db::CharacterRecord,
    pre_balls: &HashMap<i64, (glam::DVec3, f64)>,
    start_loc: i64,
) -> bool {
    let ch_dock = if session.station_id.is_none()
        && session.structure_id.is_none()
        && session.solar_system_id.is_some()
    {
        0
    } else {
        ch.station_id.or(ch.structure_id).unwrap_or(0)
    };
    let ch_sys = if ch.solar_system_id > 0 {
        ch.solar_system_id as i64
    } else {
        session.solar_system_id.map(|s| s as i64).unwrap_or(0)
    };
    let caller_ship_id = session.ship_id.filter(|&id| id > 0).unwrap_or(ch.active_ship_id);
    let mut cur = start_loc;
    for _ in 0..4 {
        if ch_dock > 0 {
            if cur == ch_dock {
                return true;
            }
            let Some(node) = snap.items.get(&cur) else {
                return false;
            };
            cur = node.location_id;
        } else if ch_sys > 0 {
            if cur == ch_sys {
                return true;
            }
            if let Some(st) = snap.structures.get(&cur) {
                if st.solar_system_id as i64 == ch_sys {
                    let caller_pos = pre_balls
                        .get(&caller_ship_id)
                        .map(|p| p.0)
                        .or_else(|| snap.item_positions.get(&caller_ship_id).copied().map(glam::DVec3::from));
                    let target_pos = pre_balls
                        .get(&cur)
                        .map(|p| p.0)
                        .or_else(|| snap.item_positions.get(&cur).copied().map(glam::DVec3::from))
                        .or(Some(glam::DVec3::from_array(st.position)));
                    return match (caller_pos, target_pos) {
                        (Some(s), Some(t)) => s.distance(t) <= 5_000.0,
                        _ => true,
                    };
                }
            }
            let Some(node) = snap.items.get(&cur) else {
                return false;
            };
            if node.location_id == ch_sys {
                if cur == ch.active_ship_id || cur == caller_ship_id {
                    return true;
                }
                let caller_pos = pre_balls
                    .get(&caller_ship_id)
                    .map(|p| p.0)
                    .or_else(|| snap.item_positions.get(&caller_ship_id).copied().map(glam::DVec3::from));
                let target_pos = pre_balls
                    .get(&cur)
                    .map(|p| p.0)
                    .or_else(|| snap.item_positions.get(&cur).copied().map(glam::DVec3::from));
                return match (caller_pos, target_pos) {
                    (Some(s), Some(t)) => s.distance(t) <= 5_000.0,
                    _ => true,
                };
            }
            cur = node.location_id;
        } else {
            return false;
        }
    }
    false
}

/// Upwell structure fuel bay & destination reachability authorization helper for `MultiAdd`.
pub(crate) fn validate_multiadd_destination(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    ch: &eve_db::CharacterRecord,
    char_id: i64,
    new_loc: i64,
    dest_is_struct_fuel: bool,
    pre_balls: &HashMap<i64, (glam::DVec3, f64)>,
) -> Result<(), BridgeRpcError> {
    if !dest_is_struct_fuel {
        if let Some(st) = snap.structures.get(&new_loc) {
            if st.state == "Destroyed" {
                return Err(BridgeRpcError {
                    code: "StructureDestroyed".into(),
                    message: format!("Structure {new_loc} is destroyed"),
                });
            }
            if st.owner_corp_id != ch.corporation_id
                && !crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap(
                    snap, new_loc, char_id,
                )
            {
                return Err(BridgeRpcError {
                    code: "DestinationAccessDenied".into(),
                    message: format!("Character {char_id} lacks structure access at {new_loc}"),
                });
            }
        }
        let ch_dock = if session.station_id.is_none()
            && session.structure_id.is_none()
            && session.solar_system_id.is_some()
        {
            0
        } else {
            ch.station_id.or(ch.structure_id).unwrap_or(0)
        };
        let dest_item_opt = snap.items.get(&new_loc);
        let dst_reachable = is_inventory_location_colocated(snap, session, ch, pre_balls, new_loc)
            && (ch_dock == new_loc
                || dest_item_opt.is_some_and(|d| {
                    d.owner_id == char_id
                        || d.owner_id == 0
                        || d.type_id == 23
                        || (ch.corporation_id > 0 && d.owner_id == ch.corporation_id)
                }));
        if !dst_reachable {
            return Err(BridgeRpcError {
                code: "DestinationAccessDenied".into(),
                message: format!("Destination {new_loc} not reachable"),
            });
        }
    }
    Ok(())
}

/// Upwell structure fuel bay loading/unloading authorization helper for `MultiAdd`.
pub(crate) fn validate_multiadd_structure_fuel_transfers(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    item: &eve_db::InventoryItemRecord,
    new_loc: i64,
    dest_is_struct_fuel: bool,
    ch_dock: i64,
    ch_sys: i64,
    caller_ship_id: i64,
    pre_balls: &HashMap<i64, (glam::DVec3, f64)>,
) -> Result<bool, BridgeRpcError> {
    let is_struct_loc = snap.structures.contains_key(&item.location_id);
    let is_core_item = is_struct_loc && (
        ctx.sde.types.get(&item.type_id).is_some_and(|t| t.group_id == 4086 || t.category_id == 66)
            || snap.structures.get(&item.location_id)
                .and_then(|st| crate::services::station_structure::structure_lifecycle::structure_profile(ctx, st.type_id))
                .and_then(|p| p.core_type_id)
                .is_some_and(|c| c == item.type_id)
    );
    if is_struct_loc && (item.flag == InventoryFlag::StructureDeedBay || item.flag as u16 == 180 || ((item.flag == InventoryFlag::StructureFuel || item.flag as u16 == 172) && is_core_item)) {
        return Err(BridgeRpcError {
            code: "ItemLocked".into(),
            message: "Quantum core cannot be removed from structure deed bay".into(),
        });
    }
    let src_is_struct_fuel = is_struct_loc
        && !is_core_item
        && crate::services::structure_acl_defense_rules::is_structure_fuel_flag(item.flag);
    if src_is_struct_fuel {
        crate::services::structure_acl_defense_rules::validate_structure_fuel_bay_transfer(
            ctx, snap, session, char_id, item.location_id, item.type_id, false, ch_dock, ch_sys, caller_ship_id, pre_balls,
        )?;
    }
    if dest_is_struct_fuel {
        crate::services::structure_acl_defense_rules::validate_structure_fuel_bay_transfer(
            ctx, snap, session, char_id, new_loc, item.type_id, true, ch_dock, ch_sys, caller_ship_id, pre_balls,
        )?;
    }
    Ok(src_is_struct_fuel)
}

pub(crate) fn emit_inv_item_change(
    ctx: &ServerContext,
    char_id: i64,
    item: &eve_db::InventoryItemRecord,
    changes: Vec<(i32, PyRep)>,
) {
    ctx.emit_item_change_routed(char_id, item, changes);
}

pub(crate) fn emit_inv_item_deleted(
    ctx: &ServerContext,
    char_id: i64,
    old_item: &eve_db::InventoryItemRecord,
) {
    ctx.emit_item_deleted_routed(char_id, old_item);
}

/// Resolves destination `locationID` for bound/unbound `invbroker.Add` and `invbroker.MultiAdd`.
///
/// Bound container/ship/structure handles (`GetInventoryFromId(dest_id)`) always receive
/// `args[1] = sourceLocationID` from the V24 client, so a bound ship/item/structure `object_id`
/// takes precedence unless the bound object is a top-level station/solar-system inventory manager
/// dispatching into a different container/ship passed in `args[1]`.
pub(crate) fn resolve_inv_target_location(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    src_loc_opt: Option<i64>,
    raw_flag_opt: Option<u16>,
    items_are_fittable_modules: bool,
) -> Option<i64> {
    let a1 = args.get(1).and_then(|a| a.as_i64()).filter(|&id| id > 0);
    let bid = bound_entry.map(|b| b.object_id).filter(|&id| id > 0);
    // When the caller passes `args[1] == sourceLocationID` on an unbound or station-bound
    // `invbroker` handle together with a ship-only flag (fitting slot, ship bay/cargo, or
    // `flagAutoFit` on fittable modules/rigs), `args[1]` is `sourceLocationID` from the
    // V24 client rather than a station destination, so redirect to `session.ship_id`.
    let target_is_ship_only_flag = matches!(
        raw_flag_opt,
        Some(5 | 11..=34 | 87 | 90 | 92..=94 | 125..=132 | 134..=151 | 158)
    ) || (items_are_fittable_modules && matches!(raw_flag_opt, Some(0) | None));
    let redirect_source_station_to_ship = |a1_opt: Option<i64>| -> Option<i64> {
        let a1_id = a1_opt?;
        if target_is_ship_only_flag
            && a1_opt == src_loc_opt
            && session.ship_id.is_some_and(|sid| sid != a1_id)
        {
            // Upwell structures own fitting/rig/fighter slots (`11..=34 | 92..=94 | 158`);
            // do not redirect Standup modules or structure weapon charges targeting `a1_id`
            // onto `session.ship_id`.
            let blocked_by_structure_slots = !items_are_fittable_modules
                && matches!(raw_flag_opt, Some(11..=34 | 92..=94 | 158))
                && ctx
                    .db
                    .with_snapshot(|snap| snap.structures.contains_key(&a1_id));
            let target_is_non_ship_container = ctx.db.with_snapshot(|snap| {
                snap.items.get(&a1_id).is_some_and(|it| {
                    ctx.sde
                        .types
                        .get(&it.type_id)
                        .map_or(true, |t| !matches!(t.category_id, 6 | 65))
                })
            });
            if blocked_by_structure_slots || target_is_non_ship_container {
                None
            } else {
                session.ship_id
            }
        } else {
            None
        }
    };
    let resolved = match (a1, bid) {
        (a1_opt, Some(bid_val)) => {
            let bound_is_container_or_struct = ctx.db.with_snapshot(|snap| {
                snap.items.contains_key(&bid_val) || snap.structures.contains_key(&bid_val)
            });
            if bound_is_container_or_struct {
                Some(bid_val)
            } else if a1_opt.is_some() && a1_opt == src_loc_opt && a1_opt != Some(bid_val) {
                Some(bid_val)
            } else if let Some(ship_id) = redirect_source_station_to_ship(a1_opt) {
                Some(ship_id)
            } else {
                a1_opt.or(Some(bid_val))
            }
        }
        (a1_opt, None) => redirect_source_station_to_ship(a1_opt).or(a1_opt),
    };
    resolved
        .or(session.ship_id)
        .or(session.station_id)
        .or(session.structure_id)
}

#[cfg(test)]
#[path = "inventory_bound_tests.rs"]
mod inventory_bound_tests;

