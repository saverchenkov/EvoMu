//! `inventory_stacking` — Stack, Split, Merge, and StackAll Operations for `invbroker`.

#![forbid(unsafe_code)]

use crate::router::{context::BoundObjectEntry, ServerContext};
use crate::services::inventory_fitting::{
    fitting_containers_ships, get_effective_corp_roles_at_location, item_to_packed_cells,
};
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use std::collections::{HashMap, HashSet};

#[path = "inventory_stacking_ops.rs"]
pub(crate) mod inventory_stacking_ops;
pub(super) use inventory_stacking_ops::handle_stack_ops;
use super::fitting_containers_ships::{
    edge_distance_in_space_snap, foreign_ship_deny_err, list_with_optional_ship_unlock,
    verify_nested_unit_list_in_snap, verify_ship_proximity_in_snap,
};

fn validate_multi_add_dest_flag(flag_u16: u16) -> Result<(), BridgeRpcError> {
    if matches!(flag_u16, 11..=34 | 89 | 92..=94 | 125..=132 | 164..=171) {
        return Err(BridgeRpcError {
            code: "InvalidLocation".into(),
            message: "MultiAdd cannot be used to fit modules or rigs".into(),
            client_error: None,
        });
    }
    if matches!(flag_u16, 36 | 63 | 72) {
        return Err(BridgeRpcError {
            code: if flag_u16 == 72 {
                "ImpoundedItem"
            } else if flag_u16 == 36 {
                "AssetSafetyLocked"
            } else {
                "ItemLocked"
            }
            .into(),
            message: format!("Cannot MultiAdd items into restricted inventory flag {flag_u16}"),
            client_error: None,
        });
    }
    if matches!(flag_u16, 62 | 173 | 187) {
        return Err(BridgeRpcError {
            code: "InvalidInventoryFlag".into(),
            message: format!("Invalid or restricted inventory flag {flag_u16}"),
            client_error: None,
        });
    }
    Ok(())
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
    let raw_flag_opt = args
        .get(2)
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()))
        .map(|v| v as u16);
    let (first_src_loc, items_are_fittable_modules) = ctx.db.with_snapshot(|s| {
        let first_loc = item_ids
            .first()
            .and_then(|&id| s.items.get(&id).map(|i| i.location_id));
        let all_fittable = !item_ids.is_empty()
            && item_ids.iter().all(|&id| {
                s.items.get(&id).is_some_and(|it| {
                    ctx.sde
                        .types
                        .get(&it.type_id)
                        .is_some_and(|t| t.category_id == 7 || t.category_id == 32)
                })
            });
        (first_loc, all_fittable)
    });
    let Some(new_loc) = super::inventory_bound::resolve_inv_target_location(
        ctx,
        session,
        bound_entry,
        args,
        first_src_loc,
        raw_flag_opt,
        items_are_fittable_modules,
    ) else {
        return Err(BridgeRpcError {
            code: "InvalidLocation".into(),
            message: "MultiAdd requires target location".into(),
            client_error: None,
        });
    };
    if let Some(rf) = raw_flag_opt {
        validate_multi_add_dest_flag(rf)?;
    }
    let should_auto_fit = (raw_flag_opt == Some(0) || raw_flag_opt.is_none())
        && ctx.db.with_snapshot(|snap| {
            let dest_is_owned_ship = snap.items.get(&new_loc).is_some_and(|d| {
                d.owner_id == char_id
                    && ctx
                        .sde
                        .types
                        .get(&d.type_id)
                        .is_some_and(|t| t.category_id == 6)
            });
            dest_is_owned_ship
                && item_ids.iter().all(|&id| {
                    snap.items.get(&id).is_some_and(|it| {
                        ctx.sde
                            .types
                            .get(&it.type_id)
                            .is_some_and(|t| t.category_id == 7 || t.category_id == 32)
                    })
                })
        });
    if should_auto_fit {
        return handle_multi_add_auto_fit(ctx, session, char_id, new_loc, &item_ids, kwargs).await;
    }
    let raw_flag_u16 = raw_flag_opt
        .filter(|&f| f != 0)
        .unwrap_or(InventoryFlag::Cargo as u16);
    validate_multi_add_dest_flag(raw_flag_u16)?;
    let flag = InventoryFlag::from_u16(raw_flag_u16);
    let flag_u16 = flag as u16;
    validate_multi_add_dest_flag(flag_u16)?;
    ctx.db.with_snapshot(|snap| {
        super::inventory_bound::validate_ship_only_bay_target_in_snap(ctx, snap, new_loc, flag_u16)
    })?;
    let supplied_pw = kwargs
        .get("password")
        .and_then(|v| v.as_str())
        .or_else(|| args.iter().find_map(|a| a.as_str()));
    let active_ship_id = session
        .ship_id
        .or_else(|| {
            ctx.db
                .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.active_ship_id))
        })
        .unwrap_or(0);
    if item_ids.contains(&active_ship_id)
        || ctx.db.with_snapshot(|snap| item_ids.iter().any(|&id| snap.is_character_boarded_or_saved_ship(char_id, session.ship_id, id)))
    {
        return Err(BridgeRpcError {
            code: "CannotMoveActiveShip".into(),
            message: "Cannot move active ship".into(),
            client_error: None,
        });
    }
    for &item_id in &item_ids {
        fitting_containers_ships::validate_container_and_bay_move_for_add_with_args(
            ctx, char_id, item_id, new_loc, flag_u16, args, kwargs, None,
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
        && matches!(flag_u16, 5 | 87 | 158 | 133..=151 | 177 | 179 | 181 | 182 | 185);
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
    let (fitted_unfit_ids, _) = ctx.db.with_snapshot(|snap| {
        validate_multi_add_in_snap(
            ctx,
            snap,
            session,
            char_id,
            new_loc,
            flag_u16,
            &item_ids,
            &looted_from_space_ids,
            dest_is_struct_fuel,
            is_ship_hold,
            eval_cargo_cap,
            &pre_balls,
            supplied_pw,
        )
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
                client_error: None,
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
                client_error: None,
            });
        }
    }
    for &(old_loc, item_id, old_flag) in &fitted_unfit_ids {
        if (125..=132).contains(&old_flag) {
            let has_remaining_fitted_modules = ctx.db.with_snapshot(|snap| {
                snap.items.values().any(|other| {
                    other.location_id == old_loc
                        && other.item_id != item_id
                        && !fitted_unfit_ids
                            .iter()
                            .any(|&(_, unf_id, _)| unf_id == other.item_id)
                        && (11..=34).contains(&(other.flag as u16))
                        && ctx
                            .sde
                            .types
                            .get(&other.type_id)
                            .is_none_or(|t| t.category_id != 8)
                })
            });
            if has_remaining_fitted_modules {
                return Err(BridgeRpcError {
                    code: "SubsystemHasFittedModules".into(),
                    message: "Cannot unfit a T3 Strategic Cruiser subsystem while modules are fitted".into(),
                    client_error: None,
                });
            }
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
                        client_error: None,
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
    let mut old_ship_attrs: std::collections::HashMap<i64, std::collections::BTreeMap<u16, f64>> =
        std::collections::HashMap::new();
    for &(old_loc, _, _) in &fitted_unfit_ids {
        if !old_ship_attrs.contains_key(&old_loc) {
            if let Some(snap) =
                crate::services::notify_emitters_skills_trade_godma::snapshot_ship_fitting_attrs_for_ship(
                    ctx,
                    char_id,
                    old_loc,
                )
            {
                old_ship_attrs.insert(old_loc, snap);
            }
        }
    }
    let (moved, old_containers, dst_ship_bay_foreign_owner) = ctx.db.try_with_mut_snapshot(|snap| {
        let (_, src_foreign_ship_bay_ids) = validate_multi_add_in_snap(
            ctx,
            snap,
            session,
            char_id,
            new_loc,
            flag_u16,
            &item_ids,
            &looted_from_space_ids,
            dest_is_struct_fuel,
            is_ship_hold,
            eval_cargo_cap,
            &pre_balls,
            supplied_pw,
        )?;
        let mut moved = Vec::with_capacity(item_ids.len());
        let mut old_containers = HashSet::new();
        let dest_ship_bay_owner = super::inventory_bound::ship_bay_target_owner_in_snap(
            ctx, snap, new_loc, flag_u16,
        );
        let dst_ship_bay_foreign_owner = dest_ship_bay_owner.filter(|&o| o != char_id);
        let dst_struct_corp = crate::services::structure_acl_defense_rules::is_structure_fuel_flag(flag)
            .then(|| snap.structures.get(&new_loc).map(|st| st.owner_corp_id))
            .flatten()
            .unwrap_or(0);
        let active_ship_id = session.ship_id.or_else(|| snap.characters.get(&char_id).map(|c| c.active_ship_id));
        for &item_id in &item_ids {
            let pre_item = snap
                .items
                .get(&item_id)
                .cloned()
                .ok_or_else(|| BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Item {item_id} not found"),
                    client_error: None,
                })?;
            let it_snapshot = &pre_item;
            let old_flag = it_snapshot.flag;
            let old_flag_u16 = old_flag as u16;
            let old_loc = it_snapshot.location_id;
            let it_tid = it_snapshot.type_id;
            let it_qty = it_snapshot.quantity;
            let src_ship_bay_owner = super::inventory_bound::foreign_ship_owner_at_loc_in_snap(
                ctx, snap, old_loc, old_flag_u16, char_id,
            );
            let new_owner = if dst_struct_corp > 0 {
                dst_struct_corp
            } else if (116..=122).contains(&flag_u16) && caller_corp_id > 0 {
                caller_corp_id
            } else if let Some(ship_owner) = dest_ship_bay_owner {
                ship_owner
            } else if (116..=122).contains(&old_flag_u16)
                || matches!(old_flag_u16, 62 | 133 | 173 | 187)
                || matches!(flag, InventoryFlag::Hangar | InventoryFlag::Cargo)
                || is_ship_hold
                || src_ship_bay_owner.is_some()
                || looted_from_space_ids.contains(&item_id)
            {
                char_id
            } else {
                it_snapshot.owner_id
            };
            let is_space_loot = looted_from_space_ids.contains(&item_id);
            let is_struct_fuel_src = snap.structures.contains_key(&old_loc)
                && (old_flag == InventoryFlag::StructureFuel || old_flag_u16 == 172);
            let bypass_take =
                is_space_loot || is_struct_fuel_src || src_foreign_ship_bay_ids.contains(&item_id);
            let pre_descendants: Vec<eve_db::InventoryItemRecord> = if new_owner != pre_item.owner_id
                || dest_ship_bay_owner.is_some()
                || src_ship_bay_owner.is_some()
            {
                let mut visited = HashSet::from([item_id]);
                let mut frontier = vec![item_id];
                let mut out = Vec::new();
                for _ in 0..8 {
                    if frontier.is_empty() {
                        break;
                    }
                    let next_ids: Vec<i64> = snap
                        .items
                        .values()
                        .filter(|it| frontier.contains(&it.location_id) && !visited.contains(&it.item_id))
                        .map(|it| it.item_id)
                        .collect();
                    frontier.clear();
                    for cid in next_ids {
                        if visited.insert(cid) {
                            frontier.push(cid);
                            if let Some(child) = snap.items.get(&cid) {
                                if child.owner_id != new_owner {
                                    out.push(child.clone());
                                }
                            }
                        }
                    }
                }
                out
            } else {
                Vec::new()
            };
            let mut spec = eve_db::TransferItemSpec::move_whole(
                if bypass_take { None } else { Some(char_id) },
                if bypass_take { Some(it_snapshot.owner_id) } else { None },
                Some(new_owner),
                new_loc,
                flag,
            );
            spec.reject_fitted = false;
            spec.active_ship_id = active_ship_id;
            let tx_item = snap.transfer_inventory_item(item_id, spec).map_err(|e| BridgeRpcError {
                code: "InventoryMoveFailed".into(),
                message: e.to_string(),
                client_error: None,
            })?;
            if old_loc != new_loc {
                old_containers.insert((old_loc, item_id, it_tid, it_qty));
            }
            snap.module_online_states.remove(&item_id);
            snap.module_active_states.remove(&item_id);
            snap.module_active_charges.remove(&item_id);
            crate::router::ServerContext::clear_persisted_module_overload(snap, old_loc, item_id);
            let (fin_opt, _) = crate::services::structure_acl_defense_rules::finalize_fuel_bay_move_in_snap(
                ctx, snap, item_id, old_loc, old_flag, new_loc, flag, char_id, caller_corp_id,
            );
            let merged_into_other =
                tx_item.item_id != item_id || fin_opt.as_ref().is_some_and(|f| f.item_id != item_id);
            let recursive_owner_changes: Vec<(eve_db::InventoryItemRecord, eve_db::InventoryItemRecord)> =
                if !merged_into_other
                    && (new_owner != pre_item.owner_id
                        || dest_ship_bay_owner.is_some()
                        || src_ship_bay_owner.is_some())
                {
                    let mut extra = snap.set_owner_recursive(item_id, new_owner);
                    extra.retain(|(pre_c, _)| pre_c.item_id != item_id);
                    let mut out: Vec<(eve_db::InventoryItemRecord, eve_db::InventoryItemRecord)> =
                        pre_descendants
                            .into_iter()
                            .filter_map(|pre_c| {
                                let post_c = snap.items.get(&pre_c.item_id)?;
                                (post_c.owner_id != pre_c.owner_id)
                                    .then(|| (pre_c, post_c.clone()))
                            })
                            .collect();
                    for pair in extra {
                        if !out.iter().any(|(p, _)| p.item_id == pair.0.item_id) {
                            out.push(pair);
                        }
                    }
                    out
                } else {
                    Vec::new()
                };
            let post_it = snap.items.get(&item_id).cloned();
            moved.push((
                pre_item,
                post_it,
                fin_opt,
                it_tid,
                it_qty,
                src_ship_bay_owner,
                recursive_owner_changes,
            ));
        }
        let now_unix = ctx.now_unix_secs();
        for (pre_item, post_it, fin_opt, it_tid, it_qty, _, _) in &moved {
            let fin_id = fin_opt
                .as_ref()
                .map(|r| r.item_id)
                .or_else(|| post_it.as_ref().map(|r| r.item_id))
                .unwrap_or(pre_item.item_id);
            if fin_id != pre_item.item_id
                && fitting_containers_ships::custom_item_flag_snap(snap, pre_item.item_id).is_some()
            {
                fitting_containers_ships::set_custom_item_flag_snap(
                    snap,
                    pre_item.item_id,
                    None,
                    now_unix,
                )?;
            }
            if matches!(flag_u16, 90 | 155) {
                fitting_containers_ships::set_custom_item_flag_snap(
                    snap,
                    fin_id,
                    Some(flag_u16),
                    now_unix,
                )?;
            } else if fitting_containers_ships::custom_item_flag_snap(snap, fin_id).is_some() {
                fitting_containers_ships::set_custom_item_flag_snap(snap, fin_id, None, now_unix)?;
            }
            fitting_containers_ships::record_container_add_log_in_snap(
                &ctx.sde, snap, char_id, fin_id, *it_tid, *it_qty, new_loc, now_unix,
            )?;
            if pre_item.location_id != new_loc && pre_item.location_id > 0 {
                fitting_containers_ships::record_container_take_log_in_snap(
                    &ctx.sde,
                    snap,
                    char_id,
                    pre_item.item_id,
                    *it_tid,
                    *it_qty,
                    pre_item.location_id,
                    now_unix,
                )?;
            }
        }
        if supplied_pw.is_some() {
            let src_pairs: Vec<(i64, u16)> = moved
                .iter()
                .map(|(pre, _, _, _, _, _, _)| (pre.location_id, pre.flag as u16))
                .collect();
            for (src_loc, src_flag_u16) in src_pairs {
                fitting_containers_ships::record_ship_bay_unlock_for_ancestors_snap(
                    ctx,
                    snap,
                    char_id,
                    new_loc,
                    flag_u16,
                    src_loc,
                    src_flag_u16,
                    supplied_pw,
                )?;
            }
        }
        Ok((moved, old_containers, dst_ship_bay_foreign_owner))
    })?;
    let mut despawn_set = HashSet::new();
    for (old_cont, _item_id, it_tid, _it_qty) in old_containers {
        crate::services::pve_dungeon_runtime::on_dungeon_item_looted(
            ctx,
            char_id,
            session.solar_system_id,
            old_cont,
            it_tid,
        )
        .await;
        despawn_set.insert(old_cont);
    }
    let mut moved_ids = Vec::with_capacity(moved.len());
    for (pre_item, post_it, fin_opt, it_tid, it_qty, src_ship_bay_owner, recursive_owner_changes) in moved {
        let fin_id = fin_opt
            .as_ref()
            .map(|r| r.item_id)
            .or_else(|| post_it.as_ref().map(|r| r.item_id))
            .unwrap_or(pre_item.item_id);
        let transferred_to_corp = (116..=122).contains(&flag_u16)
            || flag == InventoryFlag::CorpDeliveries
            || post_it.as_ref().is_some_and(|u| u.owner_id != pre_item.owner_id && u.owner_id != char_id)
            || fin_opt.as_ref().is_some_and(|u| u.owner_id != pre_item.owner_id && u.owner_id != char_id);
        if transferred_to_corp {
            let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::void_ship_insurance_contract(ctx, fin_id);
        }
        if pre_item.location_id != new_loc {
            crate::services::pve_homefront_sites::on_homefront_inventory_event_sync(
                ctx,
                char_id,
                session.solar_system_id,
                pre_item.location_id,
                it_tid,
            );
            crate::services::pve_homefront_sites::on_homefront_item_moved_sync(
                ctx,
                char_id,
                new_loc,
                it_tid,
                u64::try_from(it_qty).unwrap_or(0),
            );
        }
        let post_rec = fin_opt.as_ref().or(post_it.as_ref());
        if post_rec.is_some_and(|post| {
            post.item_id == pre_item.item_id
                && post.location_id == pre_item.location_id
                && post.flag == pre_item.flag
                && post.owner_id == pre_item.owner_id
                && post.quantity == pre_item.quantity
                && post.singleton == pre_item.singleton
                && recursive_owner_changes.is_empty()
        }) {
            moved_ids.push(fin_id);
            continue;
        }
        if let Some(fin) = fin_opt {
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::ProcessInventoryChange {
                    arg0: PyRep::Integer(fin.item_id),
                    arg1: PyRep::Integer(fin.location_id),
                    arg2: PyRep::Integer(fin.flag as i64),
                    arg3: None,
                },
            );
            for (idx, extra_owner) in [dst_ship_bay_foreign_owner, src_ship_bay_owner]
                .into_iter()
                .flatten()
                .enumerate()
            {
                let prev_dup = idx == 1 && dst_ship_bay_foreign_owner == Some(extra_owner);
                if extra_owner != char_id && !prev_dup {
                    ctx.emit_typed(
                        crate::router::NotificationTarget::Character(extra_owner),
                        crate::router::notify_registry::ProcessInventoryChange {
                            arg0: PyRep::Integer(fin.item_id),
                            arg1: PyRep::Integer(fin.location_id),
                            arg2: PyRep::Integer(fin.flag as i64),
                            arg3: None,
                        },
                    );
                }
            }
        } else if let Some(updated) = post_it {
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::ProcessInventoryChange {
                    arg0: PyRep::Integer(updated.item_id),
                    arg1: PyRep::Integer(updated.location_id),
                    arg2: PyRep::Integer(updated.flag as i64),
                    arg3: None,
                },
            );
            for (idx, extra_owner) in [dst_ship_bay_foreign_owner, src_ship_bay_owner]
                .into_iter()
                .flatten()
                .enumerate()
            {
                let prev_dup = idx == 1 && dst_ship_bay_foreign_owner == Some(extra_owner);
                if extra_owner != char_id && !prev_dup {
                    ctx.emit_typed(
                        crate::router::NotificationTarget::Character(extra_owner),
                        crate::router::notify_registry::ProcessInventoryChange {
                            arg0: PyRep::Integer(updated.item_id),
                            arg1: PyRep::Integer(updated.location_id),
                            arg2: PyRep::Integer(updated.flag as i64),
                            arg3: None,
                        },
                    );
                }
            }
        }
        moved_ids.push(fin_id);
    }
    for old_cont in despawn_set {
        let _ = despawn_empty_jetcan_if_needed(ctx, session.solar_system_id, old_cont).await;
    }
    let moved = moved_ids;
    let mut source_ships = HashSet::new();
    for &(old_loc, item_id, _) in &fitted_unfit_ids {
        let _ = crate::services::notify_emitters_space_dungeons::flush_module_profile_and_ewar_end(
            ctx,
            char_id,
            session.solar_system_id,
            old_loc,
            item_id,
        );
        crate::services::tactical_modules::remove_projected_remote_assistance(ctx, old_loc, item_id);
        if let Some(sys_id) = session.solar_system_id {
            let _ = ctx.systems.get_or_spawn(sys_id).remove_module_cycle(old_loc, item_id).await;
        }
        source_ships.insert(old_loc);
    }
    for old_ship in source_ships {
        ctx.invalidate_ship_fitting(old_ship);
        crate::services::notify_emitters_skills_trade_godma::emit_ship_fitting_attr_changes_for_ship(
            ctx,
            char_id,
            old_ship,
            old_ship_attrs.get(&old_ship),
            false,
        );
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

fn validate_auto_fit_ship_in_snap<'a>(
    ctx: &'a ServerContext,
    snap: &'a eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    ship_id: i64,
) -> Result<(
    &'a eve_db::CharacterRecord,
    &'a eve_sde::InvType,
), BridgeRpcError> {
    let ch = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError {
        code: "CharacterNotFound".into(),
        message: format!("Character {char_id} not found"),
        client_error: None,
    })?;
    let ship_item = snap.items.get(&ship_id).ok_or_else(|| BridgeRpcError {
        code: "ShipNotFound".into(),
        message: format!("Target ship {ship_id} not found"),
        client_error: None,
    })?;
    let ship_t = ctx.sde.types.get(&ship_item.type_id).ok_or_else(|| BridgeRpcError {
        code: "InvalidFittingTarget".into(),
        message: format!("Unknown hull type {}", ship_item.type_id),
        client_error: None,
    })?;
    if ship_item.owner_id != char_id || ship_t.category_id != 6 {
        return Err(BridgeRpcError {
            code: "InvalidFittingTarget".into(),
            message: format!("Target {ship_id} is not an owned ship"),
            client_error: None,
        });
    }
    fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, ship_item)?;
    let ch_dock = if session.station_id.is_none()
        && session.structure_id.is_none()
        && session.solar_system_id.is_some()
    {
        0
    } else {
        ch.station_id.or(ch.structure_id).unwrap_or(0)
    };
    if ch_dock > 0 {
        crate::services::structure_acl_defense_rules::validate_docked_ship_fitting_facility_in_snap(
            ctx, snap, ch_dock,
        )?;
    }
    Ok((ch, ship_t))
}

