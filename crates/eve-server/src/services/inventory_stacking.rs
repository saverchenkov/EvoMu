//! `inventory_stacking` — Stack, Split, Merge, and StackAll Operations for `invbroker`.

#![forbid(unsafe_code)]

use crate::router::{context::BoundObjectEntry, ServerContext};
use crate::services::inventory_fitting::{
    fitting_containers_ships, get_effective_corp_roles_at_location, item_to_packed_cells,
};
use eve_db::{InventoryFlag, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
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
            let alsc_locked_set = fitting_containers_ships::read_container_ship_state(ctx).alsc_locked_items;
            for (mut src_id, dst_id, qty_opt) in ops {
                if alsc_locked_set.contains(&src_id) || alsc_locked_set.contains(&dst_id) {
                    return Some(Err(BridgeRpcError {
                        code: "ALSCItemLocked".into(),
                        message: "Cannot merge items locked in an Audit Log Secure Container".into(),
                    }));
                }
                let pre_check = ctx.db.with_snapshot(|snap| {
                    let is_courier = |id| snap.item_custom_names.get(&id).map_or(false, |s| s.starts_with("courier_cargo:"));
                    if is_courier(src_id) || is_courier(dst_id) {
                        return Err("Cannot merge courier mission cargo".to_string());
                    }
                    if snap.is_item_locked_in_contract_or_job(src_id) || snap.is_item_locked_in_contract_or_job(dst_id) {
                        return Err("Cannot merge locked items".to_string());
                    }
                    let Some(src) = snap.items.get(&src_id) else {
                        return Err(format!("Source item {src_id} not found"));
                    };
                    let Some(dst) = snap.items.get(&dst_id) else {
                        return Err(format!("Destination item {dst_id} not found"));
                    };
                    if src_id == dst_id {
                        return Err("Cannot merge stack into itself".to_string());
                    }
                    if src.owner_id != char_id || dst.owner_id != char_id {
                        return Err("Stack ownership mismatch".to_string());
                    }
                    if src.type_id != dst.type_id || src.location_id != dst.location_id || src.flag != dst.flag {
                        return Err("Incompatible stacks for merge".to_string());
                    }
                    if src.singleton || dst.singleton || src.is_blueprint_copy || dst.is_blueprint_copy {
                        return Err("Cannot merge singleton or BPC items".to_string());
                    }
                    if !src.dynamic_attributes.is_empty() || !dst.dynamic_attributes.is_empty() || src.source_mutaplasmid_id.is_some() || dst.source_mutaplasmid_id.is_some() {
                        return Err("Cannot merge mutated/dynamic items".to_string());
                    }
                    if src.flag == InventoryFlag::Locked || dst.flag == InventoryFlag::Locked {
                        return Err("Cannot merge locked items".to_string());
                    }
                    Ok(src.quantity)
                });
                let src_qty = match pre_check {
                    Ok(q) => q,
                    Err(msg) => {
                        return Some(Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: msg,
                        }));
                    }
                };
                if let Some(req_qty) = qty_opt {
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

            let alsc_locked_set = fitting_containers_ships::read_container_ship_state(ctx).alsc_locked_items;
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
                        && !alsc_locked_set.contains(&item.item_id)
                        && !snap.is_item_locked_in_contract_or_job(item.item_id)
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
    let first_src_loc = item_ids
        .first()
        .and_then(|&id| ctx.db.with_snapshot(|s| s.items.get(&id).map(|i| i.location_id)));
    let Some(new_loc) = (match (
        args.get(1).and_then(|a| a.as_i64()),
        bound_entry.map(|b| b.object_id),
        first_src_loc,
    ) {
        (Some(a1), Some(bid), Some(sloc)) if a1 == sloc && bid != sloc => Some(bid),
        (a1, bid, _) => a1.or(bid),
    })
    .or(session.ship_id)
    .or(session.station_id)
    .or(session.structure_id) else {
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
    let mut looted_from_space_ids: HashSet<i64> = HashSet::new();
    let mut any_foreign_theft = false;
    for &item_id in &item_ids {
        let src_opt = ctx.db.with_snapshot(|snap| snap.items.get(&item_id).cloned());
        if let Some(src_item) = src_opt {
            let (from_space, foreign_theft) =
                evaluate_space_wreck_or_jetcan_loot(ctx, char_id, &src_item)?;
            if from_space {
                looted_from_space_ids.insert(item_id);
            }
            if foreign_theft {
                any_foreign_theft = true;
            }
        }
    }
    let dest_is_struct_fuel = crate::services::structure_acl_defense_rules::is_structure_fuel_flag(flag)
        && ctx.db.with_snapshot(|s| s.structures.contains_key(&new_loc));
    let is_ship_hold = !dest_is_struct_fuel
        && matches!(flag_u16, 5 | 87 | 158 | 133..=151 | 179 | 181 | 182 | 185);
    let eval_cargo_cap = if flag_u16 == 5 {
        ctx.get_or_rebuild_ship_fitting(char_id, new_loc)
            .ok()
            .map(|f| f.evaluated_context(&ctx.sde).ship.get_attribute(38, &ctx.sde))
            .filter(|&c| c > 0.0)
    } else {
        None
    };
    let pre_balls = crate::services::structure_acl_defense_rules::snapshot_system_balls_for_inventory(
        ctx, session, char_id,
    );
    let mut fitted_unfit_ids: Vec<(i64, i64, u16)> = Vec::new();
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
        })?;
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

        let foreign_dest = snap.items.get(&new_loc).is_some_and(|d| d.owner_id != char_id);
        let dest_type_opt = snap.items.get(&new_loc).and_then(|d| ctx.sde.types.get(&d.type_id));

        crate::services::inventory_bound::validate_multiadd_destination(
            snap, session, ch, char_id, new_loc, dest_is_struct_fuel, &pre_balls,
        )?;

        let mut batch_vol = 0.0;
        for &item_id in &item_ids {
            let Some(item) = snap.items.get(&item_id) else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Item {item_id} not found"),
                });
            };
            let src_is_struct_fuel = crate::services::inventory_bound::validate_multiadd_structure_fuel_transfers(
                ctx, snap, session, char_id, item, new_loc, dest_is_struct_fuel, ch_dock, ch_sys, caller_ship_id, &pre_balls,
            )?;
            let is_corp_item = ch.corporation_id > 0 && item.owner_id == ch.corporation_id;
            if is_corp_item && (116..=122).contains(&(item.flag as i64)) {
                let (is_ceo, roles) = get_effective_corp_roles_at_location(
                    ctx,
                    ch.corporation_id,
                    char_id,
                    Some(item.location_id),
                );
                let div_idx = ((item.flag as i64) - 116) as u32;
                let req_take_role = 1u64 << (13 + div_idx);
                if !is_ceo && !eve_db::corp_roles::has_role(roles, req_take_role) {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: format!("Corporation role denied for division {} take", div_idx + 1),
                    });
                }
            }
            if !src_is_struct_fuel && item.owner_id != char_id && !is_corp_item && !looted_from_space_ids.contains(&item_id) {
                return Err(BridgeRpcError {
                    code: "ItemOwnershipViolation".into(),
                    message: format!("Character {char_id} is not authorized to move item {item_id}"),
                });
            }
            if item_id == new_loc
                || (ch.active_ship_id > 0
                    && item_id == ch.active_ship_id
                    && snap.items.contains_key(&new_loc))
            {
                return Err(BridgeRpcError {
                    code: "CannotMoveActiveShip".into(),
                    message: format!("Cannot move item {item_id} into {new_loc}"),
                });
            }
            if flag == InventoryFlag::Locked
                || item.flag == InventoryFlag::Locked
                || snap.is_item_locked_in_contract_or_job(item_id)
            {
                return Err(BridgeRpcError {
                    code: "ItemLocked".into(),
                    message: format!("Item {item_id} is locked"),
                });
            }
            if matches!(item.flag as u16, 92..=94) {
                return Err(BridgeRpcError {
                    code: "CannotUnfitRig".into(),
                    message: "Fitted rigs cannot be moved via MultiAdd".into(),
                });
            }
            if snap.structures.contains_key(&item.location_id)
                && matches!(item.flag as u16, 11..=34 | 164..=171)
            {
                return Err(BridgeRpcError {
                    code: "InvalidLocation".into(),
                    message: "Structure modules must be unfitted individually".into(),
                });
            }
            if matches!(item.flag as u16, 11..=34 | 125..=132) || matches!(flag_u16, 11..=34 | 125..=132) {
                if ch_dock > 0 {
                    crate::services::structure_acl_defense_rules::validate_docked_ship_fitting_facility_in_snap(
                        ctx, snap, ch_dock,
                    )?;
                }
                if matches!(item.flag as u16, 11..=34 | 125..=132) {
                    fitted_unfit_ids.push((item.location_id, item_id, item.flag as u16));
                }
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
            if !src_is_struct_fuel && !crate::services::inventory_bound::is_inventory_location_colocated(
                snap, session, ch, &pre_balls, item.location_id,
            ) {
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
        if ! near_service {
            return Err(BridgeRpcError {
                code: "CannotUnfitInSpace".into(),
                message: "Unfitting modules requires being docked at a station/structure or within range of a Mobile Depot or capital SMA".into(),
            });
        }
    }
    for &(old_loc, item_id, old_flag) in &fitted_unfit_ids {
        if (125..=129).contains(&old_flag) {
            if let Ok(mut check_fit) = ctx.get_or_rebuild_ship_fitting(char_id, old_loc) {
                for &(_, other_id, other_flag) in &fitted_unfit_ids {
                    if other_id != item_id && (11..=34).contains(&other_flag) {
                        let _ = check_fit.modules.remove(&other_id);
                    }
                }
                if let Err(msg) = check_fit.unfit_module(item_id, &ctx.sde) {
                    return Err(BridgeRpcError {
                        code: "SubsystemHasFittedModules".into(),
                        message: msg,
                    });
                }
            }
        }
    }
    let caller_corp_id = session
        .corporation_id
        .or_else(|| {
            ctx.db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id))
        })
        .unwrap_or(0);
    let unfitted_active_cloaks: Vec<(i64, i64, u32, u32)> =
        if !fitted_unfit_ids.is_empty() && session.station_id.is_none() && session.structure_id.is_none() {
            ctx.db.with_snapshot(|snap| {
                fitted_unfit_ids
                    .iter()
                    .filter_map(|&(old_loc, item_id, old_flag)| {
                        if !(11..=34).contains(&old_flag) || !snap.module_active_states.contains(&item_id) {
                            return None;
                        }
                        let it = snap.items.get(&item_id)?;
                        if !ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 330) {
                            return None;
                        }
                        let ship_type_id = snap.items.get(&old_loc).map(|s| s.type_id).unwrap_or(0);
                        Some((old_loc, item_id, it.type_id, ship_type_id))
                    })
                    .collect()
            })
        } else {
            Vec::new()
        };
    let mut precomputed_cloak_delays: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();
    if !unfitted_active_cloaks.is_empty() {
        let cloaking_skill = ctx.character_skill_levels(char_id).get(&11579).copied().unwrap_or(0) as u8;
        for &(old_ship, cloak_item_id, cloak_type_id, ship_type_id) in &unfitted_active_cloaks {
            let delay_s = crate::services::space_navigation_advanced::compute_decloak_targeting_delay_for_module(
                ctx,
                char_id,
                old_ship,
                ship_type_id,
                Some(cloak_item_id),
                cloak_type_id,
                cloaking_skill,
            );
            let entry = precomputed_cloak_delays.entry(old_ship).or_insert(0.0);
            *entry = entry.max(delay_s);
        }
    }
    let (moved, old_containers) = ctx.db.try_with_mut_snapshot(|snap| {
        let mut moved = Vec::with_capacity(item_ids.len());
        let mut old_containers = HashSet::new();
        let dst_struct_corp = crate::services::structure_acl_defense_rules::is_structure_fuel_flag(flag)
            .then(|| snap.structures.get(&new_loc).map(|st| st.owner_corp_id))
            .flatten()
            .unwrap_or(0);
        for &item_id in &item_ids {
            let it = snap.items.get_mut(&item_id).ok_or_else(|| BridgeRpcError {
                code: "ItemNotFound".into(),
                message: format!("Item {item_id} not found"),
            })?;
            let old_flag = it.flag;
            let old_flag_u16 = old_flag as u16;
            let old_loc = it.location_id;
            let it_tid = it.type_id;
            let it_qty = it.quantity;
            if old_loc != new_loc {
                old_containers.insert((old_loc, item_id, it_tid, it_qty));
            }
            it.location_id = new_loc;
            it.flag = flag;
            if dst_struct_corp > 0 {
                it.owner_id = dst_struct_corp;
            } else if (116..=122).contains(&flag_u16) && caller_corp_id > 0 {
                it.owner_id = caller_corp_id;
            } else if (116..=122).contains(&old_flag_u16)
                || old_flag_u16 == 133
                || matches!(flag, InventoryFlag::Hangar | InventoryFlag::Cargo)
                || is_ship_hold
                || looted_from_space_ids.contains(&item_id)
            {
                it.owner_id = char_id;
            }
            it.version += 1;
            snap.module_online_states.remove(&item_id);
            snap.module_active_states.remove(&item_id);
            snap.module_active_charges.remove(&item_id);
            let (fin_opt, _) = crate::services::structure_acl_defense_rules::finalize_fuel_bay_move_in_snap(
                ctx, snap, item_id, old_loc, old_flag, new_loc, flag, char_id, caller_corp_id,
            );
            moved.push((fin_opt.map(|r| r.item_id).unwrap_or(item_id), it_tid, it_qty));
        }
        Ok((moved, old_containers))
    })?;
    let mut despawn_set = HashSet::new();
    for (old_cont, item_id, it_tid, it_qty) in old_containers {
        fitting_containers_ships::record_container_take_log(ctx, char_id, item_id, it_tid, it_qty, old_cont);
        despawn_set.insert(old_cont);
    }
    for &(item_id, it_tid, it_qty) in &moved {
        fitting_containers_ships::record_container_add_log(ctx, char_id, item_id, it_tid, it_qty, new_loc);
    }
    for old_cont in despawn_set {
        let _ = despawn_empty_jetcan_if_needed(ctx, session.solar_system_id, old_cont).await;
    }
    let moved: Vec<i64> = moved.into_iter().map(|(id, _, _)| id).collect();
    let mut source_ships = HashSet::new();
    for &(old_loc, item_id, _) in &fitted_unfit_ids {
        ctx.remove_projected_ewar_by_module(item_id);
        if let Some(sys_id) = session.solar_system_id {
            let _ = ctx.systems.get_or_spawn(sys_id).remove_module_cycle(old_loc, item_id).await;
        }
        source_ships.insert(old_loc);
    }
    for old_ship in source_ships {
        ctx.invalidate_ship_fitting(old_ship);
        let _ = ctx.get_or_rebuild_ship_fitting(char_id, old_ship);
        if session.station_id.is_none() && session.structure_id.is_none() {
            if let Some(sys_id) = session.solar_system_id {
                if let Some(&delay_s) = precomputed_cloak_delays.get(&old_ship) {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    if let Ok(Some(mut cs)) = sys.get_combat_state(old_ship).await {
                        cs.cloak_configured_targeting_delay_s =
                            cs.cloak_configured_targeting_delay_s.max(delay_s);
                        let _ = sys.set_combat_state(old_ship, cs).await;
                    }
                }
                crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(
                    ctx, char_id, sys_id, old_ship,
                )
                .await;
            }
        }
    }
    if is_ship_hold || matches!(flag_u16, 90 | 155) {
        ctx.invalidate_ship_fitting(new_loc);
    }
    if any_foreign_theft {
        apply_space_loot_suspect_flag(ctx, session, char_id);
    }
    Ok(PyRep::keyval([
        ("movedCount", PyRep::Integer(moved.len() as i64)),
        ("items", PyRep::List(moved.into_iter().map(PyRep::Integer).collect())),
        ("suspectFlagged", PyRep::Bool(any_foreign_theft)),
    ]))
}

