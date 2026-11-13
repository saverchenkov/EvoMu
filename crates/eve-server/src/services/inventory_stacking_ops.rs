//! `inventory_stacking_ops` — `SplitStack`, `MultiMerge`, and `StackAll` validation & execution (`#487`).

#![forbid(unsafe_code)]

use crate::router::{context::BoundObjectEntry, ServerContext};
use crate::services::inventory_bound;
use crate::services::inventory_fitting::{
    fitting_containers_ships, get_effective_corp_roles_at_location,
};
use eve_db::{InventoryFlag, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

fn parse_split_args(
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<(Option<i64>, i64, i32), BridgeRpcError> {
    let rpc = crate::router::RpcArgs::new(args, kwargs);
    let (loc_hint, item_id, raw_qty_opt) = if args.len() >= 3 {
        (
            rpc.opt_positive_i64(0, "locationID")?,
            rpc.req_positive_i64(1, "itemID")?,
            rpc.opt_i64(2, "qty")?,
        )
    } else {
        let loc = bound_entry.map(|b| b.object_id).filter(|&id| id > 0);
        let iid = rpc.req_positive_i64(0, "itemID")?;
        let qty = if args.get(1).is_some() {
            rpc.opt_i64(1, "qty")?
        } else if kwargs.contains_key("qty") {
            rpc.opt_i64(1, "qty")?
        } else if kwargs.contains_key("quantity") {
            rpc.opt_i64(1, "quantity")?
        } else {
            Some(1)
        };
        (loc, iid, qty)
    };
    let split_quantity = raw_qty_opt
        .and_then(|q| i32::try_from(q).ok())
        .filter(|&q| q > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "SplitFailed".into(),
            message: "Split quantity must be a positive 32-bit integer".into(),
            client_error: None,
        })?;
    if item_id <= 0 {
        return Err(BridgeRpcError {
            code: "SplitFailed".into(),
            message: "SplitStack requires a valid positive itemID".into(),
            client_error: None,
        });
    }
    Ok((loc_hint, item_id, split_quantity))
}

fn validate_split_stack_access(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    loc_hint: Option<i64>,
    item_id: i64,
    kwargs: &HashMap<String, PyRep>,
) -> Result<i64, BridgeRpcError> {
    let cs_state = fitting_containers_ships::read_container_ship_state(ctx);
    let loaded_tube_items: HashSet<i64> = ctx
        .fighter_tube_loads
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .copied()
        .collect();
    let pre_balls = if tokio::runtime::Handle::try_current().is_ok() {
        crate::services::structure_acl_defense_rules::snapshot_system_balls_for_inventory(
            ctx, session, char_id,
        )
    } else {
        HashMap::new()
    };
    let provided_pw = kwargs.get("password").and_then(|v| v.as_str());

    let (effective_owner_id, newly_unlocked) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
            client_error: None,
        })?;
        let item = snap.items.get(&item_id).ok_or_else(|| BridgeRpcError {
            code: "SplitFailed".into(),
            message: format!("Item {item_id} not found"),
            client_error: None,
        })?;

        if cs_state.alsc_locked_items.contains(&item_id) {
            return Err(BridgeRpcError {
                code: "ALSCItemLocked".into(),
                message: "Cannot split an item locked in an Audit Log Secure Container".into(),
                client_error: None,
            });
        }
        if loaded_tube_items.contains(&item_id) || matches!(item.flag as u16, 159..=163) {
            return Err(BridgeRpcError {
                code: "SplitFailed".into(),
                message: "Cannot split fighters loaded in an active fighter tube".into(),
                client_error: None,
            });
        }
        if matches!(item.flag, InventoryFlag::Locked | InventoryFlag::Impounded)
            || matches!(item.flag as u16, 36 | 63 | 72)
            || item.type_id == 3_468
            || snap.is_item_locked_in_contract_or_job(item_id)
        {
            return Err(BridgeRpcError {
                code: "SplitFailed".into(),
                message: format!("Item {item_id} is locked, impounded, or in Asset Safety"),
                client_error: None,
            });
        }
        let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        let in_asset_safety = ext.asset_safety_wraps.values().any(|w| {
            w.wrap_id == item_id
                || w.wrap_id == item.location_id
                || w.item_ids.contains(&item_id)
        });
        if in_asset_safety {
            return Err(BridgeRpcError {
                code: "SplitFailed".into(),
                message: format!("Item {item_id} is sealed inside an Asset Safety wrap"),
                client_error: None,
            });
        }

        let is_corp_item = ch.corporation_id > 0 && item.owner_id == ch.corporation_id;
        if item.owner_id != char_id && !is_corp_item {
            return Err(BridgeRpcError {
                code: "SplitFailed".into(),
                message: format!("Character {char_id} does not own stack {item_id}"),
                client_error: None,
            });
        }
        if is_corp_item {
            crate::services::inventory_bound::validate_corp_source_item_take_access_in_snap(
                ctx,
                snap,
                char_id,
                ch.corporation_id,
                item,
            )?;
        }

        let mut unlock_ids = Vec::new();
        let mut cur_loc = item.location_id;
        for _ in 0..8 {
            let Some(anc) = snap.items.get(&cur_loc) else {
                break;
            };
            if cs_state.alsc_locked_items.contains(&anc.item_id) {
                return Err(BridgeRpcError {
                    code: "ALSCItemLocked".into(),
                    message: "Parent Audit Log Secure Container is locked".into(),
                    client_error: None,
                });
            }
            if anc.type_id == 3_468 {
                return Err(BridgeRpcError {
                    code: "SplitFailed".into(),
                    message: "Cannot split items inside a sealed Plastic Wrap".into(),
                    client_error: None,
                });
            }
            if matches!(anc.flag, InventoryFlag::Locked | InventoryFlag::Impounded)
                || matches!(anc.flag as u16, 36 | 63 | 72)
                || snap.is_item_locked_in_contract_or_job(anc.item_id)
            {
                return Err(BridgeRpcError {
                    code: "SplitFailed".into(),
                    message: "Parent container is locked, impounded, or in Asset Safety".into(),
                    client_error: None,
                });
            }
            if let Some(req_pw) = cs_state.container_passwords.get(&anc.item_id) {
                if !req_pw.is_empty()
                    && !cs_state.unlocked_containers.contains(&(anc.item_id, char_id))
                {
                    if provided_pw == Some(req_pw.as_str()) {
                        unlock_ids.push(anc.item_id);
                    } else {
                        return Err(BridgeRpcError {
                            code: "ALSCPasswordRequired".into(),
                            message: format!(
                                "Password required to access Secure Container {}",
                                anc.item_id
                            ),
                            client_error: None,
                        });
                    }
                }
            }
            if is_corp_item
                && anc.owner_id == ch.corporation_id
                && (116..=122).contains(&(anc.flag as u16))
            {
                let div_idx = (anc.flag as u16 - 116) as usize;
                let (is_ceo, role_mask) = get_effective_corp_roles_at_location(
                    ctx,
                    ch.corporation_id,
                    char_id,
                    Some(anc.location_id),
                );
                let container_take = eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[div_idx];
                if !is_ceo
                    && !eve_db::corp_roles::has_role(role_mask, eve_db::corp_roles::ROLE_DIRECTOR)
                    && (role_mask & container_take) == 0
                {
                    return Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: format!(
                            "Character {char_id} lacks container take role in division {}",
                            div_idx + 1
                        ),
                        client_error: None,
                    });
                }
            }
            cur_loc = anc.location_id;
        }

        if let Some(sess_dock) = session.station_id.or(session.structure_id) {
            if ch
                .station_id
                .or(ch.structure_id)
                .is_some_and(|cd| cd > 0 && cd != sess_dock)
            {
                return Err(BridgeRpcError {
                    code: "SplitFailed".into(),
                    message: "Session docked location does not match character location".into(),
                    client_error: None,
                });
            }
        }
        if let Some(hint_loc) = loc_hint {
            if hint_loc != item.location_id && hint_loc != cur_loc {
                return Err(BridgeRpcError {
                    code: "SplitFailed".into(),
                    message: format!("Item {item_id} is not located at {hint_loc}"),
                    client_error: None,
                });
            }
        }
        let colocated = crate::services::inventory_bound::is_inventory_location_colocated(
            snap,
            session,
            ch,
            &pre_balls,
            item.location_id,
        ) || (ch.station_id.is_none()
            && ch.structure_id.is_none()
            && session.solar_system_id.is_none()
            && session
                .station_id
                .or(session.structure_id)
                .is_some_and(|sd| sd > 0 && sd == cur_loc));
        if !colocated {
            return Err(BridgeRpcError {
                code: "SplitFailed".into(),
                message: format!("Item {item_id} is not physically co-located with pilot"),
                client_error: None,
            });
        }

        Ok((item.owner_id, unlock_ids))
    })?;

    if !newly_unlocked.is_empty() {
        fitting_containers_ships::write_container_ship_state(ctx, |st| {
            for &cid in &newly_unlocked {
                st.unlocked_containers.insert((cid, char_id));
            }
        });
    }
    Ok(effective_owner_id)
}