#[allow(clippy::too_many_arguments)]
fn validate_auto_fit_item_in_snap<'a>(
    ctx: &'a ServerContext,
    snap: &'a eve_db::WorldStateSnapshot,
    session: &SessionState,
    ch: &eve_db::CharacterRecord,
    char_id: i64,
    ship_id: i64,
    ship_t: &eve_sde::InvType,
    item_id: i64,
    looted_from_space_ids: &HashSet<i64>,
    pre_balls: &HashMap<i64, (glam::DVec3, f64)>,
) -> Result<(&'a eve_db::InventoryItemRecord, &'a eve_sde::InvType), BridgeRpcError> {
    let item = snap.items.get(&item_id).ok_or_else(|| BridgeRpcError {
        code: "ItemNotFound".into(),
        message: format!("Item {item_id} not found"),
        client_error: None,
    })?;
    fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, item)?;
    if snap.is_item_in_alsc_lock(item_id) {
        let msg = format!("Item {item_id} is locked inside an Audit Log Secure Container");
        return Err(BridgeRpcError::with_client_error(
            "ALSCItemLocked",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        ));
    }
    if snap.is_item_or_ancestor_locked_or_impounded(item_id) {
        return Err(BridgeRpcError {
            code: "ItemLocked".into(),
            message: format!("Item {item_id} is locked by an active contract or job"),
            client_error: None,
        });
    }
    if snap
        .item_custom_names
        .get(&item_id)
        .is_some_and(|s| s.starts_with("courier_cargo:"))
    {
        return Err(BridgeRpcError {
            code: "CannotDeliverCourierCargo".into(),
            message: "Courier cargo cannot be fitted".into(),
            client_error: None,
        });
    }
    let is_corp_item = ch.corporation_id > 0 && item.owner_id == ch.corporation_id;
    if is_corp_item {
        crate::services::inventory_bound::validate_corp_source_item_take_access_in_snap(
            ctx, snap, char_id, ch.corporation_id, item,
        )?;
    } else if item.owner_id != char_id && !looted_from_space_ids.contains(&item_id) {
        return Err(BridgeRpcError {
            code: "ItemOwnershipViolation".into(),
            message: format!("Item {item_id} is not owned by {char_id}"),
            client_error: None,
        });
    }
    if !crate::services::inventory_bound::is_inventory_location_colocated_with_sde(
        &ctx.sde, snap, session, ch, pre_balls, item.location_id, false,
    ) {
        return Err(BridgeRpcError {
            code: "ItemOwnershipViolation".into(),
            message: format!("Item {item_id} location not reachable"),
            client_error: None,
        });
    }
    if item.location_id == ship_id && matches!(item.flag as u16, 11..=34 | 92..=94 | 125..=132) {
        return Err(BridgeRpcError {
            code: "InvalidLocation".into(),
            message: format!("Item {item_id} is already fitted on ship {ship_id}"),
            client_error: None,
        });
    }
    let mod_t = ctx.sde.types.get(&item.type_id).ok_or_else(|| BridgeRpcError {
        code: "ItemNotFittableModule".into(),
        message: format!("Unknown type {}", item.type_id),
        client_error: None,
    })?;
    if mod_t.category_id != 7 && mod_t.category_id != 32 {
        return Err(BridgeRpcError {
            code: "ItemNotFittableModule".into(),
            message: format!("Item {item_id} (type {}, category {}) cannot be fitted", item.type_id, mod_t.category_id),
            client_error: None,
        });
    }
    let is_ehe = matches!(item.type_id, 40714 | 40717) || mod_t.effect_ids.contains(&6484);
    if is_ehe
        && item
            .dynamic_attributes
            .get(&eve_sde::attr_ids::DAMAGE)
            .copied()
            .unwrap_or(0.0)
            > 0.0
    {
        return Err(BridgeRpcError {
            code: "CannotRefitDamagedModule".into(),
            message: "UserError: Damaged or burned-out Capital Emergency Hull Energizer cannot be fitted".into(),
            client_error: None,
        });
    }
    crate::services::tactical_module_mechanics::validate_module_fitting_constraints(
        ctx, snap, ship_id, item_id, mod_t, ship_t,
    )?;
    Ok((item, mod_t))
}

