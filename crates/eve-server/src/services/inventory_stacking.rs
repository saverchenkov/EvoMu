//! `inventory_stacking` — Stack, Split, Merge, and StackAll Operations for `invbroker`.

#![forbid(unsafe_code)]

use crate::router::{context::BoundObjectEntry, ServerContext};
use crate::services::inventory_fitting::fitting_containers_ships;
use eve_db::{InventoryFlag, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

pub(super) fn handle_stack_ops(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("invbroker", "SplitStack") | ("contractProxy", "SplitStack") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
                let has_courier_tag = ctx.db.with_snapshot(|snap| {
                    let is_courier = |id| snap.item_custom_names.get(&id).map_or(false, |s| s.starts_with("courier_cargo:"));
                    is_courier(src_id) || is_courier(dst_id)
                });
                if has_courier_tag {
                    return Some(Err(BridgeRpcError {
                        code: "StackFailed".into(),
                        message: "Cannot merge courier mission cargo".into(),
                    }));
                }
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
                                    message: format!("Split before merge failed: {e}"),
                                }));
                            }
                        }
                    }
                }
                match ctx.db.stack_items(char_id, dst_id, src_id) {
                    Ok(upd) => updated_records.push(upd),
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: e.to_string(),
                        }));
                    }
                }
            }
            let last = updated_records.last().cloned().unwrap_or_default();
            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(last.item_id)),
                ("locationID", PyRep::Integer(last.location_id)),
                ("flagID", PyRep::Integer(last.flag as i64)),
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
                        && !snap.item_custom_names.get(&item.item_id).map_or(false, |s| s.starts_with("courier_cargo:"))
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

        _ => None,
    }
}

