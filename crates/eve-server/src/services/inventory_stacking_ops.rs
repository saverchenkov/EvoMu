//! `inventory_stacking_ops` — `SplitStack`, `MultiMerge`, and `StackAll` validation & execution (`#487`).

#![forbid(unsafe_code)]

use crate::router::{context::BoundObjectEntry, ServerContext};
use crate::services::inventory_bound;
use crate::services::inventory_fitting::{
    fitting_containers_ships, get_effective_corp_roles_at_location,
};
use eve_db::InventoryFlag;
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

#[derive(Clone, Copy)]
struct ContainerChainPolicy {
    reject_sealed_wrap: bool,
    reject_alsc_locked: bool,
    reject_locked_or_escrow: bool,
    check_corp_take_role: bool,
    locked_err_code: &'static str,
}

struct ContainerChainInfo {
    chain_locs: Vec<i64>,
    root_loc: i64,
}

fn snapshot_pre_balls(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
) -> HashMap<i64, (glam::DVec3, f64)> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return crate::services::structure_acl_defense_rules::snapshot_system_balls_for_inventory(
            ctx, session, char_id,
        );
    }
    let sys_id = session
        .solar_system_id
        .or_else(|| {
            ctx.db.with_snapshot(|s| {
                s.characters
                    .get(&char_id)
                    .map(|c| c.solar_system_id)
                    .filter(|&id| id > 0)
            })
        })
        .unwrap_or(0);
    if sys_id == 0 {
        return HashMap::new();
    }
    let Some(sys_h) = ctx.systems.get_handle(sys_id) else {
        return HashMap::new();
    };
    let Ok(bp) = sys_h.ballpark.read() else {
        return HashMap::new();
    };
    bp.balls
        .iter()
        .map(|(&id, b)| (id, (b.position, b.radius)))
        .collect()
}

fn snapshot_loaded_tube_items(ctx: &ServerContext) -> HashSet<i64> {
    ctx.fighter_tube_loads
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .copied()
        .collect()
}

fn character_or_session_fallback(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
) -> Result<eve_db::CharacterRecord, BridgeRpcError> {
    if let Some(ch) = snap.characters.get(&char_id) {
        return Ok(ch.clone());
    }
    if session
        .station_id
        .or(session.structure_id)
        .or_else(|| session.solar_system_id.map(i64::from))
        .is_none()
    {
        return Err(BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
            client_error: None,
        });
    }
    let mut ch = eve_db::CharacterRecord::default();
    ch.character_id = char_id;
    ch.user_id = session.user_id.unwrap_or(char_id as u64);
    ch.corporation_id = session.corporation_id.unwrap_or(0);
    ch.station_id = session.station_id;
    ch.structure_id = session.structure_id;
    ch.solar_system_id = session.solar_system_id.unwrap_or(0);
    ch.active_ship_id = session.ship_id.unwrap_or(0);
    Ok(ch)
}

fn persist_unlocked_containers_in_snap(
    guard: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    unlock_ids: &[i64],
) {
    if unlock_ids.is_empty() {
        return;
    }
    fitting_containers_ships::write_container_ship_state_snap(guard, |st| {
        for &cid in unlock_ids {
            st.unlocked_containers.insert((cid, char_id));
        }
    });
}

fn parse_opt_positive_id(
    val: Option<&PyRep>,
    err_code: &'static str,
    field_label: &str,
) -> Result<Option<i64>, BridgeRpcError> {
    match val {
        None | Some(PyRep::None) => Ok(None),
        Some(v) => v
            .as_i64()
            .filter(|&id| id > 0)
            .map(Some)
            .ok_or_else(|| BridgeRpcError {
                code: err_code.into(),
                message: format!("{field_label} must be a positive integer"),
                client_error: None,
            }),
    }
}

