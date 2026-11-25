//! `inventory_stacking_ops` — `SplitStack`, `MultiMerge`, and `StackAll` validation & execution (`#487`).

#![forbid(unsafe_code)]

use crate::router::{context::BoundObjectEntry, ServerContext};
use crate::services::inventory_bound;
use crate::services::inventory_fitting::fitting_containers_ships;
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
    let view = sys_h.view();
    view.balls
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
    unlock_pairs: &[(i64, i64)],
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    for &(target_id, char_id) in unlock_pairs {
        fitting_containers_ships::set_container_unlocked_snap(
            guard, target_id, char_id, true, now_unix,
        )?;
    }
    Ok(())
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
    char_id: i64,
    corp_id: i64,
    check_corp_containers: bool,
    chain: &ContainerChainInfo,
    provided_pw: Option<&str>,
    policy: ContainerChainPolicy,
) -> Result<Vec<(i64, i64)>, BridgeRpcError> {
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
                crate::services::inventory_bound::get_effective_corp_roles_at_location_in_snap_at(
                    snap,
                    corp_id,
                    char_id,
                    Some(anc.location_id),
                    ctx.now_unix(),
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
        if policy.reject_alsc_locked && snap.is_item_in_alsc_lock(anc.item_id) {
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

    let mut unlock_pairs = Vec::new();
    for &anc_id in container_hops {
        if let Some(req_pw) = fitting_containers_ships::container_password_snap(snap, anc_id, 1) {
            let password_per_action =
                fitting_containers_ships::container_config_snap(snap, anc_id).password_per_action;
            let already_unlocked = !password_per_action
                && fitting_containers_ships::is_container_unlocked_snap(snap, anc_id, char_id);
            if !already_unlocked {
                if provided_pw == Some(req_pw.as_str()) {
                    if !password_per_action {
                        unlock_pairs.push((anc_id, char_id));
                    }
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

    Ok(unlock_pairs)
}

fn validate_location_reachability_in_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    ch: &eve_db::CharacterRecord,
    char_id: i64,
    pre_balls: &HashMap<i64, (glam::DVec3, f64)>,
    target_loc: i64,
    target_bay_flag: Option<InventoryFlag>,
    chain: &ContainerChainInfo,
    provided_pw: Option<&str>,
    access_err_code: &'static str,
    range_err_code: &'static str,
    check_container_owner: bool,
) -> Result<Option<(i64, i64)>, BridgeRpcError> {
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

    if let Some(st) = snap.structures.get(&chain.root_loc) {
        if st.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "StructureDestroyed".into(),
                message: format!("Structure {} is destroyed", chain.root_loc),
                client_error: None,
            });
        }
        let docked_at_structure = (ch_dock == chain.root_loc
            && session.structure_id == Some(chain.root_loc))
            || (ch.station_id.is_none()
                && ch.structure_id.is_none()
                && session.solar_system_id.is_none()
                && session.structure_id == Some(chain.root_loc));
        if !docked_at_structure {
            return Err(BridgeRpcError {
                code: access_err_code.into(),
                message: format!(
                    "Character {char_id} is not docked at structure {}",
                    chain.root_loc
                ),
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

    if ch_dock == 0 && ch_sys > 0 && chain.root_loc == ch_sys {
        // In-space targets rooted in the active solar system use exact chain + 2,500m/5,000m range
        // verification directly so targets >5,000m away still return `range_err_code` (`OutOfRange`)
        // before ship-bay/container password checks.
        let Some(outer_id) = chain.chain_locs.iter().rev().nth(1).copied() else {
            return Err(BridgeRpcError {
                code: access_err_code.into(),
                message: format!("Inventory {target_loc} is not physically co-located with pilot"),
                client_error: None,
            });
        };
        if outer_id != caller_ship_id && outer_id != ch.active_ship_id {
            let Some(outer_node) = snap.items.get(&outer_id) else {
                return Err(BridgeRpcError {
                    code: access_err_code.into(),
                    message: format!("Inventory {target_loc} is not physically co-located with pilot"),
                    client_error: None,
                });
            };
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
            let Some((s_pos, t_pos)) = ship_pos.zip(cont_pos) else {
                return Err(BridgeRpcError {
                    code: range_err_code.into(),
                    message: format!(
                        "Cannot verify range to container {outer_id}: missing ship or container position"
                    ),
                    client_error: None,
                });
            };
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
                .or_else(|| ctx.sde.types.get(&outer_node.type_id).map(|t| t.radius))
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
    } else {
        // `is_inventory_location_colocated` walks 4 hops; for deeper container chains pass the
        // nearest child below `root_loc` so depth up to `max_hops` resolves against dock/system.
        let coloc_probe = if chain.chain_locs.len() > 4 {
            chain.chain_locs[chain.chain_locs.len().saturating_sub(2)]
        } else {
            target_loc
        };
        let colocated = crate::services::inventory_bound::is_inventory_location_colocated_with_sde(
            &ctx.sde,
            snap,
            session,
            ch,
            pre_balls,
            coloc_probe,
            false,
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
    }

    let is_in_space_root = (30_000_000..=39_999_999).contains(&chain.root_loc)
        && !snap.structures.contains_key(&chain.root_loc);
    let target_flag_u16 = target_bay_flag.map(|f| f as u16).unwrap_or(InventoryFlag::Cargo as u16);
    let foreign_ship_ancestor = (!session.has_gm_role())
        .then(|| {
            inventory_bound::find_ship_bay_ancestor_in_snap(ctx, snap, target_loc, target_flag_u16)
                .filter(|(host, _)| host.owner_id != char_id)
        })
        .flatten();
    let container_hops = &chain.chain_locs[..chain.chain_locs.len().saturating_sub(1)];
    if check_container_owner || !is_in_space_root {
        for &anc_id in container_hops {
            if let Some(node) = snap.items.get(&anc_id) {
                let inside_foreign_ship_bay = foreign_ship_ancestor
                    .is_some_and(|(host, _)| anc_id == host.item_id || node.owner_id == host.owner_id);
                let allowed_container = session.has_gm_role()
                    || inside_foreign_ship_bay
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

    // Check foreign ship bay access and password strictly after co-location and range checks
    // (for both docked and in-space ships) so out-of-range callers cannot probe passwords.
    let mut ship_bay_unlock = None;
    if let Some((host_ship, entry_flag_u16)) = foreign_ship_ancestor {
        if check_container_owner && !matches!(entry_flag_u16, 90 | 155) {
            return Err(BridgeRpcError {
                code: access_err_code.into(),
                message: format!("Character {char_id} cannot access inventory {target_loc}"),
                client_error: None,
            });
        }
        let (is_sma, deny_code) = match entry_flag_u16 {
            155 => (false, "FleetHangarAccessDenied"),
            90 | 138 => (true, "SMAAccessDenied"),
            _ => {
                return Err(BridgeRpcError {
                    code: access_err_code.into(),
                    message: format!("Character {char_id} cannot access inventory {target_loc}"),
                    client_error: None,
                });
            }
        };
        if !fitting_containers_ships::can_character_access_ship_bay_with_password_snap(
            snap,
            char_id,
            host_ship.item_id,
            host_ship.owner_id,
            is_sma,
            provided_pw,
        ) {
            return Err(BridgeRpcError {
                code: deny_code.into(),
                message: format!(
                    "Character {char_id} lacks ship bay access or valid password for {}",
                    host_ship.item_id
                ),
                client_error: None,
            });
        }
        let cfg = fitting_containers_ships::ship_access_config_snap(snap, host_ship.item_id);
        if let Some(req_pw) = cfg.password.as_deref().filter(|s| !s.is_empty()) {
            let already_unlocked = fitting_containers_ships::is_container_unlocked_snap(
                snap,
                host_ship.item_id,
                char_id,
            );
            if !already_unlocked && provided_pw == Some(req_pw) {
                ship_bay_unlock = Some((host_ship.item_id, char_id));
            }
        }
    } else if is_in_space_root {
        if let Some(&root_obj_id) = container_hops.last() {
            if let Some(root_item) = snap.items.get(&root_obj_id) {
                let is_foreign_ship = root_item.owner_id != char_id
                    && root_item.owner_id != 0
                    && !session.has_gm_role()
                    && ctx
                        .sde
                        .types
                        .get(&root_item.type_id)
                        .is_some_and(|t| t.category_id == 6);
                if is_foreign_ship {
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

    Ok(ship_bay_unlock)
}

fn execute_split_stack_in_tx(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    bound_loc: Option<i64>,
    loc_hint: Option<i64>,
    item_id: i64,
    split_quantity: i32,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<(eve_db::InventoryItemRecord, eve_db::InventoryItemRecord, Option<i64>), BridgeRpcError> {
    let loaded_tube_items = snapshot_loaded_tube_items(ctx);
    let pre_balls = snapshot_pre_balls(ctx, session, char_id);
    let provided_pw = kwargs
        .get("password")
        .and_then(|v| v.as_str())
        .or_else(|| args.iter().find_map(|a| a.as_str()));

    let now_unix = ctx.now_unix_secs();
    ctx.db.try_with_mut_snapshot(|guard| {
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

        inventory_bound::validate_movable_item_not_piloted_or_bare_space_in_snap(
            guard,
            char_id,
            &item,
            0,
            item.flag as u16,
        )?;
        let foreign_ship_bay_owner = inventory_bound::foreign_ship_owner_at_loc_in_snap(
            ctx,
            guard,
            item.location_id,
            item.flag as u16,
            char_id,
        );
        let is_corp_item = ch.corporation_id > 0 && item.owner_id == ch.corporation_id;
        if item.owner_id != char_id && !is_corp_item && foreign_ship_bay_owner.is_none() {
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
        let ship_unlock = validate_location_reachability_in_snap(
            ctx,
            guard,
            session,
            &ch,
            char_id,
            &pre_balls,
            item.location_id,
            Some(item.flag),
            &chain,
            provided_pw,
            "SplitFailed",
            "SplitFailed",
            false,
        )?;
        if is_corp_item && foreign_ship_bay_owner.is_none() {
            crate::services::inventory_bound::validate_corp_source_item_take_access_in_snap(
                ctx,
                guard,
                char_id,
                ch.corporation_id,
                &item,
            )?;
        }

        if guard.is_item_in_alsc_lock(item_id) {
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
        if eve_db::WorldStateSnapshot::has_locked_flag_or_wrap_type(&item)
            || guard.is_item_locked_in_contract_or_job(item_id)
        {
            return Err(BridgeRpcError {
                code: "SplitFailed".into(),
                message: format!("Item {item_id} is locked, impounded, or in Asset Safety"),
                client_error: None,
            });
        }
        if guard.is_item_in_active_asset_safety_wrap(item_id) {
            return Err(BridgeRpcError {
                code: "SplitFailed".into(),
                message: format!("Item {item_id} is sealed inside an Asset Safety wrap"),
                client_error: None,
            });
        }

        let mut unlock_pairs = enforce_container_chain_policies(
            ctx,
            guard,
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
        if let Some(pair) = ship_unlock {
            unlock_pairs.push(pair);
        }

        let (orig, mut new_i) = guard
            .split_stack_in_snap(item.owner_id, item_id, split_quantity)
            .map_err(|e| BridgeRpcError {
                code: "SplitFailed".into(),
                message: e.to_string(),
                client_error: None,
            })?;
        if let Some(ship_owner) = foreign_ship_bay_owner {
            if let Some(rec) = guard.items.get_mut(&new_i.item_id) {
                rec.owner_id = ship_owner;
            }
            new_i.owner_id = ship_owner;
        }
        // Persist container unlock and native bay custom flag only after `split_stack_in_snap` succeeds.
        persist_unlocked_containers_in_snap(guard, &unlock_pairs, now_unix)?;
        if matches!(new_i.flag as u16, 90 | 155) {
            fitting_containers_ships::set_custom_item_flag_snap(
                guard,
                new_i.item_id,
                Some(new_i.flag as u16),
                now_unix,
            )?;
        }
        Ok((orig, new_i, foreign_ship_bay_owner))
    })
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(async move {
        crate::services::inventory_fitting::wait_in_space_post_unlock_barrier(ctx, session).await;
        handle_stack_ops(
            ctx,
            session,
            bound_entry,
            service,
            method,
            args,
            kwargs,
        )
    })
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("invbroker", "SplitStack", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("contractProxy", "SplitStack", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "MultiMerge", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "StackAll", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
];

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
                args,
                kwargs,
            ) {
                Ok((orig, new_i, _foreign_ship_bay_owner)) => {
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
            let now_unix = ctx.now_unix_secs();
            let tx_res = ctx.db.try_with_mut_snapshot(|guard| {
                let ch = character_or_session_fallback(guard, session, char_id)?;
                let lock_index = guard.build_inventory_lock_index();
                let mut events = Vec::with_capacity(ops.len());
                let mut checked_reachability = HashSet::new();
                let mut unlock_pairs = Vec::new();
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
                    let src_foreign_ship_owner = inventory_bound::foreign_ship_owner_at_loc_in_snap(
                        ctx,
                        guard,
                        src.location_id,
                        src.flag as u16,
                        char_id,
                    );
                    let owner_ok = |it: &eve_db::InventoryItemRecord| {
                        it.owner_id == char_id
                            || (src_foreign_ship_owner.is_some()
                                && Some(it.owner_id) == src_foreign_ship_owner)
                    };
                    if !owner_ok(&src) || !owner_ok(&tgt) {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Stack ownership mismatch".into(),
                            client_error: None,
                        });
                    }
                    inventory_bound::validate_movable_item_not_piloted_or_bare_space_in_snap(
                        guard,
                        char_id,
                        &src,
                        0,
                        src.flag as u16,
                    )?;
                    inventory_bound::validate_movable_item_not_piloted_or_bare_space_in_snap(
                        guard,
                        char_id,
                        &tgt,
                        0,
                        tgt.flag as u16,
                    )?;
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
                    if checked_reachability.insert((src.location_id, src.flag as u16)) {
                        if let Some(ship_unlock) = validate_location_reachability_in_snap(
                            ctx,
                            guard,
                            session,
                            &ch,
                            char_id,
                            &pre_balls,
                            src.location_id,
                            Some(src.flag),
                            &chain,
                            provided_pw,
                            "DestinationAccessDenied",
                            "OutOfRange",
                            false,
                        )? {
                            unlock_pairs.push(ship_unlock);
                        }
                    }
                    if guard.is_item_in_alsc_lock(src_id) || guard.is_item_in_alsc_lock(dst_id) {
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
                    if lock_index.unpaid_wrap_items.contains(&src_id)
                        || lock_index.unpaid_wrap_items.contains(&dst_id)
                        || chain
                            .chain_locs
                            .iter()
                            .any(|loc| lock_index.unpaid_wrap_items.contains(loc))
                    {
                        return Err(BridgeRpcError {
                            code: "StackFailed".into(),
                            message: "Cannot merge items inside an Asset Safety wrap".into(),
                            client_error: None,
                        });
                    }
                    let chain_unlocks = enforce_container_chain_policies(
                        ctx,
                        guard,
                        char_id,
                        ch.corporation_id,
                        true,
                        &chain,
                        provided_pw,
                        ContainerChainPolicy {
                            reject_sealed_wrap: true,
                            reject_alsc_locked: true,
                            reject_locked_or_escrow: false,
                            check_corp_take_role: true,
                            locked_err_code: "StackFailed",
                        },
                    )?;
                    unlock_pairs.extend(chain_unlocks);
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
                    if merge_qty < old_src_qty {
                        let src_mut = guard.items.get_mut(&src_id).unwrap();
                        src_mut.quantity -= merge_qty;
                        src_mut.version += 1;
                    } else {
                        guard.remove_system_cleanup_item(src_id);
                    }
                    let updated_dst = {
                        let tgt_mut = guard.items.get_mut(&dst_id).unwrap();
                        tgt_mut.quantity = new_tgt_qty;
                        if let Some(ship_owner) = src_foreign_ship_owner {
                            tgt_mut.owner_id = ship_owner;
                        }
                        tgt_mut.version += 1;
                        tgt_mut.clone()
                    };
                    events.push(updated_dst);
                }
                persist_unlocked_containers_in_snap(guard, &unlock_pairs, now_unix)?;
                Ok(events)
            });
            let updated_records = match tx_res {
                Ok(ev) => ev,
                Err(e) => return Some(Err(e)),
            };
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
            let now_unix = ctx.now_unix_secs();
            let outcome = match ctx.db.try_with_mut_snapshot(|guard| {
                let ch = character_or_session_fallback(guard, session, char_id)?;
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
                let ship_unlock = validate_location_reachability_in_snap(
                    ctx,
                    guard,
                    session,
                    &ch,
                    char_id,
                    &pre_balls,
                    target_loc,
                    Some(target_flag),
                    &chain,
                    provided_pw,
                    "DestinationAccessDenied",
                    "OutOfRange",
                    true,
                )?;
                let mut unlock_pairs = enforce_container_chain_policies(
                    ctx,
                    guard,
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
                if let Some(pair) = ship_unlock {
                    unlock_pairs.push(pair);
                }
                let foreign_ship_owner = inventory_bound::foreign_ship_owner_at_loc_in_snap(
                    ctx,
                    guard,
                    target_loc,
                    target_flag as u16,
                    char_id,
                );
                let stack_owner = foreign_ship_owner.unwrap_or(char_id);
                let outcome = guard.stack_all_items_in_snap(
                    stack_owner,
                    target_loc,
                    target_flag,
                    &loaded_tube_items,
                );
                persist_unlocked_containers_in_snap(guard, &unlock_pairs, now_unix)?;
                Ok(outcome)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            let mut survivors = Vec::with_capacity(outcome.survivors.len());
            for survivor in &outcome.survivors {
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