pub(in crate::services) fn handle_stack_ops(
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
            let (loc_hint, item_id, split_quantity) =
                match parse_split_args(bound_entry, args, kwargs) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            let owner_id = match validate_split_stack_access(
                ctx, session, char_id, loc_hint, item_id, kwargs,
            ) {
                Ok(oid) => oid,
                Err(e) => return Some(Err(e)),
            };
            match ctx.db.split_stack(owner_id, item_id, split_quantity) {
                Ok((orig, new_i)) => {
                    let old_orig_qty = (orig.quantity + new_i.quantity) as i64;
                    inventory_bound::emit_inv_item_change(
                        ctx,
                        char_id,
                        &orig,
                        vec![
                            // Omit ixQuantity (5): V24.01 invCache uses ixStackSize (9).
                            (9, PyRep::Integer(old_orig_qty)),
                        ],
                    );
                    inventory_bound::emit_inv_item_change(
                        ctx,
                        char_id,
                        &new_i,
                        vec![(3, PyRep::Integer(0))],
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
                    client_error: None,
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
                                let qty_opt = match seq.get(2).and_then(|v| v.as_i64()) {
                                    Some(raw_q) => match i32::try_from(raw_q) {
                                        Ok(q) => Some(q),
                                        Err(_) => {
                                            return Some(Err(BridgeRpcError {
                                                code: "StackFailed".into(),
                                                message: "Merge quantity overflows i32".into(),
                                                client_error: None,
                                            }));
                                        }
                                    },
                                    None => None,
                                };
                                ops.push((src_id, dst_id, qty_opt));
                            }
                        }
                    }
                }
                Some(PyRep::Integer(dst_id)) => {
                    let src_id = match crate::router::RpcArgs::positional(args).req_positive_i64(1, "srcID") {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    let qty_opt = match args.get(2).and_then(|a| a.as_i64()) {
                        Some(raw_q) => match i32::try_from(raw_q) {
                            Ok(q) => Some(q),
                            Err(_) => {
                                return Some(Err(BridgeRpcError {
                                    code: "StackFailed".into(),
                                    message: "Merge quantity overflows i32".into(),
                                    client_error: None,
                                }));
                            }
                        },
                        None => None,
                    };
                    ops.push((src_id, *dst_id, qty_opt));
                }
                _ => {}
            }
            if ops.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "StackFailed".into(),
                    message: "MultiMerge requires at least one (sourceItemID, destItemID, qty) merge operation".into(),
                    client_error: None,
                }));
            }
            enum MergeChangeEvent {
                Partial {
                    updated_src: eve_db::InventoryItemRecord,
                    old_src_qty: i32,
                    updated_dst: eve_db::InventoryItemRecord,
                    old_dst_qty: i32,
                },
                Full {
                    deleted_src: eve_db::InventoryItemRecord,
                    updated_dst: eve_db::InventoryItemRecord,
                    old_dst_qty: i32,
                },
            }
            let provided_pw = kwargs.get("password").and_then(|v| v.as_str());
            let loaded_tube_items: HashSet<i64> = ctx
                .fighter_tube_loads
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .copied()
                .collect();
            let tx_res = ctx.db.try_with_mut_snapshot(|guard| {
                let cs_state_merge =
                    fitting_containers_ships::read_container_ship_state_snap(guard);
                let ext =
                    crate::services::station_structure::structure_upwell_ops::load_ext_state(
                        guard,
                    );
                let mut events = Vec::with_capacity(ops.len());
                for &(src_id, dst_id, qty_opt) in &ops {
                    if cs_state_merge.alsc_locked_items.contains(&src_id)
                        || cs_state_merge.alsc_locked_items.contains(&dst_id)
                    {
                        return Err(BridgeRpcError {
                            code: "ALSCItemLocked".into(),
                            message: "Cannot merge items locked in an Audit Log Secure Container"
                                .into(),
                            client_error: None,
                        });
                    }
                    if src_id == dst_id {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot stack an item into itself".into(),
                            client_error: None,
                        });
                    }
                    if guard
                        .item_custom_names
                        .get(&src_id)
                        .is_some_and(|s| s.starts_with("courier_cargo:"))
                        || guard
                            .item_custom_names
                            .get(&dst_id)
                            .is_some_and(|s| s.starts_with("courier_cargo:"))
                    {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot stack courier mission cargo".into(),
                            client_error: None,
                        });
                    }
                    if guard.is_item_locked_in_contract_or_job(src_id)
                        || guard.is_item_locked_in_contract_or_job(dst_id)
                    {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot merge locked items".into(),
                            client_error: None,
                        });
                    }
                    if loaded_tube_items.contains(&src_id) || loaded_tube_items.contains(&dst_id) {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot merge fighters loaded in a fighter tube".into(),
                            client_error: None,
                        });
                    }
                    let src = guard
                        .items
                        .get(&src_id)
                        .ok_or_else(|| BridgeRpcError {
                            code: "StackFailed".into(),
                            message: format!("Source item {src_id} not found"),
                            client_error: None,
                        })?
                        .clone();
                    let tgt = guard
                        .items
                        .get(&dst_id)
                        .ok_or_else(|| BridgeRpcError {
                            code: "StackFailed".into(),
                            message: format!("Destination item {dst_id} not found"),
                            client_error: None,
                        })?
                        .clone();
                    if src.owner_id != char_id || tgt.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Stack ownership mismatch".into(),
                            client_error: None,
                        });
                    }
                    if tgt.type_id != src.type_id
                        || tgt.location_id != src.location_id
                        || tgt.flag != src.flag
                    {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Incompatible stacks for merge".into(),
                            client_error: None,
                        });
                    }
                    if matches!(src.flag, InventoryFlag::Locked | InventoryFlag::Impounded)
                        || matches!(tgt.flag, InventoryFlag::Locked | InventoryFlag::Impounded)
                        || src.type_id == 3_468
                        || tgt.type_id == 3_468
                    {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot merge locked, impounded, or wrapped items".into(),
                            client_error: None,
                        });
                    }
                    if eve_db::WorldStateSnapshot::is_fitted_slot_flag(src.flag)
                        || guard.is_container_non_empty(src_id)
                        || guard.is_container_non_empty(dst_id)
                    {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot stack fitted modules or non-empty containers".into(),
                            client_error: None,
                        });
                    }
                    if src.singleton
                        || src.is_blueprint_copy
                        || !src.dynamic_attributes.is_empty()
                        || src.source_mutaplasmid_id.is_some()
                        || tgt.singleton
                        || tgt.is_blueprint_copy
                        || !tgt.dynamic_attributes.is_empty()
                        || tgt.source_mutaplasmid_id.is_some()
                    {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot stack singleton, blueprint copy, or mutated item"
                                .into(),
                            client_error: None,
                        });
                    }
                    if ext.asset_safety_wraps.values().any(|w| {
                        w.wrap_id == src.location_id
                            || w.item_ids.contains(&src_id)
                            || w.item_ids.contains(&dst_id)
                    }) {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot merge items inside an Asset Safety wrap".into(),
                            client_error: None,
                        });
                    }
                    let mut cur = src.location_id;
                    for _ in 0..8 {
                        let Some(anc) = guard.items.get(&cur) else {
                            break;
                        };
                        if anc.type_id == 3_468 {
                            return Err(BridgeRpcError {
                                code: "StackFailed".into(),
                                message: "Cannot merge items inside a sealed container".into(),
                                client_error: None,
                            });
                        }
                        if cs_state_merge.alsc_locked_items.contains(&anc.item_id) {
                            return Err(BridgeRpcError {
                                code: "ALSCItemLocked".into(),
                                message: "Cannot merge items inside a locked ALSC container".into(),
                                client_error: None,
                            });
                        }
                        if let Some(req_pw) = cs_state_merge.container_passwords.get(&anc.item_id) {
                            if !req_pw.is_empty()
                                && !cs_state_merge
                                    .unlocked_containers
                                    .contains(&(anc.item_id, char_id))
                                && !cs_state_merge
                                    .unlocked_containers
                                    .contains(&(char_id, anc.item_id))
                                && provided_pw != Some(req_pw.as_str())
                            {
                                return Err(BridgeRpcError {
                                    code: "ALSCPasswordRequired".into(),
                                    message: format!(
                                        "Password required to merge items inside container {}",
                                        anc.item_id
                                    ),
                                    client_error: None,
                                });
                            }
                        }
                        cur = anc.location_id;
                    }
                    if guard.is_item_or_ancestor_locked_or_impounded(dst_id)
                        || guard.is_item_or_ancestor_locked_or_impounded(src_id)
                    {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Item is locked or impounded".into(),
                            client_error: None,
                        });
                    }
                    let merge_qty = match qty_opt {
                        Some(req_qty) => {
                            if req_qty <= 0 || req_qty > src.quantity {
                                return Err(BridgeRpcError {
                                    code: "StackFailed".into(),
                                    message: format!(
                                        "Invalid merge quantity {req_qty} for source stack {src_id} (qty {})",
                                        src.quantity
                                    ),
                                    client_error: None,
                                });
                            }
                            req_qty
                        }
                        None => src.quantity,
                    };
                    let new_tgt_qty =
                        tgt.quantity.checked_add(merge_qty).ok_or_else(|| BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Stack quantity overflow".into(),
                            client_error: None,
                        })?;
                    let old_src_qty = src.quantity;
                    let old_dst_qty = tgt.quantity;
                    if merge_qty < old_src_qty {
                        let updated_src = {
                            let src_mut = guard.items.get_mut(&src_id).unwrap();
                            src_mut.quantity -= merge_qty;
                            src_mut.version += 1;
                            src_mut.clone()
                        };
                        let updated_dst = {
                            let tgt_mut = guard.items.get_mut(&dst_id).unwrap();
                            tgt_mut.quantity = new_tgt_qty;
                            tgt_mut.version += 1;
                            tgt_mut.clone()
                        };
                        events.push(MergeChangeEvent::Partial {
                            updated_src,
                            old_src_qty,
                            updated_dst,
                            old_dst_qty,
                        });
                    } else {
                        guard.remove_system_cleanup_item(src_id);
                        let updated_dst = {
                            let tgt_mut = guard.items.get_mut(&dst_id).unwrap();
                            tgt_mut.quantity = new_tgt_qty;
                            tgt_mut.version += 1;
                            tgt_mut.clone()
                        };
                        events.push(MergeChangeEvent::Full {
                            deleted_src: src,
                            updated_dst,
                            old_dst_qty,
                        });
                    }
                }
                Ok(events)
            });
            let events = match tx_res {
                Ok(ev) => ev,
                Err(e) => return Some(Err(e)),
            };
            let mut updated_records = Vec::with_capacity(events.len());
            for ev in events {
                match ev {
                    MergeChangeEvent::Partial {
                        updated_src,
                        old_src_qty,
                        updated_dst,
                        old_dst_qty,
                    } => {
                        inventory_bound::emit_inv_item_change(
                            ctx,
                            char_id,
                            &updated_src,
                            vec![(9, PyRep::Integer(old_src_qty as i64))],
                        );
                        inventory_bound::emit_inv_item_change(
                            ctx,
                            char_id,
                            &updated_dst,
                            vec![(9, PyRep::Integer(old_dst_qty as i64))],
                        );
                        updated_records.push(updated_dst);
                    }
                    MergeChangeEvent::Full {
                        deleted_src,
                        updated_dst,
                        old_dst_qty,
                    } => {
                        inventory_bound::emit_inv_item_deleted(ctx, char_id, &deleted_src);
                        inventory_bound::emit_inv_item_change(
                            ctx,
                            char_id,
                            &updated_dst,
                            vec![(9, PyRep::Integer(old_dst_qty as i64))],
                        );
                        updated_records.push(updated_dst);
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
                    client_error: None,
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

            let alsc_locked_set =
                fitting_containers_ships::read_container_ship_state(ctx).alsc_locked_items;
            let loaded_tube_items: HashSet<i64> = ctx
                .fighter_tube_loads
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .copied()
                .collect();
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
                        && !matches!(item.flag, InventoryFlag::Locked | InventoryFlag::Impounded)
                        && !matches!(item.flag as u16, 36 | 63 | 72 | 159..=163)
                        && !alsc_locked_set.contains(&item.item_id)
                        && !loaded_tube_items.contains(&item.item_id)
                        && !snap.is_item_locked_in_contract_or_job(item.item_id)
                        && !snap
                            .item_custom_names
                            .get(&item.item_id)
                            .is_some_and(|s| s.starts_with("courier_cargo:"))
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
                let (old_dst_qty, src_snaps) = ctx.db.with_snapshot(|snap| {
                    let dst_q = snap.items.get(&dst_id).map(|i| i.quantity).unwrap_or(0);
                    let srcs: Vec<(i64, Option<eve_db::InventoryItemRecord>)> = ids[1..]
                        .iter()
                        .map(|&sid| (sid, snap.items.get(&sid).cloned()))
                        .collect();
                    (dst_q, srcs)
                });
                let mut latest = None;
                for (src_id, old_src_opt) in src_snaps {
                    if let Ok(upd) = ctx.db.stack_items(char_id, dst_id, src_id) {
                        merged_count += 1;
                        if let Some(old_src) = old_src_opt {
                            inventory_bound::emit_inv_item_deleted(ctx, char_id, &old_src);
                        }
                        latest = Some(upd);
                    }
                }
                if let Some(upd) = latest {
                    inventory_bound::emit_inv_item_change(
                        ctx,
                        char_id,
                        &upd,
                        vec![
                            // Omit ixQuantity (5): V24.01 invCache uses ixStackSize (9).
                            (9, PyRep::Integer(old_dst_qty as i64)),
                        ],
                    );
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