pub(crate) async fn handle_multi_add(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let mut item_ids: Vec<i64> = match args.first() {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => list.iter().filter_map(|v| v.as_i64()).collect(),
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    {
        let mut seen = HashSet::new();
        item_ids.retain(|id| seen.insert(*id));
    }
    if item_ids.is_empty() {
        return Ok(PyRep::keyval([
            ("movedCount", PyRep::Integer(0)),
            ("items", PyRep::List(Vec::new())),
        ]));
    }
    let Some(new_loc) = args
        .get(1)
        .and_then(|a| a.as_i64())
        .or_else(|| bound_entry.map(|b| b.object_id))
        .or(session.ship_id)
        .or(session.station_id)
        .or(session.structure_id)
    else {
        return Err(BridgeRpcError {
            code: "InvalidLocation".into(),
            message: "MultiAdd requires target location".into(),
        });
    };
    let flag = args
        .get(2)
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()))
        .map(|v| InventoryFlag::from_u16(v as u16))
        .unwrap_or(InventoryFlag::Cargo);
    let flag_u16 = flag as u16;
    if matches!(flag_u16, 11..=34 | 89 | 92..=94 | 125..=132 | 164..=171) {
        return Err(BridgeRpcError {
            code: "InvalidLocation".into(),
            message: "MultiAdd cannot be used to fit modules or rigs".into(),
        });
    }
    let active_ship_id = session
        .ship_id
        .or_else(|| {
            ctx.db
                .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.active_ship_id))
        })
        .unwrap_or(0);
    if item_ids.contains(&active_ship_id) {
        return Err(BridgeRpcError {
            code: "CannotMoveActiveShip".into(),
            message: "Cannot move active ship".into(),
        });
    }
    for &item_id in &item_ids {
        fitting_containers_ships::validate_container_and_bay_move(
            ctx, char_id, item_id, new_loc, flag_u16, kwargs, None,
        )?;
    }
    let is_ship_hold = matches!(flag_u16, 5 | 87 | 158 | 133..=151 | 179 | 181 | 182 | 185);
    let eval_cargo_cap = if flag_u16 == 5 {
        ctx.get_or_rebuild_ship_fitting(char_id, new_loc)
            .ok()
            .map(|f| f.evaluated_context(&ctx.sde).ship.get_attribute(38, &ctx.sde))
            .filter(|&c| c > 0.0)
    } else {
        None
    };
    let mut fitted_unfit_ids = Vec::new();
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
        })?;
        let ch_dock = ch.station_id.or(ch.structure_id).unwrap_or(0);
        let ch_sys = ch.solar_system_id as i64;
        let active_colocated =
            snap.is_ship_colocated_with_character(char_id, ch.active_ship_id, ch.station_id.or(ch.structure_id));

        let foreign_dest = snap.items.get(&new_loc).is_some_and(|d| d.owner_id != char_id);
        let dest_item_opt = snap.items.get(&new_loc);
        let dest_type_opt = dest_item_opt.and_then(|d| ctx.sde.types.get(&d.type_id));

        let dst_reachable = (ch.station_id == Some(new_loc))
            || (ch.structure_id == Some(new_loc))
            || (active_colocated && ch.active_ship_id == new_loc)
            || dest_item_opt.is_some_and(|d| {
                d.owner_id == char_id
                    || d.location_id == ch_dock
                    || d.location_id == ch_sys
                    || (active_colocated && d.location_id == ch.active_ship_id)
            });
        if !dst_reachable {
            return Err(BridgeRpcError {
                code: "DestinationAccessDenied".into(),
                message: format!("Destination {new_loc} not reachable"),
            });
        }

        let mut batch_vol = 0.0;
        for &item_id in &item_ids {
            let Some(item) = snap.items.get(&item_id) else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Item {item_id} not found"),
                });
            };
            if item.owner_id != char_id {
                return Err(BridgeRpcError {
                    code: "ItemOwnershipViolation".into(),
                    message: format!("Character {char_id} is not authorized to move item {item_id}"),
                });
            }
            if matches!(item.flag as u16, 92..=94) {
                return Err(BridgeRpcError {
                    code: "CannotUnfitRig".into(),
                    message: "Fitted rigs cannot be moved via MultiAdd".into(),
                });
            }
            if matches!(item.flag as u16, 11..=34 | 125..=132) {
                fitted_unfit_ids.push(item_id);
            }
            let is_courier = snap
                .item_custom_names
                .get(&item_id)
                .is_some_and(|s| s.starts_with("courier_cargo:"));
            if is_courier && ((116..=122).contains(&flag_u16) || foreign_dest) {
                return Err(BridgeRpcError {
                    code: "CannotDeliverCourierCargo".into(),
                    message: "Courier cargo cannot be moved into corporation hangar divisions or foreign containers/ships".into(),
                });
            }
            let src_reachable = (ch.station_id == Some(item.location_id))
                || (ch.structure_id == Some(item.location_id))
                || (active_colocated && ch.active_ship_id == item.location_id)
                || snap.items.get(&item.location_id).is_some_and(|p| {
                    p.owner_id == char_id
                        || p.location_id == ch_dock
                        || p.location_id == ch_sys
                        || (active_colocated && p.location_id == ch.active_ship_id)
                });
            if !src_reachable {
                return Err(BridgeRpcError {
                    code: "ItemOwnershipViolation".into(),
                    message: format!("Item {item_id} location not reachable"),
                });
            }
            if let Some(dest_type) = dest_type_opt {
                if is_ship_hold && dest_type.category_id == 6 {
                    let Some(item_type) = ctx.sde.types.get(&item.type_id) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Type {} not found", item.type_id),
                        });
                    };
                    if !crate::services::fitting_saved_apply::is_item_allowed_in_ship_hold(
                        flag_u16, item_type, dest_type,
                    ) {
                        return Err(BridgeRpcError::user_error("InvalidItemTypeForHold"));
                    }
                }
            }
            batch_vol += fitting_containers_ships::effective_item_unit_volume(&ctx.sde, item)
                * (item.quantity as f64);
        }

        if let Some(dest_type) = dest_type_opt {
            if is_ship_hold && dest_type.category_id == 6 {
                let hold_cap =
                    crate::services::fitting_saved_apply::get_ship_hold_capacity(flag_u16, dest_type, eval_cargo_cap);
                if flag_u16 != 5 && hold_cap <= 0.0 {
                    return Err(BridgeRpcError {
                        code: "CannotMoveIntoHold".into(),
                        message: format!("Ship {} has no hold for flag {flag_u16}", dest_type.name),
                    });
                }
                if hold_cap > 0.0 {
                    let used_vol: f64 = snap
                        .items
                        .values()
                        .filter(|i| {
                            i.location_id == new_loc && (i.flag as u16) == flag_u16 && !item_ids.contains(&i.item_id)
                        })
                        .map(|i| {
                            fitting_containers_ships::effective_item_unit_volume(&ctx.sde, i)
                                * (i.quantity as f64)
                        })
                        .sum();
                    if used_vol + batch_vol > hold_cap + 1e-4 {
                        return Err(BridgeRpcError {
                            code: "NotEnoughCargoSpace".into(),
                            message: format!(
                                "Cumulative batch volume {batch_vol:.1} m³ exceeds remaining capacity of hold {flag_u16}"
                            ),
                        });
                    }
                }
            } else if fitting_containers_ships::is_container_group(
                dest_type.group_id,
                dest_type.type_id,
            ) {
                let cap = dest_type.capacity;
                if cap > 0.0 {
                    let used: f64 = snap
                        .items
                        .values()
                        .filter(|i| i.location_id == new_loc && !item_ids.contains(&i.item_id))
                        .map(|i| {
                            fitting_containers_ships::effective_item_unit_volume(&ctx.sde, i)
                                * (i.quantity as f64)
                        })
                        .sum();
                    if used + batch_vol > cap + 1e-4 {
                        return Err(BridgeRpcError {
                            code: "CapacityExceeded".into(),
                            message: format!("Container capacity of {cap} m³ exceeded"),
                        });
                    }
                }
            } else if flag_u16 == 90 && dest_type.category_id == 6 {
                let sma_cap = dest_type.default_attributes.get(&908).copied().unwrap_or(0.0);
                if sma_cap > 0.0 {
                    let used: f64 = snap
                        .items
                        .values()
                        .filter(|i| {
                            i.location_id == new_loc && matches!(i.flag as u16, 90 | 138) && !item_ids.contains(&i.item_id)
                        })
                        .map(|i| fitting_containers_ships::effective_item_unit_volume(&ctx.sde, i))
                        .sum();
                    if used + batch_vol > sma_cap + 1e-4 {
                        return Err(BridgeRpcError {
                            code: "CapacityExceeded".into(),
                            message: format!("Ship Maintenance Bay capacity {sma_cap} m³ exceeded"),
                        });
                    }
                }
            } else if flag_u16 == 155 && dest_type.category_id == 6 {
                let fh_cap = dest_type.default_attributes.get(&912).copied().unwrap_or(0.0);
                if fh_cap > 0.0 {
                    let used: f64 = snap
                        .items
                        .values()
                        .filter(|i| {
                            i.location_id == new_loc && (i.flag as u16) == 155 && !item_ids.contains(&i.item_id)
                        })
                        .map(|i| {
                            fitting_containers_ships::effective_item_unit_volume(&ctx.sde, i)
                                * (i.quantity as f64)
                        })
                        .sum();
                    if used + batch_vol > fh_cap + 1e-4 {
                        return Err(BridgeRpcError {
                            code: "CapacityExceeded".into(),
                            message: format!("Fleet Hangar capacity {fh_cap} m³ exceeded"),
                        });
                    }
                }
            }
        }
        Ok(())
    })?;
    if !fitted_unfit_ids.is_empty() && session.station_id.is_none() && session.structure_id.is_none() {
        let now_unix = ctx.now_unix_secs();
        let has_weapons_timer = ctx
            .crimewatch
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .states
            .get(&char_id)
            .is_some_and(|cw| cw.is_weapon_timer_active(now_unix));
        if has_weapons_timer {
            return Err(BridgeRpcError {
                code: "CannotRefitWithWeaponsTimer".into(),
                message: "Cannot refit or unfit modules in space while weapons timer is active".into(),
            });
        }
        let mut near_service = false;
        if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
            near_service =
                fitting_containers_ships::is_near_in_space_fitting_service(ctx, sys_id, ship_id).await;
        }
        if !near_service {
            return Err(BridgeRpcError {
                code: "CannotUnfitInSpace".into(),
                message: "Unfitting modules requires being docked at a station/structure or within range of a Mobile Depot or capital SMA".into(),
            });
        }
    }
    let moved = ctx.db.try_with_mut_snapshot(|snap| {
        let mut moved = Vec::with_capacity(item_ids.len());
        for &item_id in &item_ids {
            let it = snap.items.get_mut(&item_id).ok_or_else(|| BridgeRpcError {
                code: "ItemNotFound".into(),
                message: format!("Item {item_id} not found"),
            })?;
            it.location_id = new_loc;
            it.flag = flag;
            it.version += 1;
            snap.module_online_states.remove(&item_id);
            snap.module_active_states.remove(&item_id);
            snap.module_active_charges.remove(&item_id);
            moved.push(item_id);
        }
        Ok(moved)
    })?;
    for &item_id in &fitted_unfit_ids {
        ctx.remove_projected_ewar_by_module(item_id);
    }
    if is_ship_hold || matches!(flag_u16, 90 | 155) {
        ctx.invalidate_ship_fitting(new_loc);
    }
    Ok(PyRep::keyval([
        ("movedCount", PyRep::Integer(moved.len() as i64)),
        ("items", PyRep::List(moved.into_iter().map(PyRep::Integer).collect())),
    ]))
}