fn collect_container_chain(
    snap: &eve_db::WorldStateSnapshot,
    start_loc: i64,
    max_hops: usize,
) -> ContainerChainInfo {
    let mut chain_locs = Vec::with_capacity(max_hops + 1);
    let mut cur_loc = start_loc;
    chain_locs.push(cur_loc);
    for _ in 0..max_hops {
        let Some(anc) = snap.items.get(&cur_loc) else {
            break;
        };
        cur_loc = anc.location_id;
        chain_locs.push(cur_loc);
    }
    ContainerChainInfo {
        chain_locs,
        root_loc: cur_loc,
    }
}

fn validate_bound_and_hint_chain(
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    bound_loc: Option<i64>,
    hint_locs: &[Option<i64>],
    chain: &ContainerChainInfo,
    err_code: &'static str,
    subject: &str,
) -> Result<(), BridgeRpcError> {
    let is_upwell_inv_mgr_bound = |bl: i64| -> bool {
        session.station_id.is_none()
            && session.structure_id == Some(chain.root_loc)
            && snap
                .structures
                .get(&chain.root_loc)
                .is_some_and(|st| i64::from(st.solar_system_id) == bl)
    };
    let bound_pos = match bound_loc {
        Some(bl) => Some(
            if let Some(pos) = chain.chain_locs.iter().position(|&id| id == bl) {
                pos
            } else if is_upwell_inv_mgr_bound(bl) {
                // V24 `GetInventoryMgr()` binds to `solar_system_id` when docked in an Upwell
                // structure, acting as the logical parent above `chain.root_loc == structure_id`.
                chain.chain_locs.len()
            } else {
                return Err(BridgeRpcError {
                    code: err_code.into(),
                    message: format!("{subject} is not located within bound inventory {bl}"),
                    client_error: None,
                });
            },
        ),
        None => None,
    };
    for hint in hint_locs.iter().copied().flatten() {
        let Some(hint_pos) = chain.chain_locs.iter().position(|&id| id == hint) else {
            return Err(BridgeRpcError {
                code: err_code.into(),
                message: format!("{subject} is not located at {hint}"),
                client_error: None,
            });
        };
        if let Some(b_pos) = bound_pos {
            if hint_pos > b_pos {
                return Err(BridgeRpcError {
                    code: err_code.into(),
                    message: format!(
                        "{subject} location hint {hint} mismatches bound inventory {}",
                        bound_loc.unwrap_or_default()
                    ),
                    client_error: None,
                });
            }
        }
    }
    Ok(())
}

