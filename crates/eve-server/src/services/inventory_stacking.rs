//! `inventory_stacking` — Stack, Split, Merge, and StackAll Operations for `invbroker`.

#![forbid(unsafe_code)]

use crate::router::{context::BoundObjectEntry, ServerContext};
use eve_db::{InventoryFlag, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

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