pub(crate) fn apply_space_loot_suspect_flag(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
) {
    let until = ctx.now_unix_secs().saturating_add(900);
    ctx.crimewatch
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_create(char_id)
        .suspect_flag_until_unix = until;
    let sys_id = session.solar_system_id.or_else(|| {
        ctx.db
            .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
            .filter(|&s| s > 0)
    });
    if let Some(sys_id) = sys_id {
        // V24 client derives `OnSuspectsAndCriminalsUpdate` from `OnSystemCriminalFlagUpdates`.
        let _ = ctx.notify_system_criminal_flag_updates(sys_id, &[], &[char_id], &[]);
    }
}

/// Evaluates CrimeWatch theft legality when taking an item from a space wreck (`group_id == 186`, any flag)
/// or jetcan (`type_id == 23`) (`feat:lifecycle.wreck-take_item`).
pub(crate) fn evaluate_space_wreck_or_jetcan_loot(
    ctx: &ServerContext,
    char_id: i64,
    src_item: &eve_db::InventoryItemRecord,
) -> Result<(bool, bool), BridgeRpcError> {
    let now_unix = ctx.now_unix_secs();
    let info = ctx.db.with_snapshot(|snap| {
        let ch_sys = snap
            .characters
            .get(&char_id)
            .map(|c| c.solar_system_id as i64)
            .unwrap_or(0);
        let cont = snap.items.get(&src_item.location_id)?;
        let is_wreck_or_mtu = ctx
            .sde
            .types
            .get(&cont.type_id)
            .is_some_and(|t| matches!(t.group_id, 186 | 1250));
        if !(cont.type_id == 23 || is_wreck_or_mtu) || cont.location_id != ch_sys || ch_sys <= 0 {
            return None;
        }
        let owner_id = if cont.owner_id != 0 {
            cont.owner_id
        } else {
            src_item.owner_id
        };
        let c1 = snap.characters.get(&char_id).map(|c| c.corporation_id);
        let c2 = snap.characters.get(&owner_id).map(|c| c.corporation_id);
        let same_player_corp = (c1.is_some() && c1 == c2 && c1.unwrap_or(0) > 2_000_000)
            || (owner_id > 2_000_000 && c1 == Some(owner_id));
        let same_fleet = snap
            .fleets
            .values()
            .any(|f| f.members.contains(&char_id) && f.members.contains(&owner_id))
            || snap
                .characters
                .get(&char_id)
                .and_then(|c| c.fleet_id)
                .is_some_and(|fid| snap.characters.get(&owner_id).and_then(|c| c.fleet_id) == Some(fid));
        Some((
            cont.item_id,
            owner_id,
            cont.owner_id == 0,
            same_player_corp || same_fleet,
        ))
    });
    let Some((cont_id, owner_id, snap_abandoned, same_corp_or_fleet)) = info else {
        return Ok((false, false));
    };
    let owner_is_criminal = ctx
        .crimewatch
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .states
        .get(&owner_id)
        .is_some_and(|st| st.is_criminal(now_unix));
    let rt_abandoned = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.container_lifetimes
            .get(&cont_id)
            .is_some_and(|c| c.is_abandoned)
    });
    let is_abandoned = snap_abandoned || owner_is_criminal || rt_abandoned;
    if owner_id == char_id || is_abandoned || same_corp_or_fleet {
        return Ok((true, false));
    }
    let safety = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.safety_levels
            .get(&char_id)
            .copied()
            .unwrap_or(crate::services::combat_crimewatch::SafetyLevel::Red)
    });
    if safety == crate::services::combat_crimewatch::SafetyLevel::Green {
        return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
    }
    Ok((true, true))
}

