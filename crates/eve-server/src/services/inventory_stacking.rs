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
mod inventory_stacking_ops;
pub(super) use inventory_stacking_ops::handle_stack_ops;

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
    let Some(new_loc) = super::inventory_bound::resolve_inv_target_location(
        ctx,
        session,
        bound_entry,
        args,
        first_src_loc,
    ) else {
        return Err(BridgeRpcError {
            code: "InvalidLocation".into(),
            message: "MultiAdd requires target location".into(),
        });
    };
    let raw_flag_opt = args
        .get(2)
        .and_then(|a| a.as_i64())
        .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()))
        .map(|v| v as u16);
    if matches!(raw_flag_opt, Some(11..=34 | 89 | 92..=94 | 125..=132 | 164..=171)) {
        return Err(BridgeRpcError {
            code: "InvalidLocation".into(),
            message: "MultiAdd cannot be used to fit modules or rigs".into(),
        });
    }
    if matches!(raw_flag_opt, Some(36 | 63 | 72)) {
        let bf = raw_flag_opt.unwrap_or(0);
        return Err(BridgeRpcError {
            code: if bf == 72 {
                "ImpoundedItem"
            } else if bf == 36 {
                "AssetSafetyLocked"
            } else {
                "ItemLocked"
            }
            .into(),
            message: format!("Cannot MultiAdd items into restricted inventory flag {bf}"),
        });
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
    if matches!(raw_flag_u16, 36 | 63 | 72) {
        return Err(BridgeRpcError {
            code: if raw_flag_u16 == 72 {
                "ImpoundedItem"
            } else if raw_flag_u16 == 36 {
                "AssetSafetyLocked"
            } else {
                "ItemLocked"
            }
            .into(),
            message: format!("Cannot MultiAdd items into restricted inventory flag {raw_flag_u16}"),
        });
    }
    let flag = InventoryFlag::from_u16(raw_flag_u16);
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
    if item_ids.contains(&active_ship_id)
        || ctx.db.with_snapshot(|snap| item_ids.iter().any(|&id| snap.is_character_boarded_or_saved_ship(char_id, session.ship_id, id)))
    {
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
            if is_corp_item && !src_is_struct_fuel {
                crate::services::inventory_bound::validate_corp_source_item_take_access_in_snap(
                    ctx,
                    snap,
                    char_id,
                    ch.corporation_id,
                    item,
                )?;
            }
            if !src_is_struct_fuel && item.owner_id != char_id && !is_corp_item && !looted_from_space_ids.contains(&item_id) {
                return Err(BridgeRpcError {
                    code: "ItemOwnershipViolation".into(),
                    message: format!("Character {char_id} is not authorized to move item {item_id}"),
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
                });
            }
            crate::services::fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(
                snap, item,
            )?;
            if let Some(dest_item) = snap.items.get(&new_loc) {
                crate::services::fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(
                    snap, dest_item,
                )?;
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
    let mut old_ship_loads: std::collections::HashMap<i64, (f64, f64)> =
        std::collections::HashMap::new();
    for &(old_loc, _, _) in &fitted_unfit_ids {
        if !old_ship_loads.contains_key(&old_loc) {
            if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, old_loc) {
                old_ship_loads.insert(
                    old_loc,
                    (
                        fit.ship.get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde),
                        fit.ship.get_attribute(eve_sde::attr_ids::POWER_LOAD, &ctx.sde),
                    ),
                );
            }
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
            let pre_item = snap
                .items
                .get(&item_id)
                .cloned()
                .ok_or_else(|| BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Item {item_id} not found"),
                })?;
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
            let post_it = snap.items.get(&item_id).cloned();
            moved.push((pre_item, post_it, fin_opt, it_tid, it_qty));
        }
        Ok((moved, old_containers))
    })?;
    let mut despawn_set = HashSet::new();
    for (old_cont, item_id, it_tid, it_qty) in old_containers {
        fitting_containers_ships::record_container_take_log(ctx, char_id, item_id, it_tid, it_qty, old_cont);
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
    for (pre_item, post_it, fin_opt, it_tid, it_qty) in moved {
        let fin_id = fin_opt
            .as_ref()
            .map(|r| r.item_id)
            .or_else(|| post_it.as_ref().map(|r| r.item_id))
            .unwrap_or(pre_item.item_id);
        fitting_containers_ships::record_container_add_log(ctx, char_id, fin_id, it_tid, it_qty, new_loc);
        if let Some(fin) = fin_opt {
            if fin.item_id != pre_item.item_id {
                super::inventory_bound::emit_inv_item_deleted(ctx, char_id, &pre_item);
                let old_target_qty = (fin.quantity - it_qty).max(0) as i64;
                super::inventory_bound::emit_inv_item_change(
                    ctx,
                    char_id,
                    &fin,
                    vec![
                        (5, PyRep::Integer(old_target_qty)),
                        (9, PyRep::Integer(old_target_qty)),
                    ],
                );
            } else {
                let mut changes = vec![
                    (3, PyRep::Integer(pre_item.location_id)),
                    (4, PyRep::Integer(pre_item.flag as i64)),
                ];
                if fin.owner_id != pre_item.owner_id {
                    changes.push((2, PyRep::Integer(pre_item.owner_id)));
                }
                super::inventory_bound::emit_inv_item_change(ctx, char_id, &fin, changes);
            }
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "ProcessInventoryChange",
                "charid",
                vec![
                    PyRep::Integer(fin.item_id),
                    PyRep::Integer(fin.location_id),
                    PyRep::Integer(fin.flag as i64),
                ],
            );
        } else if let Some(updated) = post_it {
            let mut changes = vec![
                (3, PyRep::Integer(pre_item.location_id)),
                (4, PyRep::Integer(pre_item.flag as i64)),
            ];
            if updated.owner_id != pre_item.owner_id {
                changes.push((2, PyRep::Integer(pre_item.owner_id)));
            }
            super::inventory_bound::emit_inv_item_change(ctx, char_id, &updated, changes);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "ProcessInventoryChange",
                "charid",
                vec![
                    PyRep::Integer(updated.item_id),
                    PyRep::Integer(updated.location_id),
                    PyRep::Integer(updated.flag as i64),
                ],
            );
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
        if let Ok(new_fit) = ctx.get_or_rebuild_ship_fitting(char_id, old_ship) {
            if let Some(&(old_cpu, old_pg)) = old_ship_loads.get(&old_ship) {
                let new_cpu = new_fit.ship.get_attribute(eve_sde::attr_ids::CPU_LOAD, &ctx.sde);
                let new_pg = new_fit.ship.get_attribute(eve_sde::attr_ids::POWER_LOAD, &ctx.sde);
                let _ = ctx.notify_ship_fitting_load_changes(
                    char_id, old_ship, old_cpu, new_cpu, old_pg, new_pg,
                );
            }
        }
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
    let (mut used_slots, ch_dock_for_fit, item_meta) = ctx.db.with_snapshot(|snap| -> Result<_, BridgeRpcError> {
        let ch = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {char_id} not found"),
        })?;
        let ship_item = snap.items.get(&ship_id).ok_or_else(|| BridgeRpcError {
            code: "ShipNotFound".into(),
            message: format!("Target ship {ship_id} not found"),
        })?;
        let ship_t = ctx.sde.types.get(&ship_item.type_id).ok_or_else(|| BridgeRpcError {
            code: "InvalidFittingTarget".into(),
            message: format!("Unknown hull type {}", ship_item.type_id),
        })?;
        if ship_item.owner_id != char_id || ship_t.category_id != 6 {
            return Err(BridgeRpcError {
                code: "InvalidFittingTarget".into(),
                message: format!("Target {ship_id} is not an owned ship"),
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

        let used: HashSet<u16> = snap
            .items
            .values()
            .filter(|other| {
                other.location_id == ship_id
                    && matches!(other.flag as u16, 11..=34 | 92..=94 | 125..=132)
                    && ctx
                        .sde
                        .types
                        .get(&other.type_id)
                        .is_none_or(|t| t.category_id == 7 || t.category_id == 32)
            })
            .map(|other| other.flag as u16)
            .collect();

        let mut meta = Vec::with_capacity(item_ids.len());
        for &item_id in item_ids {
            let item = snap.items.get(&item_id).ok_or_else(|| BridgeRpcError {
                code: "ItemNotFound".into(),
                message: format!("Item {item_id} not found"),
            })?;
            fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, item)?;
            if snap.is_item_locked_in_contract_or_job(item_id) {
                return Err(BridgeRpcError {
                    code: "ItemLocked".into(),
                    message: format!("Item {item_id} is locked by an active contract or job"),
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
                });
            }
            let is_corp_item = ch.corporation_id > 0 && item.owner_id == ch.corporation_id;
            if is_corp_item {
                crate::services::inventory_bound::validate_corp_source_item_take_access_in_snap(
                    ctx,
                    snap,
                    char_id,
                    ch.corporation_id,
                    item,
                )?;
            } else if item.owner_id != char_id && !looted_from_space_ids.contains(&item_id) {
                return Err(BridgeRpcError {
                    code: "ItemOwnershipViolation".into(),
                    message: format!("Item {item_id} is not owned by {char_id}"),
                });
            }
            if !crate::services::inventory_bound::is_inventory_location_colocated(
                snap,
                session,
                ch,
                &pre_balls,
                item.location_id,
            ) {
                return Err(BridgeRpcError {
                    code: "ItemOwnershipViolation".into(),
                    message: format!("Item {item_id} location not reachable"),
                });
            }
            if item.location_id == ship_id
                && matches!(item.flag as u16, 11..=34 | 92..=94 | 125..=132)
            {
                return Err(BridgeRpcError {
                    code: "InvalidLocation".into(),
                    message: format!("Item {item_id} is already fitted on ship {ship_id}"),
                });
            }
            let mod_t = ctx.sde.types.get(&item.type_id).ok_or_else(|| BridgeRpcError {
                code: "ItemNotFittableModule".into(),
                message: format!("Unknown type {}", item.type_id),
            })?;
            if mod_t.category_id != 7 && mod_t.category_id != 32 {
                return Err(BridgeRpcError {
                    code: "ItemNotFittableModule".into(),
                    message: format!(
                        "Item {item_id} (type {}, category {}) cannot be fitted",
                        item.type_id, mod_t.category_id
                    ),
                });
            }
            let is_ehe = matches!(item.type_id, 40714 | 40717)
                || mod_t.effect_ids.contains(&6484);
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
                });
            }
            crate::services::tactical_module_mechanics::validate_module_fitting_constraints(
                ctx, snap, ship_id, item_id, mod_t, ship_t,
            )?;
            fitting_containers_ships::validate_container_and_bay_move(
                ctx,
                char_id,
                item_id,
                ship_id,
                27,
                kwargs,
                (item.quantity > 1 && !item.singleton).then_some(1),
            )?;
            let (cand_slots, default_online): (Vec<u16>, bool) = if mod_t.effect_ids.contains(&12) {
                ((27_u16..=34).collect(), true)
            } else if mod_t.effect_ids.contains(&13) {
                ((19_u16..=26).collect(), true)
            } else if mod_t.effect_ids.contains(&11) {
                ((11_u16..=18).collect(), true)
            } else if mod_t.effect_ids.contains(&2663) {
                ((92_u16..=94).collect(), false)
            } else if mod_t.effect_ids.contains(&3772) {
                if let Some(&req_slot_f) = mod_t.default_attributes.get(&1366) {
                    (vec![req_slot_f as u16], false)
                } else {
                    ((125_u16..=132).collect(), false)
                }
            } else {
                return Err(BridgeRpcError {
                    code: "ItemNotFittableModule".into(),
                    message: format!("Module {} lacks a fitting slot effect", mod_t.name),
                });
            };
            meta.push((
                item.clone(),
                cand_slots,
                default_online,
            ));
        }
        Ok((used, ch_dock, meta))
    })?;
    let _ = ch_dock_for_fit;

    struct PlannedFit {
        src_item: eve_db::InventoryItemRecord,
        needs_split: bool,
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
                    last_err = Some(msg);
                }
            }
        }
        let Some(target_flag) = chosen else {
            return Err(BridgeRpcError {
                code: "FittingValidationFailed".into(),
                message: last_err.unwrap_or_else(|| {
                    format!("No free compatible fitting slot for item {}", src_item.item_id)
                }),
            });
        };
        planned.push(PlannedFit {
            src_item,
            needs_split,
            target_flag,
            online,
        });
    }

    let mut fitted_ids = Vec::with_capacity(planned.len());
    let mut despawn_set = HashSet::new();
    for plan in planned {
        let old_loc = plan.src_item.location_id;
        let old_flag_u16 = plan.src_item.flag as u16;
        let (fitted_id, split_orig_opt) = if plan.needs_split {
            let split_owner = plan.src_item.owner_id;
            let (orig_rec, split_item) = ctx
                .db
                .split_stack(split_owner, plan.src_item.item_id, 1)
                .map_err(|e| BridgeRpcError {
                    code: "SplitFailed".into(),
                    message: e.to_string(),
                })?;
            let old_orig_qty = orig_rec.quantity + split_item.quantity;
            (split_item.item_id, Some((orig_rec, old_orig_qty)))
        } else {
            (plan.src_item.item_id, None)
        };

        let target_flag_enum = InventoryFlag::from_u16(plan.target_flag);
        let updated = ctx.db.try_with_mut_snapshot(|snap| {
            let it = snap.items.get_mut(&fitted_id).ok_or_else(|| BridgeRpcError {
                code: "ItemNotFound".into(),
                message: format!("Item {fitted_id} not found"),
            })?;
            it.owner_id = char_id;
            it.location_id = ship_id;
            it.flag = target_flag_enum;
            it.singleton = true;
            it.version += 1;
            snap.module_online_states.insert(fitted_id, plan.online);
            Ok(it.clone())
        })?;

        if old_loc != ship_id {
            fitting_containers_ships::record_container_take_log(
                ctx,
                char_id,
                updated.item_id,
                updated.type_id,
                updated.quantity,
                old_loc,
            );
            crate::services::pve_dungeon_runtime::on_dungeon_item_looted(
                ctx,
                char_id,
                session.solar_system_id,
                old_loc,
                updated.type_id,
            )
            .await;
            despawn_set.insert(old_loc);
        }
        fitting_containers_ships::record_container_add_log(
            ctx,
            char_id,
            updated.item_id,
            updated.type_id,
            updated.quantity,
            ship_id,
        );
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
        let _ = ctx.get_or_rebuild_ship_fitting(char_id, ship_id);

        if let Some((orig_rec, old_orig_qty)) = split_orig_opt {
            super::inventory_bound::emit_inv_item_change(
                ctx,
                char_id,
                &orig_rec,
                vec![
                    (5, PyRep::Integer(old_orig_qty as i64)),
                    (9, PyRep::Integer(old_orig_qty as i64)),
                ],
            );
        }
        let changes = if plan.needs_split {
            vec![(3, PyRep::Integer(0))]
        } else {
            vec![
                (3, PyRep::Integer(old_loc)),
                (4, PyRep::Integer(old_flag_u16 as i64)),
            ]
        };
        super::inventory_bound::emit_inv_item_change(ctx, char_id, &updated, changes);
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "ProcessInventoryChange",
            "charid",
            vec![
                PyRep::Integer(updated.item_id),
                PyRep::Integer(updated.location_id),
                PyRep::Integer(updated.flag as i64),
            ],
        );
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnModuleFitted",
            "charid",
            vec![
                PyRep::Integer(updated.item_id),
                PyRep::Integer(updated.flag as i64),
            ],
        );
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnModuleAttributeChanges",
            "charid",
            vec![PyRep::Integer(updated.item_id)],
        );
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
    crate::services::pve_crab_beacons::check_crab_loot_access(
        ctx,
        container_id,
        char_id,
        session.has_gm_role(),
    )?;
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
    let unit_check: Result<bool, BridgeRpcError> = ctx.db.with_snapshot(|snap| {
        if let Some(it) = snap.items.get(&bound_loc_id) {
            let cat = ctx.sde.types.get(&it.type_id).map(|t| t.category_id).unwrap_or(0);
            let grp = ctx.sde.types.get(&it.type_id).map(|t| t.group_id).unwrap_or(0);
            let is_personal = cat == 22 || grp == 340;
            let is_unit = cat == 22 || matches!(grp, 12 | 186 | 340 | 448 | 649 | 4137) || it.type_id == 23;
            let is_fleet_auth = !is_personal
                && snap.fleets.values().any(|f| f.members.contains(&char_id) && f.members.contains(&it.owner_id))
                && fleet_shipping_authorized;
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
    let rows = if let Some(flags) = flags_filter {
        if flags.is_empty() {
            Vec::new()
        } else if is_deployable_or_shipping_unit {
            ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| i.location_id == bound_loc_id && flags.contains(&(i.flag as i64)))
                    .map(|i| item_to_packed_cells(ctx, i))
                    .collect()
            })
        } else {
            let mut out_rows = Vec::new();
            if flags.iter().any(|f| (116..=122).contains(f)) {
                let corp_id = session.corporation_id.or_else(|| {
                    ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id))
                }).unwrap_or(0);
                ctx.db.with_snapshot(|snap| {
                    out_rows.extend(
                        snap.items
                            .values()
                            .filter(|i| {
                                i.location_id == bound_loc_id
                                    && i.owner_id == corp_id
                                    && (116..=122).contains(&(i.flag as i64))
                                    && flags.contains(&(i.flag as i64))
                            })
                            .map(|i| item_to_packed_cells(ctx, i)),
                    );
                });
            }
            if flags.iter().any(|f| !(116..=122).contains(f)) {
                out_rows.extend(
                    ctx.db
                        .get_items_for_character(char_id)
                        .into_iter()
                        .filter(|i| {
                            i.location_id == bound_loc_id
                                && !(116..=122).contains(&(i.flag as i64))
                                && flags.contains(&(i.flag as i64))
                        })
                        .map(|i| item_to_packed_cells(ctx, &i)),
                );
            }
            out_rows
        }
    } else if is_deployable_or_shipping_unit {
        ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| i.location_id == bound_loc_id)
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

#[cfg(test)]
#[path = "inventory_stacking_tests.rs"]
mod inventory_stacking_tests;
