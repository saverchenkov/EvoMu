//! `inventory_fitting` — Domain 3: Inventory Broker, Ship Fitting, Insurance, Repair, & Abyssal Mutaplasmids (`§3.4`).

#![forbid(unsafe_code)]

use crate::formulas::{abyssal_attribute_roll, compute_insurance_quote};
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{InventoryFlag, IskCents, SavedFittingRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use eve_sde::{attr_ids, AttributeId, TypeId};
use glam::DVec3;
use std::collections::HashMap;

fn item_to_packed_cells(ctx: &ServerContext, i: &eve_db::InventoryItemRecord) -> Vec<PyRep> {
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
    vec![
        PyRep::Integer(i.item_id),
        PyRep::Integer(i.type_id as i64),
        PyRep::Integer(i.owner_id),
        PyRep::Integer(i.location_id),
        PyRep::Integer(i.flag as i64),
        PyRep::Integer(qty),
        PyRep::Integer(group_id),
        PyRep::Integer(category_id),
        PyRep::String(String::new()),
    ]
}

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
        ("invbroker", "GetItemDescriptor") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            Some(Ok(DBRowDescriptor::inv_item_header().to_pyrep()))
        }

        ("invbroker", "List") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(bound_loc_id) = bound_entry
                .map(|b| b.object_id)
                .or(session.station_id)
                .or(session.structure_id)
                .or(session.ship_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLocation".into(),
                    message: "invbroker.List requires a bound inventory location or active station/ship".into(),
                }));
            };
            let list_flag_filter = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()))
                .filter(|&f| f > 0);
            let rows = ctx
                .db
                .get_items_for_character(char_id)
                .into_iter()
                .filter(|i| {
                    i.location_id == bound_loc_id
                        && list_flag_filter.map(|f| (i.flag as i64) == f).unwrap_or(true)
                })
                .map(|i| item_to_packed_cells(ctx, &i))
                .collect();
            Some(Ok(DBRowDescriptor::inv_item_header().build_crowset(rows)))
        }

        ("invbroker", "GetContainerContents") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(container_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetContainerContents requires containerID".into(),
                }));
            };
            Some(
                ctx.get_container_contents(session, bound_entry, char_id, container_id)
                    .await,
            )
        }

        ("invbroker", "MoveItem") | ("invbroker", "Add") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "MoveItem requires itemID".into() }));
            };
            let Some(new_loc) = args.get(1).and_then(|a| a.as_i64()).or_else(|| bound_entry.map(|b| b.object_id)).or(session.ship_id).or(session.station_id).or(session.structure_id) else {
                return Some(Err(BridgeRpcError { code: "InvalidLocation".into(), message: "MoveItem requires a target locationID, bound inventory, or active ship/station".into() }));
            };
            let raw_flag_opt = args.get(2).and_then(|a| a.as_i64()).or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()));
            if let Some(rf) = raw_flag_opt {
                if !matches!(rf, 4 | 5 | 11..=34 | 87 | 90 | 92..=94 | 125..=151 | 179 | 181 | 182) {
                    return Some(Err(BridgeRpcError { code: "InvalidInventoryFlag".into(), message: format!("Invalid or restricted inventory flag {rf}") }));
                }
            }
            let flag = raw_flag_opt.map(|v| InventoryFlag::from_u16(v as u16)).unwrap_or(InventoryFlag::Cargo);
            let flag_u16 = flag as u16;
            let source_item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&item_id).cloned());
            if let Some(src_item) = &source_item_opt {
                if matches!(src_item.flag as u16, 92..=94) && !matches!(flag_u16, 92..=94) {
                    let old_loc = src_item.location_id;
                    ctx.db.with_mut_snapshot(|snap| {
                        snap.items.remove(&item_id);
                        snap.module_online_states.remove(&item_id);
                    });
                    ctx.invalidate_ship_fitting(old_loc);
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnModuleUnfitted",
                        "charid",
                        vec![PyRep::Integer(item_id)],
                    );
                    return Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(item_id)),
                        ("destroyed", PyRep::Bool(true)),
                    ])));
                }
            }

            let online = kwargs
                .get("online")
                .and_then(|v| match v {
                    PyRep::Bool(b) => Some(*b),
                    _ => None,
                })
                .unwrap_or(true);

            let is_fitting_slot = matches!(flag_u16, 11..=34 | 92..=94 | 125..=132);
            if is_fitting_slot {
                let item_cat_opt = ctx.db.with_snapshot(|snap| {
                    snap.items.get(&item_id).and_then(|item| ctx.sde.types.get(&item.type_id).map(|t| t.category_id))
                });
                if item_cat_opt == Some(8) && matches!(flag_u16, 27..=34) {
                    let current_dock = session.station_id.or(session.structure_id);
                    return match ctx.load_charge_into_weapon_slot(char_id, new_loc, flag, item_id, current_dock) {
                        Ok((loaded_id, loaded_type_id, loaded_qty, loaded_ver)) => Some(Ok(PyRep::keyval([
                            ("itemID", PyRep::Integer(loaded_id)),
                            ("typeID", PyRep::Integer(loaded_type_id as i64)),
                            ("locationID", PyRep::Integer(new_loc)),
                            ("flagID", PyRep::Integer(flag as i64)),
                            ("quantity", PyRep::Integer(loaded_qty as i64)),
                            ("version", PyRep::Integer(loaded_ver as i64)),
                        ]))),
                        Err(e) => Some(Err(e)),
                    };
                }
                if session.station_id.is_none() && session.structure_id.is_none() {
                    return Some(Err(BridgeRpcError { code: "CannotFitInSpace".into(), message: "Fitting modules into ship slots requires being docked at a station or structure".into() }));
                }
                let pre_check = ctx.db.with_snapshot(|snap| {
                    let Some(item) = snap.items.get(&item_id) else {
                        return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Item {item_id} not found") });
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
                    fit.fit_module_with_state(item_id, mod_type_id, flag_u16, online, &ctx.sde)
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

            let dest_is_ship_hold = matches!(flag_u16, 5 | 133..=151 | 181 | 182);
            if dest_is_ship_hold {
                let cap_check = ctx.db.with_snapshot(|snap| {
                    let Some(moving_item) = snap.items.get(&item_id) else { return Ok(()); };
                    let Some(ship_item) = snap.items.get(&new_loc) else { return Ok(()); };
                    let Some(ship_type) = ctx.sde.types.get(&ship_item.type_id) else { return Ok(()); };
                    let hold_capacity = match flag_u16 {
                        134 | 151 => ship_type.default_attributes.get(&1556).copied()
                            .or_else(|| ship_type.default_attributes.get(&1770).copied())
                            .unwrap_or(0.0),
                        181 => ship_type.default_attributes.get(&3136).copied()
                            .or_else(|| ship_type.default_attributes.get(&1556).copied())
                            .unwrap_or(0.0),
                        135 => ship_type.default_attributes.get(&1557).copied().unwrap_or(0.0),
                        5 => ship_type.capacity,
                        _ => 0.0,
                    };
                    if hold_capacity > 0.0 {
                        let used_vol: f64 = snap.items.values()
                            .filter(|i| i.location_id == new_loc && (i.flag as u16) == flag_u16 && i.item_id != item_id)
                            .map(|i| {
                                let vol = ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.0);
                                vol * (i.quantity as f64)
                            })
                            .sum();
                        let item_vol = ctx.sde.types.get(&moving_item.type_id).map(|t| t.volume).unwrap_or(0.0) * (moving_item.quantity as f64);
                        if used_vol + item_vol > hold_capacity + 1e-4 {
                            return Err(BridgeRpcError {
                                code: "CapacityExceeded".into(),
                                message: format!("Hold capacity of {hold_capacity}m³ exceeded (need {item_vol:.1}m³, have {:.1}m³ free)", (hold_capacity - used_vol).max(0.0)),
                            });
                        }
                    }
                    Ok(())
                });
                if let Err(e) = cap_check {
                    return Some(Err(e));
                }

                // Stack merging if matching unstacked item exists at destination
                let merged_opt: Result<Option<eve_db::InventoryItemRecord>, eve_db::DbError> = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(moving) = snap.items.get(&item_id).cloned() else {
                        return Ok(None);
                    };
                    if moving.singleton || moving.is_blueprint_copy {
                        return Ok(None);
                    }
                    let existing_target = snap.items.values().find(|i| {
                        i.item_id != item_id
                            && i.location_id == new_loc
                            && i.flag == flag
                            && i.type_id == moving.type_id
                            && !i.singleton
                            && !i.is_blueprint_copy
                    }).map(|i| i.item_id);
                    if let Some(target_iid) = existing_target {
                        snap.items.remove(&item_id);
                        let target_item = snap.items.get_mut(&target_iid).unwrap();
                        target_item.quantity += moving.quantity;
                        target_item.version += 1;
                        Ok(Some(target_item.clone()))
                    } else {
                        Ok(None)
                    }
                });
                if let Ok(Some(merged_item)) = merged_opt {
                    if let Some(ship_id) = session.ship_id {
                        ctx.invalidate_ship_fitting(ship_id);
                    }
                    ctx.invalidate_ship_fitting(new_loc);
                    return Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(merged_item.item_id)),
                        ("locationID", PyRep::Integer(merged_item.location_id)),
                        ("flagID", PyRep::Integer(merged_item.flag as i64)),
                        ("quantity", PyRep::Integer(merged_item.quantity as i64)),
                        ("version", PyRep::Integer(merged_item.version as i64)),
                    ])));
                }
            }

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

                    if is_fitting_slot {
                        ctx.db.with_mut_snapshot(|snap| {
                            snap.module_online_states.insert(item_id, online);
                        });
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnModuleFitted",
                            "charid",
                            vec![PyRep::Integer(item_id), PyRep::Integer(flag as i64)],
                        );
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnModuleAttributeChanges",
                            "charid",
                            vec![PyRep::Integer(item_id)],
                        );
                    } else if let Some(src_item) = &source_item_opt {
                        if matches!(src_item.flag as u16, 11..=34 | 92..=94 | 125..=132) {
                            ctx.db.with_mut_snapshot(|snap| {
                                snap.module_online_states.remove(&item_id);
                            });
                            ctx.notify(
                                crate::router::NotificationTarget::Character(char_id),
                                "OnModuleUnfitted",
                                "charid",
                                vec![PyRep::Integer(item_id)],
                            );
                        }
                    }

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

        ("dogmaIM", "DestroyFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DestroyFitting requires itemID".into(),
                }));
            };
            let loc_opt = ctx.db.with_mut_snapshot(|snap| {
                if let Some(item) = snap.items.get(&item_id) {
                    if item.owner_id != char_id {
                        return None;
                    }
                    let loc = item.location_id;
                    snap.items.remove(&item_id);
                    snap.module_online_states.remove(&item_id);
                    Some(loc)
                } else {
                    None
                }
            });
            if let Some(loc) = loc_opt {
                ctx.invalidate_ship_fitting(loc);
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnModuleUnfitted",
                    "charid",
                    vec![PyRep::Integer(item_id)],
                );
            }
            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(item_id)),
                ("destroyed", PyRep::Bool(true)),
            ])))
        }

        ("dogmaIM", "SetModuleOnline") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let module_id = args.get(1).and_then(|a| a.as_i64()).or_else(|| args.first().and_then(|a| a.as_i64())).unwrap_or(0);

            if session.station_id.is_none() && session.structure_id.is_none() {
                if let Some(sys_id) = session.solar_system_id {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                        if cs.max_capacitor > 0.0 && cs.capacitor < 0.75 * cs.max_capacitor {
                            return Some(Err(BridgeRpcError {
                                code: "NotEnoughCapacitorToOnline".into(),
                                message: format!(
                                    "Capacitor level ({:.1} GJ / {:.1} GJ) is below 75% requirement to online modules in space",
                                    cs.capacitor, cs.max_capacitor
                                ),
                            }));
                        }
                    }
                }
            }

            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            if let Err(e) = fit.set_module_online(module_id, &ctx.sde) {
                return Some(Err(BridgeRpcError {
                    code: "FittingValidationFailed".into(),
                    message: e,
                }));
            }
            ctx.store_ship_fitting(ship_id, fit);
            ctx.db.with_mut_snapshot(|snap| {
                snap.module_online_states.insert(module_id, true);
            });
            ctx.invalidate_ship_fitting(ship_id);

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnModuleOnlineChange",
                "charid",
                vec![PyRep::Integer(module_id), PyRep::Bool(true)],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnModuleAttributeChanges",
                "charid",
                vec![PyRep::Integer(module_id)],
            );

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("online", PyRep::Bool(true)),
            ])))
        }

        ("dogmaIM", "TakeModuleOffline") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let module_id = args.get(1).and_then(|a| a.as_i64()).or_else(|| args.first().and_then(|a| a.as_i64())).unwrap_or(0);

            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            if let Err(e) = fit.take_module_offline(module_id, &ctx.sde) {
                return Some(Err(BridgeRpcError {
                    code: "FittingValidationFailed".into(),
                    message: e,
                }));
            }
            ctx.store_ship_fitting(ship_id, fit);
            ctx.db.with_mut_snapshot(|snap| {
                snap.module_online_states.insert(module_id, false);
            });
            ctx.invalidate_ship_fitting(ship_id);

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnModuleOnlineChange",
                "charid",
                vec![PyRep::Integer(module_id), PyRep::Bool(false)],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnModuleAttributeChanges",
                "charid",
                vec![PyRep::Integer(module_id)],
            );

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("online", PyRep::Bool(false)),
            ])))
        }

        ("dogmaIM", "AddTarget") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let Some(target_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "AddTarget requires integer targetID".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session is not in space".into(),
                }));
            };
            let sys = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = sys.get_ball_snapshot(ship_id).await.unwrap_or(None) else {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Ship ball {ship_id} not found in system"),
                }));
            };
            let Some((_, target_ball)) = sys.get_ball_snapshot(target_id).await.unwrap_or(None) else {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Target ball {target_id} not found in system"),
                }));
            };

            let fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            let max_range = fit.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde);
            let dist = (ship_ball.position - target_ball.position).length();
            if dist > max_range {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Target distance ({dist:.1} m) exceeds maxTargetRange ({max_range:.1} m)"),
                }));
            }

            let max_locked = fit.ship.get_attribute(attr_ids::MAX_LOCKED_TARGETS, &ctx.sde).max(1.0) as usize;
            let current_locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
            if current_locks.len() >= max_locked {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Max locked targets exceeded ({max_locked})"),
                }));
            }

            let scan_res = fit.ship.get_attribute(attr_ids::SCAN_RESOLUTION, &ctx.sde).max(1.0);
            let target_sig = target_ball.radius.max(35.0);
            let lock_sec = eve_destiny::combat::compute_lock_time_seconds(scan_res, target_sig);
            let lock_time_ms = lock_sec * 1000.0;

            let lock = eve_destiny::TargetLock {
                target_id,
                lock_time_ms,
                remaining_lock_ms: lock_time_ms,
                locked: false,
            };
            if let Err(e) = sys.add_target_lock(ship_id, lock).await {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: e,
                }));
            }

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnTarget",
                "charid",
                vec![PyRep::Integer(target_id), PyRep::String("locking".into())],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnTargets",
                "charid",
                vec![PyRep::List(vec![PyRep::Integer(target_id)])],
            );

            Some(Ok(PyRep::keyval([
                ("targetID", PyRep::Integer(target_id)),
                ("locked", PyRep::Bool(false)),
                ("lockTimeMs", PyRep::Real(lock_time_ms)),
            ])))
        }

        ("dogmaIM", "RemoveTarget") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let Some(target_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "RemoveTarget requires targetID".into(),
                }));
            };
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.remove_target_lock(ship_id, target_id).await;
            }
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnTarget",
                "charid",
                vec![PyRep::Integer(target_id), PyRep::String("lost".into())],
            );
            Some(Ok(PyRep::Integer(target_id)))
        }

        ("dogmaIM", "ClearTargets") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.clear_target_locks(ship_id).await;
            }
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnTargetClear",
                "charid",
                vec![],
            );
            Some(Ok(PyRep::None))
        }

        ("dogmaIM" | "charFittingMgr" | "shipGroupingSvc", "LinkWeapons") => {
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let master_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let slave_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
            ctx.db.with_mut_snapshot(|snap| {
                let banks = snap.weapon_banks.entry(ship_id).or_default();
                let bank = banks.entry(master_id).or_insert_with(|| vec![master_id]);
                if !bank.contains(&slave_id) {
                    bank.push(slave_id);
                }
            });
            Some(Ok(PyRep::None))
        }

        ("dogmaIM" | "charFittingMgr" | "shipGroupingSvc", "PeelAndLink") => {
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let new_mod_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let target_bank_mod_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
            ctx.db.with_mut_snapshot(|snap| {
                let banks = snap.weapon_banks.entry(ship_id).or_default();
                let mut found_master = None;
                for (&m, list) in banks.iter() {
                    if m == target_bank_mod_id || list.contains(&target_bank_mod_id) {
                        found_master = Some(m);
                        break;
                    }
                }
                let master = found_master.unwrap_or(target_bank_mod_id);
                let list = banks.entry(master).or_insert_with(|| vec![master]);
                if !list.contains(&new_mod_id) {
                    list.push(new_mod_id);
                }
            });
            Some(Ok(PyRep::None))
        }

        ("dogmaIM" | "charFittingMgr" | "shipGroupingSvc", "MergeModuleGroups") => {
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let master_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let other_master_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
            ctx.db.with_mut_snapshot(|snap| {
                let banks = snap.weapon_banks.entry(ship_id).or_default();
                let other_modules = banks.remove(&other_master_id).unwrap_or_else(|| vec![other_master_id]);
                let master_list = banks.entry(master_id).or_insert_with(|| vec![master_id]);
                for id in other_modules {
                    if !master_list.contains(&id) {
                        master_list.push(id);
                    }
                }
            });
            Some(Ok(PyRep::None))
        }

        ("dogmaIM" | "charFittingMgr" | "shipGroupingSvc", "UnlinkModule") => {
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let mod_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(banks) = snap.weapon_banks.get_mut(&ship_id) {
                    for list in banks.values_mut() {
                        list.retain(|&id| id != mod_id);
                    }
                    banks.retain(|k, v| *k != mod_id && v.len() > 1);
                }
            });
            Some(Ok(PyRep::None))
        }

        ("dogmaIM" | "charFittingMgr" | "shipGroupingSvc", "DestroyWeaponBank") => {
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let master_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(banks) = snap.weapon_banks.get_mut(&ship_id) {
                    banks.remove(&master_id);
                }
            });
            Some(Ok(PyRep::None))
        }

        ("dogmaIM" | "charFittingMgr" | "shipGroupingSvc", "LinkAllWeapons") => {
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            ctx.db.with_mut_snapshot(|snap| {
                let mut weapons: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.location_id == ship_id
                            && matches!(i.flag as u16, 27..=34)
                            && ctx.sde.types.get(&i.type_id).map(|t| t.category_id == 7).unwrap_or(false)
                    })
                    .map(|i| i.item_id)
                    .collect();
                weapons.sort();
                if let Some(&master) = weapons.first() {
                    let banks = snap.weapon_banks.entry(ship_id).or_default();
                    banks.insert(master, weapons);
                }
            });
            Some(Ok(PyRep::None))
        }

        ("dogmaIM" | "charFittingMgr" | "shipGroupingSvc", "UnlinkAllModules") => {
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            ctx.db.with_mut_snapshot(|snap| {
                snap.weapon_banks.remove(&ship_id);
            });
            Some(Ok(PyRep::None))
        }

        ("dogmaIM", "LoadAmmo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let requested_module_ids: Vec<i64> = match args.get(1) {
                Some(PyRep::List(v) | PyRep::Tuple(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
                Some(v) => v.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            let charge_item_ids: Vec<i64> = match args.get(2) {
                Some(PyRep::List(v) | PyRep::Tuple(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
                Some(v) => v.as_i64().into_iter().collect(),
                None => Vec::new(),
            };

            let Some(&first_charge_id) = charge_item_ids.first() else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "LoadAmmo requires charge itemID".into(),
                }));
            };

            let charge_info_opt = ctx.db.with_snapshot(|snap| {
                snap.items.get(&first_charge_id).map(|i| (i.type_id, i.quantity))
            });
            let Some((charge_type_id, avail_qty)) = charge_info_opt else {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Charge item {first_charge_id} not found"),
                }));
            };

            let charge_vol = ctx.sde.types.get(&charge_type_id).map(|t| t.volume.max(0.0001)).unwrap_or(0.0025);

            let all_target_modules: Vec<i64> = ctx.db.with_snapshot(|snap| {
                let mut out = Vec::new();
                for &req_id in &requested_module_ids {
                    let resolved_mod_id = if snap.items.contains_key(&req_id) {
                        req_id
                    } else if let Some(found) = snap.items.values().find(|i| i.location_id == ship_id && (i.flag as i64) == req_id) {
                        found.item_id
                    } else {
                        req_id
                    };
                    if let Some(bank) = snap.weapon_banks.get(&ship_id).and_then(|b| b.get(&resolved_mod_id)) {
                        for &m in bank {
                            if !out.contains(&m) {
                                out.push(m);
                            }
                        }
                    } else if !out.contains(&resolved_mod_id) {
                        out.push(resolved_mod_id);
                    }
                }
                out
            });

            for &mod_id in &all_target_modules {
                if let Some(mod_item) = ctx.db.with_snapshot(|snap| snap.items.get(&mod_id).cloned()) {
                    if let (Some(wt), Some(ct)) = (ctx.sde.types.get(&mod_item.type_id), ctx.sde.types.get(&charge_type_id)) {
                        if !ServerContext::is_charge_compatible_with_weapon(wt, ct) {
                            return Some(Err(BridgeRpcError {
                                code: "IncompatibleAmmo".into(),
                                message: format!("Charge {} is incompatible with weapon {}", ct.name, wt.name),
                            }));
                        }
                    }
                }
            }

            let mut rounds_per_mod = 1i32;
            let mut reload_time_ms = 10_000.0;
            if let Some(&first_mod_id) = all_target_modules.first() {
                if let Some(mod_item) = ctx.db.with_snapshot(|snap| snap.items.get(&first_mod_id).cloned()) {
                    if let Some(mod_t) = ctx.sde.types.get(&mod_item.type_id) {
                        let cap = mod_t.capacity.max(0.0);
                        if cap > 0.0 {
                            rounds_per_mod = (cap / charge_vol).floor() as i32;
                        }
                        if let Some(&rt) = mod_t.default_attributes.get(&1795) {
                            reload_time_ms = rt;
                        }
                    }
                }
            }
            if rounds_per_mod <= 0 {
                rounds_per_mod = 1;
            }

            let total_needed = rounds_per_mod * (all_target_modules.len() as i32);
            if avail_qty < total_needed {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientQuantity".into(),
                    message: format!("Need {total_needed} charges, but only {avail_qty} available in stack"),
                }));
            }

            ctx.db.with_mut_snapshot(|snap| {
                if let Some(stack) = snap.items.get_mut(&first_charge_id) {
                    stack.quantity -= total_needed;
                }
                for &mod_id in &all_target_modules {
                    let Some(mod_item) = snap.items.get(&mod_id).cloned() else {
                        continue;
                    };
                    let slot_flag = mod_item.flag;
                    let prev_charges: Vec<i64> = snap
                        .items
                        .values()
                        .filter(|i| i.location_id == ship_id && i.flag == slot_flag && i.item_id != mod_id)
                        .map(|i| i.item_id)
                        .collect();

                    for pid in prev_charges {
                        if let Some(pi) = snap.items.get_mut(&pid) {
                            pi.flag = InventoryFlag::Cargo;
                        }
                    }

                    snap.next_entity_id += 1;
                    let new_charge_id = snap.next_entity_id;
                    snap.items.insert(
                        new_charge_id,
                        eve_db::InventoryItemRecord {
                            item_id: new_charge_id,
                            type_id: charge_type_id,
                            owner_id: char_id,
                            location_id: ship_id,
                            flag: slot_flag,
                            quantity: rounds_per_mod,
                            singleton: false,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                }
            });

            ctx.invalidate_ship_fitting(ship_id);

            Some(Ok(PyRep::keyval([
                ("reloadTimeMs", PyRep::Real(reload_time_ms)),
                ("totalLoaded", PyRep::Integer(total_needed as i64)),
            ])))
        }

        ("dogmaIM", "UnloadAmmo") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let requested_module_ids: Vec<i64> = match args.get(1) {
                Some(PyRep::List(v) | PyRep::Tuple(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
                Some(v) => v.as_i64().into_iter().collect(),
                None => Vec::new(),
            };

            let all_target_modules: Vec<i64> = ctx.db.with_snapshot(|snap| {
                let mut out = Vec::new();
                for &req_id in &requested_module_ids {
                    let resolved_mod_id = if snap.items.contains_key(&req_id) {
                        req_id
                    } else if let Some(found) = snap.items.values().find(|i| i.location_id == ship_id && (i.flag as i64) == req_id) {
                        found.item_id
                    } else {
                        req_id
                    };
                    if let Some(bank) = snap.weapon_banks.get(&ship_id).and_then(|b| b.get(&resolved_mod_id)) {
                        for &m in bank {
                            if !out.contains(&m) {
                                out.push(m);
                            }
                        }
                    } else if !out.contains(&resolved_mod_id) {
                        out.push(resolved_mod_id);
                    }
                }
                out
            });

            ctx.db.with_mut_snapshot(|snap| {
                for &mod_id in &all_target_modules {
                    let Some(mod_item) = snap.items.get(&mod_id).cloned() else {
                        continue;
                    };
                    let slot_flag = mod_item.flag;
                    let charges: Vec<i64> = snap
                        .items
                        .values()
                        .filter(|i| i.location_id == ship_id && i.flag == slot_flag && i.item_id != mod_id)
                        .map(|i| i.item_id)
                        .collect();
                    for cid in charges {
                        if let Some(ci) = snap.items.get_mut(&cid) {
                            ci.flag = InventoryFlag::Cargo;
                        }
                    }
                }
            });

            ctx.invalidate_ship_fitting(ship_id);
            Some(Ok(PyRep::None))
        }

        ("dogmaIM", "Activate") | ("dogmaIM", "Overload") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let effect_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("defaultEffect").to_string();
            let target_id_opt = args.get(2).and_then(|a| a.as_i64())
                .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                .or_else(|| kwargs.get("targetID").and_then(|v| v.as_i64()))
                .or_else(|| kwargs.get("target").and_then(|v| v.as_i64()))
                .or_else(|| kwargs.get("asteroidID").and_then(|v| v.as_i64()));
            let repeat = args.get(3).and_then(|a| a.as_i64()).unwrap_or(1) as i32;

            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };

            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Cannot activate modules outside of space".into(),
                }));
            };
            let sys = ctx.systems.get_or_spawn(sys_id);

            let fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            let Some(m) = fit.modules.get(&module_id).cloned() else {
                return Some(Err(BridgeRpcError {
                    code: "ModuleNotFound".into(),
                    message: format!("Module {module_id} not found on ship"),
                }));
            };
            if m.state == eve_dogma::ModuleState::Offline {
                return Some(Err(BridgeRpcError {
                    code: "ModuleOffline".into(),
                    message: format!("Module {module_id} is offline"),
                }));
            }

            let Some(mod_type) = ctx.sde.types.get(&m.type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "ModuleNotFound".into(),
                    message: format!("Module type {} not found in SDE", m.type_id),
                }));
            };

            let is_mining_module = matches!(mod_type.group_id, 54 | 464 | 483 | 737 | 2004 | 4138)
                || mod_type.default_attributes.contains_key(&77);

            if !is_mining_module {
                if let Some(target_id) = target_id_opt {
                    let locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
                    if !locks.iter().any(|l| l.target_id == target_id && l.locked) {
                        return Some(Err(BridgeRpcError {
                            code: "TargetNotLocked".into(),
                            message: format!("Target {target_id} is not locked"),
                        }));
                    }
                }
                let mod_state = fit.build_effective_module_state(module_id, m.type_id, method == "Overload", &ctx.sde);

            let duration_ms = {
                let d = mod_state.get_attribute(attr_ids::DURATION, &ctx.sde);
                if d > 0.0 {
                    d
                } else {
                    mod_state.get_attribute(attr_ids::SPEED, &ctx.sde)
                }
            };
            let duration_s = if duration_ms > 0.0 { duration_ms / 1000.0 } else { 1.0 };
            let cap_need = mod_state.get_attribute(attr_ids::CAPACITOR_NEED, &ctx.sde);
            let speed_factor = mod_state.get_attribute(attr_ids::SPEED_FACTOR, &ctx.sde);
            let speed_boost_factor = if speed_factor > 0.0 { speed_factor / 100.0 } else { 0.0 };

            let linked_turrets: Vec<i64> = ctx.db.with_snapshot(|snap| {
                if let Some(bank) = snap.weapon_banks.get(&ship_id).and_then(|b| b.get(&module_id)) {
                    bank.clone()
                } else if matches!(m.flag_id, 27..=34) {
                    vec![module_id]
                } else {
                    Vec::new()
                }
            });

            if !linked_turrets.is_empty() {
                ctx.db.with_mut_snapshot(|snap| {
                    for &tid in &linked_turrets {
                        let tflag = snap.items.get(&tid).map(|i| i.flag);
                        if let Some(tf) = tflag {
                            if let Some(charge_item) = snap.items.values_mut().find(|i| {
                                i.location_id == ship_id && i.flag == tf && i.item_id != tid && i.quantity > 0
                            }) {
                                charge_item.quantity = charge_item.quantity.saturating_sub(1);
                            }
                        }
                    }
                });
            }

            let cap_drain_target = mod_state.get_attribute(97, &ctx.sde);
            let cap_transfer_target = mod_state.get_attribute(90, &ctx.sde);

            let cycle = crate::system_actor::ActiveModuleCycle {
                ship_id,
                module_id,
                module_type_id: m.type_id,
                effect_name: effect_name.clone(),
                target_id: target_id_opt,
                repeat,
                duration_s,
                elapsed_s: 0.0,
                cap_need_per_cycle: cap_need,
                speed_boost_factor,
                original_max_velocity: None,
                linked_turrets,
                ammo_type_id: m.charge_type_id,
                damage_vector: eve_destiny::DamageVector::new(20.0, 20.0, 20.0, 20.0),
                cap_drain_target,
                cap_transfer_target,
            };

            let _ = sys.add_module_cycle(cycle).await;

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnGodmaShipEffect",
                "charid",
                vec![PyRep::Integer(module_id), PyRep::String(effect_name), PyRep::Bool(true)],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnModuleActivated",
                "charid",
                vec![PyRep::Integer(module_id), PyRep::Integer(repeat as i64)],
            );

            if method == "Overload" {
                let mut fit_ov = fit.clone();
                let _ = fit_ov.activate_module(module_id, true, &ctx.sde);
                let max_vel = fit_ov.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("overloaded", PyRep::Bool(true)),
                    ("effectiveMaxVelocity", PyRep::Real(max_vel)),
                ])))
            } else {
                Some(Ok(PyRep::None))
            }
            } else {
                let items = ctx.db.get_items_for_character(char_id);
                let Some(mod_item) = items.iter().find(|i| i.item_id == module_id && i.owner_id == char_id).cloned() else {
                    return Some(Err(BridgeRpcError {
                        code: "ModuleNotFound".into(),
                        message: format!("Module {module_id} not found on character ship"),
                    }));
                };

                // Determine target asteroid ID
                let target_id_opt = args.get(2).and_then(|a| a.as_i64())
                    .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                    .or_else(|| kwargs.get("targetID").and_then(|v| v.as_i64()))
                    .or_else(|| kwargs.get("target").and_then(|v| v.as_i64()))
                    .or_else(|| kwargs.get("asteroidID").and_then(|v| v.as_i64()));

                let target_id = match target_id_opt {
                    Some(tid) if tid > 0 => tid,
                    _ => {
                        let sys_id = session.solar_system_id.unwrap_or(0);
                        let any_ast = ctx.db.with_snapshot(|snap| {
                            snap.asteroids.values().find(|a| a.solar_system_id == sys_id && a.quantity > 0).map(|a| a.asteroid_id)
                        });
                        match any_ast {
                            Some(tid) => tid,
                            None => {
                                return Some(Err(BridgeRpcError {
                                    code: "InvalidTarget".into(),
                                    message: "Mining module requires a locked target asteroid".into(),
                                }));
                            }
                        }
                    }
                };

                let Some(asteroid) = ctx.db.get_asteroid(target_id) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidTarget".into(),
                        message: format!("Target asteroid {target_id} not found"),
                    }));
                };
                if asteroid.quantity <= 0 {
                    return Some(Err(BridgeRpcError {
                        code: "AsteroidDepleted".into(),
                        message: format!("Target asteroid {target_id} is depleted"),
                    }));
                }

                let mut in_warp = false;
                let is_cloaked = false;
                let mut ship_pos = DVec3::ZERO;
                let mut sys_id_val = session.solar_system_id.unwrap_or(asteroid.solar_system_id);
                if let Some(sys_id) = session.solar_system_id {
                    sys_id_val = sys_id;
                    let sys_handle = ctx.systems.get_or_spawn(sys_id);
                    if let Ok(Some((_, ship_ball))) = sys_handle.get_ball_snapshot(ship_id).await {
                        ship_pos = ship_ball.position;
                        if matches!(ship_ball.mode, eve_destiny::MovementMode::Warp { .. }) {
                            in_warp = true;
                        }
                    }
                }
                if in_warp {
                    return Some(Err(BridgeRpcError {
                        code: "CannotActivateWhileWarping".into(),
                        message: "Cannot activate mining lasers while in warp".into(),
                    }));
                }
                if is_cloaked {
                    return Some(Err(BridgeRpcError {
                        code: "CannotActivateWhileCloaked".into(),
                        message: "Cannot activate mining lasers while cloaked".into(),
                    }));
                }
                let max_range = mod_type.default_attributes.get(&54).copied().unwrap_or(10_000.0);
                let ast_pos = DVec3::new(asteroid.position.0, asteroid.position.1, asteroid.position.2);
                let distance = if ship_pos != DVec3::ZERO {
                    (ship_pos - ast_pos).length()
                } else {
                    0.0
                };
                if distance > max_range && ship_pos != DVec3::ZERO {
                    return Some(Err(BridgeRpcError {
                        code: "TargetOutOfRange".into(),
                        message: format!("Target asteroid is {distance:.1}m away (max range {max_range:.1}m)"),
                    }));
                }

                let cap_need = mod_type.default_attributes.get(&6).copied().unwrap_or(0.0);
                let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                    Ok(f) => f,
                    Err(e) => return Some(Err(e)),
                };
                let cap_capacity = fit.ship.get_attribute(eve_sde::attr_ids::CAPACITOR_CAPACITY, &ctx.sde);
                if cap_need > cap_capacity {
                    return Some(Err(BridgeRpcError {
                        code: "InsufficientCapacitor".into(),
                        message: format!("Module requires {cap_need} GJ capacitor, ship capacity is {cap_capacity} GJ"),
                    }));
                }

                let base_duration_ms = mod_type.default_attributes.get(&73).copied().unwrap_or(15_000.0);
                let base_amount = mod_type.default_attributes.get(&77).copied().unwrap_or(10.0);

                let (mining_lvl, astro_lvl, ice_lvl) = ctx.db.with_snapshot(|snap| {
                    let sk = snap.skills.get(&char_id);
                    let m = sk.and_then(|m| m.get(&3386)).map(|s| s.level).unwrap_or(0);
                    let a = sk.and_then(|m| m.get(&3410)).map(|s| s.level).unwrap_or(0);
                    let i = sk.and_then(|m| m.get(&16281)).map(|s| s.level).unwrap_or(0);
                    (m, a, i)
                });

                let ship_item_res = ctx.db.get_item(ship_id);
                let ship_type = ship_item_res.as_ref().and_then(|si| ctx.sde.types.get(&si.type_id));
                let mut ship_mining_mult = 1.0;
                let mut ship_gas_mult = 1.0;
                if let Some(st) = ship_type {
                    if st.type_id == 32880 {
                        // Venture: 100% mining yield bonus, 100% gas yield
                        ship_mining_mult *= 2.0;
                        ship_gas_mult *= 2.0;
                    } else {
                        if let Some(&bonus) = st.default_attributes.get(&1178) {
                            ship_mining_mult *= 1.0 + bonus / 100.0;
                        }
                        if let Some(&bonus) = st.default_attributes.get(&1200) {
                            ship_gas_mult *= 1.0 + bonus / 100.0;
                        }
                    }
                }

                let mut mlu_mult = 1.0;
                let mut ice_cycle_bonus = 0.0;
                for item in &items {
                    if item.location_id == ship_id && (item.flag as u16) >= 11 && (item.flag as u16) <= 34 {
                        if let Some(it) = ctx.sde.types.get(&item.type_id) {
                            if it.group_id == 546 {
                                if let Some(&b) = it.default_attributes.get(&434) {
                                    mlu_mult *= 1.0 + b / 100.0;
                                }
                                if let Some(&b) = it.default_attributes.get(&780) {
                                    ice_cycle_bonus += b / 100.0;
                                }
                            }
                        }
                    }
                }

                let crystal_item_opt = items.iter().find(|i| {
                    i.location_id == ship_id
                        && i.flag == mod_item.flag
                        && ctx.sde.types.get(&i.type_id).map(|t| t.group_id == 482 || t.group_id == 663).unwrap_or(false)
                });
                let mut crystal_yield_mult = 1.0;
                let mut crystal_damage_inflicted = 0.0;
                let mut crystal_destroyed = false;
                let mut crystal_id_to_update = None;
                let mut crystal_new_hp = 1.0;

                if let Some(crystal) = crystal_item_opt {
                    if let Some(c_type) = ctx.sde.types.get(&crystal.type_id) {
                        let ore_type_entry = ctx.sde.types.get(&asteroid.type_id);
                        let ore_name = ore_type_entry.map(|t| t.name.to_lowercase()).unwrap_or_default();
                        let crystal_name = c_type.name.to_lowercase();
                        let ore_base = ore_name.split_whitespace().last().unwrap_or(&ore_name);
                        let crystal_matches = crystal_name.contains(ore_base) || crystal_name.contains(&ore_name);
                        if crystal_matches {
                            let mult = c_type.default_attributes.get(&782).copied().unwrap_or(1.0);
                            crystal_yield_mult = mult;
                        }
                        let vol_damage = c_type.default_attributes.get(&784).copied().unwrap_or(0.025);
                        let current_hp = crystal.dynamic_attributes.get(&eve_sde::attr_ids::HP_STRUCT).copied().unwrap_or(1.0);
                        crystal_damage_inflicted = vol_damage;
                        let rem_hp = (current_hp - vol_damage).max(0.0);
                        crystal_new_hp = rem_hp;
                        crystal_id_to_update = Some(crystal.item_id);
                        if rem_hp <= 1e-6 {
                            crystal_destroyed = true;
                        }
                    }
                }

                let is_ice = mod_type.group_id == 464 && (mod_type.type_id == 16278 || mod_type.type_id == 22229 || mod_type.name.contains("Ice"));
                let is_gas = mod_type.group_id == 737 || mod_type.group_id == 4138;
                let (duration_ms, volume_yield) = if is_ice {
                    let duration = (base_duration_ms * (1.0 - 0.05 * (ice_lvl as f64) + ice_cycle_bonus)).max(1000.0);
                    (duration, base_amount)
                } else if is_gas {
                    (base_duration_ms, base_amount * ship_gas_mult)
                } else {
                    let skill_mult = (1.0 + 0.05 * (mining_lvl as f64)) * (1.0 + 0.05 * (astro_lvl as f64));
                    let vol = base_amount * skill_mult * ship_mining_mult * mlu_mult * crystal_yield_mult;
                    (base_duration_ms, vol)
                };

                let ore_unit_volume = ctx.sde.types.get(&asteroid.type_id).map(|t| t.volume).unwrap_or(0.1).max(0.001);
                let potential_units = ((volume_yield / ore_unit_volume).floor() as i64).max(1);

                let (actual_units, cycle_time_ms, depleted) = if asteroid.quantity < potential_units {
                    let actual = asteroid.quantity;
                    let partial_cycle = ((actual as f64) / (potential_units as f64) * duration_ms).round() as i64;
                    (actual, partial_cycle.max(1000), true)
                } else {
                    (potential_units, duration_ms as i64, asteroid.quantity == potential_units)
                };

                let waste_prob = mod_type.default_attributes.get(&3154).copied().unwrap_or(0.0);
                let waste_mult = mod_type.default_attributes.get(&3153).copied().unwrap_or(1.0);
                let residue_units = if waste_prob > 0.0 {
                    ((actual_units as f64) * waste_mult).floor() as i64
                } else {
                    0
                };

                let (destination_flag, hold_capacity) = if is_ice {
                    let ice_cap = ship_type.and_then(|st| st.default_attributes.get(&3136).copied().or_else(|| st.default_attributes.get(&1556).copied())).unwrap_or(0.0);
                    if ice_cap > 0.0 {
                        (InventoryFlag::SpecializedIceHold, ice_cap)
                    } else {
                        (InventoryFlag::Cargo, ship_type.map(|st| st.capacity).unwrap_or(500.0))
                    }
                } else if is_gas {
                    let gas_cap = ship_type.and_then(|st| st.default_attributes.get(&1557).copied().or_else(|| st.default_attributes.get(&1556).copied())).unwrap_or(0.0);
                    if gas_cap > 0.0 {
                        (InventoryFlag::SpecializedGasHold, gas_cap)
                    } else {
                        (InventoryFlag::Cargo, ship_type.map(|st| st.capacity).unwrap_or(500.0))
                    }
                } else {
                    let ore_cap = ship_type.and_then(|st| st.default_attributes.get(&1556).copied().or_else(|| st.default_attributes.get(&1770).copied())).unwrap_or(0.0);
                    if ore_cap > 0.0 {
                        (InventoryFlag::SpecializedOreHold, ore_cap)
                    } else {
                        (InventoryFlag::Cargo, ship_type.map(|st| st.capacity).unwrap_or(500.0))
                    }
                };

                let used_volume: f64 = items.iter()
                    .filter(|i| i.location_id == ship_id && i.flag == destination_flag)
                    .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.0) * (i.quantity as f64))
                    .sum();
                let remaining_vol = (hold_capacity - used_volume).max(0.0);
                let max_fit_units = (remaining_vol / ore_unit_volume).floor() as i64;
                let (units_to_deposit, hold_full) = if max_fit_units < actual_units {
                    (max_fit_units.max(0), true)
                } else {
                    (actual_units, false)
                };

                let mut deposited_units_out = 0;
                let res: Result<(), eve_db::DbError> = ctx.db.try_with_mut_snapshot(|snap| {
                    let total_removed_from_rock = units_to_deposit + residue_units;
                    if let Some(ast) = snap.asteroids.get_mut(&target_id) {
                        ast.quantity = ast.quantity.saturating_sub(total_removed_from_rock);
                    }
                    if units_to_deposit > 0 {
                        deposited_units_out = units_to_deposit;
                        let existing_opt = snap.items.values_mut().find(|i| {
                            i.location_id == ship_id
                                && i.flag == destination_flag
                                && i.type_id == asteroid.type_id
                                && !i.singleton
                                && !i.is_blueprint_copy
                        });
                        if let Some(existing) = existing_opt {
                            existing.quantity += units_to_deposit as i32;
                            existing.version += 1;
                        } else {
                            snap.next_entity_id += 1;
                            let new_id = snap.next_entity_id;
                            snap.items.insert(
                                new_id,
                                eve_db::InventoryItemRecord {
                                    item_id: new_id,
                                    type_id: asteroid.type_id,
                                    owner_id: char_id,
                                    location_id: ship_id,
                                    flag: destination_flag,
                                    quantity: units_to_deposit as i32,
                                    singleton: false,
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
                    if let Some(cid) = crystal_id_to_update {
                        if crystal_destroyed {
                            snap.items.remove(&cid);
                        } else if let Some(c_item) = snap.items.get_mut(&cid) {
                            c_item.dynamic_attributes.insert(eve_sde::attr_ids::HP_STRUCT, crystal_new_hp);
                            c_item.version += 1;
                        }
                    }
                    Ok(())
                });
                if let Err(e) = res {
                    return Some(Err(BridgeRpcError { code: "MiningFailed".into(), message: format!("{e:?}") }));
                }

                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnOreMined",
                    "character",
                    vec![
                        PyRep::Integer(asteroid.type_id as i64),
                        PyRep::Integer(deposited_units_out),
                        PyRep::Integer(destination_flag as i64),
                    ],
                );
                ctx.notify(
                    crate::router::NotificationTarget::SolarSystem(sys_id_val),
                    "OnMiningLaserFiredLocal",
                    "solarsystemid",
                    vec![
                        PyRep::Integer(char_id),
                        PyRep::Integer(ship_id),
                        PyRep::Integer(module_id),
                        PyRep::Integer(target_id),
                    ],
                );

                let _ = fit.activate_module(module_id, method == "Overload", &ctx.sde);
                ctx.store_ship_fitting(ship_id, fit);

                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("targetID", PyRep::Integer(target_id)),
                    ("cycleTimeMs", PyRep::Integer(cycle_time_ms)),
                    ("yieldUnits", PyRep::Integer(deposited_units_out)),
                    ("yieldVolume", PyRep::Real(deposited_units_out as f64 * ore_unit_volume)),
                    ("oreTypeID", PyRep::Integer(asteroid.type_id as i64)),
                    ("destinationFlag", PyRep::Integer(destination_flag as i64)),
                    ("depleted", PyRep::Bool(depleted)),
                    ("holdFull", PyRep::Bool(hold_full)),
                    ("residueUnits", PyRep::Integer(residue_units)),
                    ("crystalDamage", PyRep::Real(crystal_damage_inflicted)),
                    ("crystalDestroyed", PyRep::Bool(crystal_destroyed)),
                ])))
            }
        }

        ("dogmaIM", "Deactivate") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let effect_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("defaultEffect").to_string();
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.remove_module_cycle(ship_id, module_id).await;
            }

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnModulesDeactivating",
                "charid",
                vec![PyRep::List(vec![PyRep::Integer(module_id)])],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnGodmaShipEffect",
                "charid",
                vec![PyRep::Integer(module_id), PyRep::String(effect_name), PyRep::Bool(false)],
            );

            let items = ctx.db.get_items_for_character(char_id);
            let mod_item_opt = items.iter().find(|i| i.item_id == module_id && i.owner_id == char_id);
            let is_mining = mod_item_opt.and_then(|m| ctx.sde.types.get(&m.type_id))
                .map(|t| matches!(t.group_id, 54 | 464 | 483 | 737 | 2004 | 4138) || t.default_attributes.contains_key(&77))
                .unwrap_or(false);

            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let deact_res = fit.deactivate_module(module_id, &ctx.sde);
            if let Err(e) = deact_res {
                if !is_mining {
                    return Some(Err(BridgeRpcError { code: "DogmaDeactivateFailed".into(), message: e.to_string() }));
                }
            }
            let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);

            if is_mining {
                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("active", PyRep::Bool(false)),
                ])))
            } else {
                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("overloaded", PyRep::Bool(false)),
                    ("effectiveMaxVelocity", PyRep::Real(max_vel)),
                ])))
            }
        }

        ("miningScanMgr", "perform_scan") | ("surveyScanMgr", "perform_scan") => {
            if args.is_empty() && kwargs.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "UnimplementedRPC".into(),
                    message: "miningScanMgr.perform_scan requires explicit scanner moduleID or parameters".into(),
                }));
            }
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let items = ctx.db.get_items_for_character(char_id);
            let explicit_mod_id = args.first().and_then(|a| a.as_i64());
            let scanner_opt = items.iter().find(|i| {
                i.location_id == ship_id
                    && (i.flag as u16) >= 11
                    && (i.flag as u16) <= 34
                    && explicit_mod_id.map(|mid| i.item_id == mid).unwrap_or(true)
                    && ctx.sde.types.get(&i.type_id).map(|t| t.group_id == 49 || t.default_attributes.contains_key(&197)).unwrap_or(false)
            });
            let Some(scanner) = scanner_opt else {
                return Some(Err(BridgeRpcError {
                    code: "ModuleNotFound".into(),
                    message: "No survey scanner fitted to ship".into(),
                }));
            };
            let scanner_type = ctx.sde.types.get(&scanner.type_id);
            let scan_range = scanner_type
                .and_then(|t| t.default_attributes.get(&197).copied().or_else(|| t.default_attributes.get(&54).copied()))
                .unwrap_or(15_000.0);

            let sys_id = session.solar_system_id.unwrap_or(0);
            let mut ship_pos = DVec3::ZERO;
            if sys_id > 0 {
                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                if let Ok(Some((_, ship_ball))) = sys_handle.get_ball_snapshot(ship_id).await {
                    ship_pos = ship_ball.position;
                }
            }

            let asteroids = ctx.db.get_asteroids_in_system(sys_id);
            let mut scanned_asteroids = Vec::new();
            for ast in asteroids {
                if ast.quantity <= 0 {
                    continue;
                }
                let ast_pos = DVec3::new(ast.position.0, ast.position.1, ast.position.2);
                let dist = if ship_pos != DVec3::ZERO {
                    (ship_pos - ast_pos).length()
                } else {
                    0.0
                };
                if dist <= scan_range || ship_pos == DVec3::ZERO {
                    let type_name = ctx.sde.types.get(&ast.type_id).map(|t| t.name.clone()).unwrap_or_else(|| format!("Asteroid {}", ast.type_id));
                    let vol = ctx.sde.types.get(&ast.type_id).map(|t| t.volume).unwrap_or(0.1);
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnAsteroidRevealed",
                        "character",
                        vec![
                            PyRep::Integer(ast.asteroid_id),
                            PyRep::Integer(ast.type_id as i64),
                            PyRep::Integer(ast.quantity),
                        ],
                    );
                    scanned_asteroids.push(PyRep::keyval([
                        ("asteroidID", PyRep::Integer(ast.asteroid_id)),
                        ("typeID", PyRep::Integer(ast.type_id as i64)),
                        ("typeName", PyRep::String(type_name)),
                        ("quantity", PyRep::Integer(ast.quantity)),
                        ("initialQuantity", PyRep::Integer(ast.initial_quantity)),
                        ("distance", PyRep::Real(dist)),
                        ("volume", PyRep::Real(vol)),
                    ]));
                }
            }

            Some(Ok(PyRep::keyval([
                ("scanRange", PyRep::Real(scan_range)),
                ("count", PyRep::Integer(scanned_asteroids.len() as i64)),
                ("asteroids", PyRep::List(scanned_asteroids)),
            ])))
        }

        ("charFittingMgr", "SaveFitting") | ("corpFittingMgr", "SaveFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let name = args.first().and_then(|a| a.as_str()).unwrap_or("Rifter Combat Fit");
            let Some(raw_ship_type) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidShipType".into(), message: "SaveFitting requires shipTypeID".into() }));
            };
            let ship_type_id = raw_ship_type as TypeId;

            let is_valid_hull = ctx.sde.types.get(&ship_type_id).map(|t| t.category_id == 6).unwrap_or(false);
            if !is_valid_hull {
                return Some(Err(BridgeRpcError { code: "InvalidShipType".into(), message: format!("Type {ship_type_id} is not a valid ship hull") }));
            }

            let (description, items_arg) = match args.get(2) {
                Some(PyRep::String(s)) => (s.clone(), args.get(3)),
                Some(list_or_tuple @ (PyRep::List(_) | PyRep::Tuple(_))) => ("Saved via charFittingMgr".to_string(), Some(list_or_tuple)),
                _ => ("Saved via charFittingMgr".to_string(), args.get(3)),
            };

            let parsed_items: Vec<(TypeId, InventoryFlag, i32)> = if let Some(arg) = items_arg {
                let entries = match arg {
                    PyRep::List(v) | PyRep::Tuple(v) => v,
                    _ => return Some(Err(BridgeRpcError { code: "InvalidFittingItems".into(), message: "Fitting items must be a List or Tuple of (typeID, flagID, quantity)".into() })),
                };
                let mut out = Vec::with_capacity(entries.len());
                for entry in entries {
                    let seq = match entry {
                        PyRep::Tuple(v) | PyRep::List(v) => v,
                        _ => return Some(Err(BridgeRpcError { code: "InvalidFittingItem".into(), message: "Each fitting item must be a (typeID, flagID, quantity) tuple".into() })),
                    };
                    let Some(mod_type_id) = seq.first().and_then(|v| v.as_i64()).filter(|&v| v > 0) else {
                        return Some(Err(BridgeRpcError { code: "InvalidFittingItem".into(), message: "Fitting item is missing a positive typeID".into() }));
                    };
                    let Some(flag_raw) = seq.get(1).and_then(|v| v.as_i64()).filter(|&v| v >= 0) else {
                        return Some(Err(BridgeRpcError { code: "InvalidFittingItem".into(), message: "Fitting item is missing a valid flagID".into() }));
                    };
                    let qty_i64 = seq.get(2).and_then(|v| v.as_i64()).unwrap_or(1);
                    let Ok(qty) = i32::try_from(qty_i64) else {
                        return Some(Err(BridgeRpcError { code: "InvalidFittingItem".into(), message: format!("Fitting item quantity overflows i32 bounds, got {qty_i64}") }));
                    };
                    if qty <= 0 {
                        return Some(Err(BridgeRpcError { code: "InvalidFittingItem".into(), message: format!("Fitting item quantity must be positive, got {qty}") }));
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
            let Some(ship_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "InsureShip requires shipID argument or active ship in session".into(),
                }));
            };
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
            let Some(ship_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "GetInsurancePrice requires shipID argument or active ship in session"
                        .into(),
                }));
            };
            let info = ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&ship_id)
                    .filter(|i| i.owner_id == char_id)
                    .map(|i| (i.type_id, i.insured_payout_isk))
            });
            let Some((type_id, current_payout)) = info else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: format!("Ship {ship_id} not found or not owned by character {char_id}"),
                }));
            };
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
            let Some(raw_muta) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMutaplasmid".into(),
                    message: "CreateDynamicItem requires mutaplasmid typeID".into(),
                }));
            };
            let mutaplasmid_type_id = raw_muta as TypeId;
            let Some(target_item_id) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "TargetItemNotFound".into(),
                    message: "CreateDynamicItem requires target itemID".into(),
                }));
            };

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
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: "GetDynamicItemInfo requires itemID".into(),
                }));
            };
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
            let Some(item_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: "AssembleShip requires itemID argument or active ship in session"
                        .into(),
                }));
            };
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
            let Some(target_ship_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: "Board requires target shipID argument or active ship in session"
                        .into(),
                }));
            };

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
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
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

        ("invBroker", "StackItems") | ("invbroker", "StackItems") | ("inventory", "StackItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let source_item_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            match ctx.db.stack_items(char_id, target_item_id, source_item_id) {
                Ok(updated) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(updated.item_id)),
                    ("typeID", PyRep::Integer(updated.type_id as i64)),
                    ("quantity", PyRep::Integer(updated.quantity as i64)),
                    ("version", PyRep::Integer(updated.version as i64)),
                ]))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "StackFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("invBroker", "SplitStack") | ("invbroker", "SplitStack") | ("inventory", "SplitStack") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let split_quantity = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1) as i32;
            match ctx.db.split_stack(char_id, item_id, split_quantity) {
                Ok((orig, new_i)) => Some(Ok(PyRep::keyval([
                    ("originalItemID", PyRep::Integer(orig.item_id)),
                    ("originalQuantity", PyRep::Integer(orig.quantity as i64)),
                    ("newItemID", PyRep::Integer(new_i.item_id)),
                    ("newQuantity", PyRep::Integer(new_i.quantity as i64)),
                    ("typeID", PyRep::Integer(orig.type_id as i64)),
                ]))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "SplitFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("invBroker", "DestroyItem")
        | ("invbroker", "DestroyItem")
        | ("inventory", "DestroyItem")
        | ("invBroker", "TrashItems")
        | ("invbroker", "TrashItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            match ctx.db.delete_item(char_id, item_id) {
                Ok(()) => {
                    ctx.invalidate_ship_fitting(item_id);
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(item_id)),
                        ("deleted", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DeleteFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        _ => None,
    }
}

/// Encodes a ship fitting into an EVE DNA fitting string (`INV-3`):
/// `<ship_type_id>:<type_id_1>;<qty_1>:<type_id_2>;<qty_2>:...::`
pub fn fitting_to_dna(ship_type_id: TypeId, items: &[(TypeId, i32)]) -> String {
    let mut counts: HashMap<TypeId, i32> = HashMap::new();
    let mut order: Vec<TypeId> = Vec::new();
    for &(type_id, qty) in items {
        if qty <= 0 {
            continue;
        }
        if !counts.contains_key(&type_id) {
            order.push(type_id);
        }
        *counts.entry(type_id).or_insert(0) += qty;
    }
    let mut out = format!("{ship_type_id}:");
    for type_id in order {
        let qty = counts[&type_id];
        out.push_str(&format!("{type_id};{qty}:"));
    }
    out.push(':');
    out
}

/// Encodes a `SavedFittingRecord` into an EVE DNA fitting string (`INV-3`).
pub fn saved_fitting_to_dna(fitting: &SavedFittingRecord) -> String {
    let items: Vec<(TypeId, i32)> = fitting.items.iter().map(|&(t, _, q)| (t, q)).collect();
    fitting_to_dna(fitting.ship_type_id, &items)
}

/// Decodes an EVE DNA fitting string into `(ship_type_id, Vec<(TypeId, i32)>)` (`INV-3`).
/// Supports standard format `<ship_type_id>:<type_id_1>;<qty_1>:<type_id_2>;<qty_2>:...::`
pub fn dna_to_fitting(dna: &str) -> Result<(TypeId, Vec<(TypeId, i32)>), String> {
    let trimmed = dna.trim();
    if trimmed.is_empty() {
        return Err("Empty DNA string".into());
    }
    let mut parts = trimmed.split(':');
    let ship_str = parts.next().ok_or("Missing ship type ID")?;
    let ship_type_id = ship_str
        .parse::<TypeId>()
        .map_err(|e| format!("Invalid ship type ID '{ship_str}': {e}"))?;

    let mut items = Vec::new();
    for part in parts {
        let token = part.trim();
        if token.is_empty() {
            continue;
        }
        let (type_str, qty_str) = match token.split_once(';') {
            Some((t, q)) => (t.trim(), q.trim()),
            None => (token, "1"),
        };
        let type_id = type_str
            .parse::<TypeId>()
            .map_err(|e| format!("Invalid item type ID '{type_str}': {e}"))?;
        let qty = qty_str
            .parse::<i32>()
            .map_err(|e| format!("Invalid quantity '{qty_str}': {e}"))?;
        if qty > 0 {
            items.push((type_id, qty));
        }
    }
    Ok((ship_type_id, items))
}