pub(super) async fn handle_get_container_contents(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(container_id) = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| bound_entry.map(|b| b.object_id))
    else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "GetContainerContents requires containerID".into(),
        });
    };
    let supplied_pw = kwargs
        .get("password")
        .and_then(|v| v.as_str())
        .or_else(|| args.get(1).and_then(|a| a.as_str()));
    {
        let mut st = fitting_containers_ships::read_container_ship_state(ctx);
        if let Some(req_pw) = st.container_passwords.get(&container_id).cloned() {
            if !req_pw.is_empty() && !st.unlocked_containers.contains(&(container_id, char_id)) {
                if supplied_pw == Some(req_pw.as_str()) {
                    st.unlocked_containers.insert((container_id, char_id));
                } else {
                    return Err(BridgeRpcError {
                        code: "ALSCPasswordRequired".into(),
                        message: format!("Password required to open container {container_id}"),
                    });
                }
            }
        }
        let is_alsc = ctx.db.with_snapshot(|snap| {
            snap.items.get(&container_id).is_some_and(|it| {
                ctx.sde
                    .types
                    .get(&it.type_id)
                    .is_some_and(|t| t.group_id == 448)
            })
        });
        if is_alsc {
            let now_unix = ctx.now_unix_secs();
            st.alsc_logs
                .entry(container_id)
                .or_default()
                .push(fitting_containers_ships::AlscLogEntry {
                    timestamp_unix: now_unix,
                    actor_id: char_id,
                    action: "ALSCActionOpen".into(),
                    item_id: container_id,
                    item_type_id: 0,
                    quantity: 0,
                });
        }
        fitting_containers_ships::save_container_ship_state(ctx, &st);
    }
    ctx.get_container_contents(session, bound_entry, char_id, container_id)
        .await
}

