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

/// `inventorycommon/const.pyj: containerHangar` — the character's hangar at the docked station.
const CONTAINER_HANGAR: i64 = 10_004;

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
            "GetInventoryFromId" | "GetInventory" | "GetSelfInvItem" | "GetAvailableTurretSlots"
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
            let accessible = item_id == char_id
                || is_storage_auth
                || ctx.db.with_snapshot(|snap| {
                    snap.items.get(&item_id).is_some_and(|it| {
                        let has_inventory = ctx.sde.types.get(&it.type_id).is_some_and(|t| t.capacity > 0.0);
                        if !has_inventory {
                            return false;
                        }
                        if it.owner_id == char_id || session.has_gm_role() {
                            let in_reach = it.item_id == session.ship_id.unwrap_or(0)
                                || (it.location_id == location_id && at_location)
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
            if container_id != CONTAINER_HANGAR {
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
    if let Some(item) = ctx.db.get_item(object_id).filter(|i| i.owner_id == char_id) {
        return Ok(header.build_dbrow(item_to_packed_cells(ctx, &item)));
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

pub(crate) fn get_effective_corp_roles_at_location(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
    location_id: Option<i64>,
) -> (bool, u64) {
    crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
    ctx.db.with_snapshot(|snap| {
        let (is_ceo, hq_station_id, mut role_mask) = snap
            .corporations
            .get(&corp_id)
            .map(|c| (c.ceo_id == char_id, c.hq_station_id.unwrap_or(60_003_760), c.member_roles.get(&char_id).copied().unwrap_or(0)))
            .unwrap_or((false, 60_003_760, 0));
        let raw_loc = location_id
            .or_else(|| {
                snap.characters
                    .get(&char_id)
                    .and_then(|ch| ch.station_id.or(ch.structure_id))
            })
            .unwrap_or(0);
        let loc_station_id = snap
            .items
            .get(&raw_loc)
            .and_then(|it| {
                (it.location_id >= 60_000_000 || snap.structures.contains_key(&it.location_id))
                    .then_some(it.location_id)
            })
            .unwrap_or(raw_loc);
        let base_id = snap
            .corp_governance
            .get(&corp_id)
            .and_then(|g| g.member_bases.get(&char_id).copied())
            .unwrap_or(0);
        if let Some(ra) = snap.corp_role_assignments.get(&(corp_id, char_id)) {
            role_mask |= ra.roles;
            if loc_station_id > 0 {
                if loc_station_id == hq_station_id {
                    role_mask |= ra.roles_at_hq;
                }
                if base_id > 0 && loc_station_id == base_id {
                    role_mask |= ra.roles_at_base;
                }
                if loc_station_id != hq_station_id && (base_id <= 0 || loc_station_id != base_id) {
                    role_mask |= ra.roles_at_other;
                }
            }
        }
        (is_ceo, role_mask)
    })
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
