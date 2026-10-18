//! `inventory_fitting` — Domain 3: Inventory Broker, Ship Fitting, Insurance, Repair, & Abyssal Mutaplasmids (`§3.4`).

#![forbid(unsafe_code)]

use crate::formulas::{abyssal_attribute_roll, compute_insurance_quote};
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{InventoryFlag, IskCents, SavedFittingRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, AttributeId, TypeId};
use std::collections::HashMap;

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("invbroker", "ListItems") | ("invbroker", "List") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let loc_filter = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
                .or(session.station_id)
                .or(session.structure_id)
                .or(session.ship_id);
            let items = ctx.db.get_items_for_character(char_id);
            let rows = items
                .into_iter()
                .filter(|i| loc_filter.map(|loc| i.location_id == loc).unwrap_or(false))
                .map(|i| {
                    vec![
                        PyRep::Integer(i.item_id),
                        PyRep::Integer(i.type_id as i64),
                        PyRep::Integer(i.owner_id),
                        PyRep::Integer(i.location_id),
                        PyRep::Integer(i.flag as i64),
                        PyRep::Integer(i.quantity as i64),
                        PyRep::Integer(i.version as i64),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec![
                    "itemID",
                    "typeID",
                    "ownerID",
                    "locationID",
                    "flagID",
                    "quantity",
                    "version",
                ],
                rows,
            )))
        }

        ("invbroker", "MoveItem") | ("invbroker", "Add") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "MoveItem requires itemID".into(),
                }));
            };
            let new_loc = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
                .unwrap_or_else(|| session.ship_id.unwrap_or(60_003_760));
            let raw_flag_opt = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()));
            if let Some(rf) = raw_flag_opt {
                if !matches!(
                    rf,
                    4 | 5 | 11..=34 | 87 | 90 | 92..=94 | 125..=143 | 179
                ) {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidInventoryFlag".into(),
                        message: format!("Invalid or restricted inventory flag {rf}"),
                    }));
                }
            }
            let flag = raw_flag_opt
                .map(|v| InventoryFlag::from_u16(v as u16))
                .unwrap_or(InventoryFlag::Cargo);
            let flag_u16 = flag as u16;
            let is_fitting_slot = matches!(flag_u16, 11..=34 | 92..=94 | 125..=132);
            if is_fitting_slot {
                let item_cat_opt = ctx.db.with_snapshot(|snap| {
                    snap.items.get(&item_id).and_then(|item| {
                        ctx.sde.types.get(&item.type_id).map(|t| t.category_id)
                    })
                });
                if item_cat_opt == Some(8) && matches!(flag_u16, 27..=34) {
                    let current_dock = session.station_id.or(session.structure_id);
                    return match ctx.load_charge_into_weapon_slot(
                        char_id,
                        new_loc,
                        flag,
                        item_id,
                        current_dock,
                    ) {
                        Ok((loaded_id, loaded_type_id, loaded_qty, loaded_ver)) => {
                            Some(Ok(PyRep::keyval([
                                ("itemID", PyRep::Integer(loaded_id)),
                                ("typeID", PyRep::Integer(loaded_type_id as i64)),
                                ("locationID", PyRep::Integer(new_loc)),
                                ("flagID", PyRep::Integer(flag as i64)),
                                ("quantity", PyRep::Integer(loaded_qty as i64)),
                                ("version", PyRep::Integer(loaded_ver as i64)),
                            ])))
                        }
                        Err(e) => Some(Err(e)),
                    };
                }
                if session.station_id.is_none() && session.structure_id.is_none() {
                    return Some(Err(BridgeRpcError {
                        code: "CannotFitInSpace".into(),
                        message: "Fitting modules into ship slots requires being docked at a station or structure".into(),
                    }));
                }
                let pre_check = ctx.db.with_snapshot(|snap| {
                    let Some(item) = snap.items.get(&item_id) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Item {item_id} not found"),
                        });
                    };
                    if item.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "ItemOwnershipViolation".into(),
                            message: format!("Item {item_id} is not owned by {char_id}"),
                        });
                    }
                    if item.quantity != 1 {
                        return Err(BridgeRpcError {
                            code: "CannotFitStackedModule".into(),
                            message: format!(
                                "Item {item_id} has quantity {} and must be split to 1 before fitting",
                                item.quantity
                            ),
                        });
                    }
                    let item_cat = ctx
                        .sde
                        .types
                        .get(&item.type_id)
                        .map(|t| t.category_id)
                        .unwrap_or(0);
                    if item_cat != 7 && item_cat != 32 {
                        return Err(BridgeRpcError {
                            code: "ItemNotFittableModule".into(),
                            message: format!(
                                "Item {item_id} (type {}, category {item_cat}) cannot be fitted to a module slot",
                                item.type_id
                            ),
                        });
                    }
                    let Some(ship_item) = snap.items.get(&new_loc) else {
                        return Err(BridgeRpcError {
                            code: "ShipNotFound".into(),
                            message: format!("Target ship {new_loc} not found"),
                        });
                    };
                    let ship_cat = ctx
                        .sde
                        .types
                        .get(&ship_item.type_id)
                        .map(|t| t.category_id)
                        .unwrap_or(0);
                    if ship_item.owner_id != char_id || ship_cat != 6 {
                        return Err(BridgeRpcError {
                            code: "InvalidFittingTarget".into(),
                            message: format!("Target {new_loc} is not an owned ship"),
                        });
                    }
                    if snap.items.values().any(|other| {
                        other.item_id != item_id
                            && other.location_id == new_loc
                            && other.flag == flag
                            && ctx
                                .sde
                                .types
                                .get(&other.type_id)
                                .map(|t| t.category_id == 7 || t.category_id == 32)
                                .unwrap_or(true)
                    }) {
                        return Err(BridgeRpcError {
                            code: "SlotAlreadyOccupied".into(),
                            message: format!(
                                "Slot {flag_u16} on ship {new_loc} is already occupied"
                            ),
                        });
                    }
                    Ok(item.type_id)
                });
                let mod_type_id = match pre_check {
                    Ok(t) => t,
                    Err(e) => return Some(Err(e)),
                };
                let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, new_loc) {
                    Ok(f) => f,
                    Err(e) => return Some(Err(e)),
                };
                if let Err(msg) =
                    fit.fit_and_online_module(item_id, mod_type_id, flag_u16, &ctx.sde)
                {
                    return Some(Err(BridgeRpcError {
                        code: "FittingValidationFailed".into(),
                        message: msg,
                    }));
                }
            }

            let expected_version = kwargs
                .get("expected_version")
                .and_then(|v| v.as_i64())
                .map(|v| v as u64);
            let idem_key = kwargs.get("idempotency_key").and_then(|v| v.as_str());

            match ctx.db.move_item_transactional(
                char_id,
                item_id,
                new_loc,
                flag,
                expected_version,
                idem_key,
            ) {
                Ok(updated) => {
                    if let Some(ship_id) = session.ship_id {
                        ctx.invalidate_ship_fitting(ship_id);
                    }
                    ctx.invalidate_ship_fitting(new_loc);
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(updated.item_id)),
                        ("locationID", PyRep::Integer(updated.location_id)),
                        ("flagID", PyRep::Integer(updated.flag as i64)),
                        ("version", PyRep::Integer(updated.version as i64)),
                    ])))
                }
                Err(e) => Some(Err(BridgeRpcError {
                    code: "InventoryMoveFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("dogmaIM", "GetShipAttributes") | ("dogmaIM", "GetAllInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = bound_entry
                .map(|b| b.object_id)
                .or(session.ship_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };

            let fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            let base_attrs = fit.ship.base_attributes.clone();
            let mut entries = Vec::with_capacity(base_attrs.len() + 2);
            for (&attr_id, _) in &base_attrs {
                let val = fit.ship.get_attribute(attr_id, &ctx.sde);
                entries.push((PyRep::Integer(attr_id as i64), PyRep::Real(val)));
            }
            entries.push((
                PyRep::Integer(attr_ids::CPU_LOAD as i64),
                PyRep::Real(fit.ship.get_attribute(attr_ids::CPU_LOAD, &ctx.sde)),
            ));
            entries.push((
                PyRep::Integer(attr_ids::POWER_LOAD as i64),
                PyRep::Real(fit.ship.get_attribute(attr_ids::POWER_LOAD, &ctx.sde)),
            ));
            ctx.store_ship_fitting(ship_id, fit);

            if method == "GetAllInfo" {
                Some(Ok(PyRep::keyval([
                    ("shipID", PyRep::Integer(ship_id)),
                    ("charID", PyRep::Integer(char_id)),
                    ("shipAttributes", PyRep::Dict(entries)),
                ])))
            } else {
                Some(Ok(PyRep::Dict(entries)))
            }
        }

        ("dogmaIM", "LoadAmmo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let default_ship_id = bound_entry
                .map(|b| b.object_id)
                .or(session.ship_id);
            let (ship_id, modules_arg, charges_arg) = if args.len() >= 3 {
                let sid = args.first().and_then(|a| a.as_i64()).or(default_ship_id);
                (sid, args.get(1), args.get(2))
            } else {
                (default_ship_id, args.first(), args.get(1))
            };
            let Some(ship_id) = ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let module_or_flag_ids: Vec<i64> = match modules_arg {
                Some(PyRep::List(v) | PyRep::Tuple(v)) => {
                    v.iter().filter_map(|x| x.as_i64()).collect()
                }
                Some(v) => v.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            let charge_item_id_opt = match charges_arg {
                Some(PyRep::List(v) | PyRep::Tuple(v)) => v.first().and_then(|x| x.as_i64()),
                Some(v) => v.as_i64(),
                None => kwargs.get("chargeItemID").and_then(|v| v.as_i64()),
            };
            let Some(charge_item_id) = charge_item_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "LoadAmmo requires a charge itemID".into(),
                }));
            };
            if module_or_flag_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "LoadAmmo requires at least one weapon module itemID or high-slot flag (27..=34)".into(),
                }));
            }

            let current_dock = session.station_id.or(session.structure_id);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut flags = Vec::with_capacity(module_or_flag_ids.len());
                for id_or_flag in &module_or_flag_ids {
                    if (27..=34).contains(id_or_flag) {
                        flags.push(InventoryFlag::from_u16(*id_or_flag as u16));
                    } else if let Some(mod_item) = snap.items.get(id_or_flag) {
                        if mod_item.owner_id != char_id || mod_item.location_id != ship_id {
                            return Err(BridgeRpcError {
                                code: "ModuleNotFitted".into(),
                                message: format!(
                                    "Module {id_or_flag} is not fitted on ship {ship_id}"
                                ),
                            });
                        }
                        flags.push(mod_item.flag);
                    } else {
                        return Err(BridgeRpcError {
                            code: "ModuleNotFound".into(),
                            message: format!(
                                "Weapon module or high-slot flag {id_or_flag} not found on ship {ship_id}"
                            ),
                        });
                    }
                }

                let mut loaded_slots = Vec::with_capacity(flags.len());
                let mut total_loaded: i64 = 0;
                let mut first_loaded: Option<(i64, u32, u16, i32, u64)> = None;
                for flag in flags {
                    let (lid, ltid, lqty, lver) = ctx.load_charge_into_weapon_slot_in_snapshot(
                        snap,
                        char_id,
                        ship_id,
                        flag,
                        charge_item_id,
                        current_dock,
                    )?;
                    if first_loaded.is_none() {
                        first_loaded = Some((lid, ltid, flag as u16, lqty, lver));
                    }
                    total_loaded += lqty as i64;
                    loaded_slots.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(lid)),
                        ("typeID", PyRep::Integer(ltid as i64)),
                        ("flagID", PyRep::Integer(flag as i64)),
                        ("quantity", PyRep::Integer(lqty as i64)),
                        ("version", PyRep::Integer(lver as i64)),
                    ]));
                }
                Ok((first_loaded, total_loaded, loaded_slots))
            });
            match res {
                Ok((first_loaded, total_loaded, loaded_slots)) => {
                    let (first_id, first_tid, first_flag, first_qty, first_ver) =
                        first_loaded.unwrap_or((0, 0, 27, 0, 1));
                    Some(Ok(PyRep::keyval([
                        ("shipID", PyRep::Integer(ship_id)),
                        ("itemID", PyRep::Integer(first_id)),
                        ("typeID", PyRep::Integer(first_tid as i64)),
                        ("flagID", PyRep::Integer(first_flag as i64)),
                        ("quantity", PyRep::Integer(first_qty as i64)),
                        ("version", PyRep::Integer(first_ver as i64)),
                        ("totalLoaded", PyRep::Integer(total_loaded)),
                        ("loadedSlots", PyRep::List(loaded_slots)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("dogmaIM", "Activate") | ("dogmaIM", "Overload") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let items = ctx.db.get_items_for_character(char_id);
            let Some(mod_item) = items
                .iter()
                .find(|i| i.item_id == module_id && i.owner_id == char_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "ModuleNotFound".into(),
                    message: format!("Module {module_id} not owned by character {char_id}"),
                }));
            };
            if mod_item.location_id != ship_id || (mod_item.flag as u16) < 11 {
                return Some(Err(BridgeRpcError {
                    code: "ModuleNotFitted".into(),
                    message: format!(
                        "Module {module_id} must be fitted to active ship {ship_id} before activation"
                    ),
                }));
            }

            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            if let Err(e) = fit.activate_module(module_id, method == "Overload", &ctx.sde) {
                return Some(Err(BridgeRpcError {
                    code: "DogmaActivateFailed".into(),
                    message: e.to_string(),
                }));
            }

            let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("overloaded", PyRep::Bool(method == "Overload")),
                ("effectiveMaxVelocity", PyRep::Real(max_vel)),
            ])))
        }

        ("dogmaIM", "Deactivate") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            if let Err(e) = fit.deactivate_module(module_id, &ctx.sde) {
                return Some(Err(BridgeRpcError {
                    code: "DogmaDeactivateFailed".into(),
                    message: e.to_string(),
                }));
            }
            let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);
            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("overloaded", PyRep::Bool(false)),
                ("effectiveMaxVelocity", PyRep::Real(max_vel)),
            ])))
        }

        ("charFittingMgr", "SaveFitting") | ("corpFittingMgr", "SaveFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let name = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("Rifter Combat Fit");
            let ship_type_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(587) as TypeId;

            let is_valid_hull = ctx
                .sde
                .types
                .get(&ship_type_id)
                .map(|t| t.category_id == 6)
                .unwrap_or(false);
            if !is_valid_hull {
                return Some(Err(BridgeRpcError {
                    code: "InvalidShipType".into(),
                    message: format!("Type {ship_type_id} is not a valid ship hull"),
                }));
            }

            let (description, items_arg) = match args.get(2) {
                Some(PyRep::String(s)) => (s.clone(), args.get(3)),
                Some(list_or_tuple @ (PyRep::List(_) | PyRep::Tuple(_))) => (
                    "Saved via charFittingMgr".to_string(),
                    Some(list_or_tuple),
                ),
                _ => ("Saved via charFittingMgr".to_string(), args.get(3)),
            };

            let parsed_items: Vec<(TypeId, InventoryFlag, i32)> = if let Some(arg) = items_arg {
                let entries = match arg {
                    PyRep::List(v) | PyRep::Tuple(v) => v,
                    _ => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidFittingItems".into(),
                            message: "Fitting items must be a List or Tuple of (typeID, flagID, quantity)".into(),
                        }));
                    }
                };
                let mut out = Vec::with_capacity(entries.len());
                for entry in entries {
                    let seq = match entry {
                        PyRep::Tuple(v) | PyRep::List(v) => v,
                        _ => {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: "Each fitting item must be a (typeID, flagID, quantity) tuple".into(),
                            }));
                        }
                    };
                    let Some(mod_type_id) = seq.first().and_then(|v| v.as_i64()).filter(|&v| v > 0)
                    else {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidFittingItem".into(),
                            message: "Fitting item is missing a positive typeID".into(),
                        }));
                    };
                    let Some(flag_raw) = seq.get(1).and_then(|v| v.as_i64()).filter(|&v| v >= 0)
                    else {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidFittingItem".into(),
                            message: "Fitting item is missing a valid flagID".into(),
                        }));
                    };
                    let qty_i64 = seq.get(2).and_then(|v| v.as_i64()).unwrap_or(1);
                    let Ok(qty) = i32::try_from(qty_i64) else {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidFittingItem".into(),
                            message: format!(
                                "Fitting item quantity overflows i32 bounds, got {qty_i64}"
                            ),
                        }));
                    };
                    if qty <= 0 {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidFittingItem".into(),
                            message: format!("Fitting item quantity must be positive, got {qty}"),
                        }));
                    }
                    let flag_u16 = u16::try_from(flag_raw).ok();
                    let flag = flag_u16.map(InventoryFlag::from_u16).unwrap_or(InventoryFlag::None);
                    let valid_flag = flag_u16
                        .map(|f| {
                            matches!(f, 5 | 11..=34 | 87 | 92..=94 | 125..=132)
                                && (flag as u16) == f
                        })
                        .unwrap_or(false);
                    if !valid_flag {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidFittingFlag".into(),
                            message: format!("Invalid fitting slot or bay flag {flag_raw}"),
                        }));
                    }
                    out.push((mod_type_id as TypeId, flag, qty));
                }
                out
            } else {
                // Default to extracting currently fitted modules from the caller's active ship when it matches ship_type_id
                let char_items = ctx.db.get_items_for_character(char_id);
                let active_ship_matches = session
                    .ship_id
                    .and_then(|sid| char_items.iter().find(|i| i.item_id == sid))
                    .map(|s| s.type_id == ship_type_id)
                    .unwrap_or(false);
                if active_ship_matches {
                    let sid = session.ship_id.unwrap_or(0);
                    char_items
                        .into_iter()
                        .filter(|i| {
                            let is_mod_or_drone = ctx
                                .sde
                                .types
                                .get(&i.type_id)
                                .map(|t| t.category_id == 7 || t.category_id == 32 || (i.flag as u16) == 87)
                                .unwrap_or(false);
                            i.location_id == sid
                                && is_mod_or_drone
                                && matches!(
                                    i.flag as u16,
                                    11..=34 | 87 | 92..=94 | 125..=132
                                )
                        })
                        .map(|i| (i.type_id, i.flag, i.quantity))
                        .collect()
                } else {
                    Vec::new()
                }
            };

            // Validate every fitted slot item against ShipFittingEngine for `ship_type_id`
            let all_skills_lvl5: HashMap<TypeId, u8> = ctx
                .sde
                .types
                .values()
                .filter(|t| t.category_id == 16)
                .map(|t| (t.type_id, 5))
                .collect();
            let mut validator = eve_dogma::ShipFittingEngine::from_sde(0, ship_type_id, &ctx.sde)
                .with_character_skills(all_skills_lvl5);
            for (idx, &(mod_type_id, flag, qty)) in parsed_items.iter().enumerate() {
                let flag_u16 = flag as u16;
                if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                    if qty != 1 {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidFittingQuantity".into(),
                            message: format!(
                                "Fitting slot {flag_u16} requires quantity 1, got {qty}"
                            ),
                        }));
                    }
                    if let Err(e) = validator.fit_and_online_module(
                        (idx as i64) + 1,
                        mod_type_id,
                        flag_u16,
                        &ctx.sde,
                    ) {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidFittingModule".into(),
                            message: e,
                        }));
                    }
                } else if !ctx.sde.types.contains_key(&mod_type_id) {
                    return Some(Err(BridgeRpcError {
                        code: "UnknownItemType".into(),
                        message: format!("Unknown item type {mod_type_id} in saved fitting"),
                    }));
                }
            }

            let fid = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    snap.next_entity_id += 1;
                    let id = snap.next_entity_id;
                    snap.saved_fittings.insert(
                        id,
                        SavedFittingRecord {
                            fitting_id: id,
                            owner_id: char_id,
                            name: name.to_string(),
                            ship_type_id,
                            description,
                            items: parsed_items,
                        },
                    );
                    Ok::<_, std::convert::Infallible>(id)
                })
                .unwrap();
            Some(Ok(PyRep::Integer(fid)))
        }

        ("charFittingMgr", "GetFittings") | ("corpFittingMgr", "GetFittings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.saved_fittings
                    .values()
                    .filter(|f| f.owner_id == char_id)
                    .map(|f| {
                        let item_tuples: Vec<PyRep> = f
                            .items
                            .iter()
                            .map(|&(tid, flg, qty)| {
                                PyRep::Tuple(vec![
                                    PyRep::Integer(tid as i64),
                                    PyRep::Integer(flg as i64),
                                    PyRep::Integer(qty as i64),
                                ])
                            })
                            .collect();
                        vec![
                            PyRep::Integer(f.fitting_id),
                            PyRep::String(f.name.clone()),
                            PyRep::Integer(f.ship_type_id as i64),
                            PyRep::String(f.description.clone()),
                            PyRep::List(item_tuples),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["fittingID", "name", "shipTypeID", "description", "items"],
                rows,
            )))
        }

        ("charFittingMgr", "DeleteFitting") | ("corpFittingMgr", "DeleteFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let deleted = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    if snap
                        .saved_fittings
                        .get(&fid)
                        .map(|f| f.owner_id == char_id)
                        .unwrap_or(false)
                    {
                        snap.saved_fittings.remove(&fid);
                        Ok::<_, std::convert::Infallible>(true)
                    } else {
                        Ok::<_, std::convert::Infallible>(false)
                    }
                })
                .unwrap();
            Some(Ok(PyRep::Bool(deleted)))
        }

        ("insuranceSvc", "InsureShip") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
                .unwrap_or(100_000_001);
            let level_fraction = args
                .get(1)
                .and_then(|a| a.as_f64())
                .unwrap_or(1.0)
                .clamp(0.0, 1.0);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ship) = snap.items.get(&ship_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ShipNotFound".into(),
                        message: format!("Ship {ship_id} not found"),
                    });
                };
                if ship.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "OwnershipViolation".into(),
                        message: format!("Ship {ship_id} does not belong to character {char_id}"),
                    });
                }
                let base_price = ctx
                    .sde
                    .types
                    .get(&ship.type_id)
                    .map(|t| t.base_price)
                    .unwrap_or(230_000.0);
                let (cost, payout) = compute_insurance_quote(base_price, level_fraction);
                let cost_cents = IskCents::from_f64_isk(cost);

                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let new_bal = ch.balance.checked_sub(cost_cents).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Not enough ISK to insure ship".into(),
                })?;
                ch.balance = new_bal;
                if let Some(ship_mut) = snap.items.get_mut(&ship_id) {
                    ship_mut.insured_payout_isk = payout;
                }
                Ok((cost, payout))
            });

            match res {
                Ok((cost, payout)) => Some(Ok(PyRep::keyval([
                    ("shipID", PyRep::Integer(ship_id)),
                    ("costISK", PyRep::Real(cost)),
                    ("payoutISK", PyRep::Real(payout)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("insuranceSvc", "GetInsurancePrice") | ("insuranceSvc", "GetContractForShip") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
                .unwrap_or(100_000_001);
            let info = ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&ship_id)
                    .filter(|i| i.owner_id == char_id)
                    .map(|i| (i.type_id, i.insured_payout_isk))
            });
            let (type_id, current_payout) = info.unwrap_or((587, 0.0));
            let base_price = ctx
                .sde
                .types
                .get(&type_id)
                .map(|t| t.base_price)
                .unwrap_or(230_000.0);
            let (platinum_cost, platinum_payout) = compute_insurance_quote(base_price, 1.0);
            Some(Ok(PyRep::keyval([
                ("shipID", PyRep::Integer(ship_id)),
                ("platinumCostISK", PyRep::Real(platinum_cost)),
                ("platinumPayoutISK", PyRep::Real(platinum_payout)),
                ("currentInsuredPayoutISK", PyRep::Real(current_payout)),
            ])))
        }

        ("dynamicItemService", "CreateDynamicItem") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mutaplasmid_type_id =
                args.first().and_then(|a| a.as_i64()).unwrap_or(47702) as TypeId;
            let target_item_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(100_000_003);

            let Some(muta_def) = ctx.sde.dynamic_item_attributes.get(&mutaplasmid_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMutaplasmid".into(),
                    message: format!("Unknown mutaplasmid type {mutaplasmid_type_id}"),
                }));
            };

            let current_dock = session.station_id.or(session.structure_id);
            let current_ship = session.ship_id;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Verify target item exists, belongs to char_id, is unlocked & reachable, and has an applicable type_id
                let Some(target_item) = snap.items.get(&target_item_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "TargetItemNotFound".into(),
                        message: format!("Target item {target_item_id} not found"),
                    });
                };
                if target_item.owner_id != char_id || target_item.quantity <= 0 {
                    return Err(BridgeRpcError {
                        code: "OwnershipViolation".into(),
                        message: format!(
                            "Target item {target_item_id} is not owned by character {char_id}"
                        ),
                    });
                }
                let active_ship_colocated = current_ship
                    .map(|sid| snap.is_ship_colocated_with_character(char_id, sid, current_dock))
                    .unwrap_or(false);
                let target_reachable = (current_dock.is_some()
                    && Some(target_item.location_id) == current_dock
                    && target_item.flag == InventoryFlag::Hangar)
                    || (active_ship_colocated
                        && Some(target_item.location_id) == current_ship
                        && target_item.flag != InventoryFlag::Locked);
                if target_item.flag == InventoryFlag::Locked
                    || !target_reachable
                    || snap.is_item_locked_in_contract_or_job(target_item_id)
                {
                    return Err(BridgeRpcError {
                        code: "TargetItemLockedOrUnreachable".into(),
                        message: format!(
                            "Target item {target_item_id} is locked in a contract/job or not at the caller's current station/structure or active ship"
                        ),
                    });
                }
                if !muta_def.applicable_type_ids.contains(&target_item.type_id) {
                    return Err(BridgeRpcError {
                        code: "InapplicableMutaplasmid".into(),
                        message: format!(
                            "Mutaplasmid {mutaplasmid_type_id} cannot be applied to item type {}",
                            target_item.type_id
                        ),
                    });
                }

                // Locate and consume 1 unlocked, reachable mutaplasmid item owned by char_id
                let muta_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.type_id == mutaplasmid_type_id
                            && i.quantity >= 1
                            && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
                    })
                    .map(|i| i.item_id);
                let Some(mid) = muta_item_id else {
                    return Err(BridgeRpcError {
                        code: "MutaplasmidItemNotFound".into(),
                        message: format!(
                            "Character does not own an unlocked, reachable mutaplasmid type {mutaplasmid_type_id}"
                        ),
                    });
                };

                if let Some(m_item) = snap.items.get_mut(&mid) {
                    if m_item.quantity > 1 {
                        m_item.quantity -= 1;
                        m_item.version += 1;
                    } else {
                        snap.items.remove(&mid);
                    }
                }

                let base_attrs = ctx
                    .sde
                    .types
                    .get(&target_item.type_id)
                    .map(|t| t.default_attributes.clone())
                    .unwrap_or_default();
                let now_unix = ctx.now_unix();
                let mut sorted_attr_keys: Vec<AttributeId> =
                    muta_def.attribute_ranges.keys().copied().collect();
                sorted_attr_keys.sort_unstable();
                let rolls: HashMap<AttributeId, f64> = sorted_attr_keys
                    .into_iter()
                    .map(|k| {
                        (
                            k,
                            ctx.next_invention_roll(
                                (target_item_id as u64).wrapping_add(k as u64),
                                char_id as u64,
                                now_unix,
                            ),
                        )
                    })
                    .collect();
                let rolled =
                    abyssal_attribute_roll(&base_attrs, &muta_def.attribute_ranges, &rolls);

                let mutated_item_id = if target_item.quantity > 1 {
                    if let Some(orig) = snap.items.get_mut(&target_item_id) {
                        orig.quantity -= 1;
                        orig.version += 1;
                    }
                    snap.next_entity_id += 1;
                    let new_id = snap.next_entity_id;
                    snap.items.insert(
                        new_id,
                        eve_db::InventoryItemRecord {
                            item_id: new_id,
                            type_id: muta_def.resulting_type_id,
                            owner_id: char_id,
                            location_id: target_item.location_id,
                            flag: target_item.flag,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            dynamic_attributes: rolled.clone(),
                            source_mutaplasmid_id: Some(mutaplasmid_type_id),
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                    new_id
                } else {
                    if let Some(item) = snap.items.get_mut(&target_item_id) {
                        item.type_id = muta_def.resulting_type_id;
                        item.quantity = 1;
                        item.singleton = true;
                        item.dynamic_attributes = rolled.clone();
                        item.source_mutaplasmid_id = Some(mutaplasmid_type_id);
                        item.version += 1;
                    }
                    target_item_id
                };
                Ok((mutated_item_id, rolled))
            });

            match res {
                Ok((mutated_item_id, rolled)) => {
                    let spd = rolled
                        .get(&attr_ids::SPEED_FACTOR)
                        .copied()
                        .unwrap_or(150.0);
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(mutated_item_id)),
                        (
                            "resultingTypeID",
                            PyRep::Integer(muta_def.resulting_type_id as i64),
                        ),
                        ("rolledSpeedFactor", PyRep::Real(spd)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("dynamicItemService", "GetDynamicItemInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_003);
            let res = ctx.db.with_snapshot(|snap| snap.items.get(&item_id).cloned());
            match res {
                Some(item) => {
                    if item.owner_id != char_id {
                        return Some(Err(BridgeRpcError {
                            code: "OwnershipViolation".into(),
                            message: format!(
                                "Item {item_id} is not owned by character {char_id}"
                            ),
                        }));
                    }
                    let attr_entries: Vec<(PyRep, PyRep)> = item
                        .dynamic_attributes
                        .iter()
                        .map(|(&k, &v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
                        .collect();
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(item.item_id)),
                        ("typeID", PyRep::Integer(item.type_id as i64)),
                        (
                            "mutaplasmidTypeID",
                            item.source_mutaplasmid_id
                                .map(|m| PyRep::Integer(m as i64))
                                .unwrap_or(PyRep::None),
                        ),
                        ("attributes", PyRep::Dict(attr_entries)),
                    ])))
                }
                None => Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Item {item_id} not found"),
                })),
            }
        }

        ("repackagingSvc", "RepackageItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Repackaging items requires being docked".into(),
                }));
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if Some(item_id) == session.ship_id {
                return Some(Err(BridgeRpcError {
                    code: "CannotRepackageActiveShip".into(),
                    message: "Cannot repackage your currently active ship".into(),
                }));
            }
            let has_module_damage = ctx
                .ship_fittings
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&item_id)
                .map(|fit| {
                    fit.rack_heat.iter().any(|&h| h > 0.0)
                        || fit.modules.values().any(|m| m.damage_hp > 0.0)
                })
                .unwrap_or(false);
            if has_module_damage {
                return Some(Err(BridgeRpcError {
                    code: "CannotRepackageDamagedItem".into(),
                    message: format!(
                        "Item {item_id} has heat or module damage and must be repaired before repackaging"
                    ),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.active_ship_id == item_id)
                    .unwrap_or(false)
                {
                    return Err(BridgeRpcError {
                        code: "CannotRepackageActiveShip".into(),
                        message: "Cannot repackage your currently active ship".into(),
                    });
                }
                if snap.is_item_locked_in_contract_or_job(item_id) {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!(
                            "Item {item_id} is locked in an active contract or industry job"
                        ),
                    });
                }
                let has_contained_items = snap
                    .items
                    .values()
                    .any(|other| other.item_id != item_id && other.location_id == item_id && other.quantity > 0);
                if has_contained_items {
                    return Err(BridgeRpcError {
                        code: "CannotRepackageNonEmptyItem".into(),
                        message: format!(
                            "Cannot repackage item {item_id} while it contains fitted modules, rigs, or cargo"
                        ),
                    });
                }
                let Some(item) = snap.items.get_mut(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Item {item_id} not found"),
                    });
                };
                if item.owner_id != char_id || item.location_id != station_id {
                    return Err(BridgeRpcError {
                        code: "OwnershipOrLocationViolation".into(),
                        message: format!(
                            "Item {item_id} must be owned by character {char_id} at station {station_id}"
                        ),
                    });
                }
                if item.flag != InventoryFlag::Hangar {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!(
                            "Item {item_id} must be in your station Hangar (current flag: {:?}) to repackage",
                            item.flag
                        ),
                    });
                }
                if item.is_blueprint_copy {
                    return Err(BridgeRpcError {
                        code: "CannotRepackageBlueprintCopy".into(),
                        message: format!("Blueprint copy {item_id} cannot be repackaged"),
                    });
                }
                if item.source_mutaplasmid_id.is_some() || !item.dynamic_attributes.is_empty() {
                    return Err(BridgeRpcError {
                        code: "CannotRepackageMutatedItem".into(),
                        message: format!("Abyssal mutated item {item_id} cannot be repackaged"),
                    });
                }
                item.singleton = false;
                item.insured_payout_isk = 0.0;
                item.version += 1;
                Ok(item.version)
            });
            match res {
                Ok(ver) => {
                    ctx.invalidate_ship_fitting(item_id);
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(item_id)),
                        ("singleton", PyRep::Bool(false)),
                        ("version", PyRep::Integer(ver as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("ship", "AssembleShip") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Assembling a ship requires being docked".into(),
                }));
            };
            let item_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
                .unwrap_or(100_000_001);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(item) = snap.items.get(&item_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Ship item {item_id} not found"),
                    });
                };
                if item.owner_id != char_id || item.location_id != station_id {
                    return Err(BridgeRpcError {
                        code: "OwnershipOrLocationViolation".into(),
                        message: format!(
                            "Ship item {item_id} must be owned by character {char_id} at station {station_id}"
                        ),
                    });
                }
                if item.flag == InventoryFlag::Locked {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!("Item {item_id} is locked"),
                    });
                }
                let is_ship = ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .map(|t| t.category_id == 6)
                    .unwrap_or(false);
                if !is_ship {
                    return Err(BridgeRpcError {
                        code: "NotAShip".into(),
                        message: format!("Item {item_id} (type {}) is not a ship", item.type_id),
                    });
                }
                if !item.singleton && item.quantity > 1 {
                    if let Some(orig) = snap.items.get_mut(&item_id) {
                        orig.quantity -= 1;
                        orig.version += 1;
                    }
                    snap.next_entity_id += 1;
                    let new_ship_id = snap.next_entity_id;
                    snap.items.insert(
                        new_ship_id,
                        eve_db::InventoryItemRecord {
                            item_id: new_ship_id,
                            type_id: item.type_id,
                            owner_id: char_id,
                            location_id: station_id,
                            flag: item.flag,
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
                    Ok((new_ship_id, 1_u64))
                } else {
                    let item_mut = snap.items.get_mut(&item_id).unwrap();
                    item_mut.singleton = true;
                    item_mut.version += 1;
                    Ok((item_id, item_mut.version))
                }
            });
            match res {
                Ok((assembled_id, ver)) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(assembled_id)),
                    ("singleton", PyRep::Bool(true)),
                    ("version", PyRep::Integer(ver as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("ship", "Board") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_ship_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
                .unwrap_or(100_000_001);

            if session.station_id.is_none() && session.structure_id.is_none() {
                let Some(sys_id) = session.solar_system_id else {
                    return Some(Err(BridgeRpcError {
                        code: "NoSolarSystem".into(),
                        message: "Session has no active solar system".into(),
                    }));
                };
                let Some(cur_ship_id) = session.ship_id else {
                    return Some(Err(BridgeRpcError {
                        code: "NoActiveShip".into(),
                        message: "Cannot board in space without an active vessel/capsule".into(),
                    }));
                };
                if cur_ship_id != target_ship_id {
                    let sys_handle = ctx.systems.get_or_spawn(sys_id);
                    let cur_ball = sys_handle.get_ball_snapshot(cur_ship_id).await.ok().flatten();
                    let tgt_ball = sys_handle
                        .get_ball_snapshot(target_ship_id)
                        .await
                        .ok()
                        .flatten();
                    let (Some((_, cb)), Some((_, tb))) = (cur_ball, tgt_ball) else {
                        return Some(Err(BridgeRpcError {
                            code: "ShipNotInSpace".into(),
                            message: format!(
                                "Target ship {target_ship_id} is not spawned on grid in system {sys_id}"
                            ),
                        }));
                    };
                    let dist = cb.position.distance(tb.position);
                    if dist > 2_500.0 {
                        return Some(Err(BridgeRpcError {
                            code: "ShipOutOfBoardingRange".into(),
                            message: format!(
                                "Target ship {target_ship_id} is {:.1} m away (must be <= 2500 m)",
                                dist
                            ),
                        }),
                        );
                    }
                }
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ship) = snap.items.get(&target_ship_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ShipNotFound".into(),
                        message: format!("Ship {target_ship_id} not found"),
                    });
                };
                if ship.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "OwnershipViolation".into(),
                        message: format!(
                            "Ship {target_ship_id} is not owned by character {char_id}"
                        ),
                    });
                }
                let reachable = if let Some(dock_id) = session.station_id.or(session.structure_id) {
                    ship.location_id == dock_id && ship.flag == InventoryFlag::Hangar
                } else {
                    Some(ship.location_id) == session.solar_system_id.map(|s| s as i64)
                };
                if !reachable
                    || !ship.singleton
                    || ship.flag == InventoryFlag::Locked
                    || snap.is_item_locked_in_contract_or_job(target_ship_id)
                {
                    return Err(BridgeRpcError {
                        code: "ShipNotBoardable".into(),
                        message: format!(
                            "Ship {target_ship_id} must be assembled, unlocked, and at your current location"
                        ),
                    });
                }
                let is_ship = ctx
                    .sde
                    .types
                    .get(&ship.type_id)
                    .map(|t| t.category_id == 6)
                    .unwrap_or(false);
                if !is_ship {
                    return Err(BridgeRpcError {
                        code: "NotAShip".into(),
                        message: format!("Item {target_ship_id} is not a ship"),
                    });
                }
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.active_ship_id = target_ship_id;
                }
                Ok(target_ship_id)
            });
            match res {
                Ok(sid) => {
                    if let Some(prev) = session.ship_id {
                        ctx.invalidate_ship_fitting(prev);
                    }
                    ctx.invalidate_ship_fitting(sid);
                    session.ship_id = Some(sid);
                    Some(Ok(PyRep::keyval([
                        ("characterID", PyRep::Integer(char_id)),
                        ("shipID", PyRep::Integer(sid)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("repairSvc", "RepairAll") | ("repairSvc", "RepairItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() && session.structure_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Repairing ship and modules requires being docked".into(),
                }));
            }
            let ship_id = session.ship_id.unwrap_or(100_000_001);
            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let mut total_repaired_hp = 0.0;
            for m in fit.modules.values_mut() {
                if m.damage_hp > 0.0 {
                    total_repaired_hp += m.damage_hp;
                    m.damage_hp = 0.0;
                }
            }
            let cost_isk = (total_repaired_hp * 500.0).round();
            if cost_isk > 0.0 {
                let debit_res = ctx.db.try_with_mut_snapshot(|snap| {
                    let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })?;
                    let fee = IskCents::from_f64_isk(cost_isk);
                    ch.balance = ch.balance.checked_sub(fee).ok_or_else(|| BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Insufficient ISK to pay repair bill".into(),
                    })?;
                    Ok(())
                });
                if let Err(e) = debit_res {
                    return Some(Err(e));
                }
            }
            ctx.store_ship_fitting(ship_id, fit);
            let repaired_count = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id && (i.item_id == ship_id || i.location_id == ship_id)
                    })
                    .count()
            });
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("shipID", PyRep::Integer(ship_id)),
                ("itemsInspected", PyRep::Integer(repaired_count as i64)),
                ("repairedModuleHP", PyRep::Real(total_repaired_hp)),
                ("repairCostISK", PyRep::Real(cost_isk)),
                ("repaired", PyRep::Bool(true)),
            ])))
        }

        _ => None,
    }
}
