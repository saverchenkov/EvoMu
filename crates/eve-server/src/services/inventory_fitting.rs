//! `inventory_fitting` — Domain 3: Inventory Broker, Ship Fitting, Insurance, Repair, & Abyssal Mutaplasmids (`§3.4`).

#![forbid(unsafe_code)]

use crate::formulas::abyssal_attribute_roll;
use crate::router::notify_registry::{OnModuleFitted, ProcessInventoryChange};
use crate::router::{BoundObjectEntry, NotificationTarget, ServerContext};
use crate::services::notify_emitters_skills_trade_godma::{
    emit_ship_fitting_attr_changes_for_ship, emit_ship_fitting_attr_changes_from_fit,
    snapshot_ship_fitting_attrs_for_ship,
};
use eve_db::{InventoryFlag, IskCents, SavedFittingRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use eve_sde::{attr_ids, AttributeId, TypeId};
use glam::DVec3;
use std::collections::HashMap;

#[path = "fitting_dogma_im.rs"]
pub mod fitting_dogma_im;
pub(crate) use fitting_dogma_im::*;

#[path = "fitting_ewar_capital.rs"]
pub mod fitting_ewar_capital;
pub(crate) use fitting_ewar_capital::*;

#[path = "fitting_mgr_service.rs"]
pub mod fitting_mgr_service;
pub(crate) use fitting_mgr_service::*;

#[path = "fitting_mining_lasers.rs"]
pub mod fitting_mining_lasers;
pub(crate) use fitting_mining_lasers::*;

#[path = "fitting_module_cycles.rs"]
pub mod fitting_module_cycles;
pub(crate) use fitting_module_cycles::*;

#[path = "fitting_saved_multifit.rs"]
pub mod fitting_saved_multifit;
pub(crate) use fitting_saved_multifit::*;

#[path = "fitting_dogma_ops.rs"]
pub mod fitting_dogma_ops;
pub(crate) use fitting_dogma_ops::*;

#[path = "fitting_containers_ships.rs"]
pub mod fitting_containers_ships;
pub(crate) use fitting_containers_ships::*;

#[path = "fitting_containers_ships_ops.rs"]
pub mod fitting_containers_ships_ops;

#[path = "fitting_ships_mastery.rs"]
pub mod fitting_ships_mastery;
pub(crate) use fitting_ships_mastery::*;

#[path = "fitting_scanners_superweapons.rs"]
pub mod fitting_scanners_superweapons;
pub(crate) use fitting_scanners_superweapons::*;

#[path = "fitting_cyno_portal.rs"]
pub mod fitting_cyno_portal;
pub(crate) use fitting_cyno_portal::*;

#[path = "fitting_ship_forcefield.rs"]
pub mod fitting_ship_forcefield;
pub(crate) use fitting_ship_forcefield::*;

#[path = "fitting_ship_bays.rs"]
pub mod fitting_ship_bays;
pub(crate) use fitting_ship_bays::*;

pub(crate) use super::inventory_bound::{get_effective_corp_roles, get_effective_corp_roles_at_location, item_to_packed_cells};

/// Mechanic roll in `[0, 1)` from the context RNG (seeded test RNG); `0.5` if the OS CSPRNG is unreadable.
pub(crate) fn csprng_unit_f64(ctx: &ServerContext) -> f64 {
    ctx.mechanic_rng.try_unit_f64().unwrap_or(0.5)
}

fn check_fitting_dock(ctx: &ServerContext, session: &SessionState, char_id: i64) -> Result<(), BridgeRpcError> {
    ctx.db.with_snapshot(|snap| {
        let dock_id = session
            .station_id
            .or(session.structure_id)
            .or_else(|| snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id)))
            .unwrap_or(0);
        crate::services::structure_acl_defense_rules::validate_docked_ship_fitting_facility_in_snap(ctx, snap, dock_id)
    })
}

#[inline(never)]
pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service == "trademgr" && method != "InitiateTrade" {
        return super::wallet_services_ext::handle_trademgr_rpc(
            ctx,
            session,
            bound_entry,
            method,
            args,
            kwargs,
        );
    }
    if service == "dogmaIM" && matches!(method, "Activate" | "Overload") {
        if let Some(res) = crate::router::box_fut(|| {
            try_handle_structure_dogma_weapons(ctx, session, service, method, args, kwargs)
        })
        .await
        {
            return Some(res);
        }
    }
    let method = super::inventory_fitting_rpc::resolve_launch_probes_alias(service, method);
    if let Some(res) = super::inventory_fitting_rpc::handle_invbroker_sync(
        ctx,
        session,
        bound_entry,
        service,
        method,
        args,
        kwargs,
    ) {
        return Some(res);
    }
    if super::inventory_fitting_rpc::is_invbroker_or_ship_async(service, method) {
        return crate::router::box_fut(|| {
            super::inventory_fitting_rpc::handle_invbroker_and_ship_async(
                ctx,
                session,
                bound_entry,
                service,
                method,
                args,
                kwargs,
            )
        })
        .await;
    }
    crate::router::box_fut(|| {
        handle_fallback_dispatch(ctx, session, bound_entry, service, method, args, kwargs)
    })
    .await
}

#[inline(never)]
async fn try_handle_structure_dogma_weapons(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if !session.ship_id.is_some_and(|sid| {
        session.structure_id == Some(sid)
            || ctx.db.with_snapshot(|s| s.structures.contains_key(&sid))
    }) {
        return None;
    }
    crate::router::box_fut(|| {
        crate::services::combat_crimewatch::combat_weapons::handle_weapons(
            ctx, session, service, method, args, kwargs,
        )
    })
    .await
}

#[inline(never)]
async fn handle_fallback_dispatch(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Some(res) = super::inventory_stacking::handle_stack_ops(
        ctx,
        session,
        bound_entry,
        service,
        method,
        args,
        kwargs,
    ) {
        return Some(res);
    }
    if let Some(res) = crate::router::box_fut(|| {
        handle_fallback_combat_and_cycles(ctx, session, bound_entry, service, method, args, kwargs)
    })
    .await
    {
        return Some(res);
    }
    if let Some(res) = crate::router::box_fut(|| {
        handle_fallback_fitting_and_ships(ctx, session, bound_entry, service, method, args, kwargs)
    })
    .await
    {
        return Some(res);
    }
    crate::router::box_fut(|| {
        handle_fallback_ext_and_portal(ctx, session, bound_entry, service, method, args, kwargs)
    })
    .await
}

#[inline(never)]
async fn handle_fallback_combat_and_cycles(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if session.structure_id.is_some() && session.structure_id == session.ship_id {
        if let Some(res) = crate::router::box_fut(|| {
            crate::services::combat_crimewatch::combat_weapons::handle_weapons(
                ctx, session, service, method, args, kwargs,
            )
        })
        .await
        {
            return Some(res);
        }
    }
    if let Some(res) = crate::router::box_fut(|| {
        handle_dogma_im(ctx, session, bound_entry, service, method, args, kwargs)
    })
    .await
    {
        return Some(res);
    }
    if let Some(res) = crate::router::box_fut(|| {
        handle_dogma_ops(ctx, session, bound_entry, service, method, args, kwargs)
    })
    .await
    {
        return Some(res);
    }
    if let Some(res) = crate::router::box_fut(|| {
        handle_module_cycles(ctx, session, bound_entry, service, method, args, kwargs)
    })
    .await
    {
        return Some(res);
    }
    crate::router::box_fut(|| {
        handle_mining_and_deactivate(ctx, session, bound_entry, service, method, args, kwargs)
    })
    .await
}

#[inline(never)]
async fn handle_fallback_fitting_and_ships(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Some(res) = crate::router::box_fut(|| {
        handle_fitting_mgr(ctx, session, bound_entry, service, method, args, kwargs)
    })
    .await
    {
        return Some(res);
    }
    if let Some(res) = crate::router::box_fut(|| {
        handle_ships_and_services(ctx, session, bound_entry, service, method, args, kwargs)
    })
    .await
    {
        return Some(res);
    }
    crate::router::box_fut(|| {
        handle_containers_ships_and_fitting(ctx, session, bound_entry, service, method, args, kwargs)
    })
    .await
}

#[inline(never)]
async fn handle_fallback_ext_and_portal(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Some(res) = crate::router::box_fut(|| {
        handle_ships_mastery_and_fitting_ext(
            ctx,
            session,
            bound_entry,
            service,
            method,
            args,
            kwargs,
        )
    })
    .await
    {
        return Some(res);
    }
    if let Some(res) = crate::router::box_fut(|| {
        handle_superweapons_dbuffs_and_clones(
            ctx,
            session,
            bound_entry,
            service,
            method,
            args,
            kwargs,
        )
    })
    .await
    {
        return Some(res);
    }
    crate::router::box_fut(|| handle_cyno_portal(ctx, session, service, method, args)).await
}