pub(super) fn handle_list(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(bound_loc_id) = bound_entry
        .map(|b| b.object_id)
        .or(session.station_id)
        .or(session.structure_id)
        .or(session.ship_id)
    else {
        return Err(BridgeRpcError {
            code: "InvalidLocation".into(),
            message: "invbroker.List requires a bound inventory location or active station/ship".into(),
        });
    };
    if ((60_000_000..=69_999_999).contains(&bound_loc_id) || bound_loc_id >= 1_000_000_000_000)
        && session.station_id != Some(bound_loc_id)
        && session.structure_id != Some(bound_loc_id)
    {
        return Err(BridgeRpcError {
            code: "BoundObjectOwnershipViolation".into(),
            message: format!("Character {char_id} is not docked at bound inventory location {bound_loc_id}"),
        });
    }
    let list_flag_filter = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()))
        .filter(|&f| f > 0);
    if let Some(flag_val) = list_flag_filter {
        if (116..=122).contains(&flag_val) {
            let corp_id = session.corporation_id.or_else(|| {
                ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id))
            }).unwrap_or(0);
            let (is_ceo, roles) = get_effective_corp_roles_at_location(ctx, corp_id, char_id, Some(bound_loc_id));
            let div_idx = (flag_val - 116) as u32;
            let req_query_role = 1u64 << (20 + div_idx);
            if !is_ceo && !eve_db::corp_roles::has_role(roles, req_query_role) {
                return Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: format!("Corporation role denied for division {} query", div_idx + 1),
                });
            }
        }
    }
    {
        let mut st = fitting_containers_ships::read_container_ship_state(ctx);
        if let Some(req_pw) = st.container_passwords.get(&bound_loc_id).cloned() {
            if !req_pw.is_empty()
                && !st.unlocked_containers.contains(&(bound_loc_id, char_id))
                && !st.unlocked_containers.contains(&(char_id, bound_loc_id))
            {
                let supplied_pw = kwargs
                    .get("password")
                    .and_then(|v| v.as_str())
                    .or_else(|| args.iter().find_map(|a| a.as_str()));
                if supplied_pw == Some(req_pw.as_str()) {
                    st.unlocked_containers.insert((bound_loc_id, char_id));
                    fitting_containers_ships::save_container_ship_state(ctx, &st);
                } else {
                    return Err(BridgeRpcError {
                        code: "ALSCPasswordRequired".into(),
                        message: format!("Password required to list contents of container {bound_loc_id}"),
                    });
                }
            }
        }
    }
    let unit_check: Result<bool, BridgeRpcError> = ctx.db.with_snapshot(|snap| {
        if let Some(it) = snap.items.get(&bound_loc_id) {
            let cat = ctx.sde.types.get(&it.type_id).map(|t| t.category_id).unwrap_or(0);
            let grp = ctx.sde.types.get(&it.type_id).map(|t| t.group_id).unwrap_or(0);
            let is_personal = cat == 22 || grp == 340;
            let is_unit = cat == 22 || matches!(grp, 12 | 340 | 448 | 649) || it.type_id == 23;
            let is_fleet_auth = !is_personal
                && snap.fleets.values().any(|f| f.members.contains(&char_id) && f.members.contains(&it.owner_id))
                && crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.authorized_shipping_storage.contains(&(bound_loc_id, char_id))
                });
            let is_auth = session.has_gm_role() || it.owner_id == char_id || it.owner_id == 0 || is_fleet_auth;
            if !is_unit || !is_auth {
                return Ok(false);
            }
            if (30_000_000..=39_999_999).contains(&it.location_id) && !session.has_gm_role() {
                let sys_id = session.solar_system_id.unwrap_or(0);
                if session.station_id.is_some()
                    || session.structure_id.is_some()
                    || sys_id == 0
                    || it.location_id != sys_id as i64
                {
                    return Err(BridgeRpcError {
                        code: "NotInSameSolarSystem".into(),
                        message: format!("Caller must be in space in system {} to list container {bound_loc_id}", it.location_id),
                    });
                }
                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                let ship_id = session.ship_id.unwrap_or(0);
                let s_pos = sys_handle
                    .get_ball_position_sync(ship_id)
                    .or_else(|| snap.item_positions.get(&ship_id).copied().map(glam::DVec3::from));
                let u_pos = sys_handle
                    .get_ball_position_sync(bound_loc_id)
                    .or_else(|| snap.item_positions.get(&bound_loc_id).copied().map(glam::DVec3::from));
                let max_range = if cat == 22 { 5_000.0 } else { 2_500.0 };
                if let Some((s, u)) = s_pos.zip(u_pos) {
                    let ship_r = snap
                        .items
                        .get(&ship_id)
                        .and_then(|si| ctx.sde.types.get(&si.type_id))
                        .map(|t| t.radius)
                        .unwrap_or(0.0);
                    let cont_r = ctx.sde.types.get(&it.type_id).map(|t| t.radius).unwrap_or(0.0);
                    let edge_dist = (s.distance(u) - ship_r - cont_r).max(0.0);
                    if edge_dist > max_range {
                        return Err(BridgeRpcError {
                            code: "OutOfRange".into(),
                            message: format!("Container {bound_loc_id} is out of range ({edge_dist:.1}m > {max_range:.0}m)"),
                        });
                    }
                } else if it.owner_id != char_id {
                    return Ok(false);
                }
            }
            Ok(true)
        } else {
            Ok(false)
        }
    });
    let is_deployable_or_shipping_unit = unit_check?;
    let rows = if is_deployable_or_shipping_unit {
        ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| i.location_id == bound_loc_id && list_flag_filter.map_or(true, |f| (i.flag as i64) == f))
                .map(|i| item_to_packed_cells(ctx, i))
                .collect()
        })
    } else if let Some(flag_val) = list_flag_filter {
        if (116..=122).contains(&flag_val) {
            let corp_id = session.corporation_id.or_else(|| {
                ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id))
            }).unwrap_or(0);
            ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| i.location_id == bound_loc_id && i.owner_id == corp_id && (i.flag as i64) == flag_val)
                    .map(|i| item_to_packed_cells(ctx, i))
                    .collect()
            })
        } else {
            ctx.db
                .get_items_for_character(char_id)
                .into_iter()
                .filter(|i| i.location_id == bound_loc_id && (i.flag as i64) == flag_val)
                .map(|i| item_to_packed_cells(ctx, &i))
                .collect()
        }
    } else {
        ctx.db
            .get_items_for_character(char_id)
            .into_iter()
            .filter(|i| i.location_id == bound_loc_id)
            .map(|i| item_to_packed_cells(ctx, &i))
            .collect()
    };
    Ok(DBRowDescriptor::inv_item_header().build_crowset(rows))
}