async fn handle_multi_add_auto_fit(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    ship_id: i64,
    item_ids: &[i64],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    if item_ids.contains(&ship_id) {
        return Err(BridgeRpcError {
            code: "CannotMoveActiveShip".into(),
            message: format!("Cannot move ship {ship_id} into itself"),
            client_error: None,
        });
    }
    if session.station_id.is_none() && session.structure_id.is_none() {
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
                message: "Cannot fit modules in space while weapons timer is active".into(),
                client_error: None,
            });
        }
        let near_fitting_service = if let (Some(sys_id), Some(active_id)) = (session.solar_system_id, session.ship_id) {
            fitting_containers_ships::is_near_in_space_fitting_service(ctx, sys_id, active_id).await
        } else {
            false
        };
        if !near_fitting_service {
            return Err(BridgeRpcError {
                code: "CannotFitInSpace".into(),
                message: "Fitting modules into ship slots requires being docked at a station or structure or within range of a Mobile Depot or capital SMA".into(),
                client_error: None,
            });
        }
    }

    let mut looted_from_space_ids: HashSet<i64> = HashSet::new();
    let mut any_foreign_theft = false;
    for &item_id in item_ids {
        if let Some(src_item) = ctx.db.with_snapshot(|snap| snap.items.get(&item_id).cloned()) {
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

    let pre_balls = crate::services::structure_acl_defense_rules::snapshot_system_balls_for_inventory(
        ctx, session, char_id,
    );
    let mut temp_fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    let mut prev_ship_attrs =
        crate::services::notify_emitters_skills_trade_godma::snapshot_ship_fitting_attrs(
            ctx,
            &temp_fit,
        );
    let supplied_pw = kwargs.get("password").and_then(|v| v.as_str());
    let (mut used_slots, item_meta) = ctx.db.with_snapshot(|snap| -> Result<_, BridgeRpcError> {
        let (ch, ship_t) = validate_auto_fit_ship_in_snap(ctx, snap, session, char_id, ship_id)?;
        let used: HashSet<u16> = snap
            .items
            .values()
            .filter(|other| {
                other.location_id == ship_id
                    && matches!(other.flag as u16, 11..=34 | 92..=94 | 125..=132)
                    && ctx.sde.types.get(&other.type_id).is_none_or(|t| t.category_id == 7 || t.category_id == 32)
            })
            .map(|other| other.flag as u16)
            .collect();
        let mut meta = Vec::with_capacity(item_ids.len());
        for &item_id in item_ids {
            let (item, mod_t) = validate_auto_fit_item_in_snap(
                ctx, snap, session, ch, char_id, ship_id, ship_t, item_id, &looted_from_space_ids, &pre_balls,
            )?;
            let (cand_slots, default_online): (Vec<u16>, bool) = if mod_t.effect_ids.contains(&12) {
                ((27_u16..=34).collect(), true)
            } else if mod_t.effect_ids.contains(&13) {
                ((19_u16..=26).collect(), true)
            } else if mod_t.effect_ids.contains(&11) {
                ((11_u16..=18).collect(), true)
            } else if mod_t.effect_ids.contains(&2663) {
                ((92_u16..=94).collect(), true)
            } else if mod_t.effect_ids.contains(&3772) {
                if let Some(&req_slot_f) = mod_t.default_attributes.get(&1366) {
                    (vec![req_slot_f as u16], true)
                } else {
                    ((125_u16..=132).collect(), true)
                }
            } else {
                return Err(BridgeRpcError {
                    code: "ItemNotFittableModule".into(),
                    message: format!("Module {} lacks a fitting slot effect", mod_t.name),
                    client_error: None,
                });
            };
            meta.push((item.clone(), cand_slots, default_online));
        }
        Ok((used, meta))
    })?;
    for (item, _, _) in &item_meta {
        fitting_containers_ships::validate_container_and_bay_move(
            ctx,
            char_id,
            item.item_id,
            ship_id,
            27,
            kwargs,
            (item.quantity > 1 && !item.singleton).then_some(1),
        )?;
    }

    struct PlannedFit {
        src_item: eve_db::InventoryItemRecord,
        target_flag: u16,
        online: bool,
    }
    let mut planned = Vec::with_capacity(item_meta.len());
    let mut planned_temp_ids: HashSet<i64> = HashSet::new();
    for (idx, (src_item, cand_slots, online)) in item_meta.into_iter().enumerate() {
        let needs_split = !src_item.singleton && !src_item.is_blueprint_copy && src_item.quantity > 1;
        let sim_id = if !needs_split && planned_temp_ids.insert(src_item.item_id) {
            src_item.item_id
        } else {
            -((idx as i64) + 1)
        };
        let mut chosen: Option<u16> = None;
        let mut last_err: Option<String> = None;
        for cand_flag in cand_slots {
            if used_slots.contains(&cand_flag) {
                continue;
            }
            match temp_fit.fit_module_with_state(sim_id, src_item.type_id, cand_flag, online, &ctx.sde) {
                Ok(()) => {
                    chosen = Some(cand_flag);
                    used_slots.insert(cand_flag);
                    break;
                }
                Err(msg) => {
                    if last_err.is_none() {
                        last_err = Some(msg);
                    }
                }
            }
        }
        let Some(target_flag) = chosen else {
            let module_name = ctx
                .sde
                .get_type(src_item.type_id)
                .map(|t| t.name.as_str())
                .unwrap_or("Module");
            let reason = last_err.unwrap_or_else(|| {
                format!("No free compatible fitting slot for item {}", src_item.item_id)
            });
            return Err(BridgeRpcError::fitting_validation_failed(
                Some(ship_id),
                Some(src_item.item_id),
                Some(i64::from(src_item.type_id)),
                module_name,
                reason,
            ));
        };
        planned.push(PlannedFit {
            src_item,
            target_flag,
            online,
        });
    }

    let now_unix = ctx.now_unix_secs();
    let committed = ctx.db.try_with_mut_snapshot(|snap| -> Result<Vec<_>, BridgeRpcError> {
        let (ch, ship_t) = validate_auto_fit_ship_in_snap(ctx, snap, session, char_id, ship_id)?;
        let ch_snap = ch.clone();
        let ship_t_snap = ship_t.clone();
        let mut out = Vec::with_capacity(planned.len());
        for plan in &planned {
            let (live_item, _) = validate_auto_fit_item_in_snap(
                ctx, snap, session, &ch_snap, char_id, ship_id, &ship_t_snap, plan.src_item.item_id, &looted_from_space_ids, &pre_balls,
            )?;
            fitting_containers_ships::validate_src_container_access_in_snap(
                ctx, snap, char_id, live_item, ship_id, supplied_pw,
            )?;
            if snap.items.values().any(|other| {
                other.location_id == ship_id
                    && (other.flag as u16) == plan.target_flag
                    && ctx.sde.types.get(&other.type_id).is_none_or(|t| t.category_id == 7 || t.category_id == 32)
            }) {
                return Err(BridgeRpcError::from_user_key(
                    eve_proto::UserErrorKey::SlotAlreadyOccupied,
                ));
            }
            let (old_loc, live_owner) = (live_item.location_id, live_item.owner_id);
            let do_split = !live_item.singleton && !live_item.is_blueprint_copy && live_item.quantity > 1;
            let fitted_id = if do_split {
                let (_orig_rec, split_item) = snap
                    .split_stack_in_snap(live_owner, plan.src_item.item_id, 1)
                    .map_err(|e| BridgeRpcError::custom_info(e.to_string()))?;
                split_item.item_id
            } else {
                plan.src_item.item_id
            };
            let target_flag_enum = InventoryFlag::from_u16(plan.target_flag);
            let it = snap.items.get_mut(&fitted_id).ok_or_else(|| BridgeRpcError {
                code: "ItemNotFound".into(),
                message: format!("Item {fitted_id} not found"),
                client_error: None,
            })?;
            it.owner_id = char_id;
            it.location_id = ship_id;
            it.flag = target_flag_enum;
            it.singleton = true;
            it.version += 1;
            let updated = it.clone();
            snap.module_online_states.insert(fitted_id, plan.online);
            fitting_containers_ships::record_inv_move_logs_in_snap(
                &ctx.sde, snap, char_id, updated.item_id, plan.src_item.item_id, updated.type_id, updated.quantity, old_loc, ship_id, true, now_unix,
            )?;
            out.push((plan, updated, old_loc));
        }
        Ok(out)
    })?;

    let mut fitted_ids = Vec::with_capacity(committed.len());
    let mut despawn_set = HashSet::new();
    for (plan, updated, old_loc) in committed {
        let target_flag_enum = InventoryFlag::from_u16(plan.target_flag);
        if old_loc != ship_id {
            crate::services::pve_dungeon_runtime::on_dungeon_item_looted(
                ctx,
                char_id,
                session.solar_system_id,
                old_loc,
                updated.type_id,
            )
            .await;
            crate::services::pve_homefront_sites::on_homefront_item_moved_sync(
                ctx,
                char_id,
                ship_id,
                updated.type_id,
                u64::try_from(updated.quantity).unwrap_or(0),
            );
            despawn_set.insert(old_loc);
        }
        crate::services::operations_manager_helpers::record_invbroker_add_operation_event(
            ctx,
            char_id,
            old_loc,
            ship_id,
            target_flag_enum,
            updated.type_id,
            updated.quantity,
        );
        ctx.invalidate_ship_fitting(ship_id);
        let rebuilt_fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();

        ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::ProcessInventoryChange {
                arg0: PyRep::Integer(updated.item_id),
                arg1: PyRep::Integer(updated.location_id),
                arg2: PyRep::Integer(updated.flag as i64),
                arg3: None,
            },
        );
        let group_id = ctx
            .sde
            .types
            .get(&updated.type_id)
            .map(|t| i64::from(t.group_id))
            .unwrap_or(0);
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnModuleFitted {
                char_id,
                flag: updated.flag as i64,
                type_id: updated.type_id as i64,
                group_id,
            },
        );
        let _ = ctx.notify_module_online_state(char_id, updated.item_id, plan.online);
        if plan.online {
            let eff_id = ctx
                .effect_id_for_module(updated.type_id, Some("online"))
                .unwrap_or(16) as i32;
            let _ = ctx.notify_godma_ship_effect(
                crate::router::NotificationTarget::Character(char_id),
                updated.item_id,
                eff_id,
                true,
                0,
                0,
            );
        }
        if let Some(ref new_fit) = rebuilt_fit {
            if let Some(next_attrs) =
                crate::services::notify_emitters_skills_trade_godma::emit_ship_fitting_attr_changes_from_fit(
                    ctx,
                    char_id,
                    ship_id,
                    Some(&prev_ship_attrs),
                    new_fit,
                    true,
                )
            {
                prev_ship_attrs = next_attrs;
            }
        }
        fitted_ids.push(updated.item_id);
    }

    for old_cont in despawn_set {
        let _ = despawn_empty_jetcan_if_needed(ctx, session.solar_system_id, old_cont).await;
    }
    if any_foreign_theft {
        apply_space_loot_suspect_flag(ctx, session, char_id);
    }
    Ok(PyRep::keyval([
        ("movedCount", PyRep::Integer(fitted_ids.len() as i64)),
        (
            "items",
            PyRep::List(fitted_ids.into_iter().map(PyRep::Integer).collect()),
        ),
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
    crate::services::combat_crimewatch::crimewatch_timers::emit_crimewatch_timer_notifications(
        ctx,
        char_id,
        ctx.now_unix_secs(),
    );
}

/// Evaluates CrimeWatch theft legality when taking an item from a space wreck (`group_id == 186`, any flag)
/// or jetcan (`type_id == 23`) (`feat:lifecycle.wreck-take_item`).
pub(crate) fn evaluate_space_wreck_or_jetcan_loot(
    ctx: &ServerContext,
    char_id: i64,
    src_item: &eve_db::InventoryItemRecord,
) -> Result<(bool, bool), BridgeRpcError> {
    if let Some(allowed) = crate::services::pve_crab_beacons::check_crab_loot_access(
        ctx,
        src_item.location_id,
        char_id,
        false,
    )? {
        return Ok((allowed, false));
    }
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
    let safety = crate::services::combat_crimewatch::char_safety_level(ctx, char_id)
        .unwrap_or(crate::services::combat_crimewatch::SafetyLevel::Red);
    if safety == crate::services::combat_crimewatch::SafetyLevel::Green {
        return Err(BridgeRpcError::user_error("CantDoInCurrentSafetyLevel"));
    }
    Ok((true, true))
}

fn check_container_password_snap(
    snap: &eve_db::WorldStateSnapshot,
    container_id: i64,
    char_id: i64,
    supplied_pw: Option<&str>,
    verb: &str,
) -> Result<bool, BridgeRpcError> {
    if let Some(req_pw) = fitting_containers_ships::container_password_snap(snap, container_id, 1) {
        if !fitting_containers_ships::is_container_unlocked_snap(snap, container_id, char_id) {
            if supplied_pw != Some(req_pw.as_str()) {
                return Err(BridgeRpcError {
                    code: "ALSCPasswordRequired".into(),
                    message: format!("Password required to {verb} container {container_id}"),
                    client_error: None,
                });
            }
            return Ok(true);
        }
    }
    Ok(false)
}

fn unlock_container_if_password_valid_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    container_id: i64,
    char_id: i64,
    supplied_pw: Option<&str>,
    verb: &str,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    if check_container_password_snap(snap, container_id, char_id, supplied_pw, verb)? {
        fitting_containers_ships::set_container_unlocked_snap(
            snap,
            container_id,
            char_id,
            true,
            now_unix,
        )?;
    }
    Ok(())
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
            client_error: None,
        });
    };
    // V24.02 sends `(containerID, locationID, password)` (`args[2]` is password, while legacy callers
    // may pass `"password"` in kwargs or `(containerID, password)` in 2-arg form).
    let supplied_pw = kwargs
        .get("password")
        .and_then(|v| v.as_str())
        .or_else(|| args.get(2).and_then(|a| a.as_str()))
        .or_else(|| args.get(1).and_then(|a| a.as_str()));
    crate::services::pve_crab_beacons::check_crab_loot_access(
        ctx,
        container_id,
        char_id,
        session.has_gm_role(),
    )?;
    let (is_secure_or_pw, is_alsc) =
        ctx.db
            .with_snapshot(|snap| -> Result<(bool, bool), BridgeRpcError> {
                let grp = snap
                    .items
                    .get(&container_id)
                    .and_then(|it| ctx.sde.types.get(&it.type_id))
                    .map(|t| t.group_id)
                    .unwrap_or(0);
                let is_alsc = grp == 448;
                let has_pw =
                    fitting_containers_ships::container_password_snap(snap, container_id, 1)
                        .is_some();
                check_container_password_snap(snap, container_id, char_id, supplied_pw, "open")?;
                Ok((matches!(grp, 340 | 448) || has_pw, is_alsc))
            })?;
    let out = ctx
        .get_container_contents(session, bound_entry, char_id, container_id)
        .await?;
    if is_secure_or_pw || is_alsc {
        let now_unix = ctx.now_unix_secs();
        ctx.db
            .try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                unlock_container_if_password_valid_snap(
                    snap,
                    container_id,
                    char_id,
                    supplied_pw,
                    "open",
                    now_unix,
                )?;
                if is_alsc {
                    fitting_containers_ships::append_alsc_log_snap(
                        snap,
                        container_id,
                        fitting_containers_ships::AlscLogEntry {
                            timestamp_unix: now_unix,
                            actor_id: char_id,
                            action: "ALSCActionOpen".into(),
                            item_id: container_id,
                            item_type_id: 0,
                            quantity: 0,
                        },
                        now_unix,
                    )?;
                }
                Ok(())
            })?;
    }
    Ok(out)
}