#[inline(never)]
pub(crate) async fn handle_invbroker_add(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(mut item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "Add requires itemID".into(), client_error: None }));
            };
            let source_item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&item_id).cloned());
            let raw_flag_opt = args.get(2).and_then(|a| a.as_i64()).or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()));
            let src_is_fittable_module = raw_flag_opt.is_some()
                && source_item_opt
                    .as_ref()
                    .and_then(|src| ctx.sde.types.get(&src.type_id))
                    .is_some_and(|t| matches!(t.category_id, 7 | 32));
            let Some(mut new_loc) = super::inventory_bound::resolve_inv_target_location(
                ctx,
                session,
                bound_entry,
                args,
                source_item_opt.as_ref().map(|i| i.location_id),
                raw_flag_opt.and_then(|v| u16::try_from(v).ok()),
                src_is_fittable_module,
            ) else {
                return Some(Err(BridgeRpcError { code: "InvalidLocation".into(), message: "Add requires a target locationID, bound inventory, or active ship/station".into(), client_error: None }));
            };
            if let Some(rf) = raw_flag_opt {
                if !matches!(rf, 0 | 4 | 5 | 11..=34 | 87 | 90 | 92..=94 | 116..=122 | 125..=151 | 155 | 158 | 164..=172 | 177 | 179..=182 | 185) {
                    return Some(Err(BridgeRpcError { code: "InvalidInventoryFlag".into(), message: format!("Invalid or restricted inventory flag {rf}"), client_error: None }));
                }
                // Fighter Bay (158) only accepts fighters (SDE category 87).
                if rf == 158 && source_item_opt.as_ref().and_then(|i| ctx.sde.types.get(&i.type_id)).is_some_and(|t| t.category_id != 87) {
                    return Some(Err(BridgeRpcError { code: "InvalidInventoryFlag".into(), message: "Fighter Bay only accepts fighters".into(), client_error: None }));
                }
            }
            let mut flag_u16 = raw_flag_opt.map(|v| v as u16).unwrap_or(InventoryFlag::Cargo as u16);
            if flag_u16 == 0 {
                let target_ship_type_id = ctx
                    .db
                    .with_snapshot(|snap| snap.items.get(&new_loc).map(|i| i.type_id))
                    .filter(|tid| ctx.sde.types.get(tid).is_some_and(|t| t.category_id == 6));
                if let Some(ship_type_id) = target_ship_type_id {
                    let mod_type_opt = source_item_opt
                        .as_ref()
                        .and_then(|src| ctx.sde.types.get(&src.type_id))
                        .filter(|t| matches!(t.category_id, 7 | 32));
                    if let Some(mod_type) = mod_type_opt {
                        let rack = if mod_type.effect_ids.contains(&12) {
                            Some((27_u16, Some(eve_dogma::attr_ids::HI_SLOTS), false))
                        } else if mod_type.effect_ids.contains(&13) {
                            Some((19_u16, Some(eve_dogma::attr_ids::MED_SLOTS), false))
                        } else if mod_type.effect_ids.contains(&11) {
                            Some((11_u16, Some(eve_dogma::attr_ids::LOW_SLOTS), false))
                        } else if mod_type.effect_ids.contains(&2663) {
                            Some((92_u16, Some(eve_dogma::attr_ids::RIG_SLOTS), false))
                        } else if mod_type.category_id == 32 || mod_type.effect_ids.contains(&3772) {
                            Some((125_u16, None, true))
                        } else {
                            None
                        };
                        if let Some((base_flag, slot_attr_opt, is_sub)) = rack {
                            let max_slots = if is_sub {
                                if ctx.sde.types.get(&ship_type_id).is_some_and(|t| t.group_id == 963) {
                                    4_u16
                                } else {
                                    0_u16
                                }
                            } else if let Some(attr) = slot_attr_opt {
                                if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, new_loc) {
                                    let raw = fit.ship.get_attribute(attr, &ctx.sde).max(0.0) as u16;
                                    if attr == eve_dogma::attr_ids::RIG_SLOTS {
                                        raw.min(3)
                                    } else {
                                        raw.min(8)
                                    }
                                } else {
                                    ctx.sde
                                        .types
                                        .get(&ship_type_id)
                                        .and_then(|t| t.default_attributes.get(&attr))
                                        .copied()
                                        .unwrap_or(0.0)
                                        .max(0.0) as u16
                                }
                            } else {
                                0_u16
                            };
                            let occupied: std::collections::HashSet<u16> = ctx.db.with_snapshot(|snap| {
                                snap.items
                                    .values()
                                    .filter(|i| {
                                        i.location_id == new_loc
                                            && i.item_id != item_id
                                            && ctx
                                                .sde
                                                .types
                                                .get(&i.type_id)
                                                .is_some_and(|t| matches!(t.category_id, 7 | 32))
                                    })
                                    .map(|i| i.flag as u16)
                                    .collect()
                            });
                            let preferred_sub = if is_sub {
                                mod_type
                                    .default_attributes
                                    .get(&1366)
                                    .map(|v| *v as u16)
                                    .filter(|f| *f >= base_flag && *f < base_flag + max_slots)
                            } else {
                                None
                            };
                            let chosen = preferred_sub
                                .filter(|f| !occupied.contains(f))
                                .or_else(|| (base_flag..(base_flag + max_slots)).find(|f| !occupied.contains(f)));
                            if let Some(resolved) = chosen {
                                flag_u16 = resolved;
                            } else {
                                return Some(Err(BridgeRpcError {
                                    code: "NoFreeSlot".into(),
                                    message: format!("No free fitting slot available on ship {new_loc}"),
                                    client_error: None,
                                }));
                            }
                        } else {
                            flag_u16 = InventoryFlag::Cargo as u16;
                        }
                    } else {
                        flag_u16 = InventoryFlag::Cargo as u16;
                    }
                }
            }
            // Preserve native ShipHangar (90) and FleetHangar (155) flags so SMA/FleetHangar
            // capacity, stacking, and notifications never collide with Cargo (5) or flag 138.
            let mut flag = InventoryFlag::from_u16(flag_u16);
            let requested_qty_opt = kwargs
                .get("qty")
                .or_else(|| kwargs.get("quantity"))
                .and_then(|v| v.as_i64())
                .or_else(|| args.get(3).and_then(|a| a.as_i64()));
            if let Some(q) = requested_qty_opt {
                if q <= 0 || i32::try_from(q).is_err() {
                    return Some(Err(BridgeRpcError::user_error("InvalidQuantity")));
                }
            }
            let split_qty_opt = requested_qty_opt.and_then(|q| {
                let q_i32 = i32::try_from(q).ok()?;
                source_item_opt.as_ref().and_then(|src| {
                    (!src.singleton && !src.is_blueprint_copy && q_i32 < src.quantity).then_some(q_i32)
                })
            });
            let orig_source_loc_id = source_item_opt.as_ref().map(|i| i.location_id).unwrap_or(0);
            let (looted_from_space, looted_foreign_jetcan) = if let Some(src_item) = &source_item_opt {
                match super::inventory_stacking::evaluate_space_wreck_or_jetcan_loot(ctx, char_id, src_item) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                }
            } else {
                (false, false)
            };
            // Unbound / station-style 2-arg space loot (`Add(loot_id, can_or_wreck_id[, Cargo, qty, ship_id])`)
            // resolves `new_loc == orig_source_loc_id` when no ship bound handle is provided; redirect the
            // destination to the pilot's active ship here so `dst_ok` and capacity checks validate the
            // actual ship receiving the looted item instead of requiring deposit access on the foreign
            // jetcan or wreck itself (`tx_res` already delivers `looted_from_space` items to `active_ship`).
            if looted_from_space && new_loc == orig_source_loc_id {
                if let Some(active_ship) = session.ship_id.filter(|&id| id > 0) {
                    new_loc = active_ship;
                }
            }
            if let Err(e) = ctx.db.with_snapshot(|snap| {
                super::inventory_bound::validate_ship_only_bay_target_in_snap(ctx, snap, new_loc, flag_u16)
            }) {
                return Some(Err(e));
            }
            if let Err(e) = fitting_containers_ships::validate_container_and_bay_move_for_add_with_args(ctx, char_id, item_id, new_loc, flag_u16, args, kwargs, split_qty_opt.map(i64::from)) {
                return Some(Err(e));
            }
            if let Some(src_item) = &source_item_opt {
                if let Err(err) = ctx.db.with_snapshot(|snap| {
                    super::inventory_bound::validate_movable_item_not_piloted_or_bare_space_in_snap(
                        snap, char_id, src_item, new_loc, flag_u16,
                    )
                }) {
                    return Some(Err(err));
                }
                let corp_id = ctx.db.with_snapshot(|snap| {
                    snap.characters.get(&char_id).map(|c| c.corporation_id)
                }).or(session.corporation_id).unwrap_or(0);
                if src_item.owner_id == corp_id && corp_id > 0 {
                    if let Err(err) = ctx.db.with_snapshot(|snap| {
                        super::inventory_bound::validate_corp_source_item_take_access_in_snap(
                            ctx, snap, char_id, corp_id, src_item,
                        )
                    }) {
                        return Some(Err(err));
                    }
                }
            }

            let explicit_online = kwargs.get("online").and_then(|v| match v {
                PyRep::Bool(b) => Some(*b),
                _ => None,
            });
            let online = explicit_online.unwrap_or(true);

            let is_structure_fit_move = ctx.db.with_snapshot(|snap| {
                (snap.structures.contains_key(&new_loc) && matches!(flag_u16, 11..=34 | 92..=94 | 164..=171))
                    || source_item_opt.as_ref().is_some_and(|s| {
                        snap.structures.contains_key(&s.location_id)
                            && matches!(s.flag as u16, 11..=34 | 92..=94 | 164..=171)
                    })
            });
            if is_structure_fit_move {
                return Some(
                    crate::services::station_structure::structure_lifecycle_rpc::handle_structure_fit_move(
                        ctx, session, item_id, new_loc, flag, explicit_online,
                    ),
                );
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
                if let Some(src_item) = &source_item_opt {
                    // parity: feat:modules.emergency-hull-energizer
                    let is_ehe = matches!(src_item.type_id, 40714..=40718)
                        || ctx
                            .sde
                            .types
                            .get(&src_item.type_id)
                            .is_some_and(|t| t.effect_ids.contains(&6484));
                    let ehe_damaged = src_item
                        .dynamic_attributes
                        .get(&eve_sde::attr_ids::DAMAGE)
                        .copied()
                        .unwrap_or(0.0)
                        > 0.0;
                    if is_ehe && ehe_damaged && (session.station_id.is_none() && session.structure_id.is_none() || is_fitting_slot) {
                        return Some(Err(BridgeRpcError {
                            code: "CannotRefitDamagedModule".into(),
                            message: "UserError: Damaged or burned-out Capital Emergency Hull Energizer cannot be refitted".into(),
                            client_error: None,
                        }));
                    }
                    if let Err(e) = validate_ship_module_unfit_auth(ctx, session, char_id, src_item, new_loc).await {
                        return Some(Err(e));
                    }
                }
                if session.station_id.is_none() && session.structure_id.is_none() {
                    let now_unix = ctx.now_unix_secs();
                    let has_weapons_timer = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner()).states.get(&char_id).is_some_and(|cw| cw.is_weapon_timer_active(now_unix));
                    if has_weapons_timer {
                        return Some(Err(BridgeRpcError { code: "CannotRefitWithWeaponsTimer".into(), message: "Cannot refit or unfit modules in space while weapons timer is active".into(), client_error: None }));
                    }
                    let mut near_fitting_service = false;
                    if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                        near_fitting_service = is_near_in_space_fitting_service(ctx, sys_id, ship_id).await;
                    }
                    if !near_fitting_service {
                        return Some(Err(BridgeRpcError { code: "CannotUnfitInSpace".into(), message: "Unfitting modules requires being docked at a station/structure or within range of a Mobile Depot or capital SMA".into(), client_error: None }));
                    }
                }
                if let Some(src_item) = &source_item_opt {
                    if matches!(src_item.flag as u16, 125..=132) {
                        let has_fitted_modules = ctx.db.with_snapshot(|snap| {
                            snap.items.values().any(|other| {
                                other.location_id == src_item.location_id
                                    && other.item_id != item_id
                                    && (11..=34).contains(&(other.flag as u16))
                                    && ctx.sde.types.get(&other.type_id).is_none_or(|t| t.category_id != 8)
                            })
                        });
                        if has_fitted_modules {
                            return Some(Err(BridgeRpcError {
                                code: "SubsystemHasFittedModules".into(),
                                message: "Cannot unfit a T3 Strategic Cruiser subsystem while modules are fitted".into(),
                                client_error: None,
                            }));
                        }
                        if let Ok(mut check_fit) = ctx.get_or_rebuild_ship_fitting(char_id, src_item.location_id) {
                            if let Err(msg) = check_fit.unfit_module(item_id, &ctx.sde) {
                                return Some(Err(BridgeRpcError {
                                    code: "SubsystemHasFittedModules".into(),
                                    message: msg,
                                    client_error: None,
                                }));
                            }
                        }
                    }
                    if let Some(sys_id) = session.solar_system_id {
                        let sys = ctx.systems.get_or_spawn(sys_id);
                        if sys.has_active_nanite_repair_sync(src_item.location_id, item_id) {
                            return Some(Err(BridgeRpcError { code: "ModuleBeingRepaired".into(), message: format!("Module {item_id} cannot be unfitted while undergoing nanite repair"), client_error: None }));
                        }
                        let _ = sys.remove_module_cycle(src_item.location_id, item_id).await;
                    }
                    let _ = crate::services::notify_emitters_space_dungeons::flush_module_profile_and_ewar_end(
                        ctx, char_id, session.solar_system_id, src_item.location_id, item_id,
                    );
                    crate::services::tactical_modules::remove_projected_remote_assistance(ctx, src_item.location_id, item_id);
                }
            }

            if let Some(src_item) = &source_item_opt {
                if matches!(src_item.flag as u16, 92..=94) && !matches!(flag_u16, 92..=94) {
                    if let Err(e) = validate_ship_module_unfit_auth(ctx, session, char_id, src_item, new_loc).await {
                        return Some(Err(e));
                    }
                    let old_loc = src_item.location_id;
                    let old_rig_ship_attrs =
                        snapshot_ship_fitting_attrs_for_ship(ctx, char_id, old_loc);
                    if let Err(e) = ctx.db.tx("inventory_rig_unfit_destroy", |tx| -> Result<(), BridgeRpcError> {
                        let snap = &mut *tx.snap;
                        let Some(it) = snap.items.get(&item_id) else {
                            return Err(BridgeRpcError {
                                code: "ItemNotFound".into(),
                                message: format!("Rig {item_id} no longer exists"),
                                client_error: None,
                            });
                        };
                        if it.owner_id != char_id
                            || it.location_id != old_loc
                            || !matches!(it.flag as u16, 92..=94)
                        {
                            return Err(BridgeRpcError {
                                code: "ItemOwnershipViolation".into(),
                                message: format!("Rig {item_id} is no longer fitted on ship {old_loc}"),
                                client_error: None,
                            });
                        }
                        if snap.is_item_or_ancestor_locked_or_impounded(item_id) {
                            return Err(BridgeRpcError {
                                code: "ItemLocked".into(),
                                message: format!("Rig {item_id} is locked"),
                                client_error: None,
                            });
                        }
                        snap.remove_system_cleanup_item(item_id);
                        // Rigs are destroyed on removal; explicit in-tx `loc=10, flag=147` (`#931` G4).
                        ServerContext::push_tx_fitted_item_trashed(
                            tx.effects,
                            &ctx.sde,
                            char_id,
                            item_id,
                            src_item.type_id as i64,
                            src_item.quantity as i64,
                            old_loc,
                            src_item.flag as i64,
                        );
                        Ok(())
                    }) {
                        return Some(Err(e));
                    }
                    ctx.invalidate_ship_fitting(old_loc);
                    let new_fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, old_loc).ok();
                    if let Some(ref new_fit) = new_fit_opt {
                        emit_ship_fitting_attr_changes_from_fit(
                            ctx,
                            char_id,
                            old_loc,
                            old_rig_ship_attrs.as_ref(),
                            new_fit,
                            false,
                        );
                    }
                    return Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(item_id)),
                        ("destroyed", PyRep::Bool(true)),
                    ])));
                }
            }

            if is_fitting_slot {
                let item_cat_opt = ctx.db.with_snapshot(|snap| {
                    snap.items.get(&item_id).and_then(|item| ctx.sde.types.get(&item.type_id).map(|t| t.category_id))
                });
                if item_cat_opt == Some(8) && matches!(flag_u16, 11..=34) {
                    let current_dock = session.station_id.or(session.structure_id);
                    return match ctx.load_charge_into_weapon_slot(char_id, new_loc, flag, item_id, current_dock) {
                        Ok((loaded_id, loaded_type_id, loaded_qty, loaded_ver)) => {
                            let mod_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
                                snap.items
                                    .values()
                                    .filter(|i| i.location_id == new_loc && i.flag == flag && i.item_id != loaded_id && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 7))
                                    .map(|i| i.item_id)
                                    .collect()
                            });
                            crate::services::notify_emitters_seasons_tactical::emit_charge_and_bank_ammo_loaded(ctx, char_id, new_loc, &mod_ids);
                            crate::services::operations_manager_helpers::record_invbroker_add_operation_event(ctx, char_id, 0, new_loc, flag, loaded_type_id, loaded_qty);
                            Some(Ok(PyRep::keyval([
                                ("itemID", PyRep::Integer(loaded_id)),
                                ("typeID", PyRep::Integer(loaded_type_id as i64)),
                                ("locationID", PyRep::Integer(new_loc)),
                                ("flagID", PyRep::Integer(flag as i64)),
                                ("quantity", PyRep::Integer(loaded_qty as i64)),
                                ("version", PyRep::Integer(loaded_ver as i64)),
                            ])))
                        }
                        Err(e) => Some(Err(e)),
                    };
                }
                if let Err(e) = check_fitting_dock(ctx, session, char_id) {
                    return Some(Err(e));
                }
                let is_ehe_damaged = ctx.db.with_snapshot(|s| {
                    let it = s.items.get(&item_id);
                    let is_ehe = it.is_some_and(|i| matches!(i.type_id, 40714..=40718) || ctx.sde.types.get(&i.type_id).is_some_and(|t| t.effect_ids.contains(&6484)));
                    let dmg = it.and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied()).unwrap_or(0.0);
                    is_ehe && dmg > 0.0
                });
                if is_ehe_damaged {
                    return Some(Err(BridgeRpcError {
                        code: "CannotRefitDamagedModule".into(),
                        message: "UserError: Damaged or burned-out Capital Emergency Hull Energizer cannot be fitted".into(),
                        client_error: None,
                    }));
                }
                if session.station_id.is_none() && session.structure_id.is_none() {
                    let now_unix = ctx.now_unix_secs();
                    let has_weapons_timer = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner()).states.get(&char_id).is_some_and(|cw| cw.is_weapon_timer_active(now_unix));
                    if has_weapons_timer {
                        return Some(Err(BridgeRpcError { code: "CannotRefitWithWeaponsTimer".into(), message: "Cannot fit modules in space while weapons timer is active".into(), client_error: None }));
                    }
                    let mut near_fitting_service = false;
                    if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                        near_fitting_service = is_near_in_space_fitting_service(ctx, sys_id, ship_id).await;
                    }
                    if !near_fitting_service {
                        return Some(Err(BridgeRpcError { code: "CannotFitInSpace".into(), message: "Fitting modules into ship slots requires being docked at a station or structure or within range of a Mobile Depot or capital SMA".into(), client_error: None }));
                    }
                }
                let pre_check = ctx.db.with_snapshot(|snap| {
                    let Some(item) = snap.items.get(&item_id) else {
                        return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Item {item_id} not found"), client_error: None });
                    };
                    if item.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "ItemOwnershipViolation".into(),
                            message: format!("Item {item_id} is not owned by {char_id}"),
                            client_error: None,
                        });
                    }
                    if item.flag == InventoryFlag::Locked
                        || snap.is_item_or_ancestor_locked_or_impounded(item_id)
                    {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!(
                                "Item {item_id} is locked by an active contract, industry job, or container"
                            ),
                            client_error: None,
                        });
                    }
                    let eff_qty = split_qty_opt.unwrap_or(item.quantity);
                    if eff_qty != 1 {
                        return Err(BridgeRpcError::cannot_fit_stacked_module(Some(new_loc), Some(item_id), eff_qty.into(), format!("Item {item_id} has quantity {eff_qty} and must be split to 1 before fitting")));
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
                            client_error: None,
                        });
                    }
                    let Some(ship_item) = snap.items.get(&new_loc) else {
                        return Err(BridgeRpcError {
                            code: "ShipNotFound".into(),
                            message: format!("Target ship {new_loc} not found"),
                            client_error: None,
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
                            client_error: None,
                        });
                    }
                    if ship_item.flag == InventoryFlag::Locked
                        || snap.is_item_or_ancestor_locked_or_impounded(new_loc)
                    {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!(
                                "Target ship {new_loc} is locked by an active contract, industry job, or container"
                            ),
                            client_error: None,
                        });
                    }
                    // parity: feat:modules.stasis-grappler-mechanics
                    let is_ehe = matches!(item.type_id, 40714 | 40717)
                        || ctx
                            .sde
                            .types
                            .get(&item.type_id)
                            .is_some_and(|t| t.effect_ids.contains(&6484));
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
                    if let (Some(mod_t), Some(ship_t)) = (
                        ctx.sde.types.get(&item.type_id),
                        ctx.sde.types.get(&ship_item.type_id),
                    ) {
                        crate::services::tactical_module_mechanics::validate_module_fitting_constraints(
                            ctx, snap, new_loc, item_id, mod_t, ship_t,
                        )?;
                    }
                    let (char_dock, char_sys, active_ship_id) = snap
                        .characters
                        .get(&char_id)
                        .map(|c| {
                            (
                                c.station_id.or(c.structure_id).unwrap_or(0),
                                c.solar_system_id as i64,
                                c.active_ship_id,
                            )
                        })
                        .unwrap_or((0, 0, 0));
                    let ch_dock = if session.station_id.is_none()
                        && session.structure_id.is_none()
                        && session.solar_system_id.is_some()
                    {
                        0
                    } else {
                        char_dock
                    };
                    let ch_sys = if char_sys > 0 {
                        char_sys
                    } else {
                        session.solar_system_id.map(|s| s as i64).unwrap_or(0)
                    };
                    let resolves_to_loc = |start_loc: i64, target_loc: i64| -> bool {
                        let mut cur = start_loc;
                        for _ in 0..4 {
                            if cur == target_loc {
                                return true;
                            }
                            let Some(node) = snap.items.get(&cur) else {
                                return false;
                            };
                            cur = node.location_id;
                        }
                        false
                    };
                    if ch_dock > 0 {
                        if !resolves_to_loc(ship_item.location_id, ch_dock) {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingTarget".into(),
                                message: format!(
                                    "Target ship {new_loc} is not located at current dock {ch_dock}"
                                ),
                                client_error: None,
                            });
                        }
                        if !resolves_to_loc(item.location_id, ch_dock) {
                            return Err(BridgeRpcError {
                                code: "ItemOwnershipViolation".into(),
                                message: format!(
                                    "Item {item_id} is not located at current dock {ch_dock}"
                                ),
                                client_error: None,
                            });
                        }
                    } else if ch_sys > 0 {
                        let caller_ship = session.ship_id.filter(|&id| id > 0).unwrap_or(active_ship_id);
                        let ship_ok = (new_loc == caller_ship && ship_item.location_id == ch_sys)
                            || resolves_to_loc(ship_item.location_id, ch_sys);
                        if !ship_ok {
                            return Err(BridgeRpcError {
                                code: "InvalidFittingTarget".into(),
                                message: format!(
                                    "Target ship {new_loc} is not in current system {ch_sys}"
                                ),
                                client_error: None,
                            });
                        }
                        if !resolves_to_loc(item.location_id, ch_sys) {
                            return Err(BridgeRpcError {
                                code: "ItemOwnershipViolation".into(),
                                message: format!(
                                    "Item {item_id} is not in current system {ch_sys}"
                                ),
                                client_error: None,
                            });
                        }
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
                            client_error: None,
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
                    let module_name = ctx
                        .sde
                        .get_type(mod_type_id)
                        .map(|t| t.name.as_str())
                        .unwrap_or("Module");
                    return Some(Err(BridgeRpcError::fitting_validation_failed(
                        Some(new_loc),
                        Some(item_id),
                        Some(i64::from(mod_type_id)),
                        module_name,
                        msg,
                    )));
                }
            }

            let expected_version = kwargs
                .get("expected_version")
                .and_then(|v| v.as_i64())
                .map(|v| v as u64);
            let idem_key = kwargs.get("idempotency_key").and_then(|v| v.as_str());

            // Pre-read live ballpark positions before entering DB snapshot (`Systems -> Ballpark -> Database Snapshot`).
            let pre_balls = crate::services::structure_acl_defense_rules::snapshot_system_balls_for_inventory(ctx, session, char_id);
            let mut pending_alsc_take: Option<(i64, fitting_containers_ships::AlscLogEntry)> = None;
            let auth_move_ok: Result<(), BridgeRpcError> = ctx.db.with_snapshot(|snap| {
                let Some(moving) = snap.items.get(&item_id) else {
                    return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Item {item_id} not found"), client_error: None });
                };
                let (corp_id, char_sys, char_dock, active_ship_id) = snap
                    .characters
                    .get(&char_id)
                    .map(|c| (c.corporation_id, c.solar_system_id as i64, c.station_id.or(c.structure_id).unwrap_or(0), c.active_ship_id))
                    .unwrap_or((0, 0, 0, 0));
                if item_id == new_loc || snap.is_structure_controller_saved_ship(item_id) || (active_ship_id > 0 && item_id == active_ship_id && (snap.items.contains_key(&new_loc) || (116..=122).contains(&flag_u16))) {
                    return Err(BridgeRpcError { code: "CannotMoveActiveShip".into(), message: format!("Cannot move item {item_id} into {new_loc}"), client_error: None });
                }
                super::inventory_bound::validate_movable_item_not_piloted_or_bare_space_in_snap(
                    snap, char_id, moving, new_loc, flag_u16,
                )?;
                if matches!(flag_u16, 36 | 63 | 72) {
                    return Err(BridgeRpcError {
                        code: if flag_u16 == 72 { "ImpoundedItem" } else if flag_u16 == 36 { "AssetSafetyLocked" } else { "ItemLocked" }.into(),
                        message: format!("Cannot move item {item_id} into restricted flag {flag_u16}"),
                        client_error: None,
                    });
                }
                fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, moving)?;
                if let Some(exp_v) = expected_version {
                    if moving.version != exp_v {
                        return Err(BridgeRpcError { code: "ItemVersionMismatch".into(), message: format!("Item {item_id} version mismatch: expected {exp_v}, actual {}", moving.version), client_error: None });
                    }
                }
                let is_courier_cargo = snap.item_custom_names.get(&item_id).is_some_and(|s| s.starts_with("courier_cargo:"));
                if is_courier_cargo && ((116..=122).contains(&flag_u16) || snap.items.get(&new_loc).is_some_and(|d| d.owner_id != char_id)) {
                    return Err(BridgeRpcError {
                        code: "CannotDeliverCourierCargo".into(),
                        message: "Courier cargo cannot be moved into corporation hangar divisions or foreign containers/ships".into(),
                        client_error: None,
                    });
                }
                let ch_sys = session.solar_system_id.filter(|&id| id > 0).map(|s| s as i64).unwrap_or(char_sys);
                let ch_dock = if session.station_id.is_none() && session.structure_id.is_none() && session.solar_system_id.is_some() { 0 } else { char_dock };
                let caller_ship_id = session.ship_id.filter(|&id| id > 0).unwrap_or(active_ship_id);
                let ball_or_snap_pos = |id: i64| -> Option<DVec3> {
                    pre_balls.get(&id).map(|p| p.0).or_else(|| snap.item_positions.get(&id).copied().map(DVec3::from))
                };
                let ball_or_sde_radius = |id: i64| -> f64 {
                    pre_balls
                        .get(&id)
                        .map(|p| p.1)
                        .filter(|&r| r > 0.0)
                        .or_else(|| {
                            snap.items
                                .get(&id)
                                .and_then(|it| ctx.sde.types.get(&it.type_id))
                                .map(|t| t.radius)
                                .or_else(|| {
                                    snap.structures
                                        .get(&id)
                                        .and_then(|st| ctx.sde.types.get(&st.type_id))
                                        .map(|t| t.radius)
                                })
                        })
                        .unwrap_or(0.0)
                        .max(0.0)
                };
                let is_storage_auth_and_near = |storage_id: i64| -> bool {
                    if ch_sys <= 0 || ch_dock != 0 { return false; }
                    if !crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| rt.authorized_shipping_storage.contains(&(storage_id, char_id))) {
                        return false;
                    }
                    let (unit_owner, is_personal) = snap.items.get(&storage_id).map(|i| {
                        let cat = ctx.sde.types.get(&i.type_id).map(|t| t.category_id).unwrap_or(0);
                        let grp = ctx.sde.types.get(&i.type_id).map(|t| t.group_id).unwrap_or(0);
                        (i.owner_id, cat == 22 || grp == 340)
                    }).unwrap_or((0, false));
                    let live_ok = session.has_gm_role() || unit_owner == char_id || unit_owner == 0
                        || (!is_personal && snap.fleets.values().any(|f| f.members.contains(&char_id) && f.members.contains(&unit_owner)));
                    live_ok && pre_balls.get(&session.ship_id.unwrap_or(0)).zip(pre_balls.get(&storage_id)).is_some_and(|(s, u)| s.0.distance(u.0) <= 5_000.0)
                };
                let is_ship_near_or_docked = |ship_id_target: i64| -> bool {
                    let Some(s_item) = snap.items.get(&ship_id_target) else { return false; };
                    if ch_dock > 0 {
                        s_item.location_id == ch_dock
                    } else {
                        ch_sys > 0
                            && s_item.location_id == ch_sys
                            && ball_or_snap_pos(caller_ship_id)
                                .zip(ball_or_snap_pos(ship_id_target))
                                .is_some_and(|(s, t)| {
                                    (s.distance(t)
                                        - ball_or_sde_radius(caller_ship_id)
                                        - ball_or_sde_radius(ship_id_target))
                                    .max(0.0)
                                        <= 2_500.0
                                })
                    }
                };
                let is_loc_colocated_with_fallback =
                    |start_loc: i64, allow_unpositioned_loot_can: bool| -> bool {
                        let mut cur = start_loc;
                        let caller_r = ball_or_sde_radius(caller_ship_id);
                        for _ in 0..8 {
                            if ch_dock > 0 {
                                if cur == ch_dock { return true; }
                                let Some(node) = snap.items.get(&cur) else { return false; };
                                cur = node.location_id;
                            } else if ch_sys > 0 {
                                if cur == ch_sys { return false; }
                                if let Some(st) = snap.structures.get(&cur) {
                                    if st.solar_system_id as i64 == ch_sys {
                                        let target_pos = ball_or_snap_pos(cur).or(Some(DVec3::from_array(st.position)));
                                        let target_r = ball_or_sde_radius(cur);
                                        return match (ball_or_snap_pos(caller_ship_id), target_pos) {
                                            (Some(s), Some(t)) => (s.distance(t) - caller_r - target_r).max(0.0) <= 5_000.0,
                                            _ => false,
                                        };
                                    }
                                }
                                let Some(node) = snap.items.get(&cur) else { return false; };
                                if node.location_id == ch_sys {
                                    if cur == active_ship_id || cur == caller_ship_id { return true; }
                                    let target_r = ball_or_sde_radius(cur);
                                    let node_type = ctx.sde.types.get(&node.type_id);
                                    let is_starbase_unit = node_type.is_some_and(|t| t.category_id == 22);
                                    let is_unanchored_loot_can = allow_unpositioned_loot_can
                                        && (node.type_id == 23
                                            || node_type.is_some_and(|t| matches!(t.group_id, 186 | 1247 | 1250)));
                                    let max_range = if is_starbase_unit { 5_000.0 } else { 2_500.0 };
                                    return match (ball_or_snap_pos(caller_ship_id), ball_or_snap_pos(cur)) {
                                        (Some(s), Some(t)) => (s.distance(t) - caller_r - target_r).max(0.0) <= max_range,
                                        (Some(_), None) if is_unanchored_loot_can => true,
                                        _ => false,
                                    };
                                }
                                cur = node.location_id;
                            } else {
                                return false;
                            }
                        }
                        false
                    };
                let is_loc_colocated = |start_loc: i64| -> bool {
                    is_loc_colocated_with_fallback(start_loc, false)
                };
                let is_struct_loc = snap.structures.contains_key(&moving.location_id);
                let is_core_item = is_struct_loc && (
                    ctx.sde.types.get(&moving.type_id).is_some_and(|t| t.group_id == 4086 || t.category_id == 66)
                        || snap.structures.get(&moving.location_id)
                            .and_then(|st| crate::services::station_structure::structure_lifecycle::structure_profile(ctx, st.type_id))
                            .and_then(|p| p.core_type_id)
                            .is_some_and(|c| c == moving.type_id)
                );
                let is_deed_bay = is_struct_loc && (
                    moving.flag == InventoryFlag::StructureDeedBay
                        || moving.flag as u16 == 180
                        || ((moving.flag == InventoryFlag::StructureFuel || moving.flag as u16 == 172) && is_core_item)
                );
                let src_is_struct_fuel = is_struct_loc
                    && !is_core_item
                    && crate::services::structure_acl_defense_rules::is_structure_fuel_flag(moving.flag);
                let supplied_pw = kwargs
                    .get("password")
                    .and_then(|v| v.as_str())
                    .or_else(|| args.iter().find_map(|a| a.as_str()));
                let src_foreign_ship_opt = if is_deed_bay || src_is_struct_fuel {
                    None
                } else {
                    super::inventory_bound::check_foreign_ship_bay_ancestor_chain_in_snap(
                        ctx,
                        snap,
                        session,
                        char_id,
                        moving.location_id,
                        moving.flag as u16,
                        supplied_pw,
                        false,
                    )?
                };
                let src_ok = if is_deed_bay {
                    return Err(BridgeRpcError { code: "ItemLocked".into(), message: "Quantum core cannot be removed from structure deed bay".into(), client_error: None });
                } else if src_is_struct_fuel {
                    crate::services::structure_acl_defense_rules::validate_structure_fuel_bay_transfer(
                        ctx, snap, session, char_id, moving.location_id, moving.type_id, false, ch_dock, ch_sys, caller_ship_id, &pre_balls,
                    )?;
                    true
                } else if moving.owner_id == char_id || (corp_id > 0 && moving.owner_id == corp_id) {
                    if let Some(foreign_ship_id) = src_foreign_ship_opt {
                        is_ship_near_or_docked(foreign_ship_id)
                    } else if let Some(parent) = snap.items.get(&moving.location_id) {
                        let p_cat = ctx.sde.types.get(&parent.type_id).map(|t| t.category_id).unwrap_or(0);
                        let p_grp = ctx.sde.types.get(&parent.type_id).map(|t| t.group_id).unwrap_or(0);
                        let is_foreign_unit = (p_cat == 22 || matches!(p_grp, 12 | 340 | 448 | 649))
                            && parent.type_id != 23
                            && parent.owner_id != char_id
                            && parent.owner_id != 0
                            && !(corp_id > 0 && parent.owner_id == corp_id);
                        let is_foreign_ship = p_cat == 6 && parent.owner_id != char_id;
                        if is_foreign_unit {
                            is_storage_auth_and_near(parent.item_id)
                        } else if is_foreign_ship {
                            let moving_flag = moving.flag as u16;
                            let is_shared_bay = matches!(moving_flag, 90 | 138 | 155);
                            is_shared_bay && is_ship_near_or_docked(parent.item_id)
                        } else {
                            let allow_unpos_can = looted_from_space
                                || parent.type_id == 23
                                || matches!(p_grp, 186 | 1247 | 1250);
                            is_loc_colocated_with_fallback(moving.location_id, allow_unpos_can)
                        }
                    } else {
                        is_loc_colocated(moving.location_id)
                    }
                } else if let Some(parent) = snap.items.get(&moving.location_id) {
                    let p_grp = ctx.sde.types.get(&parent.type_id).map(|t| t.group_id).unwrap_or(0);
                    let p_cat = ctx.sde.types.get(&parent.type_id).map(|t| t.category_id).unwrap_or(0);
                    if is_storage_auth_and_near(parent.item_id) {
                        if parent.owner_id != char_id && !session.has_gm_role() {
                            if let Some(req_pw) = fitting_containers_ships::container_password_snap(snap, parent.item_id, 1) {
                                if !fitting_containers_ships::is_container_unlocked_snap(snap, parent.item_id, char_id)
                                    && supplied_pw != Some(req_pw.as_str())
                                {
                                    return Err(BridgeRpcError { code: "ALSCPasswordRequired".into(), message: format!("Password required to take items from container {}", parent.item_id), client_error: None });
                                }
                            }
                        }
                        if p_grp == 448 {
                            let now_unix = ctx.now_unix_secs();
                            pending_alsc_take = Some((parent.item_id, fitting_containers_ships::AlscLogEntry {
                                timestamp_unix: now_unix,
                                actor_id: char_id,
                                action: "ALSCActionTake".into(),
                                item_id: moving.item_id,
                                item_type_id: moving.type_id,
                                quantity: moving.quantity,
                            }));
                        }
                        true
                    } else if fitting_containers_ships::is_container_group(p_grp, parent.type_id) || matches!(p_grp, 186 | 1247 | 1250) {
                        let is_public_or_own = parent.owner_id == char_id
                            || parent.owner_id == 0
                            || parent.type_id == 23
                            || matches!(p_grp, 186 | 1247 | 1250)
                            || (corp_id > 0 && parent.owner_id == corp_id);
                        if let Some(foreign_ship_id) = src_foreign_ship_opt {
                            is_ship_near_or_docked(foreign_ship_id)
                        } else if is_public_or_own {
                            let allow_unpos_can = looted_from_space
                                || parent.type_id == 23
                                || matches!(p_grp, 186 | 1247 | 1250);
                            is_loc_colocated_with_fallback(parent.item_id, allow_unpos_can)
                        } else {
                            is_storage_auth_and_near(parent.item_id)
                        }
                    } else if p_cat == 6 {
                        let moving_flag = moving.flag as u16;
                        let is_shared = matches!(moving_flag, 90 | 138 | 155);
                        is_shared && is_ship_near_or_docked(parent.item_id) && src_foreign_ship_opt == Some(parent.item_id)
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !src_ok {
                    return Err(BridgeRpcError { code: "ItemOwnershipViolation".into(), message: format!("Character {char_id} is not authorized to move item {item_id}"), client_error: None });
                };

                super::inventory_bound::validate_ship_only_bay_target_in_snap(ctx, snap, new_loc, flag_u16)?;
                let dst_foreign_ship_opt = super::inventory_bound::check_foreign_ship_bay_ancestor_chain_in_snap(
                    ctx,
                    snap,
                    session,
                    char_id,
                    new_loc,
                    flag_u16,
                    supplied_pw,
                    true,
                )?;
                let dst_ok = if let Some(dest) = snap.items.get(&new_loc) {
                    let d_grp = ctx.sde.types.get(&dest.type_id).map(|t| t.group_id).unwrap_or(0);
                    let d_cat = ctx.sde.types.get(&dest.type_id).map(|t| t.category_id).unwrap_or(0);
                    let foreign_ancestor_near = dst_foreign_ship_opt.is_none_or(is_ship_near_or_docked);
                    if !foreign_ancestor_near {
                        false
                    } else if dest.owner_id == char_id || (corp_id > 0 && dest.owner_id == corp_id && d_cat != 6) {
                        is_loc_colocated(new_loc)
                    } else if is_storage_auth_and_near(dest.item_id) {
                        if dest.owner_id != char_id && !session.has_gm_role() {
                            if let Some(req_pw) = fitting_containers_ships::container_password_snap(snap, dest.item_id, 1) {
                                if !fitting_containers_ships::is_container_unlocked_snap(snap, dest.item_id, char_id)
                                    && supplied_pw != Some(req_pw.as_str())
                                {
                                    return Err(BridgeRpcError { code: "ALSCPasswordRequired".into(), message: format!("Password required to move items into container {}", dest.item_id), client_error: None });
                                }
                            }
                        }
                        true
                    } else if fitting_containers_ships::is_container_group(d_grp, dest.type_id) || d_grp == 186 {
                        let is_public_or_own = dest.owner_id == char_id
                            || dest.owner_id == 0
                            || dest.type_id == 23
                            || (corp_id > 0 && dest.owner_id == corp_id);
                        if let Some(foreign_ship_id) = dst_foreign_ship_opt {
                            is_ship_near_or_docked(foreign_ship_id)
                        } else if is_public_or_own {
                            is_loc_colocated(new_loc)
                        } else {
                            is_storage_auth_and_near(dest.item_id)
                        }
                    } else if d_cat == 6 && matches!(flag_u16, 90 | 138 | 155) {
                        is_ship_near_or_docked(dest.item_id) && dst_foreign_ship_opt == Some(dest.item_id)
                    } else {
                        false
                    }
                } else if (60_000_000..=69_999_999).contains(&new_loc) {
                    ch_dock == new_loc
                } else if new_loc >= 1_000_000_000_000 || snap.structures.contains_key(&new_loc) {
                    let is_core_item = ctx.sde.types.get(&moving.type_id).is_some_and(|t| t.group_id == 4086 || t.category_id == 66);
                    let is_deed_flag = flag_u16 == 180 || flag == InventoryFlag::StructureDeedBay || ((matches!(flag_u16, 172) || flag == InventoryFlag::StructureFuel) && is_core_item);
                    let is_fuel_flag = matches!(flag_u16, 133)
                        || flag == InventoryFlag::SpecializedFuelBay
                        || ((matches!(flag_u16, 172) || flag == InventoryFlag::StructureFuel) && !is_core_item);
                    if is_fuel_flag {
                        crate::services::structure_acl_defense_rules::validate_structure_fuel_bay_transfer(
                            ctx, snap, session, char_id, new_loc, moving.type_id, true, ch_dock, ch_sys, caller_ship_id, &pre_balls,
                        )?;
                        true
                    } else if is_deed_flag {
                        let st = snap.structures.get(&new_loc).ok_or_else(|| {
                            BridgeRpcError { code: "StructureNotFound".into(), message: format!("Structure {new_loc} not found"), client_error: None }
                        })?;
                        if st.state == "Destroyed" {
                            return Err(BridgeRpcError { code: "StructureDestroyed".into(), message: format!("Structure {new_loc} is destroyed"), client_error: None });
                        }
                        if ctx.sde.solar_systems.get(&st.solar_system_id).is_some_and(|s| s.region_id == crate::services::station_structure::structure_lifecycle::POCHVEN_REGION_ID) {
                            return Err(BridgeRpcError {
                                code: "QuantumCoreDisallowedInPochven".into(),
                                message: format!("Quantum Cores cannot be installed into Upwell structure {new_loc} in Pochven"),
                                client_error: None,
                            });
                        }
                        if ch_dock != new_loc {
                            return Err(BridgeRpcError { code: "DestinationAccessDenied".into(), message: format!("Installing a quantum core requires docking in structure {new_loc}"), client_error: None });
                        }
                        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
                        if caller_corp != st.owner_corp_id {
                            return Err(BridgeRpcError { code: "StructureOwnershipViolation".into(), message: format!("Character {char_id} does not belong to structure owning corp {}", st.owner_corp_id), client_error: None });
                        }
                        use eve_db::corp_roles::{ROLE_DIRECTOR, ROLE_STATION_MANAGER};
                        let req = ROLE_DIRECTOR | ROLE_STATION_MANAGER;
                        let (is_ceo, eff_roles) = snap.effective_corp_roles_at_db(
                            caller_corp,
                            char_id,
                            Some(new_loc),
                            ctx.now_unix(),
                        );
                        let has_role = is_ceo || (eff_roles & req) != 0;
                        if !has_role {
                            return Err(BridgeRpcError { code: "CorpRoleDenied".into(), message: "Installing a quantum core requires Director or Station Manager role".into(), client_error: None });
                        }
                        let profile = crate::services::station_structure::structure_lifecycle::structure_profile(ctx, st.type_id);
                        let expected_core = profile.as_ref().and_then(|p| p.core_type_id);
                        if expected_core != Some(moving.type_id) {
                            return Err(BridgeRpcError { code: "InvalidItemType".into(), message: format!("Item {} is not the required quantum core for structure {}", moving.type_id, new_loc), client_error: None });
                        }
                        true
                    } else {
                        if let Some(st) = snap.structures.get(&new_loc) {
                            if st.state == "Destroyed" {
                                return Err(BridgeRpcError { code: "StructureDestroyed".into(), message: format!("Structure {new_loc} is destroyed"), client_error: None });
                            }
                            let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
                            if st.owner_corp_id != caller_corp
                                && !crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap(snap, new_loc, char_id)
                            {
                                return Err(BridgeRpcError { code: "DestinationAccessDenied".into(), message: format!("Character {char_id} lacks structure access at {new_loc}"), client_error: None });
                            }
                        }
                        ch_dock == new_loc
                    }
                } else {
                    false
                };
                if !dst_ok {
                    return Err(BridgeRpcError { code: "DestinationAccessDenied".into(), message: format!("Character {char_id} is not authorized to move items into {new_loc}"), client_error: None });
                }
                Ok(())
            });
            if let Err(e) = auth_move_ok {
                return Some(Err(e));
            }

            let is_quantum_core_deposit = (flag_u16 == 172 || flag_u16 == 180 || flag == InventoryFlag::StructureDeedBay)
                && ctx.db.with_snapshot(|snap| {
                    snap.structures.get(&new_loc).is_some_and(|st| {
                        let profile = crate::services::station_structure::structure_lifecycle::structure_profile(ctx, st.type_id);
                        let expected_core = profile.as_ref().and_then(|p| p.core_type_id);
                        expected_core == source_item_opt.as_ref().map(|i| i.type_id)
                    })
                });
            if is_quantum_core_deposit {
                flag = InventoryFlag::StructureDeedBay;
            }

            let dest_is_struct_fuel = crate::services::structure_acl_defense_rules::is_structure_fuel_flag(flag) && ctx.db.with_snapshot(|s| s.structures.contains_key(&new_loc));
            let dest_is_ship_hold = !dest_is_struct_fuel && matches!(flag_u16, 5 | 87 | 158 | 133..=151 | 177 | 179 | 181 | 182 | 185);
            let eval_cargo_cap = if dest_is_ship_hold && flag_u16 == 5 {
                ctx.get_or_rebuild_ship_fitting(char_id, new_loc).ok()
                    .map(|f| f.evaluated_context(&ctx.sde).ship.get_attribute(38, &ctx.sde))
                    .filter(|&c| c > 0.0)
            } else {
                None
            };
            let orig_item_id = item_id;
            let active_ship_opt = session.ship_id;
            let orig_source_snapshot = source_item_opt.clone();
            let was_singleton_before_move = orig_source_snapshot.as_ref().map(|i| i.singleton).unwrap_or(false);
            let unfit_ship_id = orig_source_snapshot
                .as_ref()
                .filter(|src| matches!(src.flag as u16, 11..=34 | 92..=94 | 125..=132))
                .map(|src| src.location_id);
            let old_src_ship_attrs = unfit_ship_id
                .filter(|&sid| sid != new_loc)
                .and_then(|sid| snapshot_ship_fitting_attrs_for_ship(ctx, char_id, sid));
            let attr_ship_id = if is_fitting_slot { Some(new_loc) } else { unfit_ship_id };
            let old_ship_attrs =
                attr_ship_id.and_then(|sid| snapshot_ship_fitting_attrs_for_ship(ctx, char_id, sid));

            #[allow(clippy::large_enum_variant)]
            enum AddTxOutcome {
                Merged {
                    merged_item: eve_db::InventoryItemRecord,
                    moved_tid: u32,
                    moved_qty: i32,
                    dst_ship_bay_foreign_owner: Option<i64>,
                },
                Moved {
                    updated: eve_db::InventoryItemRecord,
                    effective_item_id: i64,
                    orig_qty: i32,
                    old_loc_id: i64,
                    old_flag_id: i64,
                    old_owner_id: i64,
                    old_was_fitted: bool,
                    st_fuel: Option<u32>,
                    dst_corp_owner: i64,
                    src_ship_bay_owner: Option<i64>,
                    dst_ship_bay_foreign_owner: Option<i64>,
                    recursive_owner_changes: Vec<(eve_db::InventoryItemRecord, eve_db::InventoryItemRecord)>,
                },
            }

            let tx_res: Result<AddTxOutcome, BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(orig_item) = snap.items.get(&orig_item_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "InventoryMoveFailed".into(),
                        message: format!("Item {orig_item_id} not found"),
                        client_error: None,
                    });
                };

                if orig_item.location_id != orig_source_loc_id {
                    return Err(BridgeRpcError {
                        code: "InventoryMoveFailed".into(),
                        message: format!(
                            "Item {orig_item_id} moved concurrently from location {orig_source_loc_id} to {}",
                            orig_item.location_id
                        ),
                        client_error: None,
                    });
                }

                super::inventory_bound::validate_movable_item_not_piloted_or_bare_space_in_snap(
                    snap, char_id, &orig_item, new_loc, flag_u16,
                )?;
                if let Some(ch) = snap.characters.get(&char_id) {
                    if orig_item_id == new_loc
                        || orig_item_id == ch.active_ship_id
                        || snap.is_structure_controller_saved_ship(orig_item_id)
                        || flag == InventoryFlag::Locked
                    {
                        return Err(BridgeRpcError {
                            code: "InventoryMoveFailed".into(),
                            message: format!("Destination location {new_loc} is not authorized for character {char_id}"),
                            client_error: None,
                        });
                    }
                    if let Some(dock_id) = session.station_id.or(session.structure_id) {
                        if ch.station_id.or(ch.structure_id) != Some(dock_id) {
                            return Err(BridgeRpcError {
                                code: "InventoryMoveFailed".into(),
                                message: format!("Character {char_id} is no longer docked at {dock_id}"),
                                client_error: None,
                            });
                        }
                    } else if let Some(sess_sys) = session.solar_system_id {
                        if ch.station_id.is_some() || ch.structure_id.is_some() || ch.solar_system_id != sess_sys {
                            return Err(BridgeRpcError {
                                code: "InventoryMoveFailed".into(),
                                message: format!("Character {char_id} changed location concurrently"),
                                client_error: None,
                            });
                        }
                    }
                } else if flag == InventoryFlag::Locked || orig_item_id == new_loc {
                    return Err(BridgeRpcError {
                        code: "InventoryMoveFailed".into(),
                        message: format!("Item {orig_item_id} move is locked or invalid"),
                        client_error: None,
                    });
                }

                if matches!(flag_u16, 36 | 63 | 72) {
                    return Err(BridgeRpcError {
                        code: if flag_u16 == 72 { "ImpoundedItem" } else if flag_u16 == 36 { "AssetSafetyLocked" } else { "ItemLocked" }.into(),
                        message: format!("Cannot move item {orig_item_id} into restricted flag {flag_u16}"),
                        client_error: None,
                    });
                }
                fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, &orig_item)?;
                if let Some(dest_item) = snap.items.get(&new_loc) {
                    fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, dest_item)?;
                }
                super::inventory_bound::recheck_invbroker_add_auth_in_snap(
                    ctx,
                    snap,
                    session,
                    char_id,
                    &orig_item,
                    orig_source_snapshot.as_ref(),
                    new_loc,
                    flag,
                    args,
                    kwargs,
                )?;
                let corp_id_for_owner = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0);

                if is_fitting_slot {
                    if orig_item.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "ItemOwnershipViolation".into(),
                            message: format!("Item {orig_item_id} is not owned by {char_id}"),
                            client_error: None,
                        });
                    }
                    let eff_qty = split_qty_opt.unwrap_or(orig_item.quantity);
                    if eff_qty != 1 {
                        return Err(BridgeRpcError::cannot_fit_stacked_module(Some(new_loc), Some(orig_item_id), eff_qty.into(), format!("Item {orig_item_id} has quantity {eff_qty} and must be split to 1 before fitting")));
                    }
                    let Some(ship_item) = snap.items.get(&new_loc) else {
                        return Err(BridgeRpcError {
                            code: "ShipNotFound".into(),
                            message: format!("Target ship {new_loc} not found"),
                            client_error: None,
                        });
                    };
                    let ship_cat = ctx.sde.types.get(&ship_item.type_id).map(|t| t.category_id).unwrap_or(0);
                    if ship_item.owner_id != char_id || ship_cat != 6 {
                        return Err(BridgeRpcError {
                            code: "InvalidFittingTarget".into(),
                            message: format!("Target {new_loc} is not an owned ship"),
                            client_error: None,
                        });
                    }
                    if ship_item.flag == InventoryFlag::Locked
                        || snap.is_item_or_ancestor_locked_or_impounded(new_loc)
                    {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!(
                                "Target ship {new_loc} is locked by an active contract, industry job, or container"
                            ),
                            client_error: None,
                        });
                    }
                    if snap.items.values().any(|other| {
                        other.item_id != orig_item_id
                            && other.location_id == new_loc
                            && other.flag == flag
                            && ctx.sde.types.get(&other.type_id).map(|t| t.category_id == 7 || t.category_id == 32).unwrap_or(true)
                    }) {
                        return Err(BridgeRpcError {
                            code: "SlotAlreadyOccupied".into(),
                            message: format!("Slot {flag_u16} on ship {new_loc} is already occupied"),
                            client_error: None,
                        });
                    }
                    if let (Some(mod_t), Some(ship_t)) = (ctx.sde.types.get(&orig_item.type_id), ctx.sde.types.get(&ship_item.type_id)) {
                        crate::services::tactical_module_mechanics::validate_module_fitting_constraints(
                            ctx, snap, new_loc, orig_item_id, mod_t, ship_t,
                        )?;
                    }
                }

                if !is_fitting_slot {
                    if let (Some(dest_item), Some(item_type)) = (snap.items.get(&new_loc), ctx.sde.types.get(&orig_item.type_id)) {
                        if let Some(dest_type) = ctx.sde.types.get(&dest_item.type_id) {
                            let eff_qty = split_qty_opt.unwrap_or(orig_item.quantity).max(1);
                            let mut moving_vol = effective_item_unit_volume(&ctx.sde, &orig_item) * (eff_qty as f64);
                            let mut charge_tids = Vec::new();
                            if item_type.category_id != 8 && (11..=34).contains(&(orig_item.flag as u16)) {
                                for ci in snap.items.values().filter(|i| {
                                    i.location_id == orig_item.location_id
                                        && i.flag == orig_item.flag
                                        && i.item_id != orig_item_id
                                        && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 8)
                                }) {
                                    charge_tids.push(ci.type_id);
                                    moving_vol += effective_item_unit_volume(&ctx.sde, ci) * (ci.quantity.max(1) as f64);
                                }
                            }
                            if fitting_containers_ships::validate_container_dest_rules_in_snap(
                                ctx, char_id, &orig_item, dest_item, dest_type,
                            )? {
                                fitting_containers_ships::validate_container_capacity_in_snap(
                                    &ctx.sde, snap, dest_item, dest_type, new_loc, &[orig_item_id], moving_vol,
                                )?;
                                let supplied_pw = kwargs.get("password").and_then(|v| v.as_str()).or_else(|| args.iter().find_map(|a| a.as_str()));
                                fitting_containers_ships::validate_container_password_in_snap(snap, new_loc, char_id, supplied_pw, true)?;
                            } else if dest_is_ship_hold && dest_type.category_id == 6 {
                                if !crate::services::fitting_saved_apply::is_item_allowed_in_ship_hold(flag_u16, item_type, dest_type) {
                                    return Err(BridgeRpcError::user_error("InvalidItemTypeForHold"));
                                }
                                for ctid in charge_tids {
                                    if let Some(ct) = ctx.sde.types.get(&ctid) {
                                        if !crate::services::fitting_saved_apply::is_item_allowed_in_ship_hold(flag_u16, ct, dest_type) {
                                            return Err(BridgeRpcError::user_error("InvalidItemTypeForHold"));
                                        }
                                    }
                                }
                                if matches!(flag_u16, 177 | 179)
                                    && (dest_type.default_attributes.get(&3020).copied().unwrap_or(0.0) > 0.0 || matches!(dest_type.group_id, 27 | 898 | 900))
                                    && item_type.category_id == 6
                                {
                                    if !orig_item.singleton || orig_item.quantity != 1 {
                                        return Err(BridgeRpcError { code: "ShipMustBeAssembled".into(), message: "UserError: Only an assembled frigate can be placed in the Frigate Escape Bay".into(), client_error: None });
                                    }
                                    let has_non_charge_cargo = snap.items.values().any(|c| {
                                        c.location_id == orig_item_id && !matches!(c.flag as u16, 11..=34 | 92..=94 | 125..=132) && ctx.sde.types.get(&c.type_id).is_none_or(|t| t.category_id != 8)
                                    });
                                    if has_non_charge_cargo {
                                        return Err(BridgeRpcError { code: "ShipHasItemsInCargo".into(), message: "UserError: Frigate in Frigate Escape Bay cannot contain non-charge cargo items".into(), client_error: None });
                                    }
                                    let existing_frigate = snap.items.values().any(|i| {
                                        i.location_id == new_loc && matches!(i.flag as u16, 177 | 179) && i.item_id != orig_item_id && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 6)
                                    });
                                    if existing_frigate {
                                        return Err(BridgeRpcError { code: "CapacityExceeded".into(), message: "UserError: Frigate Escape Bay can hold at most 1 assembled frigate".into(), client_error: None });
                                    }
                                }
                                let hold_capacity = crate::services::fitting_saved_apply::get_ship_hold_capacity(flag_u16, dest_type, eval_cargo_cap);
                                if flag_u16 != 5 && hold_capacity <= 0.0 {
                                    return Err(ship_hold_capacity_error(flag_u16, 0.0, 0.0, moving_vol));
                                }
                                if !matches!(flag_u16, 177 | 179) && hold_capacity > 0.0 {
                                    let used_vol: f64 = snap.items.values()
                                        .filter(|i| {
                                            i.location_id == new_loc
                                                && i.item_id != orig_item_id
                                                && (i.flag as u16) == flag_u16
                                        })
                                        .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64)).sum();
                                    if used_vol + moving_vol > hold_capacity + 1e-4 {
                                        return Err(ship_hold_capacity_error(flag_u16, hold_capacity, used_vol, moving_vol));
                                    }
                                }
                            } else if matches!(flag_u16, 90 | 155) && dest_type.category_id == 6 {
                                let supplied_pw = kwargs
                                    .get("password")
                                    .and_then(|v| v.as_str())
                                    .or_else(|| args.iter().find_map(|a| a.as_str()));
                                fitting_containers_ships::validate_ship_bay_capacity_and_rules_in_snap(
                                    ctx,
                                    snap,
                                    char_id,
                                    orig_item_id,
                                    &orig_item,
                                    new_loc,
                                    dest_item,
                                    flag_u16,
                                    moving_vol,
                                    supplied_pw,
                                    true,
                                )?;
                            }
                        }
                    }
                }

                if split_qty_opt.is_none() {
                    if let Some(key) = idem_key {
                        if snap.committed_idempotency_keys.contains(&(char_id, key.to_string())) {
                            return Ok(AddTxOutcome::Moved {
                                updated: orig_item.clone(),
                                effective_item_id: orig_item_id,
                                orig_qty: orig_item.quantity,
                                old_loc_id: orig_item.location_id,
                                old_flag_id: orig_item.flag as i64,
                                old_owner_id: orig_item.owner_id,
                                old_was_fitted: false,
                                st_fuel: None,
                                dst_corp_owner: 0,
                                src_ship_bay_owner: None,
                                dst_ship_bay_foreign_owner: None,
                                recursive_owner_changes: Vec::new(),
                            });
                        }
                    }
                    if let Some(exp_v) = expected_version {
                        if orig_item.version != exp_v {
                            return Err(BridgeRpcError {
                                code: "InventoryMoveFailed".into(),
                                message: format!("Stale entity version (expected {exp_v}, found {})", orig_item.version),
                                client_error: None,
                            });
                        }
                    }
                }

                let dest_ship_bay_owner = super::inventory_bound::ship_bay_target_owner_in_snap(
                    ctx, snap, new_loc, flag_u16,
                );
                let src_ship_bay_owner = super::inventory_bound::foreign_ship_owner_at_loc_in_snap(
                    ctx, snap, orig_item.location_id, orig_item.flag as u16, char_id,
                );
                let dst_ship_bay_foreign_owner = dest_ship_bay_owner.filter(|&o| o != char_id);
                let struct_fuel_owner = (flag_u16 == 133 || flag_u16 == 172 || flag == InventoryFlag::StructureFuel)
                    .then(|| snap.structures.get(&new_loc).map(|st| st.owner_corp_id))
                    .flatten()
                    .unwrap_or(0);
                let dst_corp_owner = if struct_fuel_owner > 0 {
                    struct_fuel_owner
                } else if (116..=122).contains(&flag_u16) || flag_u16 == 180 || flag == InventoryFlag::StructureDeedBay {
                    corp_id_for_owner
                } else {
                    0
                };

                let (effective_item_id, active_src_record) = if looted_from_space {
                    let active_ship = active_ship_opt.unwrap_or(new_loc);
                    if let Some(split_qty) = split_qty_opt {
                        let orig = snap.items.get_mut(&orig_item_id).ok_or_else(|| BridgeRpcError {
                            code: "SplitFailed".into(),
                            message: format!("Item {orig_item_id} not found"),
                            client_error: None,
                        })?;
                        if orig.singleton || split_qty <= 0 || split_qty >= orig.quantity {
                            return Err(BridgeRpcError { code: "SplitFailed".into(), message: "Cannot split".into(), client_error: None });
                        }
                        orig.quantity -= split_qty;
                        orig.version += 1;
                        snap.next_entity_id += 1;
                        let new_id = snap.next_entity_id;
                        let split_rec = eve_db::InventoryItemRecord {
                            item_id: new_id,
                            type_id: orig.type_id,
                            owner_id: char_id,
                            location_id: active_ship,
                            flag,
                            quantity: split_qty,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        };
                        snap.items.insert(new_id, split_rec.clone());
                        (new_id, split_rec)
                    } else {
                        let it = snap.items.get_mut(&orig_item_id).ok_or_else(|| BridgeRpcError {
                            code: "InventoryMoveFailed".into(),
                            message: format!("Item {orig_item_id} not found"),
                            client_error: None,
                        })?;
                        it.owner_id = char_id;
                        it.location_id = active_ship;
                        it.flag = flag;
                        it.version += 1;
                        (orig_item_id, it.clone())
                    }
                } else if let Some(split_qty) = split_qty_opt {
                    let split_owner = orig_source_snapshot.as_ref().map(|i| i.owner_id).unwrap_or(orig_item.owner_id);
                    let target_owner = if struct_fuel_owner > 0 {
                        struct_fuel_owner
                    } else if (116..=122).contains(&flag_u16) || flag_u16 == 180 || flag == InventoryFlag::StructureDeedBay {
                        corp_id_for_owner
                    } else if let Some(ship_owner) = dest_ship_bay_owner {
                        ship_owner
                    } else {
                        char_id
                    };
                    let orig = snap.items.get_mut(&orig_item_id).ok_or_else(|| BridgeRpcError {
                        code: "SplitFailed".into(),
                        message: format!("Item {orig_item_id} not found"),
                        client_error: None,
                    })?;
                    if orig.owner_id != split_owner {
                        return Err(BridgeRpcError {
                            code: "SplitFailed".into(),
                            message: format!("Item {orig_item_id} is not owned by character {split_owner}"),
                            client_error: None,
                        });
                    }
                    if orig.singleton || split_qty <= 0 || split_qty >= orig.quantity {
                        return Err(BridgeRpcError { code: "SplitFailed".into(), message: "Cannot split".into(), client_error: None });
                    }
                    orig.quantity -= split_qty;
                    orig.version += 1;
                    let mut split_rec = orig.clone();
                    snap.next_entity_id += 1;
                    let new_id = snap.next_entity_id;
                    split_rec.item_id = new_id;
                    split_rec.owner_id = target_owner;
                    split_rec.quantity = split_qty;
                    split_rec.singleton = false;
                    split_rec.version = 1;
                    snap.items.insert(new_id, split_rec.clone());
                    (new_id, split_rec)
                } else {
                    (orig_item_id, orig_item.clone())
                };
                let orig_qty = active_src_record.quantity;

                let dest_is_ship_fleet_hangar = flag_u16 == 155
                    && snap
                        .items
                        .get(&new_loc)
                        .and_then(|d| ctx.sde.types.get(&d.type_id))
                        .is_some_and(|t| t.category_id == 6);
                if dest_is_ship_hold || dest_is_ship_fleet_hangar {
                    let is_courier = |id: i64| snap.item_custom_names.get(&id).is_some_and(|s| s.starts_with("courier_cargo:"));
                    let can_merge = !active_src_record.singleton
                        && !active_src_record.is_blueprint_copy
                        && active_src_record.dynamic_attributes.is_empty()
                        && active_src_record.source_mutaplasmid_id.is_none()
                        && fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, &active_src_record).is_ok()
                        && !is_courier(effective_item_id);
                    if can_merge {
                        let merge_owner = if dst_corp_owner > 0 {
                            dst_corp_owner
                        } else if let Some(ship_owner) = dest_ship_bay_owner {
                            ship_owner
                        } else {
                            char_id
                        };
                        let existing_target = snap.items.values().find(|i| {
                            i.item_id != effective_item_id
                                && i.location_id == new_loc
                                && i.flag == flag
                                && i.type_id == active_src_record.type_id
                                && (i.owner_id == merge_owner || i.owner_id == active_src_record.owner_id || i.owner_id == char_id)
                                && !i.singleton
                                && !i.is_blueprint_copy
                                && i.dynamic_attributes.is_empty()
                                && i.source_mutaplasmid_id.is_none()
                                && i.flag != InventoryFlag::Locked
                                && !snap.is_item_locked_in_contract_or_job(i.item_id)
                                && !is_courier(i.item_id)
                        }).map(|i| i.item_id);
                        if let Some(target_iid) = existing_target {
                            if let Some(new_qty) = snap.items.get(&target_iid).and_then(|t| t.quantity.checked_add(active_src_record.quantity)) {
                                snap.remove_system_cleanup_item(effective_item_id);
                                let target_item = snap.items.get_mut(&target_iid).unwrap();
                                target_item.quantity = new_qty;
                                target_item.owner_id = merge_owner;
                                target_item.version += 1;
                                let merged_snapshot = target_item.clone();
                                let supplied_pw = kwargs.get("password").and_then(|v| v.as_str()).or_else(|| args.iter().find_map(|a| a.as_str()));
                                let clear_ids = split_qty_opt.is_none().then_some(effective_item_id);
                                fitting_containers_ships::finalize_add_move_side_effects_in_snap(
                                    ctx, snap, char_id, pending_alsc_take.as_ref(), clear_ids.as_slice(), None,
                                    target_iid, orig_item_id, active_src_record.type_id, active_src_record.quantity,
                                    orig_item.location_id, orig_item.flag as u16, new_loc, flag_u16, supplied_pw, ctx.now_unix_secs(),
                                )?;
                                return Ok(AddTxOutcome::Merged {
                                    merged_item: merged_snapshot,
                                    moved_tid: active_src_record.type_id,
                                    moved_qty: active_src_record.quantity,
                                    dst_ship_bay_foreign_owner,
                                });
                            }
                        }
                    }
                }

                let old_loc_id = orig_item.location_id;
                let old_flag_id = orig_item.flag as i64;
                let old_owner_id = orig_item.owner_id;
                let old_was_fitted = matches!(orig_item.flag as u16, 11..=34 | 92..=94 | 125..=132);

                if split_qty_opt.is_some() {
                    if let Some(exp_v) = expected_version {
                        if active_src_record.version != exp_v {
                            return Err(BridgeRpcError {
                                code: "InventoryMoveFailed".into(),
                                message: format!("Stale entity version (expected {exp_v}, found {})", active_src_record.version),
                                client_error: None,
                            });
                        }
                    }
                }

                let active_cat = ctx
                    .sde
                    .types
                    .get(&active_src_record.type_id)
                    .map(|t| t.category_id)
                    .unwrap_or(0);
                let mut updated = {
                    let it = snap.items.get_mut(&effective_item_id).ok_or_else(|| BridgeRpcError {
                        code: "InventoryMoveFailed".into(),
                        message: format!("Item {effective_item_id} not found"),
                        client_error: None,
                    })?;
                    it.location_id = new_loc;
                    it.flag = flag;
                    if is_fitting_slot {
                        it.singleton = true;
                        if active_cat != 8 {
                            it.quantity = 1;
                        }
                    }
                    if dst_corp_owner > 0 {
                        it.owner_id = dst_corp_owner;
                    } else if let Some(ship_owner) = dest_ship_bay_owner {
                        it.owner_id = ship_owner;
                    } else if matches!(flag, InventoryFlag::Hangar | InventoryFlag::Cargo)
                        || dest_is_ship_hold
                        || src_ship_bay_owner.is_some()
                        || (116..=122).contains(&old_flag_id)
                        || matches!(old_flag_id, 62 | 133 | 173 | 187)
                    {
                        it.owner_id = char_id;
                    }
                    it.version += 1;
                    it.clone()
                };
                let recursive_owner_changes: Vec<(eve_db::InventoryItemRecord, eve_db::InventoryItemRecord)> =
                    if updated.owner_id != old_owner_id
                        || dest_ship_bay_owner.is_some()
                        || src_ship_bay_owner.is_some()
                    {
                        snap.set_owner_recursive(effective_item_id, updated.owner_id)
                            .into_iter()
                            .filter(|(pre, _)| pre.item_id != effective_item_id)
                            .collect()
                    } else {
                        Vec::new()
                    };

                if let Some(key) = idem_key {
                    if snap.committed_idempotency_keys.len() >= 4_096 {
                        snap.committed_idempotency_keys.clear();
                    }
                    snap.committed_idempotency_keys.insert((char_id, key.to_string()));
                }

                let (fin_opt, st_fuel) = crate::services::structure_acl_defense_rules::finalize_fuel_bay_move_in_snap(
                    ctx, snap, updated.item_id, old_loc_id, InventoryFlag::from_u16(old_flag_id as u16), new_loc, flag, char_id, corp_id_for_owner,
                );
                if let Some(fin) = fin_opt {
                    updated = fin;
                }

                let mut unfitted_charges = Vec::new();
                if is_fitting_slot {
                    snap.module_online_states.insert(effective_item_id, online);
                } else if old_was_fitted {
                    snap.module_online_states.remove(&effective_item_id);
                    snap.module_active_states.remove(&effective_item_id);
                    snap.module_active_charges.remove(&effective_item_id);
                    crate::router::ServerContext::clear_persisted_module_overload(
                        snap,
                        old_loc_id,
                        effective_item_id,
                    );
                }
                if old_was_fitted && (old_loc_id != new_loc || orig_item.flag != flag) {
                    if let Some(banks) = snap.weapon_banks.get_mut(&old_loc_id) {
                        for list in banks.values_mut() {
                            list.retain(|&id| id != effective_item_id);
                        }
                        banks.retain(|k, v| *k != effective_item_id && v.len() > 1);
                    }
                }
                if old_was_fitted
                    && active_cat != 8
                    && (11..=34).contains(&(old_flag_id as u16))
                    && (old_loc_id != new_loc || orig_item.flag != flag)
                {
                    let old_slot_flag = orig_item.flag;
                    let charge_ids: Vec<i64> = snap
                        .items
                        .values()
                        .filter(|i| {
                            i.location_id == old_loc_id
                                && i.flag == old_slot_flag
                                && i.item_id != effective_item_id
                                && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 8)
                        })
                        .map(|i| i.item_id)
                        .collect();
                    for cid in charge_ids {
                        if let Some(ci) = snap.items.get_mut(&cid) {
                            let pre = ci.clone();
                            ci.location_id = new_loc;
                            ci.flag = flag;
                            ci.singleton = false;
                            ci.owner_id = updated.owner_id;
                            ci.version += 1;
                            unfitted_charges.push((pre, ci.clone()));
                        }
                    }
                }

                let supplied_pw = kwargs.get("password").and_then(|v| v.as_str()).or_else(|| args.iter().find_map(|a| a.as_str()));
                let mut custom_flag_ids = Vec::with_capacity(1 + unfitted_charges.len());
                custom_flag_ids.push(effective_item_id);
                custom_flag_ids.extend(unfitted_charges.iter().map(|(_, post_c)| post_c.item_id));
                fitting_containers_ships::finalize_add_move_side_effects_in_snap(
                    ctx, snap, char_id, pending_alsc_take.as_ref(), &custom_flag_ids,
                    matches!(flag_u16, 90 | 155).then_some(flag_u16),
                    updated.item_id, orig_item_id, updated.type_id, updated.quantity,
                    old_loc_id, old_flag_id as u16, updated.location_id, flag_u16, supplied_pw, ctx.now_unix_secs(),
                )?;

                Ok(AddTxOutcome::Moved {
                    updated,
                    effective_item_id,
                    orig_qty,
                    old_loc_id,
                    old_flag_id,
                    old_owner_id,
                    old_was_fitted,
                    st_fuel,
                    dst_corp_owner,
                    src_ship_bay_owner,
                    dst_ship_bay_foreign_owner,
                    recursive_owner_changes,
                })
            });

            let did_split = split_qty_opt.map(|sq| (orig_item_id, sq));
            match tx_res {
                Ok(AddTxOutcome::Merged {
                    merged_item,
                    moved_tid,
                    moved_qty,
                    dst_ship_bay_foreign_owner,
                }) => {
                    if looted_from_space {
                        let fid = session.fleet_id.or_else(|| crate::services::notify_emitters_seasons_tactical::char_fleet_id(ctx, char_id));
                        crate::services::notify_emitters_character_services::emit_fleet_loot_event_if_in_fleet(ctx, char_id, fid, &[(moved_tid, moved_qty)]);
                        let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::despawn_empty_jetcan_if_needed(ctx, session.solar_system_id, orig_source_loc_id).await;
                    }
                    if pending_alsc_take.is_none() && orig_source_loc_id != new_loc && orig_source_loc_id > 0 {
                        crate::services::pve_dungeon_runtime::on_dungeon_item_looted(ctx, char_id, session.solar_system_id, orig_source_loc_id, moved_tid).await;
                    }
                    if orig_source_loc_id != new_loc {
                        crate::services::pve_homefront_sites::on_homefront_item_moved_sync(
                            ctx,
                            char_id,
                            new_loc,
                            moved_tid,
                            u64::try_from(moved_qty).unwrap_or(0),
                        );
                    }
                    if looted_foreign_jetcan {
                        super::inventory_stacking::apply_space_loot_suspect_flag(ctx, session, char_id);
                    }
                    let affects_fitting = is_fitting_slot || src_is_fitted_module;
                    if let Some(ship_id) = session.ship_id {
                        if affects_fitting {
                            ctx.invalidate_ship_fitting(ship_id);
                        } else {
                            ctx.invalidate_ship_fitting_cache_only(ship_id);
                        }
                    }
                    crate::services::operations_manager_helpers::record_invbroker_add_operation_event(ctx, char_id, orig_source_loc_id, new_loc, flag, moved_tid, moved_qty);
                    if affects_fitting {
                        ctx.invalidate_ship_fitting(new_loc);
                    } else {
                        ctx.invalidate_ship_fitting_cache_only(new_loc);
                    }
                    ctx.emit_typed(
                        NotificationTarget::Character(char_id),
                        ProcessInventoryChange {
                            arg0: PyRep::Integer(merged_item.item_id),
                            arg1: PyRep::Integer(merged_item.location_id),
                            arg2: PyRep::Integer(merged_item.flag as i64),
                            arg3: None,
                        },
                    );
                    if let Some(dst_owner) = dst_ship_bay_foreign_owner {
                        ctx.emit_typed(
                            NotificationTarget::Character(dst_owner),
                            ProcessInventoryChange {
                                arg0: PyRep::Integer(merged_item.item_id),
                                arg1: PyRep::Integer(merged_item.location_id),
                                arg2: PyRep::Integer(merged_item.flag as i64),
                                arg3: None,
                            },
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(merged_item.item_id)), ("locationID", PyRep::Integer(merged_item.location_id)),
                        ("flagID", PyRep::Integer(merged_item.flag as i64)), ("quantity", PyRep::Integer(merged_item.quantity as i64)),
                        ("version", PyRep::Integer(merged_item.version as i64)), ("suspectFlagged", PyRep::Bool(looted_foreign_jetcan)),
                    ])))
                }
                Ok(AddTxOutcome::Moved {
                    updated,
                    effective_item_id,
                    orig_qty,
                    old_loc_id,
                    old_flag_id,
                    old_owner_id,
                    old_was_fitted,
                    st_fuel,
                    dst_corp_owner,
                    src_ship_bay_owner,
                    dst_ship_bay_foreign_owner,
                    recursive_owner_changes,
                }) => {
                    item_id = effective_item_id;
                    if updated.owner_id != old_owner_id {
                        crate::services::quasar_cosmetics_live::strip_ship_cosmetics_for_ships(&ctx.db, &[updated.item_id]);
                    }
                    if dst_corp_owner > 0
                        || (116..=122).contains(&flag_u16)
                        || flag == InventoryFlag::CorpDeliveries
                        || updated.owner_id != old_owner_id && updated.owner_id != char_id
                    {
                        let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::void_ship_insurance_contract(ctx, updated.item_id);
                    }
                    if flag_u16 == 180 || flag == InventoryFlag::StructureDeedBay {
                        crate::services::station_structure::structure_lifecycle::sync_all_structures(ctx);
                    }
                    if looted_from_space {
                        crate::services::notify_emitters_character_services::emit_fleet_loot_event_if_in_fleet(
                            ctx,
                            char_id,
                            session.fleet_id.or_else(|| crate::services::notify_emitters_seasons_tactical::char_fleet_id(ctx, char_id)),
                            &[(updated.type_id, updated.quantity)],
                        );
                        let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::despawn_empty_jetcan_if_needed(
                            ctx,
                            session.solar_system_id,
                            orig_source_loc_id,
                        )
                        .await;
                    }
                    if pending_alsc_take.is_none() && orig_source_loc_id != new_loc && orig_source_loc_id > 0 {
                        crate::services::pve_dungeon_runtime::on_dungeon_item_looted(
                            ctx,
                            char_id,
                            session.solar_system_id,
                            orig_source_loc_id,
                            updated.type_id,
                        )
                        .await;
                        if orig_source_loc_id != updated.location_id {
                            crate::services::pve_homefront_sites::on_homefront_item_moved_sync(
                                ctx,
                                char_id,
                                updated.location_id,
                                updated.type_id,
                                u64::try_from(updated.quantity).unwrap_or(0),
                            );
                        }
                    }
                    if looted_foreign_jetcan {
                        super::inventory_stacking::apply_space_loot_suspect_flag(ctx, session, char_id);
                    }
                    let affects_fitting = is_fitting_slot || old_was_fitted;
                    if let Some(ship_id) = session.ship_id {
                        if affects_fitting {
                            ctx.invalidate_ship_fitting(ship_id);
                        } else {
                            ctx.invalidate_ship_fitting_cache_only(ship_id);
                        }
                    }
                    crate::services::operations_manager_helpers::record_invbroker_add_operation_event(ctx, char_id, orig_source_loc_id, new_loc, flag, updated.type_id, updated.quantity);
                    if affects_fitting {
                        ctx.invalidate_ship_fitting(new_loc);
                    } else {
                        ctx.invalidate_ship_fitting_cache_only(new_loc);
                    }
                    let build_rep = |suspect| PyRep::keyval([
                        ("itemID", PyRep::Integer(updated.item_id)), ("locationID", PyRep::Integer(updated.location_id)),
                        ("flagID", PyRep::Integer(updated.flag as i64)), ("quantity", PyRep::Integer(updated.quantity as i64)),
                        ("version", PyRep::Integer(updated.version as i64)), ("suspectFlagged", PyRep::Bool(suspect)),
                        ("fuelBlocks", st_fuel.map(|f| PyRep::Integer(i64::from(f))).unwrap_or(PyRep::None)),
                    ]);
                    if did_split.is_none() && !looted_foreign_jetcan
                        && updated.item_id == effective_item_id && updated.quantity == orig_qty
                        && updated.location_id == old_loc_id && (updated.flag as i64) == old_flag_id
                        && updated.owner_id == old_owner_id && was_singleton_before_move == updated.singleton
                        && recursive_owner_changes.is_empty() && !is_fitting_slot && !old_was_fitted
                    {
                        return Some(Ok(build_rep(false)));
                    }
                    ctx.emit_typed(
                        NotificationTarget::Character(char_id),
                        ProcessInventoryChange {
                            arg0: PyRep::Integer(updated.item_id), arg1: PyRep::Integer(updated.location_id),
                            arg2: PyRep::Integer(updated.flag as i64), arg3: None,
                        },
                    );
                    let extra_ship_owners: Vec<i64> = if did_split.is_some() || updated.item_id != effective_item_id {
                        dst_ship_bay_foreign_owner.into_iter().collect()
                    } else {
                        [dst_ship_bay_foreign_owner, src_ship_bay_owner].into_iter().flatten().collect()
                    };
                    for (idx, &extra_owner) in extra_ship_owners.iter().enumerate() {
                        if extra_owner != char_id && !extra_ship_owners[..idx].contains(&extra_owner) {
                            ctx.emit_typed(
                                NotificationTarget::Character(extra_owner),
                                ProcessInventoryChange {
                                    arg0: PyRep::Integer(updated.item_id), arg1: PyRep::Integer(updated.location_id),
                                    arg2: PyRep::Integer(updated.flag as i64), arg3: None,
                                },
                            );
                        }
                    }
                    if is_fitting_slot {
                        if old_was_fitted && old_loc_id != new_loc {
                            ctx.invalidate_ship_fitting(old_loc_id);
                            emit_ship_fitting_attr_changes_for_ship(
                                ctx, char_id, old_loc_id, old_src_ship_attrs.as_ref(), false,
                            );
                        }
                        ctx.invalidate_ship_fitting(new_loc);
                        let new_fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, new_loc).ok();
                        let group_id = ctx
                            .sde
                            .types
                            .get(&updated.type_id)
                            .map(|t| i64::from(t.group_id))
                            .unwrap_or(0);
                        ctx.emit_typed(
                            NotificationTarget::Character(char_id),
                            OnModuleFitted {
                                char_id,
                                flag: flag as i64,
                                type_id: updated.type_id as i64,
                                group_id,
                            },
                        );
                        let _ = ctx.notify_module_online_state(char_id, item_id, online);
                        if let Some(ref new_fit) = new_fit_opt {
                            emit_ship_fitting_attr_changes_from_fit(
                                ctx, char_id, new_loc, old_ship_attrs.as_ref(), new_fit, false,
                            );
                        }
                    } else if old_was_fitted {
                        ctx.invalidate_ship_fitting(old_loc_id);
                        let new_fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, old_loc_id).ok();
                        let _ = ctx.notify_module_online_state(char_id, item_id, false);
                        let eff_id = ctx
                            .effect_id_for_module(updated.type_id, Some("online"))
                            .unwrap_or(16) as i32;
                        let _ = ctx.notify_godma_ship_effect(
                            crate::router::NotificationTarget::Character(char_id),
                            item_id,
                            eff_id,
                            false,
                            0,
                            0,
                        );
                        if let Some(ref new_fit) = new_fit_opt {
                            emit_ship_fitting_attr_changes_from_fit(
                                ctx, char_id, old_loc_id, old_ship_attrs.as_ref(), new_fit, false,
                            );
                        }
                    } else {
                        ctx.invalidate_ship_fitting(new_loc);
                    }

                    Some(Ok(build_rep(looted_foreign_jetcan)))
                }
                Err(e) => Some(Err(e)),
            }
}