fn enforce_container_chain_policies(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    cs_state: &fitting_containers_ships::ContainerShipRuntimeState,
    char_id: i64,
    corp_id: i64,
    check_corp_containers: bool,
    chain: &ContainerChainInfo,
    provided_pw: Option<&str>,
    policy: ContainerChainPolicy,
) -> Result<Vec<i64>, BridgeRpcError> {
    let container_hops = &chain.chain_locs[..chain.chain_locs.len().saturating_sub(1)];
    for &anc_id in container_hops {
        let Some(anc) = snap.items.get(&anc_id) else {
            break;
        };
        if policy.check_corp_take_role
            && check_corp_containers
            && corp_id > 0
            && anc.owner_id == corp_id
            && (116..=122).contains(&(anc.flag as u16))
        {
            let div_idx = (anc.flag as u16 - 116) as usize;
            let (is_ceo, role_mask) =
                get_effective_corp_roles_at_location(ctx, corp_id, char_id, Some(anc.location_id));
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
        if policy.reject_alsc_locked && cs_state.alsc_locked_items.contains(&anc.item_id) {
            return Err(BridgeRpcError {
                code: "ALSCItemLocked".into(),
                message: "Parent Audit Log Secure Container is locked".into(),
                client_error: None,
            });
        }
        if policy.reject_sealed_wrap && anc.type_id == 3_468 {
            return Err(BridgeRpcError {
                code: policy.locked_err_code.into(),
                message: "Cannot modify items inside a sealed Plastic Wrap".into(),
                client_error: None,
            });
        }
        if policy.reject_locked_or_escrow
            && (matches!(anc.flag, InventoryFlag::Locked | InventoryFlag::Impounded)
                || matches!(anc.flag as u16, 36 | 63 | 72)
                || snap.is_item_locked_in_contract_or_job(anc.item_id))
        {
            return Err(BridgeRpcError {
                code: policy.locked_err_code.into(),
                message: "Parent container is locked, impounded, or in Asset Safety".into(),
                client_error: None,
            });
        }
    }

    let mut unlock_ids = Vec::new();
    for &anc_id in container_hops {
        if let Some(req_pw) = cs_state.container_passwords.get(&anc_id) {
            if !req_pw.is_empty()
                && !cs_state.unlocked_containers.contains(&(anc_id, char_id))
                && !cs_state.unlocked_containers.contains(&(char_id, anc_id))
            {
                if provided_pw == Some(req_pw.as_str()) {
                    unlock_ids.push(anc_id);
                } else {
                    return Err(BridgeRpcError {
                        code: "ALSCPasswordRequired".into(),
                        message: format!(
                            "Password required to access Secure Container {anc_id}"
                        ),
                        client_error: None,
                    });
                }
            }
        }
    }

    Ok(unlock_ids)
}

fn validate_location_reachability_in_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    ch: &eve_db::CharacterRecord,
    char_id: i64,
    pre_balls: &HashMap<i64, (glam::DVec3, f64)>,
    target_loc: i64,
    chain: &ContainerChainInfo,
    access_err_code: &'static str,
    range_err_code: &'static str,
    check_container_owner: bool,
) -> Result<(), BridgeRpcError> {
    if let Some(sess_dock) = session.station_id.or(session.structure_id) {
        if ch
            .station_id
            .or(ch.structure_id)
            .is_some_and(|cd| cd > 0 && cd != sess_dock)
        {
            return Err(BridgeRpcError {
                code: access_err_code.into(),
                message: "Session docked location does not match character location".into(),
                client_error: None,
            });
        }
    }

    if let Some(st) = snap.structures.get(&chain.root_loc) {
        if st.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "StructureDestroyed".into(),
                message: format!("Structure {} is destroyed", chain.root_loc),
                client_error: None,
            });
        }
        let has_acl = snap.has_structure_acl_access(chain.root_loc, char_id)
            && crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap_at(
                snap,
                chain.root_loc,
                char_id,
                Some(ctx.now_unix()),
            );
        if !has_acl {
            return Err(BridgeRpcError {
                code: access_err_code.into(),
                message: format!(
                    "Character {char_id} lacks structure ACL access at {}",
                    chain.root_loc
                ),
                client_error: None,
            });
        }
    }

    let is_in_space_root = (30_000_000..=39_999_999).contains(&chain.root_loc)
        && !snap.structures.contains_key(&chain.root_loc);
    if check_container_owner || !is_in_space_root {
        for &anc_id in &chain.chain_locs[..chain.chain_locs.len().saturating_sub(1)] {
            if let Some(node) = snap.items.get(&anc_id) {
                let allowed_container = session.has_gm_role()
                    || node.owner_id == char_id
                    || node.owner_id == 0
                    || (is_in_space_root && node.type_id == 23)
                    || (ch.corporation_id > 0 && node.owner_id == ch.corporation_id);
                if !allowed_container {
                    return Err(BridgeRpcError {
                        code: access_err_code.into(),
                        message: format!(
                            "Character {char_id} cannot access inventory {target_loc}"
                        ),
                        client_error: None,
                    });
                }
            }
        }
    }

    // `is_inventory_location_colocated` walks 4 hops; for deeper container chains pass the
    // nearest child below `root_loc` so depth up to `max_hops` resolves against dock/system.
    let coloc_probe = if chain.chain_locs.len() > 4 {
        chain.chain_locs[chain.chain_locs.len().saturating_sub(2)]
    } else {
        target_loc
    };
    let colocated = crate::services::inventory_bound::is_inventory_location_colocated(
        snap,
        session,
        ch,
        pre_balls,
        coloc_probe,
    ) || (ch.station_id.is_none()
        && ch.structure_id.is_none()
        && session.solar_system_id.is_none()
        && session
            .station_id
            .or(session.structure_id)
            .is_some_and(|sd| sd > 0 && sd == chain.root_loc));
    if !colocated {
        return Err(BridgeRpcError {
            code: access_err_code.into(),
            message: format!("Inventory {target_loc} is not physically co-located with pilot"),
            client_error: None,
        });
    }

    let ch_dock = if session.station_id.is_none()
        && session.structure_id.is_none()
        && session.solar_system_id.is_some()
    {
        0
    } else {
        ch.station_id.or(ch.structure_id).unwrap_or(0)
    };
    let ch_sys = if ch.solar_system_id > 0 {
        i64::from(ch.solar_system_id)
    } else {
        session.solar_system_id.map(i64::from).unwrap_or(0)
    };
    let caller_ship_id = session.ship_id.filter(|&id| id > 0).unwrap_or(ch.active_ship_id);

    if ch_dock == 0 && ch_sys > 0 && chain.root_loc == ch_sys {
        if let Some(outer_id) = chain.chain_locs.iter().rev().nth(1).copied() {
            if outer_id != caller_ship_id && outer_id != ch.active_ship_id {
                if let Some(outer_node) = snap.items.get(&outer_id) {
                    let ship_pos = pre_balls.get(&caller_ship_id).map(|b| b.0).or_else(|| {
                        snap.item_positions
                            .get(&caller_ship_id)
                            .copied()
                            .map(glam::DVec3::from)
                    });
                    let cont_pos = pre_balls.get(&outer_id).map(|b| b.0).or_else(|| {
                        snap.item_positions
                            .get(&outer_id)
                            .copied()
                            .map(glam::DVec3::from)
                    });
                    if let Some((s_pos, t_pos)) = ship_pos.zip(cont_pos) {
                        let cat = ctx
                            .sde
                            .types
                            .get(&outer_node.type_id)
                            .map(|t| t.category_id)
                            .unwrap_or(0);
                        let max_range = if cat == 22 { 5_000.0 } else { 2_500.0 };
                        let ship_r = pre_balls
                            .get(&caller_ship_id)
                            .map(|b| b.1)
                            .or_else(|| {
                                snap.items
                                    .get(&caller_ship_id)
                                    .and_then(|si| ctx.sde.types.get(&si.type_id))
                                    .map(|t| t.radius)
                            })
                            .unwrap_or(0.0);
                        let cont_r = pre_balls
                            .get(&outer_id)
                            .map(|b| b.1)
                            .or_else(|| {
                                ctx.sde.types.get(&outer_node.type_id).map(|t| t.radius)
                            })
                            .unwrap_or(0.0);
                        let edge_dist = (s_pos.distance(t_pos) - ship_r - cont_r).max(0.0);
                        if edge_dist > max_range {
                            return Err(BridgeRpcError {
                                code: range_err_code.into(),
                                message: format!(
                                    "Container {outer_id} is out of range ({edge_dist:.1}m > {max_range:.0}m)"
                                ),
                                client_error: None,
                            });
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn execute_split_stack_in_tx(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    bound_loc: Option<i64>,
    loc_hint: Option<i64>,
    item_id: i64,
    split_quantity: i32,
    kwargs: &HashMap<String, PyRep>,
) -> Result<(eve_db::InventoryItemRecord, eve_db::InventoryItemRecord), BridgeRpcError> {
    let loaded_tube_items = snapshot_loaded_tube_items(ctx);
    let pre_balls = snapshot_pre_balls(ctx, session, char_id);
    let provided_pw = kwargs.get("password").and_then(|v| v.as_str());

    ctx.db.try_with_mut_snapshot(|guard| {
        let cs_state = fitting_containers_ships::read_container_ship_state_snap(guard);
        let ch = guard
            .characters
            .get(&char_id)
            .cloned()
            .ok_or_else(|| BridgeRpcError {
                code: "CharacterNotFound".into(),
                message: format!("Character {char_id} not found"),
                client_error: None,
            })?;
        let item = guard
            .items
            .get(&item_id)
            .cloned()
            .ok_or_else(|| BridgeRpcError {
                code: "SplitFailed".into(),
                message: format!("Item {item_id} not found"),
                client_error: None,
            })?;

        let is_corp_item = ch.corporation_id > 0 && item.owner_id == ch.corporation_id;
        if item.owner_id != char_id && !is_corp_item {
            return Err(BridgeRpcError {
                code: "SplitFailed".into(),
                message: format!("Character {char_id} does not own stack {item_id}"),
                client_error: None,
            });
        }

        let chain = collect_container_chain(guard, item.location_id, 8);
        validate_bound_and_hint_chain(
            guard,
            session,
            bound_loc,
            &[loc_hint],
            &chain,
            "SplitFailed",
            &format!("Item {item_id}"),
        )?;
        validate_location_reachability_in_snap(
            ctx,
            guard,
            session,
            &ch,
            char_id,
            &pre_balls,
            item.location_id,
            &chain,
            "SplitFailed",
            "SplitFailed",
            false,
        )?;
        if is_corp_item {
            crate::services::inventory_bound::validate_corp_source_item_take_access_in_snap(
                ctx,
                guard,
                char_id,
                ch.corporation_id,
                &item,
            )?;
        }

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
            || guard.is_item_locked_in_contract_or_job(item_id)
        {
            return Err(BridgeRpcError {
                code: "SplitFailed".into(),
                message: format!("Item {item_id} is locked, impounded, or in Asset Safety"),
                client_error: None,
            });
        }
        let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(guard);
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

        let unlock_ids = enforce_container_chain_policies(
            ctx,
            guard,
            &cs_state,
            char_id,
            ch.corporation_id,
            true,
            &chain,
            provided_pw,
            ContainerChainPolicy {
                reject_sealed_wrap: true,
                reject_alsc_locked: true,
                reject_locked_or_escrow: true,
                check_corp_take_role: true,
                locked_err_code: "SplitFailed",
            },
        )?;

        let split_res = guard
            .split_stack_in_snap(item.owner_id, item_id, split_quantity)
            .map_err(|e| BridgeRpcError {
                code: "SplitFailed".into(),
                message: e.to_string(),
                client_error: None,
            })?;
        // Persist container unlock only after `split_stack_in_snap` succeeds inside the same tx.
        persist_unlocked_containers_in_snap(guard, char_id, &unlock_ids);
        Ok(split_res)
    })
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
            let bound_loc = bound_entry.map(|b| b.object_id).filter(|&id| id > 0);
            let (loc_hint, item_id, split_quantity) =
                match parse_split_args(bound_entry, args, kwargs) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            match execute_split_stack_in_tx(
                ctx,
                session,
                char_id,
                bound_loc,
                loc_hint,
                item_id,
                split_quantity,
                kwargs,
            ) {
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
                Err(e) => Some(Err(e)),
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
            let bound_loc = bound_entry.map(|b| b.object_id).filter(|&id| id > 0);
            let source_container_hint =
                if matches!(args.first(), Some(PyRep::List(_) | PyRep::Tuple(_))) {
                    match parse_opt_positive_id(
                        args.get(1),
                        "StackFailed",
                        "MultiMerge sourceContainerID",
                    ) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    }
                } else {
                    None
                };
            let kw_loc = match parse_opt_positive_id(
                kwargs.get("locationID"),
                "StackFailed",
                "MultiMerge locationID",
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let kw_src_cont = match parse_opt_positive_id(
                kwargs.get("sourceContainerID"),
                "StackFailed",
                "MultiMerge sourceContainerID",
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let provided_pw = kwargs.get("password").and_then(|v| v.as_str());
            let loaded_tube_items = snapshot_loaded_tube_items(ctx);
            let pre_balls = snapshot_pre_balls(ctx, session, char_id);
            let tx_res = ctx.db.try_with_mut_snapshot(|guard| {
                let ch = character_or_session_fallback(guard, session, char_id)?;
                let cs_state_merge =
                    fitting_containers_ships::read_container_ship_state_snap(guard);
                let ext =
                    crate::services::station_structure::structure_upwell_ops::load_ext_state(
                        guard,
                    );
                let lock_index = guard.build_inventory_lock_index();
                let mut events = Vec::with_capacity(ops.len());
                let mut checked_reachability = HashSet::new();
                let mut unlock_ids = Vec::new();
                for &(src_id, dst_id, qty_opt) in &ops {
                    if src_id == dst_id {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot stack an item into itself".into(),
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
                    let chain = collect_container_chain(guard, src.location_id, 8);
                    validate_bound_and_hint_chain(
                        guard,
                        session,
                        bound_loc,
                        &[kw_loc, kw_src_cont, source_container_hint],
                        &chain,
                        "StackFailed",
                        &format!("Items {src_id}/{dst_id}"),
                    )?;
                    if checked_reachability.insert(src.location_id) {
                        validate_location_reachability_in_snap(
                            ctx,
                            guard,
                            session,
                            &ch,
                            char_id,
                            &pre_balls,
                            src.location_id,
                            &chain,
                            "DestinationAccessDenied",
                            "OutOfRange",
                            false,
                        )?;
                    }
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
                    if guard.is_courier_cargo_item(src_id) || guard.is_courier_cargo_item(dst_id) {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot stack courier mission cargo".into(),
                            client_error: None,
                        });
                    }
                    if guard.is_item_escrow_locked_with_index(&lock_index, src_id)
                        || guard.is_item_escrow_locked_with_index(&lock_index, dst_id)
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
                    if eve_db::WorldStateSnapshot::has_locked_flag_or_wrap_type(&src)
                        || eve_db::WorldStateSnapshot::has_locked_flag_or_wrap_type(&tgt)
                    {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot merge locked, impounded, or wrapped items".into(),
                            client_error: None,
                        });
                    }
                    if eve_db::WorldStateSnapshot::is_fitted_slot_flag(src.flag)
                        || lock_index.non_empty_containers.contains(&src_id)
                        || lock_index.non_empty_containers.contains(&dst_id)
                    {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot stack fitted modules or non-empty containers".into(),
                            client_error: None,
                        });
                    }
                    if eve_db::WorldStateSnapshot::has_non_stackable_item_state(&src)
                        || eve_db::WorldStateSnapshot::has_non_stackable_item_state(&tgt)
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
                    let chain_unlocks = enforce_container_chain_policies(
                        ctx,
                        guard,
                        &cs_state_merge,
                        char_id,
                        ch.corporation_id,
                        true,
                        &chain,
                        provided_pw,
                        ContainerChainPolicy {
                            reject_sealed_wrap: true,
                            reject_alsc_locked: true,
                            reject_locked_or_escrow: false,
                            check_corp_take_role: false,
                            locked_err_code: "StackFailed",
                        },
                    )?;
                    unlock_ids.extend(chain_unlocks);
                    if guard.is_item_or_ancestor_locked_or_impounded_with_index(&lock_index, dst_id)
                        || guard.is_item_or_ancestor_locked_or_impounded_with_index(
                            &lock_index,
                            src_id,
                        )
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
                persist_unlocked_containers_in_snap(guard, char_id, &unlock_ids);
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
            let bound_loc = bound_entry.map(|b| b.object_id).filter(|&id| id > 0);
            let kw_loc = match parse_opt_positive_id(
                kwargs.get("locationID"),
                "InvalidLocation",
                "StackAll locationID",
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let (Some(bl), Some(kl)) = (bound_loc, kw_loc) {
                if bl != kl {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidLocation".into(),
                        message: format!(
                            "StackAll locationID {kl} conflicts with bound inventory {bl}"
                        ),
                        client_error: None,
                    }));
                }
            }
            let Some(target_loc) = bound_loc
                .or(kw_loc)
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
            if (30_000_000..=39_999_999).contains(&target_loc) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLocation".into(),
                    message: format!("Cannot run StackAll directly on solar system {target_loc}"),
                    client_error: None,
                }));
            }
            let default_flag = if Some(target_loc) == session.station_id
                || Some(target_loc) == session.structure_id
            {
                InventoryFlag::Hangar
            } else {
                InventoryFlag::Cargo
            };
            let raw_flag = match args.first() {
                Some(v) if !matches!(v, PyRep::None) => Some(v),
                _ => match kwargs.get("flag") {
                    Some(v) if !matches!(v, PyRep::None) => Some(v),
                    _ => None,
                },
            };
            let target_flag = match raw_flag {
                None => default_flag,
                Some(v) => {
                    let Some(f) = v.as_i64() else {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidInventoryFlag".into(),
                            message: "StackAll flag must be an integer".into(),
                            client_error: None,
                        }));
                    };
                    if f == 0 {
                        default_flag
                    } else {
                        let Ok(flag_u16) = u16::try_from(f) else {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidInventoryFlag".into(),
                                message: format!("StackAll flag {f} out of valid range"),
                                client_error: None,
                            }));
                        };
                        InventoryFlag::from_u16(flag_u16)
                    }
                }
            };

            let provided_pw = kwargs.get("password").and_then(|v| v.as_str());
            let loaded_tube_items = snapshot_loaded_tube_items(ctx);
            let pre_balls = snapshot_pre_balls(ctx, session, char_id);
            let outcome = match ctx.db.try_with_mut_snapshot(|guard| {
                let ch = character_or_session_fallback(guard, session, char_id)?;
                let cs_state = fitting_containers_ships::read_container_ship_state_snap(guard);
                let chain = collect_container_chain(guard, target_loc, 9);
                validate_bound_and_hint_chain(
                    guard,
                    session,
                    bound_loc,
                    &[kw_loc],
                    &chain,
                    "InvalidLocation",
                    &format!("Inventory {target_loc}"),
                )?;
                validate_location_reachability_in_snap(
                    ctx,
                    guard,
                    session,
                    &ch,
                    char_id,
                    &pre_balls,
                    target_loc,
                    &chain,
                    "DestinationAccessDenied",
                    "OutOfRange",
                    true,
                )?;
                let unlock_ids = enforce_container_chain_policies(
                    ctx,
                    guard,
                    &cs_state,
                    char_id,
                    ch.corporation_id,
                    true,
                    &chain,
                    provided_pw,
                    ContainerChainPolicy {
                        reject_sealed_wrap: false,
                        reject_alsc_locked: false,
                        reject_locked_or_escrow: false,
                        check_corp_take_role: true,
                        locked_err_code: "StackFailed",
                    },
                )?;
                let outcome = guard.stack_all_items_in_snap(
                    char_id,
                    target_loc,
                    target_flag,
                    &loaded_tube_items,
                );
                persist_unlocked_containers_in_snap(guard, char_id, &unlock_ids);
                Ok(outcome)
            }) {
                Ok(outcome) => outcome,
                Err(e) => return Some(Err(e)),
            };

            for old_src in &outcome.deleted_sources {
                inventory_bound::emit_inv_item_deleted(ctx, char_id, old_src);
            }
            let mut survivors = Vec::with_capacity(outcome.survivors.len());
            for survivor in &outcome.survivors {
                inventory_bound::emit_inv_item_change(
                    ctx,
                    char_id,
                    &survivor.updated,
                    vec![
                        // Omit ixQuantity (5): V24.01 invCache uses ixStackSize (9).
                        (9, PyRep::Integer(survivor.old_quantity as i64)),
                    ],
                );
                survivors.push(PyRep::keyval([
                    ("itemID", PyRep::Integer(survivor.updated.item_id)),
                    ("typeID", PyRep::Integer(survivor.updated.type_id as i64)),
                    ("quantity", PyRep::Integer(survivor.updated.quantity as i64)),
                    ("version", PyRep::Integer(survivor.updated.version as i64)),
                ]));
            }
            Some(Ok(PyRep::keyval([
                ("locationID", PyRep::Integer(target_loc)),
                ("flagID", PyRep::Integer(target_flag as i64)),
                ("mergedCount", PyRep::Integer(outcome.merged_count)),
                ("stacks", PyRep::List(survivors)),
            ])))
        }

        _ => None,
    }
}