fn extract_flag_list(args: &[PyRep], kwargs: &HashMap<String, PyRep>) -> Vec<i64> {
    let raw = args.first().or_else(|| kwargs.get("flags"));
    match raw {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => {
            items.iter().filter_map(|v| v.as_i64()).collect()
        }
        Some(PyRep::Integer(f)) => vec![*f],
        _ => Vec::new(),
    }
}

pub(super) fn handle_list_by_flags(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let flags = extract_flag_list(args, kwargs);
    handle_list_impl(ctx, session, bound_entry, args, kwargs, Some(&flags))
}

pub(super) fn handle_list(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let list_flag_filter = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()))
        .filter(|&f| f > 0);
    handle_list_impl(
        ctx,
        session,
        bound_entry,
        args,
        kwargs,
        list_flag_filter.as_ref().map(std::slice::from_ref),
    )
}

fn handle_list_impl(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
    flags_filter: Option<&[i64]>,
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
            client_error: None,
        });
    };
    if ((60_000_000..=69_999_999).contains(&bound_loc_id) || bound_loc_id >= 1_000_000_000_000)
        && session.station_id != Some(bound_loc_id)
        && session.structure_id != Some(bound_loc_id)
    {
        return Err(BridgeRpcError {
            code: "BoundObjectOwnershipViolation".into(),
            message: format!("Character {char_id} is not docked at bound inventory location {bound_loc_id}"),
            client_error: None,
        });
    }
    let mut corp_flags: Vec<i64> = Vec::new();
    if let Some(flags) = flags_filter {
        for &f in flags {
            if (116..=122).contains(&f) && !corp_flags.contains(&f) {
                corp_flags.push(f);
            }
        }
    }
    if !corp_flags.is_empty() {
        let corp_id = session.corporation_id.or_else(|| {
            ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id))
        }).unwrap_or(0);
        let (is_ceo, roles) = get_effective_corp_roles_at_location(ctx, corp_id, char_id, Some(bound_loc_id));
        for flag_val in corp_flags {
            let div_idx = (flag_val - 116) as u32;
            let req_query_role = 1u64 << (20 + div_idx);
            if !is_ceo && !eve_db::corp_roles::has_role(roles, req_query_role) {
                return Err(BridgeRpcError {
                    code: "CorpRoleDenied".into(),
                    message: format!("Corporation role denied for division {} query", div_idx + 1),
                    client_error: None,
                });
            }
        }
    }
    {
        let supplied_pw = kwargs
            .get("password")
            .and_then(|v| v.as_str())
            .or_else(|| args.iter().find_map(|a| a.as_str()));
        let now_unix = ctx.now_unix_secs();
        ctx.db.try_with_mut_snapshot(|snap| {
            unlock_container_if_password_valid_snap(
                snap,
                bound_loc_id,
                char_id,
                supplied_pw,
                "list contents of",
                now_unix,
            )
        })?;
    }
    crate::services::pve_crab_beacons::check_crab_loot_access(
        ctx,
        bound_loc_id,
        char_id,
        session.has_gm_role(),
    )?;
    let fleet_shipping_authorized =
        crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.authorized_shipping_storage.contains(&(bound_loc_id, char_id))
        });
    enum ShipListAccess {
        NotShip,
        Owner,
        Denied,
        Foreign { can_sma: bool, can_fh: bool },
    }

    let unit_check: Result<(bool, ShipListAccess, Option<i64>), BridgeRpcError> = ctx.db.with_snapshot(|snap| {
        if let Some(it) = snap.items.get(&bound_loc_id) {
            let cat = ctx.sde.types.get(&it.type_id).map(|t| t.category_id).unwrap_or(0);
            let grp = ctx.sde.types.get(&it.type_id).map(|t| t.group_id).unwrap_or(0);
            let is_personal = cat == 22 || grp == 340;
            let is_unit = cat == 22 || matches!(grp, 12 | 186 | 340 | 448 | 649 | 4137) || it.type_id == 23;
            let is_fleet_auth = !is_personal
                && snap.fleets.values().any(|f| f.members.contains(&char_id) && f.members.contains(&it.owner_id))
                && fleet_shipping_authorized;
            let is_auth = session.has_gm_role() || it.owner_id == char_id || it.owner_id == 0 || is_fleet_auth;
            if is_unit && !session.has_gm_role() {
                if let Some((host_ship, _)) =
                    super::inventory_bound::find_ship_bay_ancestor_in_snap(
                        ctx,
                        snap,
                        it.location_id,
                        it.flag as u16,
                    )
                {
                    if host_ship.owner_id != char_id {
                        let supplied_pw = kwargs
                            .get("password")
                            .and_then(|v| v.as_str())
                            .or_else(|| args.iter().find_map(|a| a.as_str()));
                        let _ = super::inventory_bound::check_foreign_ship_bay_ancestor_chain_in_snap(
                            ctx,
                            snap,
                            session,
                            char_id,
                            it.location_id,
                            it.flag as u16,
                            supplied_pw,
                            false,
                        )?;
                        if !verify_ship_proximity_in_snap(
                            ctx, snap, session, char_id, host_ship, bound_loc_id, true,
                        )? {
                            return Ok((false, ShipListAccess::Denied, None));
                        }
                        return Ok((true, ShipListAccess::NotShip, Some(host_ship.item_id)));
                    }
                }
            }
            if is_unit && is_auth {
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
                            client_error: None,
                        });
                    }
                    let max_range = if cat == 22 { 5_000.0 } else { 2_500.0 };
                    let ship_id = session.ship_id.unwrap_or(0);
                    if let Some(edge_dist) =
                        edge_distance_in_space_snap(ctx, snap, sys_id, ship_id, bound_loc_id, it.type_id)
                    {
                        if edge_dist > max_range {
                            return Err(BridgeRpcError {
                                code: "OutOfRange".into(),
                                message: format!("Container {bound_loc_id} is out of range ({edge_dist:.1}m > {max_range:.0}m)"),
                                client_error: None,
                            });
                        }
                    } else if it.owner_id != char_id {
                        return Ok((false, ShipListAccess::NotShip, None));
                    }
                }
                return Ok((true, ShipListAccess::NotShip, None));
            }
            if cat == 6 {
                if it.owner_id == char_id || session.has_gm_role() {
                    return Ok((false, ShipListAccess::Owner, None));
                }
                if !verify_ship_proximity_in_snap(
                    ctx, snap, session, char_id, it, bound_loc_id, false,
                )? {
                    return Ok((false, ShipListAccess::Denied, None));
                }
                let supplied_pw = kwargs
                    .get("password")
                    .and_then(|v| v.as_str())
                    .or_else(|| args.iter().find_map(|a| a.as_str()));
                let can_sma = fitting_containers_ships::can_character_access_ship_bay_with_password_snap(
                    snap, char_id, bound_loc_id, it.owner_id, true, supplied_pw,
                );
                let can_fh = fitting_containers_ships::can_character_access_ship_bay_with_password_snap(
                    snap, char_id, bound_loc_id, it.owner_id, false, supplied_pw,
                );
                if let Some(flags) = flags_filter {
                    let wants_sma = flags.iter().any(|&f| matches!(f, 90 | 138));
                    let wants_fh = flags.iter().any(|&f| f == 155);
                    if (wants_sma || wants_fh) && !((wants_sma && can_sma) || (wants_fh && can_fh)) {
                        let deny_sma = wants_sma && !wants_fh;
                        return Err(BridgeRpcError {
                            code: if deny_sma { "SMAAccessDenied" } else { "FleetHangarAccessDenied" }.into(),
                            message: if deny_sma {
                                "Ship Maintenance Bay access denied by ship configuration or password"
                            } else {
                                "Fleet Hangar access denied by ship configuration or password"
                            }
                            .into(),
                            client_error: None,
                        });
                    }
                }
                return Ok((
                    false,
                    ShipListAccess::Foreign { can_sma, can_fh },
                    (can_sma || can_fh).then_some(bound_loc_id),
                ));
            }
            Ok((false, ShipListAccess::NotShip, None))
        } else {
            Ok((false, ShipListAccess::NotShip, None))
        }
    });
    let (is_deployable_or_shipping_unit, ship_list_access, foreign_unlock_ship_id) = unit_check?;
    let supplied_pw_opt = kwargs
        .get("password")
        .and_then(|v| v.as_str())
        .or_else(|| args.iter().find_map(|a| a.as_str()));
    let rows = if is_deployable_or_shipping_unit {
        if foreign_unlock_ship_id.is_some() {
            list_with_optional_ship_unlock(ctx, char_id, supplied_pw_opt.is_some(), |snap| {
                let host_ship_id = verify_nested_unit_list_in_snap(
                    ctx, snap, session, char_id, bound_loc_id, supplied_pw_opt,
                )?;
                let unlock_id = host_ship_id.zip(supplied_pw_opt).and_then(|(sid, pw)| {
                    (fitting_containers_ships::ship_access_config_snap(snap, sid).password.as_deref() == Some(pw))
                        .then_some(sid)
                });
                Ok((unlock_id, collect_unit_list_rows(ctx, snap, bound_loc_id, flags_filter.as_deref())))
            })?
        } else if flags_filter.as_deref().is_some_and(|f| f.is_empty()) {
            Vec::new()
        } else {
            ctx.db.with_snapshot(|snap| collect_unit_list_rows(ctx, snap, bound_loc_id, flags_filter.as_deref()))
        }
    } else if matches!(ship_list_access, ShipListAccess::Denied) {
        Vec::new()
    } else if let ShipListAccess::Foreign { can_sma, can_fh } = ship_list_access {
        if !can_sma && !can_fh {
            Vec::new()
        } else {
            list_with_optional_ship_unlock(
                ctx,
                char_id,
                supplied_pw_opt.is_some() && foreign_unlock_ship_id.is_some(),
                |snap| {
                    verify_and_collect_foreign_ship_rows(
                        ctx, snap, session, char_id, bound_loc_id, can_sma, can_fh,
                        flags_filter.as_deref(), supplied_pw_opt,
                    )
                },
            )?
        }
    } else if let Some(flags) = flags_filter {
        if flags.is_empty() {
            Vec::new()
        } else {
            let mut out_rows = Vec::new();
            let mut seen_ids = HashSet::new();
            let needs_snap_items = flags.iter().any(|f| matches!(f, 62 | 116..=122 | 187))
                || (matches!(ship_list_access, ShipListAccess::Owner)
                    && flags.iter().any(|&f| matches!(f, 90 | 138 | 155)));
            if needs_snap_items {
                ctx.db.with_snapshot(|snap| {
                    let corp_id = session
                        .corporation_id
                        .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id))
                        .unwrap_or(0);
                    for i in snap.items.values().filter(|i| {
                        i.location_id == bound_loc_id && flags.contains(&(i.flag as i64))
                    }) {
                        let f = i.flag as u16;
                        let corp_match = matches!(f, 116..=122) && i.owner_id == corp_id;
                        let office_match = matches!(f, 62 | 187)
                            && (i.owner_id == char_id || (corp_id > 0 && i.owner_id == corp_id));
                        let owner_bay_match = matches!(ship_list_access, ShipListAccess::Owner)
                            && matches!(f, 90 | 138 | 155);
                        if (corp_match || office_match || owner_bay_match) && seen_ids.insert(i.item_id) {
                            out_rows.push(item_to_packed_cells(ctx, i));
                        }
                    }
                });
            }
            if flags.iter().any(|f| !matches!(f, 62 | 116..=122 | 187)) {
                for i in ctx
                    .db
                    .get_items_for_character(char_id)
                    .into_iter()
                    .filter(|i| {
                        i.location_id == bound_loc_id
                            && !matches!(i.flag as i64, 62 | 116..=122 | 187)
                            && flags.contains(&(i.flag as i64))
                    })
                {
                    if seen_ids.insert(i.item_id) {
                        out_rows.push(item_to_packed_cells(ctx, &i));
                    }
                }
            }
            out_rows
        }
    } else if matches!(ship_list_access, ShipListAccess::Owner) {
        ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| {
                    i.location_id == bound_loc_id
                        && (i.owner_id == char_id || matches!(i.flag as u16, 90 | 138 | 155))
                })
                .map(|i| item_to_packed_cells(ctx, i))
                .collect()
        })
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