/// Immediately removes an empty jettisoned cargo container (`type_id == 23`) from inventory,
/// `container_lifetimes`, and the solar system `Ballpark`, broadcasting `RemoveBall` with the
/// ballpark's current destiny tick stamp (`feat:lifecycle.jetcan-empty`).
pub async fn despawn_empty_jetcan_if_needed(
    ctx: &ServerContext,
    sys_id: Option<u32>,
    cont_id: i64,
) -> bool {
    if cont_id <= 0 {
        return false;
    }
    let resolved_sys_opt = ctx.db.with_mut_snapshot(|snap| {
        let is_empty_jetcan = snap
            .items
            .get(&cont_id)
            .is_some_and(|c| c.type_id == 23 && c.flag == InventoryFlag::None)
            && !snap.items.values().any(|i| i.location_id == cont_id);
        if !is_empty_jetcan {
            return None;
        }
        let loc_sys = snap
            .items
            .remove(&cont_id)
            .and_then(|c| u32::try_from(c.location_id).ok());
        snap.item_positions.remove(&cont_id);
        snap.sp6_lifecycle.space_object_meta.remove(&cont_id);
        snap.sp6_lifecycle.ship_combat_hp.remove(&cont_id);
        Some(sys_id.or(loc_sys))
    });
    let Some(sys_opt) = resolved_sys_opt else {
        return false;
    };
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.container_lifetimes.remove(&cont_id);
    });
    if let Some(sid) = sys_opt {
        let sys_handle = ctx.systems.get_or_spawn(sid);
        let stamp = sys_handle
            .get_ball_snapshot(cont_id)
            .await
            .ok()
            .flatten()
            .map(|(t, _)| t as i64)
            .unwrap_or(1);
        let _ = sys_handle.remove_ball(cont_id).await;
        ctx.notify(
            crate::router::NotificationTarget::SolarSystem(sid),
            "DoDestinyUpdate",
            "clientID",
            vec![
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(stamp),
                    PyRep::Tuple(vec![
                        PyRep::String("RemoveBall".into()),
                        PyRep::Tuple(vec![PyRep::Integer(cont_id)]),
                    ]),
                ])]),
                PyRep::Bool(false),
            ],
        );
    }
    true
}
