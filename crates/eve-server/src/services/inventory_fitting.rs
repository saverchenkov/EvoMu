//! `inventory_fitting` — Domain 3: Inventory Broker, Ship Fitting, Insurance, Repair, & Abyssal Mutaplasmids (`§3.4`).

#![forbid(unsafe_code)]

use crate::formulas::{
    abyssal_attribute_roll, compute_insurance_quote, compute_ship_insurance_reference_value,
};
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

/// Mechanic roll in `[0, 1)` from the context RNG (seeded test RNG); `0.5` if the OS CSPRNG is unreadable.
fn mechanic_unit_f64(ctx: &ServerContext) -> f64 {
    ctx.mechanic_rng.try_unit_f64().unwrap_or(0.5)
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
    let method = if service == "dogmaIM" && method == "LaunchProbes" {
        "Activate"
    } else {
        method
    };
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

        ("invbroker", "Add") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(mut item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "Add requires itemID".into() }));
            };
            let Some(new_loc) = args.get(1).and_then(|a| a.as_i64()).or_else(|| bound_entry.map(|b| b.object_id)).or(session.ship_id).or(session.station_id).or(session.structure_id) else {
                return Some(Err(BridgeRpcError { code: "InvalidLocation".into(), message: "Add requires a target locationID, bound inventory, or active ship/station".into() }));
            };
            let raw_flag_opt = args.get(2).and_then(|a| a.as_i64()).or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()));
            if let Some(rf) = raw_flag_opt {
                if !matches!(rf, 4 | 5 | 11..=34 | 87 | 90 | 92..=94 | 125..=151 | 179 | 181 | 182) {
                    return Some(Err(BridgeRpcError { code: "InvalidInventoryFlag".into(), message: format!("Invalid or restricted inventory flag {rf}") }));
                }
            }
            let flag = raw_flag_opt.map(|v| InventoryFlag::from_u16(v as u16)).unwrap_or(InventoryFlag::Cargo);
            let flag_u16 = flag as u16;
            let requested_qty_opt = kwargs
                .get("qty")
                .or_else(|| kwargs.get("quantity"))
                .and_then(|v| v.as_i64())
                .or_else(|| args.get(3).and_then(|a| a.as_i64()));
            let mut source_item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&item_id).cloned());
            let mut looted_foreign_jetcan = false;
            if let Some(src_item) = &source_item_opt {
                let space_container_opt = ctx.db.with_snapshot(|snap| {
                    let ch_sys = snap
                        .characters
                        .get(&char_id)
                        .map(|c| c.solar_system_id as i64)
                        .unwrap_or(0);
                    snap.items.get(&src_item.location_id).and_then(|cont| {
                        let is_wreck = ctx
                            .sde
                            .types
                            .get(&cont.type_id)
                            .is_some_and(|t| t.group_id == 186);
                        if (cont.type_id == 23 || is_wreck)
                            && cont.flag == InventoryFlag::None
                            && cont.location_id == ch_sys
                        {
                            Some(cont.owner_id != char_id || src_item.owner_id != char_id)
                        } else {
                            None
                        }
                    })
                });
                if let Some(is_foreign) = space_container_opt {
                    let active_ship = session.ship_id.unwrap_or(new_loc);
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(it) = snap.items.get_mut(&item_id) {
                            it.owner_id = char_id;
                            it.location_id = active_ship;
                        }
                    });
                    if let Some(src) = source_item_opt.as_mut() {
                        src.owner_id = char_id;
                        src.location_id = active_ship;
                    }
                    looted_foreign_jetcan = is_foreign;
                }
            }
            if let (Some(req_qty), Some(src_item)) = (requested_qty_opt, source_item_opt.as_ref()) {
                if req_qty > 0
                    && !src_item.singleton
                    && !src_item.is_blueprint_copy
                    && (req_qty as i32) < src_item.quantity
                {
                    match ctx.db.split_stack(char_id, item_id, req_qty as i32) {
                        Ok((_orig, split_item)) => {
                            item_id = split_item.item_id;
                            source_item_opt = Some(split_item);
                        }
                        Err(e) => {
                            return Some(Err(BridgeRpcError {
                                code: "SplitFailed".into(),
                                message: e.to_string(),
                            }));
                        }
                    }
                }
            }

            let is_fitting_slot = matches!(flag_u16, 11..=34 | 92..=94 | 125..=132);
            let src_is_fitted_module = source_item_opt.as_ref().is_some_and(|src_item| {
                matches!(src_item.flag as u16, 11..=34 | 92..=94 | 125..=132)
                    && ctx
                        .sde
                        .types
                        .get(&src_item.type_id)
                        .map(|t| t.category_id == 7 || t.category_id == 32)
                        .unwrap_or(true)
            });

            if src_is_fitted_module && !is_fitting_slot {
                if session.station_id.is_none() && session.structure_id.is_none() {
                    let mut near_fitting_service = false;
                    if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                        let depot_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
                            snap.items
                                .values()
                                .filter(|i| {
                                    i.location_id == sys_id as i64
                                        && ctx
                                            .sde
                                            .types
                                            .get(&i.type_id)
                                            .is_some_and(|t| t.group_id == 1246)
                                })
                                .map(|i| i.item_id)
                                .collect()
                        });
                        if !depot_ids.is_empty() {
                            let sys_handle = ctx.systems.get_or_spawn(sys_id);
                            if let Ok(Some((_, ship_ball))) = sys_handle.get_ball_snapshot(ship_id).await {
                                for depot_id in depot_ids {
                                    if let Ok(Some((_, depot_ball))) = sys_handle.get_ball_snapshot(depot_id).await {
                                        if (ship_ball.position - depot_ball.position).length() <= 3_000.0 {
                                            near_fitting_service = true;
                                            break;
                                        }
                                    } else {
                                        near_fitting_service = true;
                                        break;
                                    }
                                }
                            } else {
                                near_fitting_service = true;
                            }
                        }
                    }
                    if !near_fitting_service {
                        return Some(Err(BridgeRpcError {
                            code: "CannotUnfitInSpace".into(),
                            message: "Unfitting modules requires being docked at a station/structure or within 3,000m of a deployed Mobile Depot".into(),
                        }));
                    }
                }
                ctx.remove_projected_ewar_by_module(item_id);
            }

            if let Some(src_item) = &source_item_opt {
                if matches!(src_item.flag as u16, 92..=94) && !matches!(flag_u16, 92..=94) {
                    let old_loc = src_item.location_id;
                    ctx.db.with_mut_snapshot(|snap| {
                        snap.items.remove(&item_id);
                        snap.module_online_states.remove(&item_id);
                        snap.module_active_states.remove(&item_id);
                        snap.module_active_charges.remove(&item_id);
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

            if is_fitting_slot {
                let item_cat_opt = ctx.db.with_snapshot(|snap| {
                    snap.items.get(&item_id).and_then(|item| ctx.sde.types.get(&item.type_id).map(|t| t.category_id))
                });
                if item_cat_opt == Some(8) && matches!(flag_u16, 11..=34) {
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
                    if ship_type.category_id != 6 {
                        return Ok(());
                    }
                    let Some(item_type) = ctx.sde.types.get(&moving_item.type_id) else { return Ok(()); };

                    // Enforce specialized hold item-group / category restrictions (`feat:mining.ore-mineral-holds`)
                    let allowed_in_hold = match flag_u16 {
                        5 => true,
                        134 | 151 => {
                            // SpecializedOreHold / GeneralMiningHold: asteroid ores, moon ores, ice (cat 25), or gas (group 711 / 4168)
                            item_type.category_id == 25 || matches!(item_type.group_id, 711 | 4168)
                        }
                        135 => {
                            // SpecializedGasHold: harvestable/compressed gas (group 711 / 4168)
                            matches!(item_type.group_id, 711 | 4168)
                        }
                        136 => {
                            // SpecializedMineralHold: refined minerals (group 18)
                            item_type.group_id == 18
                        }
                        181 => {
                            // SpecializedIceHold: raw/compressed ice (group 465)
                            item_type.group_id == 465
                        }
                        143 => {
                            // SpecializedAmmoHold: charges/ammo (category 8)
                            item_type.category_id == 8
                        }
                        148 => {
                            // SpecializedCommandCenterHold: Command Centers (group 1027)
                            item_type.group_id == 1027
                        }
                        149 => {
                            // SpecializedPlanetaryCommoditiesHold: planetary resources/commodities (cat 42 / 43)
                            matches!(item_type.category_id, 42 | 43)
                        }
                        _ => true,
                    };
                    if !allowed_in_hold {
                        return Err(BridgeRpcError {
                            code: "InvalidItemTypeForHold".into(),
                            message: format!(
                                "Item {} (type {}, group {}, category {}) cannot be placed in specialized hold flag {}",
                                moving_item.item_id, item_type.type_id, item_type.group_id, item_type.category_id, flag_u16
                            ),
                        });
                    }

                    let hold_capacity = match flag_u16 {
                        134 | 151 => ship_type.default_attributes.get(&1556).copied()
                            .or_else(|| ship_type.default_attributes.get(&1770).copied())
                            .unwrap_or(0.0),
                        136 => ship_type.default_attributes.get(&1558).copied().unwrap_or(0.0),
                        135 => ship_type.default_attributes.get(&1557).copied().unwrap_or(0.0),
                        181 => ship_type.default_attributes.get(&3136).copied()
                            .or_else(|| ship_type.default_attributes.get(&1556).copied())
                            .unwrap_or(0.0),
                        143 => ship_type.default_attributes.get(&1573).copied().unwrap_or(0.0),
                        148 => ship_type.default_attributes.get(&1646).copied().unwrap_or(5_000.0),
                        149 => ship_type.default_attributes.get(&1653).copied().unwrap_or(0.0),
                        5 => ship_type.capacity,
                        _ => 0.0,
                    };
                    if flag_u16 != 5 && hold_capacity <= 0.0 {
                        return Err(BridgeRpcError {
                            code: "CapacityExceeded".into(),
                            message: format!(
                                "Ship {} does not have a specialized hold for flag {flag_u16} (capacity 0 m³)",
                                ship_type.name
                            ),
                        });
                    }
                    if hold_capacity > 0.0 {
                        let used_vol: f64 = snap.items.values()
                            .filter(|i| i.location_id == new_loc && (i.flag as u16) == flag_u16 && i.item_id != item_id)
                            .map(|i| {
                                let vol = ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.0);
                                vol * (i.quantity as f64)
                            })
                            .sum();
                        let item_vol = item_type.volume * (moving_item.quantity as f64);
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
                    if looted_foreign_jetcan {
                        let now_unix = ctx.now_unix_secs();
                        let until = now_unix.saturating_add(900);
                        ctx.crimewatch
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .get_or_create(char_id)
                            .suspect_flag_until_unix = until;
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnSuspectsAndCriminalsUpdate",
                            "charid",
                            vec![PyRep::Integer(char_id), PyRep::Bool(true), PyRep::Integer(until as i64)],
                        );
                    }
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
                        ("suspectFlagged", PyRep::Bool(looted_foreign_jetcan)),
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
                    if looted_foreign_jetcan {
                        let now_unix = ctx.now_unix_secs();
                        let until = now_unix.saturating_add(900);
                        ctx.crimewatch
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .get_or_create(char_id)
                            .suspect_flag_until_unix = until;
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnSuspectsAndCriminalsUpdate",
                            "charid",
                            vec![PyRep::Integer(char_id), PyRep::Bool(true), PyRep::Integer(until as i64)],
                        );
                    }
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
                                snap.module_active_states.remove(&item_id);
                                snap.module_active_charges.remove(&item_id);
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
                        ("suspectFlagged", PyRep::Bool(looted_foreign_jetcan)),
                    ])))
                }
                Err(e) => Some(Err(BridgeRpcError {
                    code: "InventoryMoveFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("ship", "Jettison") | ("invbroker", "Jettison") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Jettison requires an active ship".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotInSpace".into(),
                    message: "Jettison can only be performed in space".into(),
                }));
            };

            let mut item_ids: Vec<i64> = Vec::new();
            if let Some(first) = args.first() {
                match first {
                    PyRep::List(list) | PyRep::Tuple(list) => {
                        for el in list {
                            if let Some(id) = el.as_i64() {
                                item_ids.push(id);
                            }
                        }
                    }
                    _ => {
                        if let Some(id) = first.as_i64() {
                            item_ids.push(id);
                        }
                    }
                }
            }
            if item_ids.is_empty() {
                if let Some(PyRep::List(list) | PyRep::Tuple(list)) = kwargs.get("itemIDs") {
                    for el in list {
                        if let Some(id) = el.as_i64() {
                            item_ids.push(id);
                        }
                    }
                } else if let Some(id) = kwargs.get("itemID").and_then(|v| v.as_i64()) {
                    item_ids.push(id);
                }
            }
            if item_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Jettison requires at least one itemID".into(),
                }));
            }

            let now_unix = ctx.now_unix_secs();
            let res: Result<(i64, i64), BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
                for &iid in &item_ids {
                    let Some(it) = snap.items.get(&iid) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Item {iid} not found"),
                        });
                    };
                    if it.owner_id != char_id || it.location_id != ship_id {
                        return Err(BridgeRpcError {
                            code: "ItemNotOnShip".into(),
                            message: format!("Item {iid} is not in active ship {ship_id}"),
                        });
                    }
                }

                snap.next_entity_id += 1;
                let jetcan_id = snap.next_entity_id;
                let mut dyn_attrs = HashMap::new();
                dyn_attrs.insert(9999, now_unix as f64); // Creation unix timestamp (`feat:mining.jetcan-mining`)
                snap.items.insert(
                    jetcan_id,
                    eve_db::InventoryItemRecord {
                        item_id: jetcan_id,
                        type_id: 23, // Cargo Container (jetcan, 27,500 m3 capacity)
                        owner_id: char_id,
                        location_id: sys_id as i64,
                        flag: InventoryFlag::None,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        dynamic_attributes: dyn_attrs,
                        ..Default::default()
                    },
                );

                let mut total_units = 0i64;
                for &iid in &item_ids {
                    if let Some(it) = snap.items.get_mut(&iid) {
                        it.location_id = jetcan_id;
                        it.flag = InventoryFlag::Cargo;
                        it.version += 1;
                        total_units += it.quantity as i64;
                    }
                }
                Ok((jetcan_id, total_units))
            });

            let (jetcan_id, total_units) = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let ship_pos = sys_handle
                .get_ball_snapshot(ship_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position)
                .unwrap_or(DVec3::ZERO);
            let can_pos = DVec3::new(ship_pos.x + 500.0, ship_pos.y, ship_pos.z);
            let can_sde = ctx.sde.types.get(&23);
            let _ = sys_handle
                .spawn_ball_with_meta(
                    eve_destiny::Ball {
                        id: jetcan_id,
                        type_id: 23,
                        position: can_pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 1.0,
                        mass: can_sde.map(|t| t.mass.max(1.0)).unwrap_or(1_000.0),
                        radius: can_sde.map(|t| t.radius.max(10.0)).unwrap_or(50.0),
                        speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Rigid,
                    },
                    eve_destiny::CombatState::default(),
                    eve_destiny::SlimItemMeta {
                        group_id: 12,
                        category_id: 2,
                        owner_id: char_id,
                        char_id: Some(char_id),
                        corp_id: session.corporation_id.unwrap_or(-1),
                        alliance_id: session.alliance_id.unwrap_or(-1),
                        security_status: 0.0,
                        is_cloaked: false,
                    },
                )
                .await;

            ctx.invalidate_ship_fitting(ship_id);
            ctx.notify(
                crate::router::NotificationTarget::SolarSystem(sys_id),
                "DoBallsAdded",
                "solarsystemid",
                vec![PyRep::Integer(jetcan_id), PyRep::Integer(23)],
            );

            Some(Ok(PyRep::keyval([
                ("jetcanID", PyRep::Integer(jetcan_id)),
                ("containerID", PyRep::Integer(jetcan_id)),
                ("typeID", PyRep::Integer(23)),
                ("ownerID", PyRep::Integer(char_id)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("jettisonedUnits", PyRep::Integer(total_units)),
                ("createdAtUnix", PyRep::Integer(now_unix as i64)),
                ("expiresAtUnix", PyRep::Integer((now_unix + 3600) as i64)),
            ])))
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

            let eval_ctx = fit.evaluated_context(&ctx.sde);
            let mut ship_attr_ids: std::collections::BTreeSet<u16> = eval_ctx
                .ship
                .base_attributes
                .keys()
                .copied()
                .chain(eval_ctx.ship.modifiers.iter().map(|m| m.target_attribute_id))
                .collect();
            ship_attr_ids.insert(attr_ids::CPU_LOAD);
            ship_attr_ids.insert(attr_ids::POWER_LOAD);
            let mut entries = Vec::with_capacity(ship_attr_ids.len());
            for attr_id in ship_attr_ids {
                let val = eval_ctx.ship.get_attribute(attr_id, &ctx.sde);
                entries.push((PyRep::Integer(attr_id as i64), PyRep::Real(val)));
            }

            let mut module_entries = Vec::with_capacity(eval_ctx.ship_located_items.len());
            for m_state in &eval_ctx.ship_located_items {
                let attr_ids_set: std::collections::BTreeSet<u16> = m_state
                    .base_attributes
                    .keys()
                    .copied()
                    .chain(m_state.modifiers.iter().map(|m| m.target_attribute_id))
                    .collect();
                let mut m_attrs = Vec::with_capacity(attr_ids_set.len());
                for aid in attr_ids_set {
                    m_attrs.push((
                        PyRep::Integer(aid as i64),
                        PyRep::Real(m_state.get_attribute(aid, &ctx.sde)),
                    ));
                }
                module_entries.push((PyRep::Integer(m_state.item_id), PyRep::Dict(m_attrs)));
            }

            ctx.store_ship_fitting(ship_id, fit);

            if method == "GetAllInfo" {
                Some(Ok(PyRep::keyval([
                    ("shipID", PyRep::Integer(ship_id)),
                    ("charID", PyRep::Integer(char_id)),
                    ("shipAttributes", PyRep::Dict(entries)),
                    ("moduleAttributes", PyRep::Dict(module_entries)),
                ])))
            } else {
                Some(Ok(PyRep::Dict(entries)))
            }
        }

        ("dogmaIM", "ItemGetInfo")
        | ("invBroker", "GetItemInfo")
        | ("invBroker", "GetDynamicItemInfo")
        | ("dogmaIM", "GetDynamicItemInfo") => {
            let Some(raw_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "ItemGetInfo requires itemID or typeID".into(),
                }));
            };

            // 1. Check if raw_id is an item in the world DB
            let item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&raw_id).cloned());

            let (type_id, owner_id, dyn_attrs, location_id, qty, singleton) = if let Some(ref item) = item_opt {
                (
                    item.type_id,
                    PyRep::Integer(item.owner_id),
                    item.dynamic_attributes.clone(),
                    Some(item.location_id),
                    item.quantity,
                    item.singleton,
                )
            } else {
                // If not found in items, raw_id is treated as a TypeId
                let tid = raw_id as TypeId;
                (tid, PyRep::None, HashMap::new(), None, 1, false)
            };

            let Some(show_info) = ctx.sde.show_info_for_type(type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "TypeNotFound".into(),
                    message: format!("Type {type_id} not found in SDE"),
                }));
            };

            // Merge default attributes with dynamic attributes
            let mut merged_attrs = show_info.default_attributes.clone();
            for (&attr_id, &val) in &dyn_attrs {
                merged_attrs.insert(attr_id, val);
            }
            if let Some(ref item) = item_opt {
                let item_cat = ctx.sde.types.get(&item.type_id).map(|t| t.category_id).unwrap_or(0);
                if item_cat == 6 {
                    if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(item.owner_id, item.item_id) {
                        let eval_ctx = fit.evaluated_context(&ctx.sde);
                        let all_aids: std::collections::BTreeSet<u16> = eval_ctx
                            .ship
                            .base_attributes
                            .keys()
                            .copied()
                            .chain(eval_ctx.ship.modifiers.iter().map(|m| m.target_attribute_id))
                            .collect();
                        for aid in all_aids {
                            merged_attrs.insert(aid, eval_ctx.ship.get_attribute(aid, &ctx.sde));
                        }
                    }
                } else if matches!(item.flag as u16, 11..=34 | 92..=94 | 125..=132) {
                    if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(item.owner_id, item.location_id) {
                        let eval_ctx = fit.evaluated_context(&ctx.sde);
                        if item_cat == 8 {
                            if let Some(c_state) = eval_ctx
                                .owned_items
                                .iter()
                                .find(|c| c.type_id == item.type_id)
                            {
                                let all_aids: std::collections::BTreeSet<u16> = c_state
                                    .base_attributes
                                    .keys()
                                    .copied()
                                    .chain(c_state.modifiers.iter().map(|m| m.target_attribute_id))
                                    .collect();
                                for aid in all_aids {
                                    merged_attrs.insert(aid, c_state.get_attribute(aid, &ctx.sde));
                                }
                            }
                        } else if let Some(m_state) = eval_ctx
                            .ship_located_items
                            .iter()
                            .find(|m| m.item_id == item.item_id)
                        {
                            let all_aids: std::collections::BTreeSet<u16> = m_state
                                .base_attributes
                                .keys()
                                .copied()
                                .chain(m_state.modifiers.iter().map(|m| m.target_attribute_id))
                                .collect();
                            for aid in all_aids {
                                merged_attrs.insert(aid, m_state.get_attribute(aid, &ctx.sde));
                            }
                        }
                    }
                }
            }

            let mut attr_entries: Vec<(PyRep, PyRep)> = merged_attrs
                .into_iter()
                .map(|(k, v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
                .collect();
            attr_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));

            let mut dyn_entries: Vec<(PyRep, PyRep)> = dyn_attrs
                .into_iter()
                .map(|(k, v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
                .collect();
            dyn_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));

            let var_reps: Vec<PyRep> = show_info
                .variations
                .into_iter()
                .map(|v| PyRep::Integer(v as i64))
                .collect();

            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(raw_id)),
                ("typeID", PyRep::Integer(show_info.type_id as i64)),
                ("typeName", PyRep::String(show_info.name)),
                ("groupID", PyRep::Integer(show_info.group_id as i64)),
                ("categoryID", PyRep::Integer(show_info.category_id as i64)),
                ("ownerID", owner_id),
                ("locationID", location_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("quantity", PyRep::Integer(qty as i64)),
                ("singleton", PyRep::Bool(singleton)),
                ("mass", PyRep::Real(show_info.mass)),
                ("volume", PyRep::Real(show_info.volume)),
                ("capacity", PyRep::Real(show_info.capacity)),
                ("portionSize", PyRep::Integer(show_info.portion_size as i64)),
                ("basePrice", PyRep::Real(show_info.base_price)),
                ("published", PyRep::Bool(show_info.published)),
                (
                    "marketGroupID",
                    show_info
                        .market_group_id
                        .map(|mg| PyRep::Integer(mg as i64))
                        .unwrap_or(PyRep::None),
                ),
                (
                    "variationParentTypeID",
                    show_info
                        .variation_parent_type_id
                        .map(|vp| PyRep::Integer(vp as i64))
                        .unwrap_or(PyRep::None),
                ),
                ("variations", PyRep::List(var_reps)),
                ("dynamicAttributes", PyRep::Dict(dyn_entries)),
                ("attributes", PyRep::Dict(attr_entries)),
            ])))
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
                    snap.module_active_states.remove(&item_id);
                    snap.module_active_charges.remove(&item_id);
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
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.remove_module_cycle(ship_id, module_id).await;
            }

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

            if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                if cs.ecm_jammed_remaining_s > 0.0 {
                    return Some(Err(BridgeRpcError {
                        code: "ShipJammed".into(),
                        message: "Targeting systems are jammed by ECM".into(),
                    }));
                }
            }

            let max_range = fit.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde);
            let dist = (ship_ball.position - target_ball.position).length();
            if dist > max_range {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Target distance ({dist:.1} m) exceeds maxTargetRange ({max_range:.1} m)"),
                }));
            }

            let ship_max = fit.ship.get_attribute(attr_ids::MAX_LOCKED_TARGETS, &ctx.sde).max(1.0) as usize;
            let tm_lvl = ctx.character_skill_levels(char_id).get(&3429).copied().unwrap_or(0);
            let atm_lvl = ctx.character_skill_levels(char_id).get(&3430).copied().unwrap_or(0);
            let max_locked = eve_dogma::compute_effective_max_locked_targets(ship_max, tm_lvl, atm_lvl);
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
                max_range_m: Some(max_range),
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
            let raw_module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let effect_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("defaultEffect").to_string();
            let target_id_opt = args.get(2).and_then(|a| a.as_i64())
                .or_else(|| args.get(1).and_then(|a| a.as_i64()))
                .or_else(|| kwargs.get("target").and_then(|v| v.as_i64()));
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

            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            let module_id = if raw_module_id != 0 {
                raw_module_id
            } else {
                fit.modules
                    .values()
                    .find(|m| {
                        ctx.sde
                            .types
                            .get(&m.type_id)
                            .is_some_and(|t| t.group_id == 481)
                    })
                    .map(|m| m.item_id)
                    .unwrap_or(0)
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

            // Scan Probe Launcher (`group_id == 481`, e.g., `Core Probe Launcher I` `17938`)
            if mod_type.group_id == 481 {
                let loaded_charge_opt = ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .find(|i| {
                            i.location_id == ship_id
                                && (i.flag as u16) == m.flag_id
                                && i.item_id != module_id
                                && i.quantity > 0
                                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 479)
                        })
                        .map(|i| (i.item_id, i.type_id, i.quantity))
                });
                let Some((charge_item_id, probe_type_id, loaded_qty)) = loaded_charge_opt else {
                    return Some(Err(BridgeRpcError {
                        code: "NoProbesLoaded".into(),
                        message: "Scan probe launcher has no scanner probes loaded".into(),
                    }));
                };

                let existing_active_count = ctx
                    .active_scanner_probes
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .values()
                    .filter(|p| p.owner_char_id == char_id && p.solar_system_id == sys_id)
                    .count();
                if existing_active_count >= 8 {
                    return Some(Err(BridgeRpcError {
                        code: "MaxProbesLaunched".into(),
                        message: "Maximum of 8 active scanner probes already in space".into(),
                    }));
                }

                let max_can_launch = (8 - existing_active_count) as i32;
                let requested_count = kwargs
                    .get("probeCount")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(loaded_qty as i64) as i32;
                let to_launch = requested_count.clamp(1, max_can_launch).min(loaded_qty);

                let (allocated_probe_ids, remaining_in_launcher) = ctx.db.with_mut_snapshot(|snap| {
                    let mut rem = 0;
                    if let Some(ci) = snap.items.get_mut(&charge_item_id) {
                        ci.quantity = (ci.quantity - to_launch).max(0);
                        rem = ci.quantity;
                    }
                    if rem == 0 {
                        snap.items.remove(&charge_item_id);
                    }
                    let mut ids = Vec::with_capacity(to_launch as usize);
                    for _ in 0..to_launch {
                        snap.next_entity_id += 1;
                        ids.push(snap.next_entity_id);
                    }
                    (ids, rem)
                });
                ctx.invalidate_ship_fitting(ship_id);

                let probe_sde = ctx.sde.types.get(&probe_type_id);
                let base_sensor_strength = probe_sde
                    .and_then(|t| t.default_attributes.get(&1909).copied())
                    .unwrap_or(18.0);
                let max_flight_time_s = probe_sde
                    .and_then(|t| t.default_attributes.get(&281).copied())
                    .map(|ms| ms / 1000.0)
                    .unwrap_or(3_600.0);
                let range_step = kwargs
                    .get("rangeStep")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(4)
                    .clamp(1, 8) as i32;
                let base_scan_range_au = probe_sde
                    .and_then(|t| t.default_attributes.get(&1911).copied())
                    .unwrap_or(0.25);
                let scan_range_m =
                    base_scan_range_au * (2.0_f64.powi(range_step - 1)) * 149_597_870_700.0;

                let ship_pos = sys
                    .get_ball_snapshot(ship_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|(_, b)| b.position)
                    .unwrap_or(DVec3::ZERO);

                let now_unix = ctx.now_unix_secs();
                let formation_offsets: [[f64; 3]; 8] = [
                    [1_000.0, 0.0, 0.0],
                    [-1_000.0, 0.0, 0.0],
                    [0.0, 1_000.0, 0.0],
                    [0.0, -1_000.0, 0.0],
                    [0.0, 0.0, 1_000.0],
                    [0.0, 0.0, -1_000.0],
                    [707.0, 707.0, 0.0],
                    [-707.0, -707.0, 0.0],
                ];

                for (idx, &probe_id) in allocated_probe_ids.iter().enumerate() {
                    let off = formation_offsets[idx % formation_offsets.len()];
                    let pos = DVec3::new(ship_pos.x + off[0], ship_pos.y + off[1], ship_pos.z + off[2]);
                    let ball = eve_destiny::Ball {
                        id: probe_id,
                        type_id: probe_type_id,
                        position: pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 3_000.0,
                        inertia: 1.0,
                        mass: probe_sde.map(|t| t.mass.max(1.0)).unwrap_or(1.0),
                        radius: probe_sde.map(|t| t.radius.max(1.0)).unwrap_or(2.0),
                        speed_fraction: 0.0,
                        mode: eve_destiny::MovementMode::Stop,
                    };
                    let _ = sys
                        .spawn_ball_with_meta(
                            ball,
                            eve_destiny::CombatState::default(),
                            eve_destiny::SlimItemMeta {
                                group_id: 479,
                                category_id: 8,
                                owner_id: char_id,
                                char_id: Some(char_id),
                                corp_id: session.corporation_id.unwrap_or(-1),
                                alliance_id: session.alliance_id.unwrap_or(-1),
                                security_status: 0.0,
                                is_cloaked: false,
                            },
                        )
                        .await;
                    ctx.active_scanner_probes
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(
                            probe_id,
                            crate::router::ActiveScannerProbe {
                                probe_id,
                                probe_type_id,
                                owner_char_id: char_id,
                                ship_id,
                                launcher_module_id: module_id,
                                solar_system_id: sys_id,
                                position: pos.to_array(),
                                destination: pos.to_array(),
                                range_step,
                                scan_range_m,
                                base_sensor_strength,
                                max_flight_time_s,
                                launched_at_unix: now_unix,
                                state: 1,
                            },
                        );
                }

                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnModuleActivated",
                    "charid",
                    vec![PyRep::Integer(module_id), PyRep::Integer(1)],
                );
                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("launcherID", PyRep::Integer(module_id)),
                    ("probeTypeID", PyRep::Integer(probe_type_id as i64)),
                    (
                        "probeIDs",
                        PyRep::List(allocated_probe_ids.into_iter().map(PyRep::Integer).collect()),
                    ),
                    ("launchedCount", PyRep::Integer(to_launch as i64)),
                    ("remainingInLauncher", PyRep::Integer(remaining_in_launcher as i64)),
                    ("baseSensorStrength", PyRep::Real(base_sensor_strength)),
                    ("maxFlightTimeS", PyRep::Real(max_flight_time_s)),
                ])));
            }

            // Interdiction Sphere Launcher (`group_id == 589`, e.g., `Interdiction Sphere Launcher I` `22782`)
            if mod_type.group_id == 589 {
                let sys_sec = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| s.security_status)
                    .unwrap_or(1.0);
                if sys_sec > 0.0 {
                    return Some(Err(BridgeRpcError {
                        code: "CannotLaunchInterdictionProbeInEmpire".into(),
                        message: format!(
                            "Interdiction sphere probes can only be launched in null-sec or wormhole space (system {sys_id} security {sys_sec:.2} > 0.0)"
                        ),
                    }));
                }
                let ship_group_id = ctx
                    .sde
                    .types
                    .get(&fit.ship.type_id)
                    .map(|t| t.group_id)
                    .unwrap_or(0);
                if ship_group_id != 541 {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidShipGroup".into(),
                        message: "Interdiction Sphere Launcher can only be activated on Interdictors (group 541)".into(),
                    }));
                }
                let loaded_charge_opt = ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .find(|i| {
                            i.location_id == ship_id
                                && (i.flag as u16) == m.flag_id
                                && i.item_id != module_id
                                && i.quantity > 0
                                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 548)
                        })
                        .map(|i| (i.item_id, i.type_id))
                });
                let Some((charge_item_id, probe_type_id)) = loaded_charge_opt else {
                    return Some(Err(BridgeRpcError {
                        code: "NoProbesLoaded".into(),
                        message: "Interdiction Sphere Launcher has no Warp Disrupt Probe loaded".into(),
                    }));
                };

                let (probe_id, remaining_in_launcher) = ctx.db.with_mut_snapshot(|snap| {
                    let mut rem = 0;
                    if let Some(ci) = snap.items.get_mut(&charge_item_id) {
                        ci.quantity = (ci.quantity - 1).max(0);
                        rem = ci.quantity;
                    }
                    if rem == 0 {
                        snap.items.remove(&charge_item_id);
                    }
                    snap.next_entity_id += 1;
                    (snap.next_entity_id, rem)
                });
                if remaining_in_launcher == 0 {
                    ctx.invalidate_ship_fitting(ship_id);
                }

                let probe_sde = ctx.sde.types.get(&probe_type_id);
                let bubble_radius = probe_sde
                    .and_then(|t| t.default_attributes.get(&103).copied())
                    .unwrap_or(20_000.0);
                let ship_pos = sys
                    .get_ball_snapshot(ship_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|(_, b)| b.position)
                    .unwrap_or(DVec3::ZERO);

                if let Err(e) = sys.add_interdiction_sphere(probe_id, ship_pos, bubble_radius).await {
                    return Some(Err(BridgeRpcError {
                        code: "SystemActorError".into(),
                        message: e,
                    }));
                }

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
                    vec![PyRep::Integer(module_id), PyRep::Integer(1)],
                );
                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("launcherID", PyRep::Integer(module_id)),
                    ("probeID", PyRep::Integer(probe_id)),
                    ("probeTypeID", PyRep::Integer(probe_type_id as i64)),
                    ("bubbleRadius", PyRep::Real(bubble_radius)),
                    ("remainingInLauncher", PyRep::Integer(remaining_in_launcher as i64)),
                ])));
            }

            // Survey Probe Launcher (`group_id == 1226`, e.g., `Survey Probe Launcher I` `35660`)
            if mod_type.group_id == 1226 {
                let loaded_probe_opt = ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .find(|i| {
                            i.location_id == ship_id
                                && (i.flag as u16) == m.flag_id
                                && i.item_id != module_id
                                && i.quantity > 0
                                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 492)
                        })
                        .or_else(|| {
                            snap.items.values().find(|i| {
                                i.location_id == ship_id
                                    && matches!(
                                        i.flag,
                                        InventoryFlag::Cargo | InventoryFlag::SpecializedAmmoHold
                                    )
                                    && i.quantity > 0
                                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 492)
                            })
                        })
                        .map(|i| (i.item_id, i.type_id))
                });
                let Some((charge_item_id, probe_type_id)) = loaded_probe_opt else {
                    return Some(Err(BridgeRpcError {
                        code: "NoProbesLoaded".into(),
                        message: "Survey probe launcher has no survey probes loaded".into(),
                    }));
                };

                let moon_id = target_id_opt.unwrap_or_else(|| {
                    ctx.sde
                        .celestials_in_system(sys_id)
                        .into_iter()
                        .find(|c| c.group_id == 8)
                        .map(|c| c.celestial_id)
                        .unwrap_or(40_000_001)
                });

                let (probe_id, remaining_in_launcher) = ctx.db.with_mut_snapshot(|snap| {
                    let mut rem = 0;
                    if let Some(ci) = snap.items.get_mut(&charge_item_id) {
                        ci.quantity = (ci.quantity - 1).max(0);
                        rem = ci.quantity;
                    }
                    if rem == 0 {
                        snap.items.remove(&charge_item_id);
                    }
                    snap.next_entity_id += 1;
                    (snap.next_entity_id, rem)
                });
                ctx.invalidate_ship_fitting(ship_id);

                let probe_sde = ctx.sde.types.get(&probe_type_id);
                let scan_delay_ms = probe_sde
                    .and_then(|t| t.default_attributes.get(&1370).copied().or_else(|| t.default_attributes.get(&281).copied()))
                    .unwrap_or(300_000.0) as i64;

                let composition =
                    crate::services::mining_advanced::derive_moon_ores_from_sde(&ctx.sde, sys_id, moon_id);
                let resources_dict = PyRep::Dict(
                    composition
                        .iter()
                        .map(|&(tid, frac)| (PyRep::Integer(tid as i64), PyRep::Real(frac)))
                        .collect(),
                );
                let composition_list = PyRep::List(
                    composition
                        .iter()
                        .map(|&(tid, frac)| {
                            PyRep::Tuple(vec![PyRep::Integer(tid as i64), PyRep::Real(frac)])
                        })
                        .collect(),
                );

                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnNewMoonProbe",
                    "charid",
                    vec![
                        PyRep::Integer(probe_id),
                        PyRep::Integer(probe_type_id as i64),
                        PyRep::Integer(moon_id),
                    ],
                );
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnMoonScanComplete",
                    "charid",
                    vec![PyRep::Integer(moon_id), resources_dict.clone()],
                );
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnRemoveMoonProbe",
                    "charid",
                    vec![PyRep::Integer(probe_id)],
                );

                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("launcherID", PyRep::Integer(module_id)),
                    ("probeID", PyRep::Integer(probe_id)),
                    ("probeTypeID", PyRep::Integer(probe_type_id as i64)),
                    ("moonID", PyRep::Integer(moon_id)),
                    ("scanDelayMs", PyRep::Integer(scan_delay_ms)),
                    ("remainingInLauncher", PyRep::Integer(remaining_in_launcher as i64)),
                    ("resources", resources_dict),
                    ("composition", composition_list),
                ])));
            }

            let is_mining_module = matches!(mod_type.group_id, 54 | 464 | 483 | 737 | 2004 | 4138)
                || mod_type.default_attributes.contains_key(&77);

            if !is_mining_module {
                if ctx.is_module_in_reactivation_delay(ship_id, module_id) {
                    return Some(Err(BridgeRpcError {
                        code: "ModuleReactivationDelay".into(),
                        message: format!("Module {module_id} cannot reactivate before delay expires"),
                    }));
                }
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

                let is_mwd = mod_type.group_id == 46
                    && (mod_type.default_attributes.get(&554).copied().unwrap_or(0.0) > 0.0
                        || mod_type.name.contains("Microwarpdrive"));
                let is_mjd = mod_type.group_id == 1189
                    || mod_type.group_id == 4769
                    || mod_type.name.contains("Micro Jump Drive");
                let is_mwd_or_mjd = is_mwd || is_mjd;

                if is_mwd_or_mjd {
                    if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                        if cs.is_mwd_mjd_disabled || (is_mjd && cs.is_warp_scrambled()) {
                            return Some(Err(BridgeRpcError {
                                code: "WarpScrambled".into(),
                                message: "Cannot activate Microwarpdrive or Micro Jump Drive while warp scrambled".into(),
                            }));
                        }
                    }
                }

                let mut warp_scramble_points = 0i32;
                let mut disables_target_mwd_mjd = false;
                let mut warp_core_stab_bonus = 0i32;
                let mut hic_bubble_radius_m: Option<f64> = None;
                let mut hic_velocity_multiplier: Option<f64> = None;
                let mut hic_sig_multiplier: Option<f64> = None;
                let mut hic_mass_addition: Option<f64> = None;
                let mut mjd_jump_distance_m: Option<f64> = None;
                let grants_interdiction_nullification = mod_type.group_id == 4117;

                // Microwarpdrive (`group_id == 46` with signatureRadiusBonus `554`, e.g., `5MN Microwarpdrive I` `434`)
                if is_mwd {
                    let sig_bonus_pct = mod_state.get_attribute(554, &ctx.sde);
                    if sig_bonus_pct > 0.0 {
                        hic_sig_multiplier = Some(1.0 + sig_bonus_pct / 100.0);
                    }
                    let mass_add = mod_state.get_attribute(attr_ids::MASS_ADDITION, &ctx.sde);
                    if mass_add > 0.0 {
                        hic_mass_addition = Some(mass_add);
                    }
                }

                // Micro Jump Drive (`group_id == 1189 | 4769`, e.g., `Large Micro Jump Drive` `4383`, `Medium Micro Jump Drive` `33915`)
                if is_mjd {
                    let jump_dist = mod_type
                        .default_attributes
                        .get(&2064)
                        .copied()
                        .unwrap_or(100_000.0);
                    mjd_jump_distance_m = Some(jump_dist);
                    let mjd_sig_pct = mod_state.get_attribute(973, &ctx.sde);
                    if mjd_sig_pct > 0.0 {
                        hic_sig_multiplier = Some(1.0 + mjd_sig_pct / 100.0);
                    }
                }

                // Warp Disruptor / Warp Scrambler (`group_id == 52`)
                if mod_type.group_id == 52 {
                    let Some(target_id) = target_id_opt else {
                        return Some(Err(BridgeRpcError {
                            code: "TargetRequired".into(),
                            message: "Warp disruptor/scrambler requires a locked target".into(),
                        }));
                    };
                    let mut max_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
                    if max_range <= 0.0 {
                        max_range = mod_type.default_attributes.get(&54).copied().unwrap_or(20_000.0);
                    }
                    if method == "Overload" {
                        let ov_bonus = mod_type.default_attributes.get(&1208).copied().unwrap_or(20.0);
                        max_range *= 1.0 + ov_bonus / 100.0;
                    }
                    if let (Ok(Some((_, ship_ball))), Ok(Some((_, target_ball)))) = (
                        sys.get_ball_snapshot(ship_id).await,
                        sys.get_ball_snapshot(target_id).await,
                    ) {
                        let dist = (ship_ball.position - target_ball.position).length();
                        if dist > max_range {
                            return Some(Err(BridgeRpcError {
                                code: "TargetOutOfRange".into(),
                                message: format!(
                                    "Target {target_id} is at {dist:.1} m, exceeding module optimal range {max_range:.1} m"
                                ),
                            }));
                        }
                    }
                    let scram_eval = eve_dogma::eval_warp_scrambler_effects(&[&mod_state], &[0.0], &ctx.sde);
                    warp_scramble_points = scram_eval.scramble_strength.max(1);
                    disables_target_mwd_mjd = scram_eval.blocks_mwd;
                }

                // SDE attribute `3265` (`activationRequiresActiveIndustrialCore`, e.g. Shipboard Compressors `group_id == 4174`):
                // require an active Industrial Core (`group_id == 515`) fitted on the ship (`docs/eve-wiki/Compression.md:39`).
                if mod_type.default_attributes.get(&3265).copied().unwrap_or(0.0) > 0.0 {
                    let has_active_core_in_fit = fit.modules.values().any(|fm| {
                        (27..=34).contains(&fm.flag_id)
                            && matches!(
                                fm.state,
                                eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                            )
                            && ctx.sde.types.get(&fm.type_id).is_some_and(|t| t.group_id == 515)
                    });
                    let has_active_core_in_db = ctx.db.with_snapshot(|snap| {
                        snap.items.values().any(|it| {
                            it.location_id == ship_id
                                && (27..=34).contains(&(it.flag as u16))
                                && snap.module_active_states.contains(&it.item_id)
                                && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 515)
                        })
                    });
                    if !has_active_core_in_fit && !has_active_core_in_db {
                        return Some(Err(BridgeRpcError {
                            code: "IndustrialCoreNotActive".into(),
                            message: "Module requires an active Industrial Core (attribute 3265 activationRequiresActiveIndustrialCore)".into(),
                        }));
                    }
                }

                // Heavy Interdictor Warp Disruption Field Generator (`group_id == 899`, e.g., `28654` T1, `4248` T2)
                if mod_type.group_id == 899 {
                    let ship_group_id = ctx
                        .sde
                        .types
                        .get(&fit.ship.type_id)
                        .map(|t| t.group_id)
                        .unwrap_or(0);
                    if ship_group_id != 894 {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidShipGroup".into(),
                            message: "Warp Disruption Field Generator can only be activated on Heavy Interdiction Cruisers (group 894)".into(),
                        }));
                    }
                    let loaded_script_id = m.charge_type_id.filter(|&cid| {
                        cid == 29003
                            || cid == 45010
                            || ctx.sde.types.get(&cid).is_some_and(|t| t.group_id == 908)
                    });
                    let base_wdfg_range = mod_type
                        .default_attributes
                        .get(&103)
                        .copied()
                        .unwrap_or(16_000.0);

                    if let Some(script_tid) = loaded_script_id {
                        // Scripted mode: Focused Warp Disruption Script (29003) or Focused Warp Scrambling Script (45010)
                        let Some(target_id) = target_id_opt else {
                            return Some(Err(BridgeRpcError {
                                code: "TargetRequired".into(),
                                message: "Scripted Warp Disruption Field Generator requires a locked target".into(),
                            }));
                        };
                        let range_mult = if script_tid == 29003 {
                            1.50 // +50% optimal range -> 24km (T1) / 30km (T2)
                        } else if script_tid == 45010 {
                            1.20 // +20% optimal range -> 19.2km (T1) / 24km (T2)
                        } else {
                            1.50
                        };
                        let ov_mult = if method == "Overload" { 1.20 } else { 1.0 };
                        let scripted_max_range = base_wdfg_range * range_mult * ov_mult;
                        if let (Ok(Some((_, ship_ball))), Ok(Some((_, target_ball)))) = (
                            sys.get_ball_snapshot(ship_id).await,
                            sys.get_ball_snapshot(target_id).await,
                        ) {
                            let dist = (ship_ball.position - target_ball.position).length();
                            if dist > scripted_max_range {
                                return Some(Err(BridgeRpcError {
                                    code: "TargetOutOfRange".into(),
                                    message: format!(
                                        "Target {target_id} is at {dist:.1} m, exceeding scripted WDFG range {scripted_max_range:.1} m"
                                    ),
                                }));
                            }
                        }
                        // Infinite point (100 warp scramble strength) + MWD/MJD suppression, no bubble
                        warp_scramble_points = 100;
                        disables_target_mwd_mjd = true;
                        hic_bubble_radius_m = None;
                    } else {
                        // Unscripted mode: AoE Warp Disruption Bubble + HIC self-penalties
                        hic_bubble_radius_m = Some(base_wdfg_range);
                        hic_velocity_multiplier = Some(0.40);
                        let sig_bonus_pct = mod_type
                            .default_attributes
                            .get(&554)
                            .copied()
                            .unwrap_or(50.0);
                        hic_sig_multiplier = Some(1.0 + sig_bonus_pct / 100.0);
                        let mass_add = mod_type
                            .default_attributes
                            .get(&796)
                            .copied()
                            .unwrap_or(80_000_000.0);
                        hic_mass_addition = Some(mass_add);
                    }
                }

                // Warp Core Stabilizer (`group_id == 315`, e.g., `Warp Core Stabilizer I` `10998`)
                if mod_type.group_id == 315 {
                    let raw_wcs = mod_type
                        .default_attributes
                        .get(&105)
                        .copied()
                        .unwrap_or(-2.0);
                    warp_core_stab_bonus = (-raw_wcs).round().max(1.0) as i32;
                }

                // Weapon Disruptor (`group_id == 291`: Tracking Disruptor 2108/2109 & Guidance Disruptor 37543/37546),
                // Remote Sensor Dampener (`group_id == 208`), Target Painter (`group_id == 379`), Stasis Webifier (`group_id == 65`)
                if matches!(mod_type.group_id, 291 | 208 | 379 | 65) {
                    let Some(target_id) = target_id_opt else {
                        return Some(Err(BridgeRpcError {
                            code: "TargetRequired".into(),
                            message: "EWAR module requires a locked target".into(),
                        }));
                    };
                    let mut distance_m = kwargs
                        .get("distance")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    if distance_m <= 0.0 {
                        if let (Ok(Some((_, ship_ball))), Ok(Some((_, target_ball)))) = (
                            sys.get_ball_snapshot(ship_id).await,
                            sys.get_ball_snapshot(target_id).await,
                        ) {
                            distance_m = (ship_ball.position - target_ball.position).length();
                        }
                    }
                    if mod_type.group_id == 65 {
                        let mut max_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
                        if max_range <= 0.0 {
                            max_range = mod_type.default_attributes.get(&54).copied().unwrap_or(10_000.0);
                        }
                        if distance_m > max_range && max_range > 0.0 {
                            return Some(Err(BridgeRpcError {
                                code: "TargetOutOfRange".into(),
                                message: format!(
                                    "Target {target_id} is at {distance_m:.1} m, exceeding webifier range {max_range:.1} m"
                                ),
                            }));
                        }
                    }
                    ctx.upsert_projected_ewar_on_ship(
                        target_id,
                        eve_dogma::ProjectedEwarEffect {
                            source_module_id: module_id,
                            source_module_type_id: m.type_id,
                            charge_type_id: m.charge_type_id,
                            distance: distance_m,
                            overloaded: method == "Overload",
                            effective_module: mod_state.clone(),
                        },
                    );
                }

            let duration_ms = {
                let d = mod_state.get_attribute(attr_ids::DURATION, &ctx.sde);
                if d > 0.0 {
                    d
                } else {
                    let d_high = mod_state.get_attribute(3115, &ctx.sde);
                    if d_high > 0.0 {
                        d_high
                    } else {
                        mod_state.get_attribute(attr_ids::SPEED, &ctx.sde)
                    }
                }
            };
            let duration_s = if duration_ms > 0.0 { duration_ms / 1000.0 } else { 1.0 };
            let cap_need = mod_state.get_attribute(attr_ids::CAPACITOR_NEED, &ctx.sde);
            let speed_factor = mod_state.get_attribute(attr_ids::SPEED_FACTOR, &ctx.sde);
            let speed_boost_factor = if speed_factor > 0.0 { speed_factor / 100.0 } else { 0.0 };

            let raw_shield = mod_state.get_attribute(68, &ctx.sde);
            let raw_armor = mod_state.get_attribute(84, &ctx.sde);
            let overload_mult = if method == "Overload" { 1.20 } else { 1.0 };
            let shield_boost_amount = raw_shield * overload_mult;
            let armor_repair_amount = raw_armor * overload_mult;

            let react_delay_ms = mod_state.get_attribute(669, &ctx.sde);
            let reactivation_delay_s = if react_delay_ms > 0.0 { react_delay_ms / 1000.0 } else { 0.0 };

            let rack_idx = match m.flag_id {
                27..=34 => 0,
                19..=26 => 1,
                _ => 2,
            };
            let heat_damage_per_cycle = {
                let hd = mod_state.get_attribute(1211, &ctx.sde);
                if hd > 0.0 { hd } else { 3.4 }
            };

            let linked_turrets: Vec<i64> = ctx.db.with_snapshot(|snap| {
                if let Some(bank) = snap.weapon_banks.get(&ship_id).and_then(|b| b.get(&module_id)) {
                    bank.clone()
                } else if matches!(m.flag_id, 27..=34) {
                    vec![module_id]
                } else {
                    Vec::new()
                }
            });

            let mut active_burst_charge_tid = m.charge_type_id;
            if !linked_turrets.is_empty() && mod_type.group_id != 899 {
                ctx.db.with_mut_snapshot(|snap| {
                    for &tid in &linked_turrets {
                        let tflag = snap.items.get(&tid).map(|i| i.flag);
                        if let Some(tf) = tflag {
                            if let Some(charge_item) = snap.items.values_mut().find(|i| {
                                i.location_id == ship_id
                                    && i.flag == tf
                                    && i.item_id != tid
                                    && i.quantity > 0
                                    && ctx.sde.types.get(&i.type_id).is_none_or(|t| t.group_id != 908)
                            }) {
                                active_burst_charge_tid = Some(charge_item.type_id);
                                if mod_type.group_id != 1770 || charge_item.quantity > 1 {
                                    charge_item.quantity = charge_item.quantity.saturating_sub(1);
                                }
                            } else if mod_type.group_id == 1770 && active_burst_charge_tid.is_none() {
                                if let Some(cargo_charge) = snap.items.values_mut().find(|i| {
                                    i.location_id == ship_id
                                        && matches!(i.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedAmmoHold)
                                        && i.quantity > 0
                                        && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 1771)
                                }) {
                                    active_burst_charge_tid = Some(cargo_charge.type_id);
                                    if cargo_charge.quantity > 1 {
                                        cargo_charge.quantity = cargo_charge.quantity.saturating_sub(1);
                                    }
                                }
                            }
                        }
                    }
                });
            }

            let cap_drain_target = mod_state.get_attribute(97, &ctx.sde);
            let cap_transfer_target = mod_state.get_attribute(90, &ctx.sde);
            let repeat = if is_mjd || grants_interdiction_nullification {
                0
            } else {
                repeat
            };
            let effective_max_capacitor = if mod_type.default_attributes.contains_key(&147) {
                Some(fit.ship.get_attribute(attr_ids::CAPACITOR_CAPACITY, &ctx.sde))
            } else {
                None
            };

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
                ammo_type_id: active_burst_charge_tid,
                damage_vector: eve_destiny::DamageVector::new(20.0, 20.0, 20.0, 20.0),
                cap_drain_target,
                cap_transfer_target,
                shield_boost_amount,
                armor_repair_amount,
                is_overloaded: method == "Overload",
                rack_idx,
                heat_damage_per_cycle,
                reactivation_delay_s,
                is_mwd_or_mjd,
                warp_scramble_points,
                disables_target_mwd_mjd,
                warp_core_stab_bonus,
                hic_bubble_radius_m,
                hic_velocity_multiplier,
                hic_sig_multiplier,
                hic_mass_addition,
                original_signature_radius: None,
                original_mass: None,
                mjd_jump_distance_m,
                grants_interdiction_nullification,
                effective_max_capacitor,
            };

            if let Err(e) = sys.add_module_cycle(cycle).await {
                let code = if e.contains("WarpScrambled") {
                    "WarpScrambled"
                } else {
                    "NotEnoughCapacitor"
                };
                return Some(Err(BridgeRpcError {
                    code: code.into(),
                    message: e,
                }));
            }

            // Targeted ECM (`group_id == 201`, e.g. `1957` Multispectral ECM I, `2567` Multispectral ECM II)
            if mod_type.group_id == 201 {
                let Some(target_id) = target_id_opt else {
                    let _ = sys.remove_module_cycle(ship_id, module_id).await;
                    return Some(Err(BridgeRpcError {
                        code: "TargetRequired".into(),
                        message: "ECM module requires a locked target".into(),
                    }));
                };
                let mut distance_m = 0.0;
                if let (Ok(Some((_, ship_ball))), Ok(Some((_, target_ball)))) = (
                    sys.get_ball_snapshot(ship_id).await,
                    sys.get_ball_snapshot(target_id).await,
                ) {
                    distance_m = (ship_ball.position - target_ball.position).length();
                }
                let target_type_id_opt = ctx
                    .db
                    .with_snapshot(|snap| snap.items.get(&target_id).map(|i| (i.owner_id, i.type_id)));
                let mut best_sensor_idx = 0usize;
                let mut target_sensor_strength = 0.0;
                if let Some((t_owner, t_tid)) = target_type_id_opt {
                    if let Ok(t_fit) = ctx.get_or_rebuild_ship_fitting(t_owner, target_id) {
                        for (idx, &s_attr) in [208u16, 209, 210, 211].iter().enumerate() {
                            let val = t_fit.ship.get_attribute(s_attr, &ctx.sde);
                            if val > target_sensor_strength {
                                target_sensor_strength = val;
                                best_sensor_idx = idx;
                            }
                        }
                    } else if let Some(t_type) = ctx.sde.types.get(&t_tid) {
                        for (idx, &s_attr) in [208u16, 209, 210, 211].iter().enumerate() {
                            let val = t_type.default_attributes.get(&s_attr).copied().unwrap_or(0.0);
                            if val > target_sensor_strength {
                                target_sensor_strength = val;
                                best_sensor_idx = idx;
                            }
                        }
                    }
                } else if let Ok(Some((_, target_ball))) = sys.get_ball_snapshot(target_id).await {
                    if let Some(t_type) = ctx.sde.types.get(&target_ball.type_id) {
                        for (idx, &s_attr) in [208u16, 209, 210, 211].iter().enumerate() {
                            let val = t_type.default_attributes.get(&s_attr).copied().unwrap_or(0.0);
                            if val > target_sensor_strength {
                                target_sensor_strength = val;
                                best_sensor_idx = idx;
                            }
                        }
                    }
                }
                if target_sensor_strength <= 0.0 {
                    target_sensor_strength = 12.0;
                }
                let jam_attr_id = 238u16 + (best_sensor_idx as u16);
                let jam_strength = mod_state.get_attribute(jam_attr_id, &ctx.sde);
                let optimal_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
                let falloff_range = mod_state.get_attribute(attr_ids::FALLOFF, &ctx.sde);
                let jam_chance = eve_dogma::compute_ecm_jam_chance(
                    jam_strength,
                    target_sensor_strength,
                    distance_m,
                    optimal_range,
                    falloff_range,
                );
                let roll = mechanic_unit_f64(ctx);
                let jam_duration_s = if duration_s > 0.0 { duration_s } else { 20.0 };
                if roll < jam_chance && jam_chance > 0.0 {
                    let _ = sys.apply_ecm_jam(target_id, jam_duration_s).await;
                }
            }

            // Burst Jammer / ECM Burst (`group_id == 80 || group_id == 353`, e.g. `580` Burst Jammer I, `2117` Burst Jammer II)
            if mod_type.group_id == 80 || mod_type.group_id == 353 {
                let mut burst_range = mod_state.get_attribute(142, &ctx.sde);
                if burst_range <= 0.0 {
                    burst_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
                }
                if burst_range <= 0.0 {
                    burst_range = 10_000.0;
                }
                let jam_strengths = [
                    mod_state.get_attribute(238, &ctx.sde),
                    mod_state.get_attribute(239, &ctx.sde),
                    mod_state.get_attribute(240, &ctx.sde),
                    mod_state.get_attribute(241, &ctx.sde),
                ];
                let roll = mechanic_unit_f64(ctx);
                let jam_duration_s = if duration_s > 0.0 { duration_s } else { 20.0 };
                let _ = sys
                    .apply_ecm_burst(ship_id, burst_range, jam_strengths, jam_duration_s, roll)
                    .await;
            }
            ctx.db.with_mut_snapshot(|snap| {
                if snap.items.contains_key(&module_id) {
                    snap.module_active_states.insert(module_id);
                    if let Some(cid) = active_burst_charge_tid {
                        snap.module_active_charges.insert(module_id, cid);
                    }
                }
            });
            let _ = fit.activate_module(module_id, method == "Overload", &ctx.sde);
            if let Some(cid) = active_burst_charge_tid {
                if let Some(fm) = fit.modules.get_mut(&module_id) {
                    fm.charge_type_id = Some(cid);
                    fm.charge_quantity = fm.charge_quantity.max(1);
                }
            }
            ctx.store_ship_fitting(ship_id, fit.clone());

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
                let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("overloaded", PyRep::Bool(true)),
                    ("effectiveMaxVelocity", PyRep::Real(max_vel)),
                ])))
            } else if mod_type.group_id == 899 {
                let weapon_timer_s = mod_type
                    .default_attributes
                    .get(&2037)
                    .copied()
                    .unwrap_or(60_000.0)
                    / 1000.0;
                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("active", PyRep::Bool(true)),
                    ("scripted", PyRep::Bool(hic_bubble_radius_m.is_none())),
                    (
                        "bubbleRadius",
                        hic_bubble_radius_m.map(PyRep::Real).unwrap_or(PyRep::None),
                    ),
                    ("warpScrambleStrength", PyRep::Integer(warp_scramble_points as i64)),
                    ("disallowAssistance", PyRep::Bool(hic_bubble_radius_m.is_some())),
                    ("weaponTimerSeconds", PyRep::Real(weapon_timer_s)),
                ])))
            } else if matches!(mod_type.group_id, 515 | 1770 | 4174) {
                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("active", PyRep::Bool(true)),
                    (
                        "chargeTypeID",
                        active_burst_charge_tid
                            .map(|c| PyRep::Integer(c as i64))
                            .unwrap_or(PyRep::None),
                    ),
                ])))
            } else if mod_type.group_id == 49 || mod_type.default_attributes.contains_key(&197) {
                let mut scan_range = mod_state.get_attribute(197, &ctx.sde);
                if scan_range <= 0.0 {
                    scan_range = mod_type
                        .default_attributes
                        .get(&197)
                        .copied()
                        .or_else(|| mod_type.default_attributes.get(&54).copied())
                        .unwrap_or(15_000.0);
                }
                let mut ship_pos = DVec3::ZERO;
                if let Ok(Some((_, ship_ball))) = sys.get_ball_snapshot(ship_id).await {
                    ship_pos = ship_ball.position;
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
                        let type_name = ctx
                            .sde
                            .types
                            .get(&ast.type_id)
                            .map(|t| t.name.clone())
                            .unwrap_or_else(|| format!("Asteroid {}", ast.type_id));
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
                    ("moduleID", PyRep::Integer(module_id)),
                    ("active", PyRep::Bool(true)),
                    ("scanRange", PyRep::Real(scan_range)),
                    ("count", PyRep::Integer(scanned_asteroids.len() as i64)),
                    ("asteroidCount", PyRep::Integer(scanned_asteroids.len() as i64)),
                    ("asteroids", PyRep::List(scanned_asteroids)),
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
                    .or_else(|| kwargs.get("target").and_then(|v| v.as_i64()));

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

                let (
                    mining_lvl,
                    astro_lvl,
                    ice_lvl,
                    mining_frig_lvl,
                    expedition_frig_lvl,
                    barge_lvl,
                    exhumer_lvl,
                    mining_destroyer_lvl,
                    spec_lvl,
                    implant_mult,
                ) = ctx.db.with_snapshot(|snap| {
                    let sk = snap.skills.get(&char_id);
                    let m = sk.and_then(|m| m.get(&3386)).map(|s| s.level).unwrap_or(0);
                    let a = sk.and_then(|m| m.get(&3410)).map(|s| s.level).unwrap_or(0);
                    let i = sk.and_then(|m| m.get(&16281)).map(|s| s.level).unwrap_or(0);
                    let mf = sk.and_then(|m| m.get(&32918)).map(|s| s.level).unwrap_or(0);
                    let ef = sk.and_then(|m| m.get(&33856)).map(|s| s.level).unwrap_or(0);
                    let b = sk.and_then(|m| m.get(&17940)).map(|s| s.level).unwrap_or(0);
                    let e = sk.and_then(|m| m.get(&22551)).map(|s| s.level).unwrap_or(0);
                    let md = sk.and_then(|m| m.get(&89238)).map(|s| s.level).unwrap_or(0);
                    let mut sp = 0u8;
                    for sid in [60377_u32, 60378, 60379, 60380, 60381, 46152, 46153, 46154, 46155, 12195, 12196] {
                        let lvl = sk.and_then(|m| m.get(&sid)).map(|s| s.level).unwrap_or(0);
                        sp = sp.max(lvl);
                    }
                    let mut imp_mult = 1.0;
                    if let Some(imps) = snap.active_implants.get(&char_id) {
                        for &imp_tid in imps {
                            if let Some(imp_type) = ctx.sde.types.get(&imp_tid) {
                                if let Some(&bonus) = imp_type.default_attributes.get(&434) {
                                    imp_mult *= 1.0 + bonus / 100.0;
                                }
                            }
                        }
                    }
                    (m, a, i, mf, ef, b, e, md, sp, imp_mult)
                });

                let ship_item_res = ctx.db.get_item(ship_id);
                let ship_type = ship_item_res.as_ref().and_then(|si| ctx.sde.types.get(&si.type_id));
                let mut ship_mining_mult = 1.0;
                let mut ship_gas_mult = 1.0;
                let mut ship_ice_duration_mult = 1.0;
                let mut ship_laser_duration_mult = 1.0;
                let mut ship_gas_duration_mult = 1.0;
                let mut ship_crit_chance_mult = 1.0;
                if let Some(st) = ship_type {
                    // Role bonuses from SDE ship dogma attributes
                    if let Some(&role_ore_pct) = st.default_attributes.get(&3177) {
                        ship_mining_mult *= 1.0 + role_ore_pct / 100.0;
                    } else if let Some(&role_ore_mult) = st.default_attributes.get(&207) {
                        if role_ore_mult > 0.0 {
                            ship_mining_mult *= role_ore_mult;
                        }
                    }
                    if let Some(&destroyer_role_pct) = st.default_attributes.get(&5986) {
                        ship_mining_mult *= 1.0 + destroyer_role_pct / 100.0;
                    }
                    if let Some(&legacy_ore_pct) = st.default_attributes.get(&1178) {
                        ship_mining_mult *= 1.0 + legacy_ore_pct / 100.0;
                    }
                    if let Some(&role_gas_pct) = st.default_attributes.get(&3239) {
                        ship_gas_mult *= 1.0 + role_gas_pct / 100.0;
                    } else if let Some(&legacy_gas_pct) = st.default_attributes.get(&1200) {
                        ship_gas_mult *= 1.0 + legacy_gas_pct / 100.0;
                    }
                    if let Some(&role_ore_dur_pct) = st.default_attributes.get(&3230) {
                        ship_laser_duration_mult *= (1.0 + role_ore_dur_pct / 100.0).max(0.1);
                    }
                    if let Some(&role_ice_dur_pct) = st.default_attributes.get(&3178) {
                        ship_ice_duration_mult *= (1.0 + role_ice_dur_pct / 100.0).max(0.1);
                    }
                    if let Some(&role_gas_dur_pct) = st.default_attributes.get(&3225) {
                        ship_gas_duration_mult *= (1.0 + role_gas_dur_pct / 100.0).max(0.1);
                    }
                    if let Some(&destroyer_gas_dur_pct) = st.default_attributes.get(&5952) {
                        ship_gas_duration_mult *= (1.0 + destroyer_gas_dur_pct / 100.0).max(0.1);
                    }
                    if let Some(&role_crit_pct) = st.default_attributes.get(&6048) {
                        ship_crit_chance_mult *= 1.0 + role_crit_pct / 100.0;
                    }
                    if let Some(&ice_crit_pct) = st.default_attributes.get(&6062) {
                        ship_crit_chance_mult *= 1.0 + ice_crit_pct / 100.0;
                    }

                    // Per-skill-level bonuses from SDE ship dogma attributes
                    if let Some(&frig_ore_pct) = st.default_attributes.get(&1842) {
                        ship_mining_mult *= 1.0 + (frig_ore_pct / 100.0) * (mining_frig_lvl as f64);
                    }
                    if let Some(&frig_gas_dur_pct) = st.default_attributes.get(&3237) {
                        ship_gas_duration_mult *=
                            (1.0 + (frig_gas_dur_pct / 100.0) * (mining_frig_lvl as f64)).max(0.1);
                    }
                    if let Some(&frig_ice_dur_pct) = st.default_attributes.get(&3240) {
                        ship_ice_duration_mult *=
                            (1.0 + (frig_ice_dur_pct / 100.0) * (mining_frig_lvl as f64)).max(0.1);
                    }
                    if let Some(&exp_ore_pct) = st.default_attributes.get(&3191) {
                        ship_mining_mult *=
                            1.0 + (exp_ore_pct / 100.0) * (expedition_frig_lvl as f64);
                    }
                    if let Some(&exp_ice_dur_pct) = st.default_attributes.get(&3167) {
                        ship_ice_duration_mult *=
                            (1.0 + (exp_ice_dur_pct / 100.0) * (expedition_frig_lvl as f64))
                                .max(0.1);
                    }
                    if let Some(&barge_ore_pct) = st.default_attributes.get(&3181) {
                        ship_mining_mult *= 1.0 + (barge_ore_pct / 100.0) * (barge_lvl as f64);
                    }
                    if let Some(&barge_ice_dur_pct) = st.default_attributes.get(&3182) {
                        ship_ice_duration_mult *=
                            (1.0 + (barge_ice_dur_pct / 100.0) * (barge_lvl as f64)).max(0.1);
                    }
                    if let Some(&barge_gas_dur_pct) = st.default_attributes.get(&3183) {
                        ship_gas_duration_mult *=
                            (1.0 + (barge_gas_dur_pct / 100.0) * (barge_lvl as f64)).max(0.1);
                    }
                    if let Some(&exh_ore_pct) = st.default_attributes.get(&3197) {
                        ship_mining_mult *= 1.0 + (exh_ore_pct / 100.0) * (exhumer_lvl as f64);
                    }
                    if let Some(&exh_ore_dur_pct) = st.default_attributes.get(&3193) {
                        ship_laser_duration_mult *=
                            (1.0 + (exh_ore_dur_pct / 100.0) * (exhumer_lvl as f64)).max(0.1);
                    }
                    if let Some(&exh_ice_dur_pct) = st.default_attributes.get(&3194) {
                        ship_ice_duration_mult *=
                            (1.0 + (exh_ice_dur_pct / 100.0) * (exhumer_lvl as f64)).max(0.1);
                    }
                    if let Some(&exh_gas_dur_pct) = st.default_attributes.get(&3226) {
                        ship_gas_duration_mult *=
                            (1.0 + (exh_gas_dur_pct / 100.0) * (exhumer_lvl as f64)).max(0.1);
                    }
                    if let Some(&dest_ore_pct) = st.default_attributes.get(&5820) {
                        ship_mining_mult *=
                            1.0 + (dest_ore_pct / 100.0) * (mining_destroyer_lvl as f64);
                    }
                }

                // Mining Laser Upgrades (`group_id == 546`, e.g., `22542` MLU I, `28576` MLU II) with EVE Dogma stacking penalty (`feat:mining.mining-upgrades`)
                let mut mlu_bonuses: Vec<f64> = Vec::new();
                let mut ice_cycle_bonus = 0.0;
                for item in &items {
                    if item.location_id == ship_id && (item.flag as u16) >= 11 && (item.flag as u16) <= 34 {
                        let is_online = fit
                            .modules
                            .get(&item.item_id)
                            .map(|m| m.state != eve_dogma::ModuleState::Offline)
                            .unwrap_or(true);
                        if !is_online {
                            continue;
                        }
                        if let Some(it) = ctx.sde.types.get(&item.type_id) {
                            if it.group_id == 546 {
                                if let Some(&b) = it.default_attributes.get(&434) {
                                    mlu_bonuses.push(b / 100.0);
                                }
                                if let Some(&b) = it.default_attributes.get(&780) {
                                    ice_cycle_bonus += b / 100.0;
                                }
                            } else if item.type_id == 22291 {
                                mlu_bonuses.push(0.09);
                            }
                        }
                    }
                }
                mlu_bonuses.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
                let mut mlu_mult = 1.0;
                for (i, &b) in mlu_bonuses.iter().enumerate() {
                    mlu_mult *= 1.0 + b * eve_dogma::stacking_penalty_factor(i);
                }

                let crystal_item_opt = items.iter().find(|i| {
                    i.location_id == ship_id
                        && i.flag == mod_item.flag
                        && ctx.sde.types.get(&i.type_id).map(|t| t.group_id == 482 || t.group_id == 663).unwrap_or(false)
                });
                let mut crystal_yield_mult = 1.0;
                let mut crystal_base_mult = 1.0;
                let mut crystal_matched = false;
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
                        crystal_matched = crystal_matches;
                        if crystal_matches {
                            let mult = c_type.default_attributes.get(&782).copied().unwrap_or(1.5);
                            crystal_base_mult = mult;
                            crystal_yield_mult = mult * (1.0 + 0.02 * (spec_lvl as f64));
                        } else {
                            crystal_base_mult = 0.25;
                            crystal_yield_mult = 0.25;
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

                // Evaluate active Fleet Mining Foreman Burst (`group_id == 1770`, charge `group_id == 1771`) in same solar system (`feat:mining.foreman-bursts`)
                let fleet_boost_active = ctx.db.with_snapshot(|snap| {
                    let mut candidate_chars = vec![char_id];
                    for fl in snap.fleets.values() {
                        if fl.members.contains(&char_id) {
                            for &m_id in &fl.members {
                                if !candidate_chars.contains(&m_id) {
                                    candidate_chars.push(m_id);
                                }
                            }
                        }
                    }
                    for cand_id in candidate_chars {
                        let Some(ch) = snap.characters.get(&cand_id) else {
                            continue;
                        };
                        if ch.solar_system_id != sys_id_val || ch.station_id.is_some() {
                            continue;
                        }
                        let booster_ship_id = ch.active_ship_id;
                        let has_active_foreman_burst = snap.items.values().any(|it| {
                            if it.location_id != booster_ship_id
                                || !(27..=34).contains(&(it.flag as u16))
                            {
                                return false;
                            }
                            let is_burst_mod = ctx
                                .sde
                                .types
                                .get(&it.type_id)
                                .is_some_and(|t| t.group_id == 1770);
                            if !is_burst_mod {
                                return false;
                            }
                            let is_active = snap.module_active_states.contains(&it.item_id);
                            if !is_active {
                                return false;
                            }
                            let has_charge = snap.module_active_charges.contains_key(&it.item_id)
                                || snap.items.values().any(|ch_it| {
                                    ch_it.location_id == booster_ship_id
                                        && (ch_it.flag == it.flag
                                            || ch_it.flag == InventoryFlag::Cargo
                                            || ch_it.flag == InventoryFlag::SpecializedAmmoHold)
                                        && ch_it.quantity > 0
                                        && ctx
                                            .sde
                                            .types
                                            .get(&ch_it.type_id)
                                            .is_some_and(|t| t.group_id == 1771)
                                });
                            has_charge
                        });
                        if has_active_foreman_burst {
                            return true;
                        }
                    }
                    false
                });
                let fleet_duration_mult = if fleet_boost_active { 0.85 } else { 1.0 };
                let fleet_waste_mult = if fleet_boost_active { 0.70 } else { 1.0 };

                // Probabilistic mining critical hit check against SDE `miningCritChance` (`5967`) (`feat:mining.mining-crits`)
                let base_crit_chance = mod_item
                    .dynamic_attributes
                    .get(&5967)
                    .copied()
                    .or_else(|| mod_type.default_attributes.get(&5967).copied())
                    .unwrap_or(0.01);
                let crit_chance = (base_crit_chance * ship_crit_chance_mult).clamp(0.0, 1.0);
                let crit_bonus = mod_item
                    .dynamic_attributes
                    .get(&5969)
                    .copied()
                    .or_else(|| mod_type.default_attributes.get(&5969).copied())
                    .unwrap_or(2.0);
                {
                    let mut wcs = ctx
                        .weapon_cycle_state
                        .write()
                        .unwrap_or_else(|e| e.into_inner());
                    let entry = wcs.entry(module_id).or_insert((None, 0));
                    entry.1 = entry.1.saturating_add(1);
                }
                let is_crit = crit_chance > 0.0 && mechanic_unit_f64(ctx) < crit_chance;
                let crit_yield_mult = if is_crit { crit_bonus } else { 1.0 };

                let is_ice = mod_type.group_id == 464 && (mod_type.type_id == 16278 || mod_type.type_id == 22229 || mod_type.name.contains("Ice"));
                let is_gas = mod_type.group_id == 737 || mod_type.group_id == 4138;
                let (duration_ms, base_volume_yield) = if is_ice {
                    let duration = (base_duration_ms * (1.0 - 0.05 * (ice_lvl as f64) + ice_cycle_bonus) * ship_ice_duration_mult * fleet_duration_mult).max(1000.0);
                    (duration, base_amount)
                } else if is_gas {
                    (base_duration_ms * ship_gas_duration_mult * fleet_duration_mult, base_amount * ship_gas_mult)
                } else {
                    let skill_mult = (1.0 + 0.05 * (mining_lvl as f64)) * (1.0 + 0.05 * (astro_lvl as f64));
                    let vol = base_amount * skill_mult * implant_mult * ship_mining_mult * mlu_mult * crystal_yield_mult;
                    (base_duration_ms * ship_laser_duration_mult * fleet_duration_mult, vol)
                };

                let ore_unit_volume = ctx.sde.types.get(&asteroid.type_id).map(|t| t.volume).unwrap_or(0.1).max(0.001);
                let potential_units = ((base_volume_yield / ore_unit_volume).floor() as i64).max(1);

                let (actual_units, cycle_time_ms, depleted) = if asteroid.quantity < potential_units {
                    let actual = asteroid.quantity;
                    let partial_cycle = ((actual as f64) / (potential_units as f64) * duration_ms).round() as i64;
                    (actual, partial_cycle.max(1000), true)
                } else {
                    (potential_units, duration_ms as i64, asteroid.quantity == potential_units)
                };

                let bonus_crit_units = if is_crit {
                    ((actual_units as f64) * (crit_bonus - 1.0)).round().max(1.0) as i64
                } else {
                    0
                };

                let waste_prob = (mod_type.default_attributes.get(&3154).copied().unwrap_or(0.0) * fleet_waste_mult).clamp(0.0, 1.0);
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

                let total_cycle_units = actual_units + bonus_crit_units;
                let (rock_units_extracted, units_to_deposit, hold_full) = if max_fit_units < total_cycle_units {
                    let fit_capped = max_fit_units.max(0);
                    (fit_capped.min(actual_units), fit_capped, true)
                } else {
                    (actual_units, total_cycle_units, false)
                };

                let mut deposited_units_out = 0;
                let mut rock_depleted_out = depleted;
                let res: Result<(), eve_db::DbError> = ctx.db.try_with_mut_snapshot(|snap| {
                    if snap.items.contains_key(&module_id) {
                        snap.module_active_states.insert(module_id);
                    }
                    let total_removed_from_rock = rock_units_extracted + residue_units;
                    if let Some(ast) = snap.asteroids.get_mut(&target_id) {
                        ast.quantity = ast.quantity.saturating_sub(total_removed_from_rock);
                        rock_depleted_out = ast.quantity == 0;
                    }
                    if rock_depleted_out {
                        let mut newly_cleared_ids = Vec::new();
                        if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id_val) {
                            for anom in anoms.iter_mut() {
                                if !anom.cleared {
                                    let anom_pos = DVec3::from_array(anom.position);
                                    if anom_pos.distance(ast_pos) <= 50_000.0 {
                                        let any_remaining = snap.asteroids.values().any(|a| {
                                            a.solar_system_id == sys_id_val
                                                && a.quantity > 0
                                                && DVec3::new(
                                                    a.position.0,
                                                    a.position.1,
                                                    a.position.2,
                                                )
                                                .distance(anom_pos)
                                                    <= 50_000.0
                                        });
                                        if !any_remaining {
                                            anom.cleared = true;
                                            anom.remaining_volume = 0.0;
                                            newly_cleared_ids.push(anom.anomaly_id.clone());
                                        }
                                    }
                                }
                            }
                        }
                        if !newly_cleared_ids.is_empty() {
                            if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id_val) {
                                for s in sigs.iter_mut() {
                                    if newly_cleared_ids.contains(&s.signature_id) {
                                        s.collapsed = true;
                                        s.signal_strength = 0.0;
                                    }
                                }
                            }
                        }
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

                if deposited_units_out > 0 {
                    let _ = ctx.db.record_mining_ledger_entry(
                        char_id,
                        sys_id_val,
                        asteroid.type_id,
                        deposited_units_out,
                        ctx.now_unix_secs(),
                    );

                    // Guarded gas sites (wormhole reservoirs and guarded nebulae) spawn defenders; no guarded gas
                    // site is modelled yet (see `mining_sites::GAS_SITES`), so harvesting spawns no NPCs.
                }

                if rock_depleted_out {
                    let sys_handle = ctx.systems.get_or_spawn(sys_id_val);
                    sys_handle.try_remove_ball(target_id);
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
                let cpu_load = fit.ship.get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde);
                ctx.store_ship_fitting(ship_id, fit);

                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("targetID", PyRep::Integer(target_id)),
                    ("cycleTimeMs", PyRep::Integer(cycle_time_ms)),
                    ("yieldUnits", PyRep::Integer(deposited_units_out)),
                    ("normalUnits", PyRep::Integer(rock_units_extracted)),
                    ("bonusCritUnits", PyRep::Integer(bonus_crit_units)),
                    ("yieldVolume", PyRep::Real(deposited_units_out as f64 * ore_unit_volume)),
                    ("oreTypeID", PyRep::Integer(asteroid.type_id as i64)),
                    ("destinationFlag", PyRep::Integer(destination_flag as i64)),
                    ("flagID", PyRep::Integer(destination_flag as i64)),
                    ("depleted", PyRep::Bool(rock_depleted_out)),
                    ("holdFull", PyRep::Bool(hold_full)),
                    ("residueUnits", PyRep::Integer(residue_units)),
                    ("crystalMatched", PyRep::Bool(crystal_matched)),
                    ("crystalMultiplier", PyRep::Real(crystal_base_mult)),
                    ("crystalDamage", PyRep::Real(crystal_damage_inflicted)),
                    ("crystalDestroyed", PyRep::Bool(crystal_destroyed)),
                    ("implantYieldMultiplier", PyRep::Real(implant_mult)),
                    ("isCrit", PyRep::Bool(is_crit)),
                    ("critYieldMultiplier", PyRep::Real(crit_yield_mult)),
                    ("fleetBoostActive", PyRep::Bool(fleet_boost_active)),
                    ("mluCount", PyRep::Integer(mlu_bonuses.len() as i64)),
                    ("mluMultiplier", PyRep::Real(mlu_mult)),
                    ("cpuLoad", PyRep::Real(cpu_load)),
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
            ctx.remove_projected_ewar_by_module(module_id);

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

            ctx.db.with_mut_snapshot(|snap| {
                snap.module_active_states.remove(&module_id);
                snap.module_active_charges.remove(&module_id);
            });

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
            if let Some(m_entry) = fit.modules.get(&module_id) {
                let mod_state = fit.build_effective_module_state(module_id, m_entry.type_id, false, &ctx.sde);
                let react_delay_ms = mod_state.get_attribute(669, &ctx.sde);
                if react_delay_ms > 0.0 {
                    ctx.record_module_reactivation_delay(ship_id, module_id, react_delay_ms / 1000.0);
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
                ("asteroidCount", PyRep::Integer(scanned_asteroids.len() as i64)),
                ("asteroids", PyRep::List(scanned_asteroids)),
            ])))
        }

        ("charFittingMgr", "SaveFitting") | ("corpFittingMgr", "SaveFitting") | ("allianceFittingMgr", "SaveFitting") | ("charFittingMgr", "SaveManyFittings") | ("corpFittingMgr", "SaveManyFittings") | ("allianceFittingMgr", "SaveManyFittings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let default_owner = match service {
                "corpFittingMgr" => session.corporation_id.unwrap_or(char_id),
                "allianceFittingMgr" => session
                    .alliance_id
                    .or(session.corporation_id)
                    .unwrap_or(char_id),
                _ => char_id,
            };

            let parse_fit_items = |items_arg: Option<&PyRep>,
                                   ship_type_id: TypeId|
             -> Result<Vec<(TypeId, InventoryFlag, i32)>, BridgeRpcError> {
                if let Some(arg) = items_arg {
                    let entries = match arg {
                        PyRep::List(v) | PyRep::Tuple(v) => v,
                        _ => {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItems".into(),
                                message:
                                    "Fitting items must be a List or Tuple of (typeID, flagID, quantity)"
                                        .into(),
                            })
                        }
                    };
                    let mut out = Vec::with_capacity(entries.len());
                    for entry in entries {
                        let seq = match entry {
                            PyRep::Tuple(v) | PyRep::List(v) => v,
                            _ => {
                                return Err(BridgeRpcError {
                                    code: "InvalidFittingItem".into(),
                                    message:
                                        "Each fitting item must be a (typeID, flagID, quantity) tuple"
                                            .into(),
                                })
                            }
                        };
                        let Some(mod_type_id) =
                            seq.first().and_then(|v| v.as_i64()).filter(|&v| v > 0)
                        else {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: "Fitting item is missing a positive typeID".into(),
                            });
                        };
                        let Some(flag_raw) =
                            seq.get(1).and_then(|v| v.as_i64()).filter(|&v| v >= 0)
                        else {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: "Fitting item is missing a valid flagID".into(),
                            });
                        };
                        let qty_i64 = seq.get(2).and_then(|v| v.as_i64()).unwrap_or(1);
                        let Ok(qty) = i32::try_from(qty_i64) else {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: format!(
                                    "Fitting item quantity overflows i32 bounds, got {qty_i64}"
                                ),
                            });
                        };
                        if qty <= 0 {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingItem".into(),
                                message: format!(
                                    "Fitting item quantity must be positive, got {qty}"
                                ),
                            });
                        }
                        let flag_u16 = u16::try_from(flag_raw).ok();
                        let flag =
                            flag_u16.map(InventoryFlag::from_u16).unwrap_or(InventoryFlag::None);
                        let valid_flag = flag_u16
                            .map(|f| {
                                matches!(f, 5 | 11..=34 | 87 | 92..=94 | 125..=132 | 158)
                                    && (flag as u16) == f
                            })
                            .unwrap_or(false);
                        if !valid_flag {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingFlag".into(),
                                message: format!("Invalid fitting slot or bay flag {flag_raw}"),
                            });
                        }
                        out.push((mod_type_id as TypeId, flag, qty));
                    }
                    Ok(out)
                } else {
                    let char_items = ctx.db.get_items_for_character(char_id);
                    let active_ship_matches = session
                        .ship_id
                        .and_then(|sid| char_items.iter().find(|i| i.item_id == sid))
                        .map(|s| s.type_id == ship_type_id)
                        .unwrap_or(false);
                    if active_ship_matches {
                        let sid = session.ship_id.unwrap_or(0);
                        Ok(char_items
                            .into_iter()
                            .filter(|i| {
                                let is_mod_or_drone = ctx
                                    .sde
                                    .types
                                    .get(&i.type_id)
                                    .map(|t| {
                                        t.category_id == 7
                                            || t.category_id == 32
                                            || (i.flag as u16) == 87
                                    })
                                    .unwrap_or(false);
                                i.location_id == sid
                                    && is_mod_or_drone
                                    && matches!(i.flag as u16, 11..=34 | 87 | 92..=94 | 125..=132)
                            })
                            .map(|i| (i.type_id, i.flag, i.quantity))
                            .collect())
                    } else {
                        Ok(Vec::new())
                    }
                }
            };

            let validate_and_build_fit =
                |name: String,
                 raw_ship_type: i64,
                 description: String,
                 items_arg: Option<&PyRep>|
                 -> Result<
                    (String, TypeId, String, Vec<(TypeId, InventoryFlag, i32)>),
                    BridgeRpcError,
                > {
                    let ship_type_id = raw_ship_type as TypeId;
                    let is_valid_hull = ctx
                        .sde
                        .types
                        .get(&ship_type_id)
                        .map(|t| t.category_id == 6)
                        .unwrap_or(false);
                    if !is_valid_hull {
                        return Err(BridgeRpcError {
                            code: "InvalidShipType".into(),
                            message: format!("Type {ship_type_id} is not a valid ship hull"),
                        });
                    }
                    let parsed_items = parse_fit_items(items_arg, ship_type_id)?;
                    let all_skills_lvl5: HashMap<TypeId, u8> = ctx
                        .sde
                        .types
                        .values()
                        .filter(|t| t.category_id == 16)
                        .map(|t| (t.type_id, 5))
                        .collect();
                    let mut validator =
                        eve_dogma::ShipFittingEngine::from_sde(0, ship_type_id, &ctx.sde)
                            .with_character_skills(all_skills_lvl5);
                    for (idx, &(mod_type_id, flag, qty)) in parsed_items.iter().enumerate() {
                        let flag_u16 = flag as u16;
                        if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                            if qty != 1 {
                                return Err(BridgeRpcError {
                                    code: "InvalidFittingQuantity".into(),
                                    message: format!(
                                        "Fitting slot {flag_u16} requires quantity 1, got {qty}"
                                    ),
                                });
                            }
                            if let Err(e) = validator.fit_and_online_module(
                                (idx as i64) + 1,
                                mod_type_id,
                                flag_u16,
                                &ctx.sde,
                            ) {
                                return Err(BridgeRpcError {
                                    code: "InvalidFittingModule".into(),
                                    message: e,
                                });
                            }
                        } else if !ctx.sde.types.contains_key(&mod_type_id) {
                            return Err(BridgeRpcError {
                                code: "UnknownItemType".into(),
                                message: format!(
                                    "Unknown item type {mod_type_id} in saved fitting"
                                ),
                            });
                        }
                    }
                    Ok((name, ship_type_id, description, parsed_items))
                };

            let extract_fit_from_rep = |rep: &PyRep| -> Result<
                (String, TypeId, String, Vec<(TypeId, InventoryFlag, i32)>),
                BridgeRpcError,
            > {
                let name = rep
                    .dict_get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Saved Fitting")
                    .to_string();
                let Some(raw_ship_type) = rep.dict_get("shipTypeID").and_then(|v| v.as_i64())
                else {
                    return Err(BridgeRpcError {
                        code: "InvalidShipType".into(),
                        message: "Fitting object requires shipTypeID".into(),
                    });
                };
                let description = rep
                    .dict_get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let items_rep = rep.dict_get("fitData").or_else(|| rep.dict_get("items"));
                validate_and_build_fit(name, raw_ship_type, description, items_rep)
            };

            let mut owner_id = default_owner;
            let mut fits_to_save: Vec<(String, TypeId, String, Vec<(TypeId, InventoryFlag, i32)>)> =
                Vec::new();

            if method == "SaveManyFittings" {
                let fittings_arg = if let Some(oid) = args.first().and_then(|a| a.as_i64()) {
                    if oid > 0 {
                        owner_id = oid;
                    }
                    args.get(1)
                } else {
                    args.first()
                };
                let Some(f_arg) = fittings_arg else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "SaveManyFittings requires fittings collection".into(),
                    }));
                };
                match f_arg {
                    PyRep::Dict(entries) => {
                        for (_, fit_rep) in entries {
                            match extract_fit_from_rep(fit_rep) {
                                Ok(f) => fits_to_save.push(f),
                                Err(e) => return Some(Err(e)),
                            }
                        }
                    }
                    PyRep::List(list) | PyRep::Tuple(list) => {
                        for fit_rep in list {
                            match extract_fit_from_rep(fit_rep) {
                                Ok(f) => fits_to_save.push(f),
                                Err(e) => return Some(Err(e)),
                            }
                        }
                    }
                    _ => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message:
                                "SaveManyFittings requires a Dict or List of fitting objects"
                                    .into(),
                        }));
                    }
                }
            } else if let Some(oid) = args.first().and_then(|a| a.as_i64()) {
                // V24 client signature: SaveFitting(ownerID, fitting)
                if oid > 0 {
                    owner_id = oid;
                }
                let Some(fit_rep) = args.get(1) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "SaveFitting(ownerID, fitting) requires fitting object".into(),
                    }));
                };
                match extract_fit_from_rep(fit_rep) {
                    Ok(f) => fits_to_save.push(f),
                    Err(e) => return Some(Err(e)),
                }
            } else if let Some(fit_rep) = args
                .first()
                .filter(|a| a.dict_get("shipTypeID").is_some())
            {
                match extract_fit_from_rep(fit_rep) {
                    Ok(f) => fits_to_save.push(f),
                    Err(e) => return Some(Err(e)),
                }
            } else {
                // Legacy positional signature: SaveFitting(name, shipTypeID, description?, items?)
                let name = args
                    .first()
                    .and_then(|a| a.as_str())
                    .unwrap_or("Rifter Combat Fit")
                    .to_string();
                let Some(raw_ship_type) = args.get(1).and_then(|a| a.as_i64()) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidShipType".into(),
                        message: "SaveFitting requires shipTypeID".into(),
                    }));
                };
                let (description, items_arg) = match args.get(2) {
                    Some(PyRep::String(s)) => (s.clone(), args.get(3)),
                    Some(list_or_tuple @ (PyRep::List(_) | PyRep::Tuple(_))) => {
                        (format!("Saved via {service}"), Some(list_or_tuple))
                    }
                    _ => (format!("Saved via {service}"), args.get(3)),
                };
                match validate_and_build_fit(name, raw_ship_type, description, items_arg) {
                    Ok(f) => fits_to_save.push(f),
                    Err(e) => return Some(Err(e)),
                }
            }

            let is_corp_or_alliance = service == "corpFittingMgr"
                || service == "allianceFittingMgr"
                || owner_id != char_id;
            let max_fittings: usize = if is_corp_or_alliance { 300 } else { 250 };

            let save_res = ctx.db.try_with_mut_snapshot(|snap| {
                let current_count = snap
                    .saved_fittings
                    .values()
                    .filter(|f| f.owner_id == owner_id)
                    .count();
                if current_count + fits_to_save.len() > max_fittings {
                    return Err(BridgeRpcError {
                        code: "OwnerMaxFittings".into(),
                        message: format!(
                            "Owner {owner_id} has {current_count} fittings; saving {} more would exceed maximum of {max_fittings}",
                            fits_to_save.len()
                        ),
                    });
                }
                let mut saved_ids = Vec::with_capacity(fits_to_save.len());
                for (name, ship_type_id, description, parsed_items) in fits_to_save {
                    snap.next_entity_id += 1;
                    let id = snap.next_entity_id;
                    snap.saved_fittings.insert(
                        id,
                        SavedFittingRecord {
                            fitting_id: id,
                            owner_id,
                            name,
                            ship_type_id,
                            description,
                            items: parsed_items,
                        },
                    );
                    saved_ids.push(id);
                }
                Ok(saved_ids)
            });

            match save_res {
                Ok(saved_ids) => {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFittingsUpdated",
                        "charid",
                        vec![PyRep::Integer(owner_id)],
                    );
                    if method == "SaveManyFittings" {
                        Some(Ok(PyRep::List(
                            saved_ids.into_iter().map(PyRep::Integer).collect(),
                        )))
                    } else {
                        Some(Ok(PyRep::Integer(saved_ids.first().copied().unwrap_or(0))))
                    }
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("charFittingMgr", "GetFittings") | ("corpFittingMgr", "GetFittings") | ("allianceFittingMgr", "GetFittings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let default_owner = match service {
                "corpFittingMgr" => session.corporation_id.unwrap_or(char_id),
                "allianceFittingMgr" => session
                    .alliance_id
                    .or(session.corporation_id)
                    .unwrap_or(char_id),
                _ => char_id,
            };
            let owner_id = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&oid| oid > 0)
                .unwrap_or(default_owner);
            let rows = ctx.db.with_snapshot(|snap| {
                let mut fittings: Vec<_> = snap
                    .saved_fittings
                    .values()
                    .filter(|f| f.owner_id == owner_id)
                    .collect();
                fittings.sort_by_key(|f| f.fitting_id);
                fittings
                    .into_iter()
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

        ("charFittingMgr", "UpdateNameAndDescription") | ("corpFittingMgr", "UpdateNameAndDescription") | ("allianceFittingMgr", "UpdateNameAndDescription") | ("charFittingMgr", "UpdateFitting") | ("corpFittingMgr", "UpdateFitting") | ("allianceFittingMgr", "UpdateFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // Supports (fittingID, ownerID, name, description) or (fittingID, name, description)
            let fid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let (owner_filter, new_name, new_desc) = if args.len() >= 4 {
                (
                    args.get(1).and_then(|a| a.as_i64()),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                    args.get(3).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                )
            } else {
                (
                    None,
                    args.get(1).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                    args.get(2).and_then(|a| a.as_str()).unwrap_or("").to_string(),
                )
            };
            if new_name.trim().is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFittingName".into(),
                    message: "Fitting name cannot be empty".into(),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(fit) = snap.saved_fittings.get_mut(&fid) else {
                    return Err(BridgeRpcError {
                        code: "FittingNotFound".into(),
                        message: format!("Saved fitting {fid} not found"),
                    });
                };
                let allowed_owner = fit.owner_id == char_id
                    || Some(fit.owner_id) == session.corporation_id
                    || Some(fit.owner_id) == session.alliance_id;
                if !allowed_owner
                    || owner_filter.is_some_and(|oid| oid > 0 && oid != fit.owner_id)
                {
                    return Err(BridgeRpcError {
                        code: "FittingOwnershipViolation".into(),
                        message: format!("Not authorized to update fitting {fid}"),
                    });
                }
                fit.name = new_name.clone();
                fit.description = new_desc.clone();
                Ok(fit.owner_id)
            });
            match res {
                Ok(owner_id) => {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFittingsUpdated",
                        "charid",
                        vec![PyRep::Integer(owner_id)],
                    );
                    Some(Ok(PyRep::keyval([
                        ("fittingID", PyRep::Integer(fid)),
                        ("ownerID", PyRep::Integer(owner_id)),
                        ("name", PyRep::String(new_name)),
                        ("description", PyRep::String(new_desc)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("charFittingMgr", "DeleteFitting") | ("corpFittingMgr", "DeleteFitting") | ("allianceFittingMgr", "DeleteFitting") | ("charFittingMgr", "DeleteManyFittings") | ("corpFittingMgr", "DeleteManyFittings") | ("allianceFittingMgr", "DeleteManyFittings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let default_owner = match service {
                "corpFittingMgr" => session.corporation_id.unwrap_or(char_id),
                "allianceFittingMgr" => session
                    .alliance_id
                    .or(session.corporation_id)
                    .unwrap_or(char_id),
                _ => char_id,
            };
            if method == "DeleteManyFittings" {
                let (owner_filter, list_arg) = match (args.first(), args.get(1)) {
                    (Some(PyRep::Integer(oid)), Some(list @ (PyRep::List(_) | PyRep::Tuple(_)))) => {
                        (Some(*oid), Some(list))
                    }
                    (Some(list @ (PyRep::List(_) | PyRep::Tuple(_))), _) => (None, Some(list)),
                    _ => (None, None),
                };
                let target_ids: Vec<i64> = match list_arg {
                    Some(PyRep::List(v) | PyRep::Tuple(v)) => {
                        v.iter().filter_map(|x| x.as_i64()).collect()
                    }
                    _ => Vec::new(),
                };
                let deleted_ids = ctx
                    .db
                    .try_with_mut_snapshot(|snap| {
                        let mut removed = Vec::new();
                        for fid in target_ids {
                            let can_delete = snap.saved_fittings.get(&fid).is_some_and(|f| {
                                let owner_ok = f.owner_id == char_id
                                    || Some(f.owner_id) == session.corporation_id
                                    || Some(f.owner_id) == session.alliance_id;
                                owner_ok
                                    && owner_filter
                                        .map(|oid| oid <= 0 || oid == f.owner_id)
                                        .unwrap_or(true)
                            });
                            if can_delete {
                                snap.saved_fittings.remove(&fid);
                                removed.push(fid);
                            }
                        }
                        Ok::<_, std::convert::Infallible>(removed)
                    })
                    .unwrap();
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnFittingsUpdated",
                    "charid",
                    vec![PyRep::Integer(owner_filter.unwrap_or(default_owner))],
                );
                Some(Ok(PyRep::List(
                    deleted_ids.into_iter().map(PyRep::Integer).collect(),
                )))
            } else {
                // Supports DeleteFitting(ownerID, fittingID) and DeleteFitting(fittingID)
                let (owner_filter, fid) = if args.len() >= 2 {
                    (
                        args.first().and_then(|a| a.as_i64()),
                        args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                    )
                } else {
                    (None, args.first().and_then(|a| a.as_i64()).unwrap_or(0))
                };
                let deleted = ctx
                    .db
                    .try_with_mut_snapshot(|snap| {
                        let can_delete = snap.saved_fittings.get(&fid).is_some_and(|f| {
                            let owner_ok = f.owner_id == char_id
                                || Some(f.owner_id) == session.corporation_id
                                || Some(f.owner_id) == session.alliance_id;
                            owner_ok
                                && owner_filter
                                    .map(|oid| oid <= 0 || oid == f.owner_id)
                                    .unwrap_or(true)
                        });
                        if can_delete {
                            snap.saved_fittings.remove(&fid);
                            Ok::<_, std::convert::Infallible>(true)
                        } else {
                            Ok::<_, std::convert::Infallible>(false)
                        }
                    })
                    .unwrap();
                if deleted {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFittingsUpdated",
                        "charid",
                        vec![PyRep::Integer(owner_filter.unwrap_or(default_owner))],
                    );
                }
                Some(Ok(PyRep::Bool(deleted)))
            }
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
                let ref_val = compute_ship_insurance_reference_value(&ctx.sde, ship.type_id);
                let (cost, payout) = compute_insurance_quote(ref_val, level_fraction);
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
            let ref_val = compute_ship_insurance_reference_value(&ctx.sde, type_id);
            let (platinum_cost, platinum_payout) = compute_insurance_quote(ref_val, 1.0);
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
            let extract_ids_from_list = |list: &[PyRep]| -> Vec<i64> {
                list.iter()
                    .filter_map(|v| {
                        v.as_i64().or_else(|| match v {
                            PyRep::Tuple(t) | PyRep::List(t) => t.first().and_then(|x| x.as_i64()),
                            _ => None,
                        })
                    })
                    .collect()
            };
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::Integer(id)) => vec![*id],
                Some(PyRep::List(list) | PyRep::Tuple(list)) => extract_ids_from_list(list),
                Some(PyRep::Dict(entries)) => {
                    let mut ids = Vec::new();
                    for (_, val) in entries {
                        match val {
                            PyRep::List(list) | PyRep::Tuple(list) => {
                                ids.extend(extract_ids_from_list(list));
                            }
                            PyRep::Integer(id) => ids.push(*id),
                            _ => {}
                        }
                    }
                    ids
                }
                _ => vec![0],
            };
            if item_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: "RepackageItems requires at least one itemID".into(),
                }));
            }
            for &item_id in &item_ids {
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
                        code: "CantRepackageDamagedItem".into(),
                        message: format!(
                            "Item {item_id} has heat or module damage and must be repaired before repackaging"
                        ),
                    }));
                }
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let active_ship_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0);
                // First pass: validate all items before mutating
                for &item_id in &item_ids {
                    if item_id == active_ship_id {
                        return Err(BridgeRpcError {
                            code: "CannotRepackageActiveShip".into(),
                            message: "Cannot repackage your currently active ship".into(),
                        });
                    }
                    let Some(item) = snap.items.get(&item_id) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Item {item_id} not found"),
                        });
                    };
                    if item.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "OwnershipOrLocationViolation".into(),
                            message: format!(
                                "Item {item_id} must be owned by character {char_id} at station {station_id}"
                            ),
                        });
                    }
                    // Fitted item check (IsShipFittingFlag: 11..=34 | 92..=94 | 125..=132 | 158) before location_id check
                    if matches!(item.flag as u16, 11..=34 | 92..=94 | 125..=132 | 158) {
                        return Err(BridgeRpcError {
                            code: "CanNotUnassembleInThisItemLoc".into(),
                            message: format!(
                                "Cannot repackage item {item_id} while it is fitted in slot {:?}",
                                item.flag
                            ),
                        });
                    }
                    if item.location_id != station_id {
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
                    if snap.is_item_locked_in_contract_or_job(item_id) {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!(
                                "Item {item_id} is locked in an active contract or industry job"
                            ),
                        });
                    }
                    let has_contained_items = snap.items.values().any(|other| {
                        other.item_id != item_id
                            && other.location_id == item_id
                            && other.quantity > 0
                    });
                    if has_contained_items {
                        return Err(BridgeRpcError {
                            code: "CannotRepackageNonEmptyItem".into(),
                            message: format!(
                                "Cannot repackage item {item_id} while it contains fitted modules, rigs, or cargo"
                            ),
                        });
                    }
                    if item.is_blueprint_copy {
                        return Err(BridgeRpcError {
                            code: "CannotRepackageBlueprintCopy".into(),
                            message: format!("Blueprint copy {item_id} cannot be repackaged"),
                        });
                    }
                    let hull_or_armor_damage = item
                        .dynamic_attributes
                        .get(&attr_ids::DAMAGE)
                        .copied()
                        .unwrap_or(0.0)
                        > 0.0
                        || item
                            .dynamic_attributes
                            .get(&attr_ids::ARMOR_DAMAGE)
                            .copied()
                            .unwrap_or(0.0)
                            > 0.0;
                    if hull_or_armor_damage {
                        return Err(BridgeRpcError {
                            code: "CantRepackageDamagedItem".into(),
                            message: format!(
                                "Item {item_id} has structural or armor damage and must be repaired before repackaging"
                            ),
                        });
                    }
                    let has_non_damage_dyn_attrs = item.dynamic_attributes.keys().any(|&k| {
                        k != attr_ids::DAMAGE
                            && k != attr_ids::ARMOR_DAMAGE
                            && k != attr_ids::SHIELD_CHARGE
                    });
                    if item.source_mutaplasmid_id.is_some() || has_non_damage_dyn_attrs {
                        return Err(BridgeRpcError {
                            code: "CannotRepackageMutatedItem".into(),
                            message: format!("Abyssal mutated item {item_id} cannot be repackaged"),
                        });
                    }
                }
                let mut last_ver = 1_u64;
                for &item_id in &item_ids {
                    if let Some(item) = snap.items.get_mut(&item_id) {
                        item.singleton = false;
                        item.insured_payout_isk = 0.0;
                        item.version += 1;
                        last_ver = item.version;
                    }
                }
                Ok((item_ids[0], last_ver, item_ids.len()))
            });
            match res {
                Ok((first_id, ver, count)) => {
                    for &item_id in &item_ids {
                        ctx.invalidate_ship_fitting(item_id);
                    }
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(first_id)),
                        ("singleton", PyRep::Bool(false)),
                        ("version", PyRep::Integer(ver as i64)),
                        ("repackagedCount", PyRep::Integer(count as i64)),
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
            let first_item_id = match args.first() {
                Some(PyRep::Integer(id)) => Some(*id),
                Some(PyRep::List(list) | PyRep::Tuple(list)) => {
                    list.first().and_then(|v| v.as_i64())
                }
                _ => None,
            };
            let Some(item_id) = first_item_id.or(session.ship_id) else {
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
                if item.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "OwnershipOrLocationViolation".into(),
                        message: format!(
                            "Ship item {item_id} must be owned by character {char_id} at station {station_id}"
                        ),
                    });
                }
                if matches!(item.flag as u16, 11..=34 | 92..=94 | 125..=132 | 158) {
                    return Err(BridgeRpcError {
                        code: "CanNotUnassembleInThisItemLoc".into(),
                        message: format!(
                            "Item {item_id} is currently fitted in slot {:?} and cannot be assembled/repackaged in this location",
                            item.flag
                        ),
                    });
                }
                if item.location_id != station_id {
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
                let hull_or_armor_damage = item
                    .dynamic_attributes
                    .get(&attr_ids::DAMAGE)
                    .copied()
                    .unwrap_or(0.0)
                    > 0.0
                    || item
                        .dynamic_attributes
                        .get(&attr_ids::ARMOR_DAMAGE)
                        .copied()
                        .unwrap_or(0.0)
                        > 0.0;
                if hull_or_armor_damage {
                    return Err(BridgeRpcError {
                        code: "CantRepackageDamagedItem".into(),
                        message: format!("Item {item_id} is damaged and must be repaired first"),
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
                };
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
                    let prev_ship_id = session.ship_id;
                    if let Some(prev) = prev_ship_id {
                        ctx.invalidate_ship_fitting(prev);
                    }
                    ctx.invalidate_ship_fitting(sid);
                    session.ship_id = Some(sid);
                    if prev_ship_id != Some(sid) {
                        let changes = vec![(
                            PyRep::String("shipid".to_string()),
                            PyRep::Tuple(vec![
                                prev_ship_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                                PyRep::Integer(sid),
                            ]),
                        )];
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnSessionChanged",
                            "clientID",
                            vec![
                                PyRep::Bool(false),
                                PyRep::Integer(session.session_id as i64),
                                PyRep::Dict(changes),
                            ],
                        );
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "ProcessActiveShipChanged",
                            "charid",
                            vec![
                                PyRep::Integer(sid),
                                prev_ship_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            ],
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("characterID", PyRep::Integer(char_id)),
                        ("shipID", PyRep::Integer(sid)),
                        (
                            "oldShipID",
                            prev_ship_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        ),
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
            let explicit_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list) | PyRep::Tuple(list)) => {
                    list.iter().filter_map(|v| v.as_i64()).collect()
                }
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
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
            for h in &mut fit.rack_heat {
                *h = 0.0;
            }
            let dyn_repaired_hp = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && (i.item_id == ship_id
                                || i.location_id == ship_id
                                || explicit_ids.contains(&i.item_id))
                    })
                    .map(|i| {
                        i.dynamic_attributes
                            .get(&attr_ids::DAMAGE)
                            .copied()
                            .unwrap_or(0.0)
                            + i.dynamic_attributes
                                .get(&attr_ids::ARMOR_DAMAGE)
                                .copied()
                                .unwrap_or(0.0)
                    })
                    .sum::<f64>()
            });
            total_repaired_hp += dyn_repaired_hp;
            let cost_isk = (total_repaired_hp * 500.0).round();
            let debit_res = ctx.db.try_with_mut_snapshot(|snap| {
                if cost_isk > 0.0 {
                    let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })?;
                    let fee = IskCents::from_f64_isk(cost_isk);
                    ch.balance = ch.balance.checked_sub(fee).ok_or_else(|| BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Insufficient ISK to pay repair bill".into(),
                    })?;
                }
                for item in snap.items.values_mut() {
                    if item.owner_id == char_id
                        && (item.item_id == ship_id
                            || item.location_id == ship_id
                            || explicit_ids.contains(&item.item_id))
                    {
                        item.dynamic_attributes.remove(&attr_ids::DAMAGE);
                        item.dynamic_attributes.remove(&attr_ids::ARMOR_DAMAGE);
                    }
                }
                Ok(())
            });
            if let Err(e) = debit_res {
                return Some(Err(e));
            }
            ctx.store_ship_fitting(ship_id, fit);
            let repaired_count = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && (i.item_id == ship_id
                                || i.location_id == ship_id
                                || explicit_ids.contains(&i.item_id))
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

        ("invbroker", "SplitStack") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // Supports both (item_id, qty) and V24 (station_id, item_id, qty, owner_id)
            let (item_id, split_quantity) = if args.len() >= 3 {
                (
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(1) as i32,
                )
            } else {
                (
                    args.first().and_then(|a| a.as_i64()).unwrap_or(0),
                    args.get(1)
                        .and_then(|a| a.as_i64())
                        .or_else(|| {
                            kwargs
                                .get("qty")
                                .or_else(|| kwargs.get("quantity"))
                                .and_then(|v| v.as_i64())
                        })
                        .unwrap_or(1) as i32,
                )
            };
            match ctx.db.split_stack(char_id, item_id, split_quantity) {
                Ok((orig, new_i)) => {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnItemChange",
                        "charid",
                        vec![
                            PyRep::keyval([
                                ("itemID", PyRep::Integer(orig.item_id)),
                                ("typeID", PyRep::Integer(orig.type_id as i64)),
                                ("locationID", PyRep::Integer(orig.location_id)),
                                ("flagID", PyRep::Integer(orig.flag as i64)),
                                ("quantity", PyRep::Integer(orig.quantity as i64)),
                            ]),
                            PyRep::Dict(vec![(
                                PyRep::Integer(5), // ixQuantity
                                PyRep::Integer((orig.quantity + new_i.quantity) as i64),
                            )]),
                        ],
                    );
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnItemChange",
                        "charid",
                        vec![
                            PyRep::keyval([
                                ("itemID", PyRep::Integer(new_i.item_id)),
                                ("typeID", PyRep::Integer(new_i.type_id as i64)),
                                ("locationID", PyRep::Integer(new_i.location_id)),
                                ("flagID", PyRep::Integer(new_i.flag as i64)),
                                ("quantity", PyRep::Integer(new_i.quantity as i64)),
                            ]),
                            PyRep::Dict(vec![(
                                PyRep::Integer(3), // ixLocationID
                                PyRep::Integer(0),
                            )]),
                        ],
                    );
                    Some(Ok(PyRep::keyval([
                        ("originalItemID", PyRep::Integer(orig.item_id)),
                        ("originalQuantity", PyRep::Integer(orig.quantity as i64)),
                        ("newItemID", PyRep::Integer(new_i.item_id)),
                        ("newQuantity", PyRep::Integer(new_i.quantity as i64)),
                        ("typeID", PyRep::Integer(orig.type_id as i64)),
                    ])))
                }
                Err(e) => Some(Err(BridgeRpcError {
                    code: "SplitFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("invbroker", "MultiMerge") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // Supports V24 MultiMerge([(src_id, dst_id, qty), ...], sourceContainerID?)
            // or direct MultiMerge(dst_id, src_id, qty?)
            let mut ops: Vec<(i64, i64, Option<i32>)> = Vec::new();
            match args.first() {
                Some(PyRep::List(list) | PyRep::Tuple(list)) => {
                    for entry in list {
                        if let PyRep::Tuple(seq) | PyRep::List(seq) = entry {
                            if let (Some(src_id), Some(dst_id)) = (
                                seq.first().and_then(|v| v.as_i64()),
                                seq.get(1).and_then(|v| v.as_i64()),
                            ) {
                                let qty_opt =
                                    seq.get(2).and_then(|v| v.as_i64()).map(|q| q as i32);
                                ops.push((src_id, dst_id, qty_opt));
                            }
                        }
                    }
                }
                Some(PyRep::Integer(dst_id)) => {
                    let src_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
                    let qty_opt = args.get(2).and_then(|a| a.as_i64()).map(|q| q as i32);
                    ops.push((src_id, *dst_id, qty_opt));
                }
                _ => {}
            }
            if ops.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "StackFailed".into(),
                    message: "MultiMerge requires at least one (sourceItemID, destItemID, qty) merge operation".into(),
                }));
            }
            let mut updated_records = Vec::with_capacity(ops.len());
            for (mut src_id, dst_id, qty_opt) in ops {
                if let Some(req_qty) = qty_opt {
                    let src_qty = ctx
                        .db
                        .with_snapshot(|snap| snap.items.get(&src_id).map(|i| i.quantity))
                        .unwrap_or(0);
                    if req_qty <= 0 || req_qty > src_qty {
                        return Some(Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: format!(
                                "Invalid merge quantity {req_qty} for source stack {src_id} (qty {src_qty})"
                            ),
                        }));
                    }
                    if req_qty < src_qty {
                        match ctx.db.split_stack(char_id, src_id, req_qty) {
                            Ok((_orig, split_part)) => {
                                src_id = split_part.item_id;
                            }
                            Err(e) => {
                                return Some(Err(BridgeRpcError {
                                    code: "StackFailed".into(),
                                    message: e.to_string(),
                                }));
                            }
                        }
                    }
                }
                match ctx.db.stack_items(char_id, dst_id, src_id) {
                    Ok(updated) => {
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnItemChange",
                            "charid",
                            vec![
                                PyRep::keyval([
                                    ("itemID", PyRep::Integer(updated.item_id)),
                                    ("typeID", PyRep::Integer(updated.type_id as i64)),
                                    ("locationID", PyRep::Integer(updated.location_id)),
                                    ("flagID", PyRep::Integer(updated.flag as i64)),
                                    ("quantity", PyRep::Integer(updated.quantity as i64)),
                                ]),
                                PyRep::Dict(vec![(
                                    PyRep::Integer(5), // ixQuantity
                                    PyRep::Integer(updated.quantity as i64),
                                )]),
                            ],
                        );
                        updated_records.push(updated);
                    }
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: e.to_string(),
                        }));
                    }
                }
            }
            let last = updated_records.last().unwrap();
            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(last.item_id)),
                ("typeID", PyRep::Integer(last.type_id as i64)),
                ("quantity", PyRep::Integer(last.quantity as i64)),
                ("version", PyRep::Integer(last.version as i64)),
                ("mergedCount", PyRep::Integer(updated_records.len() as i64)),
            ])))
        }

        ("invbroker", "StackAll") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(target_loc) = bound_entry
                .map(|b| b.object_id)
                .or_else(|| kwargs.get("locationID").and_then(|v| v.as_i64()))
                .or(session.station_id)
                .or(session.structure_id)
                .or(session.ship_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLocation".into(),
                    message: "StackAll requires a bound inventory or active station/ship".into(),
                }));
            };
            let default_flag = if Some(target_loc) == session.station_id
                || Some(target_loc) == session.structure_id
            {
                InventoryFlag::Hangar
            } else {
                InventoryFlag::Cargo
            };
            let target_flag = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()))
                .filter(|&f| f > 0)
                .map(|f| InventoryFlag::from_u16(f as u16))
                .unwrap_or(default_flag);

            let groups: HashMap<TypeId, Vec<i64>> = ctx.db.with_snapshot(|snap| {
                let mut by_type: HashMap<TypeId, Vec<i64>> = HashMap::new();
                for item in snap.items.values() {
                    if item.owner_id == char_id
                        && item.location_id == target_loc
                        && item.flag == target_flag
                        && !item.singleton
                        && !item.is_blueprint_copy
                        && item.dynamic_attributes.is_empty()
                        && item.source_mutaplasmid_id.is_none()
                        && item.quantity > 0
                    {
                        by_type.entry(item.type_id).or_default().push(item.item_id);
                    }
                }
                for ids in by_type.values_mut() {
                    ids.sort_unstable();
                }
                by_type
            });

            let mut merged_count = 0_i64;
            let mut survivors = Vec::new();
            for (_type_id, ids) in groups {
                if ids.len() < 2 {
                    continue;
                }
                let dst_id = ids[0];
                let mut latest = None;
                for &src_id in &ids[1..] {
                    if let Ok(upd) = ctx.db.stack_items(char_id, dst_id, src_id) {
                        merged_count += 1;
                        latest = Some(upd);
                    }
                }
                if let Some(upd) = latest {
                    survivors.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(upd.item_id)),
                        ("typeID", PyRep::Integer(upd.type_id as i64)),
                        ("quantity", PyRep::Integer(upd.quantity as i64)),
                        ("version", PyRep::Integer(upd.version as i64)),
                    ]));
                }
            }
            Some(Ok(PyRep::keyval([
                ("locationID", PyRep::Integer(target_loc)),
                ("flagID", PyRep::Integer(target_flag as i64)),
                ("mergedCount", PyRep::Integer(merged_count)),
                ("stacks", PyRep::List(survivors)),
            ])))
        }

        ("invbroker", "MultiAdd") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list) | PyRep::Tuple(list)) => {
                    list.iter().filter_map(|v| v.as_i64()).collect()
                }
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            };
            let Some(new_loc) = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
                .or(session.ship_id)
                .or(session.station_id)
                .or(session.structure_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLocation".into(),
                    message: "MultiAdd requires target location".into(),
                }));
            };
            let flag = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()))
                .map(|v| InventoryFlag::from_u16(v as u16))
                .unwrap_or(InventoryFlag::Cargo);
            let mut moved = Vec::with_capacity(item_ids.len());
            for item_id in item_ids {
                match ctx
                    .db
                    .move_item_transactional(char_id, item_id, new_loc, flag, None, None)
                {
                    Ok(upd) => moved.push(PyRep::Integer(upd.item_id)),
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "InventoryMoveFailed".into(),
                            message: e.to_string(),
                        }));
                    }
                }
            }
            Some(Ok(PyRep::keyval([
                ("movedCount", PyRep::Integer(moved.len() as i64)),
                ("items", PyRep::List(moved)),
            ])))
        }

        ("invbroker", "TrashItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list) | PyRep::Tuple(list)) => {
                    list.iter().filter_map(|v| v.as_i64()).collect()
                }
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            };
            if item_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "DeleteFailed".into(),
                    message: "TrashItems requires at least one itemID".into(),
                }));
            }
            let expected_station = args.get(1).and_then(|a| a.as_i64()).filter(|&st| st > 0);
            let first_id = item_ids[0];

            // Execute all item deletions atomically in a single snapshot transaction
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let active_ship_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0);
                let mut trashed: Vec<(i64, i64)> = Vec::with_capacity(item_ids.len());
                for &item_id in &item_ids {
                    let Some(item) = snap.items.get(&item_id) else {
                        return Err(BridgeRpcError {
                            code: "DeleteFailed".into(),
                            message: format!("Item {item_id} not found"),
                        });
                    };
                    if item.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "DeleteFailed".into(),
                            message: format!("Item {item_id} is not owned by character {char_id}"),
                        });
                    }
                    if item_id == active_ship_id {
                        return Err(BridgeRpcError {
                            code: "DeleteFailed".into(),
                            message: format!("Cannot trash active ship {item_id}"),
                        });
                    }
                    let is_fitted = matches!(
                        item.flag as u16,
                        11..=34     // LoSlot0..=LoSlot7, MedSlot0..=MedSlot7, HiSlot0..=HiSlot7
                        | 87        // DroneBay
                        | 92..=94   // RigSlot0..=RigSlot2
                        | 125..=129 // SubSystemSlot0..=SubSystemSlot4
                        | 158       // FighterBay
                    );
                    if is_fitted {
                        return Err(BridgeRpcError {
                            code: "CannotTrashFittedItem".into(),
                            message: format!("Cannot trash fitted item {item_id}"),
                        });
                    }
                    let in_contract = snap.contracts.values().any(|c| {
                        (c.status == "Outstanding" || c.status == "InProgress")
                            && c.item_ids.contains(&item_id)
                    });
                    let in_industry = snap.industry_jobs.values().any(|j| {
                        j.status == "Active" && j.blueprint_item_id == item_id
                    });
                    if in_contract || in_industry {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!(
                                "Item {item_id} is locked by an active contract or industry job"
                            ),
                        });
                    }
                    if let Some(st_id) = expected_station {
                        if item.location_id != st_id {
                            return Err(BridgeRpcError {
                                code: "DeleteFailed".into(),
                                message: format!(
                                    "Item {item_id} is located at {}, not expected station {st_id}",
                                    item.location_id
                                ),
                            });
                        }
                    }
                    if snap.items.values().any(|child| child.location_id == item_id) {
                        return Err(BridgeRpcError {
                            code: "DeleteFailed".into(),
                            message: format!(
                                "Cannot trash non-empty container or fitted ship {item_id}"
                            ),
                        });
                    }
                    trashed.push((item_id, item.location_id));
                }
                for &(item_id, _) in &trashed {
                    snap.items.remove(&item_id);
                    snap.item_custom_names.remove(&item_id);
                    snap.item_positions.remove(&item_id);
                }
                Ok(trashed)
            });

            let trashed = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            for &(item_id, old_location_id) in &trashed {
                ctx.invalidate_ship_fitting(item_id);
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnItemChange",
                    "charid",
                    vec![
                        PyRep::keyval([
                            ("itemID", PyRep::Integer(item_id)),
                            ("locationID", PyRep::Integer(6)), // Junkyard (locationID 6)
                        ]),
                        PyRep::Dict(vec![(
                            PyRep::Integer(3), // ixLocationID
                            PyRep::Integer(old_location_id),
                        )]),
                    ],
                );
            }
            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(first_id)),
                ("deleted", PyRep::Bool(true)),
                ("count", PyRep::Integer(trashed.len() as i64)),
            ])))
        }

        ("invbroker", "SetLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (item_id, raw_label) = if args.len() >= 2 {
                (
                    args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0),
                    args.get(1).and_then(|a| a.as_str()).unwrap_or(""),
                )
            } else {
                (
                    bound_entry
                        .map(|b| b.object_id)
                        .or(session.ship_id)
                        .unwrap_or(0),
                    args.first().and_then(|a| a.as_str()).unwrap_or(""),
                )
            };
            let label = raw_label.trim();
            if label.is_empty() || label.chars().count() > 100 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLabel".into(),
                    message: "Item/ship custom name must be 1..=100 characters".into(),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(item) = snap.items.get(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Item {item_id} not found"),
                    });
                };
                if item.owner_id != char_id {
                    let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id);
                    let has_corp_rename_role = corp_id == Some(item.owner_id)
                        && snap.corporations.get(&item.owner_id).is_some_and(|corp| {
                            if corp.ceo_id == char_id {
                                return true;
                            }
                            let roles = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                            // corpRoleDirector (1) or corpRoleEquipmentConfig (2199023255552)
                            (roles & (1 | 2_199_023_255_552)) != 0
                        });
                    if !has_corp_rename_role {
                        return Err(BridgeRpcError {
                            code: "OwnershipViolation".into(),
                            message: format!(
                                "Character {char_id} lacks permission to rename item {item_id}"
                            ),
                        });
                    }
                }
                snap.item_custom_names.insert(item_id, label.to_string());
                Ok(())
            });
            match res {
                Ok(()) => {
                    ctx.esi_state.set_custom_item_name(item_id, label.to_string());
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(item_id)),
                        ("name", PyRep::String(label.to_string())),
                    ])))
                }
                Err(e) => Some(Err(e)),
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