fn collect_unit_list_rows(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    bound_loc_id: i64,
    flags_filter: Option<&[i64]>,
) -> Vec<Vec<PyRep>> {
    if let Some(flags) = flags_filter {
        if flags.is_empty() {
            return Vec::new();
        }
    }
    snap.items
        .values()
        .filter(|i| i.location_id == bound_loc_id && flags_filter.map_or(true, |f| f.contains(&(i.flag as i64))))
        .map(|i| item_to_packed_cells(ctx, i))
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn verify_and_collect_foreign_ship_rows(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    bound_loc_id: i64,
    orig_can_sma: bool,
    orig_can_fh: bool,
    flags_filter: Option<&[i64]>,
    supplied_pw_opt: Option<&str>,
) -> Result<(Option<i64>, Vec<Vec<PyRep>>), BridgeRpcError> {
    if !orig_can_sma && !orig_can_fh {
        return Ok((None, Vec::new()));
    }
    let wants_sma = flags_filter.is_some_and(|f| f.iter().any(|&v| matches!(v, 90 | 138)));
    let wants_fh = flags_filter.is_some_and(|f| f.iter().any(|&v| v == 155));
    let deny_sma = (wants_sma && !wants_fh) || (!wants_sma && !wants_fh && orig_can_sma);
    let Some(sh) = snap.items.get(&bound_loc_id) else {
        return Err(foreign_ship_deny_err(deny_sma, format!("Ship {bound_loc_id} no longer exists")));
    };
    if !ctx.sde.types.get(&sh.type_id).is_some_and(|t| t.category_id == 6) {
        return Err(foreign_ship_deny_err(deny_sma, format!("Item {bound_loc_id} is not a ship")));
    }
    if sh.owner_id == char_id || session.has_gm_role() {
        let rows = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == bound_loc_id
                    && (i.owner_id == char_id || matches!(i.flag as u16, 90 | 138 | 155))
                    && flags_filter.map_or(true, |f| f.contains(&(i.flag as i64)))
            })
            .map(|i| item_to_packed_cells(ctx, i))
            .collect();
        return Ok((None, rows));
    }
    if !verify_ship_proximity_in_snap(ctx, snap, session, char_id, sh, bound_loc_id, false)? {
        return Ok((None, Vec::new()));
    }
    let live_can_sma = fitting_containers_ships::can_character_access_ship_bay_with_password_snap(
        snap, char_id, bound_loc_id, sh.owner_id, true, supplied_pw_opt,
    );
    let live_can_fh = fitting_containers_ships::can_character_access_ship_bay_with_password_snap(
        snap, char_id, bound_loc_id, sh.owner_id, false, supplied_pw_opt,
    );
    if (wants_sma || wants_fh) && !((wants_sma && live_can_sma) || (wants_fh && live_can_fh)) {
        let msg = if deny_sma {
            "Ship Maintenance Bay access denied by ship configuration or password"
        } else {
            "Fleet Hangar access denied by ship configuration or password"
        };
        return Err(foreign_ship_deny_err(deny_sma, msg.into()));
    }
    if flags_filter.is_none() && (orig_can_sma || orig_can_fh) && !live_can_sma && !live_can_fh {
        return Err(foreign_ship_deny_err(deny_sma, "Ship bay access revoked".into()));
    }
    let unlock_id = supplied_pw_opt
        .filter(|pw| {
            (live_can_sma || live_can_fh)
                && fitting_containers_ships::ship_access_config_snap(snap, bound_loc_id)
                    .password
                    .as_deref()
                    == Some(*pw)
        })
        .map(|_| bound_loc_id);
    if flags_filter.is_some_and(|f| f.is_empty()) {
        return Ok((unlock_id, Vec::new()));
    }
    let rows = snap
        .items
        .values()
        .filter(|i| {
            i.location_id == bound_loc_id
                && ((live_can_sma && matches!(i.flag as u16, 90 | 138))
                    || (live_can_fh && (i.flag as u16) == 155))
                && flags_filter.map_or(true, |f| f.contains(&(i.flag as i64)))
        })
        .map(|i| item_to_packed_cells(ctx, i))
        .collect();
    Ok((unlock_id, rows))
}