pub(crate) fn ship_hold_capacity_error_key(flag_u16: u16) -> eve_proto::UserErrorKey {
    match flag_u16 {
        87 => eve_proto::UserErrorKey::NotEnoughDroneBaySpace,
        158 => eve_proto::UserErrorKey::NotEnoughFighterBaySpace,
        90 | 133..=151 | 155 | 177 | 179 | 181 | 182 | 185 => {
            eve_proto::UserErrorKey::NotEnoughSpecialBaySpace
        }
        _ => eve_proto::UserErrorKey::NotEnoughCargoSpace,
    }
}

#[cfg(test)]
pub(crate) fn ship_hold_capacity_error_code(flag_u16: u16) -> &'static str {
    ship_hold_capacity_error_key(flag_u16).as_str()
}

pub(crate) fn ship_hold_capacity_error(
    flag_u16: u16,
    hold_capacity: f64,
    used_vol: f64,
    required_vol: f64,
) -> BridgeRpcError {
    let available_vol = (hold_capacity - used_vol).max(0.0);
    BridgeRpcError::hold_capacity_error(
        ship_hold_capacity_error_key(flag_u16),
        required_vol,
        available_vol,
    )
}

pub use super::fitting_saved_apply::{dna_to_fitting, fitting_to_dna, saved_fitting_to_dna};

#[cfg(test)]
#[path = "inventory_fitting_tests.rs"]
mod inventory_fitting_tests;