/// Immediately removes an empty jettisoned cargo container (`type_id == 23`) from inventory,
/// `container_lifetimes`, and the solar system `Ballpark`, broadcasting `RemoveBalls` with the
/// ballpark's current destiny tick stamp (`feat:lifecycle.jetcan-empty`, `#1531`).
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
        ctx.emit_typed(
            crate::router::NotificationTarget::SolarSystem(sid),
            crate::router::notify_registry::DoDestinyUpdate {
                state: PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(stamp),
                    PyRep::Tuple(vec![
                        PyRep::String("RemoveBalls".into()),
                        PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(cont_id)])]),
                    ]),
                ])]),
                wait_for_bubble: PyRep::Bool(false),
                dogma_messages: None,
                extra: None,
            },
        );
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn validate_multi_add_in_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    new_loc: i64,
    flag_u16: u16,
    item_ids: &[i64],
    looted_from_space_ids: &HashSet<i64>,
    dest_is_struct_fuel: bool,
    is_ship_hold: bool,
    eval_cargo_cap: Option<f64>,
    pre_balls: &HashMap<i64, (glam::DVec3, f64)>,
    supplied_pw: Option<&str>,
) -> Result<(Vec<(i64, i64, u16)>, HashSet<i64>), BridgeRpcError> {
    let mut fitted_unfit_ids: Vec<(i64, i64, u16)> = Vec::new();
    let mut src_foreign_ship_bay_ids: HashSet<i64> = HashSet::new();
    let ch = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError {
        code: "CharacterNotFound".into(),
        message: format!("Character {char_id} not found"),
        client_error: None,
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

    super::inventory_bound::validate_ship_only_bay_target_in_snap(ctx, snap, new_loc, flag_u16)?;
    let dst_foreign_ship_opt = super::inventory_bound::check_foreign_ship_bay_ancestor_chain_in_snap(
        ctx, snap, session, char_id, new_loc, flag_u16, supplied_pw, true,
    )?;
    let foreign_dest = snap.items.get(&new_loc).is_some_and(|d| d.owner_id != char_id);
    let dest_type_opt = snap.items.get(&new_loc).and_then(|d| ctx.sde.types.get(&d.type_id));

    if dst_foreign_ship_opt.is_some() {
        if !crate::services::inventory_bound::is_inventory_location_colocated_with_sde(
            &ctx.sde, snap, session, ch, pre_balls, new_loc, false,
        ) {
            return Err(BridgeRpcError {
                code: "DestinationAccessDenied".into(),
                message: format!("Destination {new_loc} not reachable"),
                client_error: None,
            });
        }
    } else {
        crate::services::inventory_bound::validate_multiadd_destination_with_sde(
            &ctx.sde, snap, session, ch, char_id, new_loc, dest_is_struct_fuel, pre_balls,
        )?;
    }

    let mut batch_vol = 0.0;
    for &item_id in item_ids {
        let Some(item) = snap.items.get(&item_id) else {
            return Err(BridgeRpcError {
                code: "ItemNotFound".into(),
                message: format!("Item {item_id} not found"),
                client_error: None,
            });
        };
        crate::services::inventory_bound::validate_movable_item_not_piloted_or_bare_space_in_snap(
            snap, char_id, item, new_loc, flag_u16,
        )?;
        let src_is_struct_fuel = crate::services::inventory_bound::validate_multiadd_structure_fuel_transfers(
            ctx, snap, session, char_id, item, new_loc, dest_is_struct_fuel, ch_dock, ch_sys, caller_ship_id, pre_balls,
        )?;
        let src_foreign_ship_opt = if src_is_struct_fuel {
            None
        } else {
            super::inventory_bound::check_foreign_ship_bay_ancestor_chain_in_snap(
                ctx, snap, session, char_id, item.location_id, item.flag as u16, supplied_pw, false,
            )?
        };
        if src_foreign_ship_opt.is_some() {
            src_foreign_ship_bay_ids.insert(item_id);
        }
        let is_corp_item = ch.corporation_id > 0 && item.owner_id == ch.corporation_id;
        if is_corp_item && !src_is_struct_fuel {
            crate::services::inventory_bound::validate_corp_source_item_take_access_in_snap(
                ctx,
                snap,
                char_id,
                ch.corporation_id,
                item,
            )?;
        }
        if !src_is_struct_fuel
            && item.owner_id != char_id
            && !is_corp_item
            && !looted_from_space_ids.contains(&item_id)
            && src_foreign_ship_opt.is_none()
        {
            return Err(BridgeRpcError {
                code: "ItemOwnershipViolation".into(),
                message: format!("Character {char_id} is not authorized to move item {item_id}"),
                client_error: None,
            });
        }
        if item_id == new_loc
            || snap.is_structure_controller_saved_ship(item_id)
            || (ch.active_ship_id > 0
                && item_id == ch.active_ship_id
                && (snap.items.contains_key(&new_loc) || (116..=122).contains(&flag_u16)))
        {
            return Err(BridgeRpcError {
                code: "CannotMoveActiveShip".into(),
                message: format!("Cannot move item {item_id} into {new_loc}"),
                client_error: None,
            });
        }
        crate::services::fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(
            snap, item,
        )?;
        if snap.is_item_in_alsc_lock(item_id) {
            let msg = format!("Item {item_id} is locked inside an Audit Log Secure Container");
            return Err(BridgeRpcError::with_client_error(
                "ALSCItemLocked",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            ));
        }
        if !src_is_struct_fuel {
            fitting_containers_ships::validate_src_container_access_in_snap(
                ctx, snap, char_id, item, new_loc, supplied_pw,
            )?;
        }
        if let Some(dest_item) = snap.items.get(&new_loc) {
            crate::services::fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(
                snap, dest_item,
            )?;
            if let Some(dest_type) = dest_type_opt {
                fitting_containers_ships::validate_container_dest_rules_in_snap(
                    ctx, char_id, item, dest_item, dest_type,
                )?;
            }
        }
        if matches!(item.flag as u16, 92..=94) {
            return Err(BridgeRpcError {
                code: "CannotUnfitRig".into(),
                message: "Fitted rigs cannot be moved via MultiAdd".into(),
                client_error: None,
            });
        }
        if snap.structures.contains_key(&item.location_id)
            && matches!(item.flag as u16, 11..=34 | 164..=171)
        {
            return Err(BridgeRpcError {
                code: "InvalidLocation".into(),
                message: "Structure modules must be unfitted individually".into(),
                client_error: None,
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
                client_error: None,
            });
        }
        let allow_unpos_can = looted_from_space_ids.contains(&item_id)
            || snap.items.get(&item.location_id).is_some_and(|p| {
                p.type_id == 23
                    || ctx
                        .sde
                        .types
                        .get(&p.type_id)
                        .is_some_and(|t| matches!(t.group_id, 186 | 1247 | 1250))
            });
        if !src_is_struct_fuel && !crate::services::inventory_bound::is_inventory_source_location_colocated_with_sde(
            &ctx.sde, snap, session, ch, pre_balls, item.location_id, false, allow_unpos_can,
        ) {
            return Err(BridgeRpcError {
                code: "ItemOwnershipViolation".into(),
                message: format!("Item {item_id} location not reachable"),
                client_error: None,
            });
        }
        if let Some(dest_item) = snap.items.get(&new_loc) {
            fitting_containers_ships::validate_ship_bay_item_rules_in_snap(
                ctx, snap, item_id, item, dest_item, flag_u16,
            )?;
        }
        if let Some(dest_type) = dest_type_opt {
            if is_ship_hold && dest_type.category_id == 6 {
                let Some(item_type) = ctx.sde.types.get(&item.type_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Type {} not found", item.type_id),
                        client_error: None,
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
            * (item.quantity.max(1) as f64);
    }

    if let Some(dest_type) = dest_type_opt {
        if is_ship_hold && dest_type.category_id == 6 {
            let hold_cap =
                crate::services::fitting_saved_apply::get_ship_hold_capacity(flag_u16, dest_type, eval_cargo_cap);
            if flag_u16 != 5 && hold_cap <= 0.0 {
                return Err(super::inventory_fitting::ship_hold_capacity_error(
                    flag_u16, 0.0, 0.0, batch_vol,
                ));
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
                    return Err(super::inventory_fitting::ship_hold_capacity_error(
                        flag_u16, hold_cap, used_vol, batch_vol,
                    ));
                }
            }
        } else if fitting_containers_ships::is_container_group(
            dest_type.group_id,
            dest_type.type_id,
        ) {
            if let Some(dest_item) = snap.items.get(&new_loc) {
                fitting_containers_ships::validate_container_capacity_in_snap(
                    &ctx.sde, snap, dest_item, dest_type, new_loc, item_ids, batch_vol,
                )?;
                fitting_containers_ships::validate_container_password_in_snap(
                    snap, new_loc, char_id, supplied_pw, true,
                )?;
            }
        } else if matches!(flag_u16, 90 | 155) && dest_type.category_id == 6 {
            if let Some(dest_item) = snap.items.get(&new_loc) {
                fitting_containers_ships::validate_ship_bay_existence_in_snap(
                    ctx, dest_item, flag_u16,
                )?;
                fitting_containers_ships::validate_ship_bay_batch_capacity_in_snap(
                    ctx,
                    snap,
                    dest_item,
                    new_loc,
                    flag_u16,
                    item_ids,
                    batch_vol,
                    true,
                )?;
                fitting_containers_ships::validate_ship_bay_acl_in_snap(
                    ctx,
                    snap,
                    char_id,
                    new_loc,
                    dest_item,
                    flag_u16,
                    supplied_pw,
                )?;
            }
        }
    }
    Ok((fitted_unfit_ids, src_foreign_ship_bay_ids))
}

#[cfg(test)]
#[path = "inventory_stacking_tests.rs"]
mod inventory_stacking_tests;

#[cfg(test)]
mod t3c2_tests {
    use crate::router::{ProjectedItemSwitchConfig, ServerContext};
    use crate::services::inventory_fitting::handle;
    use eve_db::{InventoryFlag, InventoryItemRecord};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_sde::SdeRegistry;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[tokio::test]
    async fn noop_add_and_multiadd_return_before_emitting_on_item_change() {
        let sde = Arc::new(SdeRegistry::with_canonical_seed());
        let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        let (char_id, station_id, item_id) = (90_000_001_i64, 60_003_760_i64, 991_880_001_i64);
        let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
        ctx.db.acquire_online_lock(uid, 93_202, Some(char_id), true, 1_700_000_500).expect("lock");
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(c) = snap.characters.get_mut(&char_id) {
                c.station_id = Some(station_id);
                c.structure_id = None;
            }
            snap.items.insert(item_id, InventoryItemRecord {
                item_id, type_id: 34, owner_id: char_id, location_id: station_id,
                flag: InventoryFlag::Hangar, quantity: 50, singleton: false, version: 1,
                ..Default::default()
            });
        });
        let _ = ctx.drain_notifications_for_char(char_id);
        let mut sess = SessionState {
            user_id: Some(uid), character_id: Some(char_id), corporation_id: Some(1_000_009),
            solar_system_id: Some(30_000_142), station_id: Some(station_id), ..Default::default()
        };
        let kw = HashMap::new();
        // Test with switches disabled to verify no-op handlers themselves never call emit_inv_item_change.
        ctx.set_projected_item_switches(ProjectedItemSwitchConfig::disabled());

        handle(&ctx, &mut sess, None, "invbroker", "Add",
            &[PyRep::Integer(item_id), PyRep::Integer(station_id), PyRep::Integer(InventoryFlag::Hangar as i64)], &kw)
            .await.expect("handled").expect("noop Add");
        assert!(ctx.drain_notifications_for_char(char_id).is_empty(), "noop Add must emit zero OnItemChange");

        handle(&ctx, &mut sess, None, "invbroker", "MultiAdd",
            &[PyRep::List(vec![PyRep::Integer(item_id)]), PyRep::Integer(station_id), PyRep::Integer(InventoryFlag::Hangar as i64)], &kw)
            .await.expect("handled").expect("noop MultiAdd");
        assert!(ctx.drain_notifications_for_char(char_id).is_empty(), "noop MultiAdd must emit zero OnItemChange");
    }

    #[tokio::test]
    async fn inventory_fitting_add_same_bay_fuel_merge_emits_delete_and_stack_size() {
        let ctx = ServerContext::with_sde_and_repository(Arc::new(SdeRegistry::with_canonical_seed()), Arc::new(eve_db::WorldRepository::with_demo_seed()));
        let (char_id, corp_id, sid, src_id, dst_id) = (90_000_001_i64, 98_931_001_i64, 1_000_000_997_011_i64, 991_880_011_i64, 991_880_012_i64);
        let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
        ctx.db.acquire_online_lock(uid, 93_203, Some(char_id), true, 1_700_000_500).expect("lock");
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&char_id).unwrap();
            ch.corporation_id = corp_id; ch.station_id = None; ch.structure_id = Some(sid);
            snap.corporations.entry(corp_id).or_insert_with(|| { let mut corp = eve_db::CorporationRecord::new(corp_id, "Fuel Test Corp", "FTC", char_id); corp.members.insert(char_id); corp });
            snap.corp_role_assignments.insert((corp_id, char_id), eve_db::CorpMemberRoleAssignment { character_id: char_id, roles: eve_db::corp_roles::ROLE_DIRECTOR, ..Default::default() });
            snap.structures.insert(sid, eve_db::UpwellStructureRecord { structure_id: sid, type_id: 35832, owner_corp_id: corp_id, solar_system_id: 30_000_142, state: "ShieldVulnerable".into(), quantum_core_installed: true, ..Default::default() });
            for (id, qty) in [(dst_id, 30), (src_id, 20)] {
                snap.items.insert(id, InventoryItemRecord { item_id: id, type_id: 16273, owner_id: corp_id, location_id: sid, flag: InventoryFlag::SpecializedFuelBay, quantity: qty, singleton: false, version: 1, ..Default::default() });
            }
        });
        let _ = ctx.drain_notifications_for_char(char_id);
        let mut sess = SessionState { user_id: Some(uid), character_id: Some(char_id), corporation_id: Some(corp_id), corp_role: eve_db::corp_roles::ROLE_DIRECTOR, solar_system_id: Some(30_000_142), structure_id: Some(sid), ..Default::default() };
        let rep = handle(&ctx, &mut sess, None, "invbroker", "Add", &[PyRep::Integer(src_id), PyRep::Integer(sid), PyRep::Integer(InventoryFlag::SpecializedFuelBay as i64)], &HashMap::new()).await.expect("handled").expect("fuel merge");
        assert_eq!((rep.dict_get("itemID"), rep.dict_get("quantity")), (Some(&PyRep::Integer(dst_id)), Some(&PyRep::Integer(50))));
        ctx.db.with_snapshot(|s| { assert!(!s.items.contains_key(&src_id)); assert_eq!(s.items[&dst_id].quantity, 50); });
        let notifs = ctx.drain_notifications_for_char(char_id);
        let col = |r: &PyRep, k: &str| match r { PyRep::DBRow { columns, .. } => columns.iter().find(|(c, _)| c == k).and_then(|(_, v)| v.as_i64()), _ => None };
        assert!(notifs.iter().any(|n| n.event == "ProcessInventoryChange"));
        let del = notifs.iter().find(|n| n.event == "OnItemChange" && col(&n.args[0], "itemID") == Some(src_id)).expect("delete notif");
        assert_eq!((col(&del.args[0], "locationID"), col(&del.args[0], "quantity")), (Some(6), Some(0)));
        let surv = notifs.iter().find(|n| n.event == "OnItemChange" && col(&n.args[0], "itemID") == Some(dst_id)).expect("target notif");
        assert_eq!(col(&surv.args[0], "quantity"), Some(50));
        assert!(matches!(&surv.args[1], PyRep::Dict(pairs) if pairs.contains(&(PyRep::Integer(9), PyRep::Integer(30)))));
    }
}
