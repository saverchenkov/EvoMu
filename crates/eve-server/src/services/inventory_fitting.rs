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

#[path = "fitting_dogma_im.rs"]
pub mod fitting_dogma_im;
pub(crate) use fitting_dogma_im::*;

#[path = "fitting_ewar_capital.rs"]
pub mod fitting_ewar_capital;
pub(crate) use fitting_ewar_capital::*;

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

#[path = "fitting_ships_mastery.rs"]
pub mod fitting_ships_mastery;
pub(crate) use fitting_ships_mastery::*;

#[path = "fitting_scanners_superweapons.rs"]
pub mod fitting_scanners_superweapons;
pub(crate) use fitting_scanners_superweapons::*;

#[path = "fitting_cyno_portal.rs"]
pub mod fitting_cyno_portal;
pub(crate) use fitting_cyno_portal::*;

#[path = "fitting_ship_bays.rs"]
pub mod fitting_ship_bays;
pub(crate) use fitting_ship_bays::*;

pub(crate) use super::inventory_bound::{get_effective_corp_roles, get_effective_corp_roles_at_location, item_to_packed_cells};

/// Mechanic roll in `[0, 1)` from the context RNG (seeded test RNG); `0.5` if the OS CSPRNG is unreadable.
pub(crate) fn csprng_unit_f64(ctx: &ServerContext) -> f64 {
    ctx.mechanic_rng.try_unit_f64().unwrap_or(0.5)
}

fn check_fitting_dock(ctx: &ServerContext, session: &SessionState, char_id: i64) -> Result<(), BridgeRpcError> {
    let dock_id = session.station_id.or(session.structure_id).or_else(|| {
        ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id)))
    });
    if let Some(did) = dock_id {
        if did < 1_000_000_000_000 && !crate::services::station_structure::npc_station_offers_service(&ctx.sde, did, 17) {
            return Err(BridgeRpcError {
                code: "StationServiceUnavailable".into(),
                message: format!("Station {did} does not offer fitting facilities"),
            });
        }
    }
    Ok(())
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
            if ((60_000_000..=69_999_999).contains(&bound_loc_id)
                || bound_loc_id >= 1_000_000_000_000)
                && session.station_id != Some(bound_loc_id)
                && session.structure_id != Some(bound_loc_id)
            {
                return Some(Err(BridgeRpcError {
                    code: "BoundObjectOwnershipViolation".into(),
                    message: format!(
                        "Character {char_id} is not docked at bound inventory location {bound_loc_id}"
                    ),
                }));
            }
            let list_flag_filter = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()))
                .filter(|&f| f > 0);
            if let Some(flag_val) = list_flag_filter {
                if (116..=122).contains(&flag_val) {
                    let corp_id = session.corporation_id.or_else(|| {
                        ctx.db.with_snapshot(|snap| {
                            snap.characters.get(&char_id).map(|c| c.corporation_id)
                        })
                    }).unwrap_or(0);
                    let (is_ceo, roles) =
                        get_effective_corp_roles_at_location(ctx, corp_id, char_id, Some(bound_loc_id));
                    let div_idx = (flag_val - 116) as u32;
                    let req_query_role = 1u64 << (20 + div_idx);
                    if !is_ceo && !eve_db::corp_roles::has_role(roles, req_query_role) {
                        return Some(Err(BridgeRpcError {
                            code: "CorpRoleDenied".into(),
                            message: format!("Corporation role denied for division {} query", div_idx + 1),
                        }));
                    }
                }
            }
            let is_deployable_or_shipping_unit = ctx.db.with_snapshot(|snap| {
                if let Some(it) = snap.items.get(&bound_loc_id) {
                    let cat = ctx.sde.types.get(&it.type_id).map(|t| t.category_id).unwrap_or(0);
                    let grp = ctx.sde.types.get(&it.type_id).map(|t| t.group_id).unwrap_or(0);
                    let is_personal = cat == 22 || grp == 340;
                    let is_unit = cat == 22 || matches!(grp, 12 | 340 | 448 | 649);
                    let is_fleet_auth = !is_personal
                        && snap.fleets.values().any(|f| f.members.contains(&char_id) && f.members.contains(&it.owner_id))
                        && crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                            rt.authorized_shipping_storage.contains(&(bound_loc_id, char_id))
                        });
                    let is_auth = session.has_gm_role() || it.owner_id == char_id || it.owner_id == 0 || is_fleet_auth;
                    if !is_unit || !is_auth { return false; }
                    if it.owner_id != char_id && !session.has_gm_role() {
                        let sys_id = session.solar_system_id.unwrap_or(0);
                        if sys_id == 0 || it.location_id != sys_id as i64 { return false; }
                        let sys_handle = ctx.systems.get_or_spawn(sys_id);
                        let s_pos = sys_handle.get_ball_position_sync(session.ship_id.unwrap_or(0));
                        let u_pos = sys_handle.get_ball_position_sync(bound_loc_id);
                        s_pos.zip(u_pos).map(|(s, u)| s.distance(u) <= 5_000.0).unwrap_or(false)
                    } else { true }
                } else { false }
            });
            let rows = if is_deployable_or_shipping_unit {
                ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .filter(|i| {
                            i.location_id == bound_loc_id
                                && list_flag_filter.map_or(true, |f| (i.flag as i64) == f)
                        })
                        .map(|i| item_to_packed_cells(ctx, i))
                        .collect()
                })
            } else if let Some(flag_val) = list_flag_filter {
                if (116..=122).contains(&flag_val) {
                    let corp_id = session.corporation_id.or_else(|| {
                        ctx.db.with_snapshot(|snap| {
                            snap.characters.get(&char_id).map(|c| c.corporation_id)
                        })
                    }).unwrap_or(0);
                    ctx.db.with_snapshot(|snap| {
                        snap.items
                            .values()
                            .filter(|i| {
                                i.location_id == bound_loc_id
                                    && i.owner_id == corp_id
                                    && (i.flag as i64) == flag_val
                            })
                            .map(|i| item_to_packed_cells(ctx, i))
                            .collect()
                    })
                } else {
                    ctx.db
                        .get_items_for_character(char_id)
                        .into_iter()
                        .filter(|i| {
                            i.location_id == bound_loc_id
                                && (i.flag as i64) == flag_val
                        })
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
            Some(Ok(DBRowDescriptor::inv_item_header().build_crowset(rows)))
        }

        ("invbroker", "GetContainerContents") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(container_id) = args.first().and_then(|a| a.as_i64()).or_else(|| bound_entry.map(|b| b.object_id)) else {
                return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "GetContainerContents requires containerID".into() }));
            };
            let supplied_pw = kwargs.get("password").and_then(|v| v.as_str()).or_else(|| args.get(1).and_then(|a| a.as_str()));
            {
                let mut st = read_container_ship_state(ctx);
                if let Some(req_pw) = st.container_passwords.get(&container_id).cloned() {
                    if !req_pw.is_empty() && !st.unlocked_containers.contains(&(container_id, char_id)) {
                        if supplied_pw == Some(req_pw.as_str()) {
                            st.unlocked_containers.insert((container_id, char_id));
                        } else {
                            return Some(Err(BridgeRpcError { code: "ALSCPasswordRequired".into(), message: format!("Password required to open container {container_id}") }));
                        }
                    }
                }
                let is_alsc = ctx.db.with_snapshot(|snap| snap.items.get(&container_id).is_some_and(|it| ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 448)));
                if is_alsc {
                    let now_unix = ctx.now_unix_secs();
                    st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry {
                        timestamp_unix: now_unix, actor_id: char_id, action: "ALSCActionOpen".into(), item_id: container_id, item_type_id: 0, quantity: 0,
                    });
                }
                save_container_ship_state(ctx, &st);
            }
            Some(ctx.get_container_contents(session, bound_entry, char_id, container_id).await)
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
                if !matches!(rf, 0 | 4 | 5 | 11..=34 | 87 | 90 | 92..=94 | 116..=122 | 125..=151 | 155 | 164..=171 | 179 | 181 | 182 | 185) {
                    return Some(Err(BridgeRpcError { code: "InvalidInventoryFlag".into(), message: format!("Invalid or restricted inventory flag {rf}") }));
                }
            }
            let flag_u16 = raw_flag_opt.map(|v| v as u16).unwrap_or(InventoryFlag::Cargo as u16);
            let flag = match flag_u16 {
                90 => InventoryFlag::SpecializedShipHold,
                155 => InventoryFlag::Cargo,
                other => InventoryFlag::from_u16(other),
            };
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
            let mut source_item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&item_id).cloned());
            let split_qty_opt = requested_qty_opt.and_then(|q| {
                let q_i32 = i32::try_from(q).ok()?;
                source_item_opt.as_ref().and_then(|src| {
                    (!src.singleton && !src.is_blueprint_copy && q_i32 < src.quantity).then_some(q_i32)
                })
            });
            if let Err(e) = validate_container_and_bay_move(ctx, char_id, item_id, new_loc, flag_u16, kwargs, split_qty_opt.map(i64::from)) {
                return Some(Err(e));
            }
            if let Some(src_item) = &source_item_opt {
                let corp_id = session.corporation_id.or_else(|| {
                    ctx.db.with_snapshot(|snap| {
                        snap.characters.get(&char_id).map(|c| c.corporation_id)
                    })
                }).unwrap_or(0);
                if src_item.owner_id == corp_id {
                    let flag_val = src_item.flag as i64;
                    if (116..=122).contains(&flag_val) {
                        let (is_ceo, roles) = get_effective_corp_roles_at_location(
                            ctx,
                            corp_id,
                            char_id,
                            Some(src_item.location_id),
                        );
                        let div_idx = (flag_val - 116) as u32;
                        let req_take_role = 1u64 << (13 + div_idx);
                        if !is_ceo && !eve_db::corp_roles::has_role(roles, req_take_role) {
                            return Some(Err(BridgeRpcError {
                                code: "CorpRoleDenied".into(),
                                message: format!("Corporation role denied for division {} take", div_idx + 1),
                            }));
                        }
                    }
                }
            }
            let (looted_from_space, looted_foreign_jetcan) = source_item_opt.as_ref().map(|src_item| {
                ctx.db.with_snapshot(|snap| {
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
                            Some((true, cont.owner_id != char_id || src_item.owner_id != char_id))
                        } else {
                            None
                        }
                    })
                }).unwrap_or((false, false))
            }).unwrap_or((false, false));

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
                if let Err(e) = check_fitting_dock(ctx, session, char_id) {
                    return Some(Err(e));
                }
                if session.station_id.is_none() && session.structure_id.is_none() {
                    let now_unix = ctx.now_unix_secs();
                    let has_weapons_timer = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner()).states.get(&char_id).is_some_and(|cw| cw.is_weapon_timer_active(now_unix));
                    if has_weapons_timer {
                        return Some(Err(BridgeRpcError { code: "CannotRefitWithWeaponsTimer".into(), message: "Cannot refit or unfit modules in space while weapons timer is active".into() }));
                    }
                    let mut near_fitting_service = false;
                    if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                        near_fitting_service = is_near_in_space_fitting_service(ctx, sys_id, ship_id).await;
                    }
                    if !near_fitting_service {
                        return Some(Err(BridgeRpcError { code: "CannotUnfitInSpace".into(), message: "Unfitting modules requires being docked at a station/structure or within range of a Mobile Depot or capital SMA".into() }));
                    }
                }
                if let Some(src_item) = &source_item_opt {
                    if matches!(src_item.flag as u16, 125..=129) {
                        if let Ok(mut check_fit) = ctx.get_or_rebuild_ship_fitting(char_id, src_item.location_id) {
                            if let Err(msg) = check_fit.unfit_module(item_id, &ctx.sde) {
                                return Some(Err(BridgeRpcError {
                                    code: "SubsystemHasFittedModules".into(),
                                    message: msg,
                                }));
                            }
                        }
                    }
                    if let Some(sys_id) = session.solar_system_id {
                        let _ = ctx.systems.get_or_spawn(sys_id).remove_module_cycle(src_item.location_id, item_id).await;
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
                    let _ = ctx.get_or_rebuild_ship_fitting(char_id, old_loc);
                    // Rigs are destroyed on removal; the V24 client derives `OnModuleUnfitted`
                    // from this `OnItemChange` (`ixFlag` left a fitting flag).
                    let _ = ctx.notify_fitted_item_trashed(
                        char_id,
                        item_id,
                        src_item.type_id as i64,
                        src_item.quantity as i64,
                        old_loc,
                        src_item.flag as i64,
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
                        ctx, session, item_id, new_loc, flag, online,
                    ),
                );
            }

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
                if let Err(e) = check_fitting_dock(ctx, session, char_id) {
                    return Some(Err(e));
                }
                if session.station_id.is_none() && session.structure_id.is_none() {
                    let now_unix = ctx.now_unix_secs();
                    let has_weapons_timer = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner()).states.get(&char_id).is_some_and(|cw| cw.is_weapon_timer_active(now_unix));
                    if has_weapons_timer {
                        return Some(Err(BridgeRpcError { code: "CannotRefitWithWeaponsTimer".into(), message: "Cannot fit modules in space while weapons timer is active".into() }));
                    }
                    let mut near_fitting_service = false;
                    if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                        near_fitting_service = is_near_in_space_fitting_service(ctx, sys_id, ship_id).await;
                    }
                    if !near_fitting_service {
                        return Some(Err(BridgeRpcError { code: "CannotFitInSpace".into(), message: "Fitting modules into ship slots requires being docked at a station or structure or within range of a Mobile Depot or capital SMA".into() }));
                    }
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
                    if item.flag == InventoryFlag::Locked
                        || snap.is_item_locked_in_contract_or_job(item_id)
                    {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!(
                                "Item {item_id} is locked by an active contract, industry job, or container"
                            ),
                        });
                    }
                    let eff_qty = split_qty_opt.unwrap_or(item.quantity);
                    if eff_qty != 1 {
                        return Err(BridgeRpcError {
                            code: "CannotFitStackedModule".into(),
                            message: format!(
                                "Item {item_id} has quantity {eff_qty} and must be split to 1 before fitting"
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
                            });
                        }
                        if !resolves_to_loc(item.location_id, ch_dock) {
                            return Err(BridgeRpcError {
                                code: "ItemOwnershipViolation".into(),
                                message: format!(
                                    "Item {item_id} is not located at current dock {ch_dock}"
                                ),
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
                            });
                        }
                        if !resolves_to_loc(item.location_id, ch_sys) {
                            return Err(BridgeRpcError {
                                code: "ItemOwnershipViolation".into(),
                                message: format!(
                                    "Item {item_id} is not in current system {ch_sys}"
                                ),
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

            // Verify source and destination ownership/access before stack merge or move
            let mut pending_alsc_take: Option<(i64, fitting_containers_ships::AlscLogEntry)> = None;
            let auth_move_ok: Result<(), BridgeRpcError> = ctx.db.with_snapshot(|snap| {
                let Some(moving) = snap.items.get(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Item {item_id} not found"),
                    });
                };
                let (corp_id, char_sys, char_dock, active_ship_id) = snap
                    .characters
                    .get(&char_id)
                    .map(|c| {
                        (
                            c.corporation_id,
                            c.solar_system_id as i64,
                            c.station_id.or(c.structure_id).unwrap_or(0),
                            c.active_ship_id,
                        )
                    })
                    .unwrap_or((0, 0, 0, 0));
                if item_id == new_loc
                    || (active_ship_id > 0
                        && item_id == active_ship_id
                        && snap.items.contains_key(&new_loc))
                {
                    return Err(BridgeRpcError {
                        code: "CannotMoveActiveShip".into(),
                        message: format!("Cannot move item {item_id} into {new_loc}"),
                    });
                }
                if flag == InventoryFlag::Locked
                    || moving.flag == InventoryFlag::Locked
                    || snap.is_item_locked_in_contract_or_job(item_id)
                {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!(
                            "Item {item_id} is locked by an active contract, industry job, or container"
                        ),
                    });
                }
                if let Some(exp_v) = expected_version {
                    if moving.version != exp_v {
                        return Err(BridgeRpcError {
                            code: "ItemVersionMismatch".into(),
                            message: format!(
                                "Item {item_id} version mismatch: expected {exp_v}, actual {}",
                                moving.version
                            ),
                        });
                    }
                }
                let is_courier_cargo = snap.item_custom_names.get(&item_id).is_some_and(|s| s.starts_with("courier_cargo:"));
                if is_courier_cargo && ((116..=122).contains(&flag_u16) || snap.items.get(&new_loc).is_some_and(|d| d.owner_id != char_id)) {
                    return Err(BridgeRpcError {
                        code: "CannotDeliverCourierCargo".into(),
                        message: "Courier cargo cannot be moved into corporation hangar divisions or foreign containers/ships".into(),
                    });
                }
                let ch_sys = if char_sys > 0 {
                    char_sys
                } else {
                    session.solar_system_id.map(|s| s as i64).unwrap_or(0)
                };
                let ch_dock = if session.station_id.is_none()
                    && session.structure_id.is_none()
                    && session.solar_system_id.is_some()
                {
                    0
                } else {
                    char_dock
                };
                let is_storage_auth_and_near = |storage_id: i64| -> bool {
                    if ch_sys <= 0 || ch_dock != 0 {
                        return false;
                    }
                    let authorized = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                        rt.authorized_shipping_storage.contains(&(storage_id, char_id))
                    });
                    if !authorized {
                        return false;
                    }
                    let (unit_owner, is_personal) = snap.items.get(&storage_id).map(|i| {
                        let cat = ctx.sde.types.get(&i.type_id).map(|t| t.category_id).unwrap_or(0);
                        let grp = ctx.sde.types.get(&i.type_id).map(|t| t.group_id).unwrap_or(0);
                        (i.owner_id, cat == 22 || grp == 340)
                    }).unwrap_or((0, false));
                    let live_ok = session.has_gm_role()
                        || unit_owner == char_id
                        || unit_owner == 0
                        || (!is_personal && snap.fleets.values().any(|f| f.members.contains(&char_id) && f.members.contains(&unit_owner)));
                    if !live_ok {
                        return false;
                    }
                    let sys_handle = ctx.systems.get_or_spawn(ch_sys as u32);
                    let ship_id = session.ship_id.unwrap_or(0);
                    let ship_pos = sys_handle.get_ball_position_sync(ship_id);
                    let unit_pos = sys_handle.get_ball_position_sync(storage_id);
                    ship_pos.zip(unit_pos).map(|(s, u)| s.distance(u) <= 5_000.0).unwrap_or(false)
                };
                let is_ship_near_or_docked = |ship_id_target: i64| -> bool {
                    let Some(s_item) = snap.items.get(&ship_id_target) else { return false; };
                    if ch_dock > 0 {
                        s_item.location_id == ch_dock
                    } else if ch_sys > 0 {
                        if s_item.location_id != ch_sys {
                            return false;
                        }
                        let sys_handle = ctx.systems.get_or_spawn(ch_sys as u32);
                        let caller_ship_id = session.ship_id.unwrap_or(0);
                        let caller_pos = sys_handle.get_ball_position_sync(caller_ship_id);
                        let target_pos = sys_handle.get_ball_position_sync(ship_id_target);
                        caller_pos.zip(target_pos).map(|(s, t)| s.distance(t) <= 5_000.0).unwrap_or(false)
                    } else {
                        false
                    }
                };
                let is_loc_colocated = |start_loc: i64| -> bool {
                    let mut cur = start_loc;
                    for _ in 0..4 {
                        if ch_dock > 0 {
                            if cur == ch_dock {
                                return true;
                            }
                            let Some(node) = snap.items.get(&cur) else {
                                return false;
                            };
                            cur = node.location_id;
                        } else if ch_sys > 0 {
                            if cur == ch_sys {
                                return true;
                            }
                            let Some(node) = snap.items.get(&cur) else {
                                return false;
                            };
                            if node.location_id == ch_sys {
                                let caller_ship_id = session
                                    .ship_id
                                    .filter(|&id| id > 0)
                                    .unwrap_or(active_ship_id);
                                if cur == active_ship_id || cur == caller_ship_id {
                                    return true;
                                }
                                let sys_handle = ctx.systems.get_or_spawn(ch_sys as u32);
                                let caller_pos = sys_handle
                                    .get_ball_position_sync(caller_ship_id)
                                    .or_else(|| {
                                        snap.item_positions
                                            .get(&caller_ship_id)
                                            .copied()
                                            .map(glam::DVec3::from)
                                    });
                                let target_pos = sys_handle
                                    .get_ball_position_sync(cur)
                                    .or_else(|| {
                                        snap.item_positions
                                            .get(&cur)
                                            .copied()
                                            .map(glam::DVec3::from)
                                    });
                                return match (caller_pos, target_pos) {
                                    (Some(s), Some(t)) => s.distance(t) <= 5_000.0,
                                    _ => true,
                                };
                            }
                            cur = node.location_id;
                        } else {
                            return false;
                        }
                    }
                    false
                };
                let src_ok = if moving.owner_id == char_id || (corp_id > 0 && moving.owner_id == corp_id) {
                    if let Some(parent) = snap.items.get(&moving.location_id) {
                        let p_cat = ctx.sde.types.get(&parent.type_id).map(|t| t.category_id).unwrap_or(0);
                        let p_grp = ctx.sde.types.get(&parent.type_id).map(|t| t.group_id).unwrap_or(0);
                        let is_foreign_unit = (p_cat == 22 || matches!(p_grp, 12 | 340 | 448 | 649)) && parent.owner_id != char_id && parent.owner_id != 0;
                        let is_foreign_ship = p_cat == 6 && parent.owner_id != char_id;
                        if is_foreign_unit {
                            is_storage_auth_and_near(parent.item_id)
                        } else if is_foreign_ship {
                            let moving_flag = moving.flag as u16;
                            let is_shared_bay = matches!(moving_flag, 90 | 138 | 155);
                            is_shared_bay && is_ship_near_or_docked(parent.item_id)
                        } else {
                            is_loc_colocated(moving.location_id)
                        }
                    } else {
                        is_loc_colocated(moving.location_id)
                    }
                } else if let Some(parent) = snap.items.get(&moving.location_id) {
                    let p_grp = ctx.sde.types.get(&parent.type_id).map(|t| t.group_id).unwrap_or(0);
                    let p_cat = ctx.sde.types.get(&parent.type_id).map(|t| t.category_id).unwrap_or(0);
                    if is_storage_auth_and_near(parent.item_id) {
                        if parent.owner_id != char_id && !session.has_gm_role() {
                            let st = fitting_containers_ships::read_container_ship_state_snap(snap);
                            if let Some(req_pw) = st.container_passwords.get(&parent.item_id).cloned() {
                                if !req_pw.is_empty() && !st.unlocked_containers.contains(&(parent.item_id, char_id)) {
                                    let supplied_pw = kwargs.get("password").and_then(|v| v.as_str()).or_else(|| args.iter().find_map(|a| a.as_str()));
                                    if supplied_pw != Some(req_pw.as_str()) {
                                        return Err(BridgeRpcError { code: "ALSCPasswordRequired".into(), message: format!("Password required to take items from container {}", parent.item_id) });
                                    }
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
                    } else if fitting_containers_ships::is_container_group(p_grp, parent.type_id) || p_grp == 186 {
                        let is_public_or_own = parent.owner_id == char_id
                            || parent.owner_id == 0
                            || parent.type_id == 23
                            || p_grp == 186
                            || (corp_id > 0 && parent.owner_id == corp_id);
                        if is_public_or_own {
                            is_loc_colocated(parent.item_id)
                        } else {
                            is_storage_auth_and_near(parent.item_id)
                        }
                    } else if p_cat == 6 {
                        let moving_flag = moving.flag as u16;
                        let is_sma = matches!(moving_flag, 90 | 138);
                        let is_shared = is_sma || moving_flag == 155;
                        if !is_shared || !is_ship_near_or_docked(parent.item_id) {
                            false
                        } else {
                            let supplied_pw = kwargs.get("password").and_then(|v| v.as_str()).or_else(|| args.iter().find_map(|a| a.as_str()));
                            fitting_containers_ships::can_character_access_ship_bay_with_password_snap(
                                snap,
                                char_id,
                                parent.item_id,
                                parent.owner_id,
                                is_sma,
                                supplied_pw,
                            )
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !src_ok {
                    return Err(BridgeRpcError { code: "ItemOwnershipViolation".into(), message: format!("Character {char_id} is not authorized to move item {item_id}") });
                };

                let dst_ok = if let Some(dest) = snap.items.get(&new_loc) {
                    let d_grp = ctx.sde.types.get(&dest.type_id).map(|t| t.group_id).unwrap_or(0);
                    let d_cat = ctx.sde.types.get(&dest.type_id).map(|t| t.category_id).unwrap_or(0);
                    if dest.owner_id == char_id || (corp_id > 0 && dest.owner_id == corp_id) {
                        is_loc_colocated(new_loc)
                    } else if is_storage_auth_and_near(dest.item_id) {
                        if dest.owner_id != char_id && !session.has_gm_role() {
                            let st = fitting_containers_ships::read_container_ship_state_snap(snap);
                            if let Some(req_pw) = st.container_passwords.get(&dest.item_id).cloned() {
                                if !req_pw.is_empty() && !st.unlocked_containers.contains(&(dest.item_id, char_id)) {
                                    let supplied_pw = kwargs.get("password").and_then(|v| v.as_str()).or_else(|| args.iter().find_map(|a| a.as_str()));
                                    if supplied_pw != Some(req_pw.as_str()) {
                                        return Err(BridgeRpcError { code: "ALSCPasswordRequired".into(), message: format!("Password required to move items into container {}", dest.item_id) });
                                    }
                                }
                            }
                        }
                        true
                    } else if fitting_containers_ships::is_container_group(d_grp, dest.type_id) || d_grp == 186 {
                        let is_public_or_own = dest.owner_id == char_id
                            || dest.owner_id == 0
                            || dest.type_id == 23
                            || (corp_id > 0 && dest.owner_id == corp_id);
                        if is_public_or_own {
                            is_loc_colocated(new_loc)
                        } else {
                            is_storage_auth_and_near(dest.item_id)
                        }
                    } else if d_cat == 6 && matches!(flag_u16, 90 | 138 | 155) {
                        if !is_ship_near_or_docked(dest.item_id) {
                            false
                        } else {
                            let supplied_pw = kwargs.get("password").and_then(|v| v.as_str()).or_else(|| args.iter().find_map(|a| a.as_str()));
                            fitting_containers_ships::can_character_access_ship_bay_with_password_snap(
                                snap,
                                char_id,
                                new_loc,
                                dest.owner_id,
                                matches!(flag_u16, 90 | 138),
                                supplied_pw,
                            )
                        }
                    } else {
                        false
                    }
                } else if (60_000_000..=69_999_999).contains(&new_loc) {
                    ch_dock == new_loc
                } else if new_loc >= 1_000_000_000_000 {
                    let is_fuel_flag = matches!(flag_u16, 133) || flag == InventoryFlag::SpecializedFuelBay;
                    if is_fuel_flag {
                        let m_grp = ctx.sde.types.get(&moving.type_id).map(|t| t.group_id).unwrap_or(0);
                        if !matches!(m_grp, 423 | 497 | 1136 | 4729) {
                            return Err(BridgeRpcError {
                                code: "InvalidInventoryFlag".into(),
                                message: format!("Item {} is not fuel and cannot be loaded into structure fuel bay", moving.type_id),
                            });
                        }
                    }
                    if ch_dock == new_loc {
                        true
                    } else if !is_fuel_flag {
                        false
                    } else if let Some(st) = snap.structures.get(&new_loc) {
                        if st.solar_system_id as i64 != ch_sys {
                            false
                        } else {
                            let sys_h = ctx.systems.get_or_spawn(ch_sys as u32);
                            let s_pos = sys_h.get_ball_position_sync(session.ship_id.unwrap_or(0));
                            let u_pos = sys_h.get_ball_position_sync(new_loc);
                            s_pos.zip(u_pos).map(|(s, u)| s.distance(u) <= 5_000.0).unwrap_or(false)
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !dst_ok {
                    return Err(BridgeRpcError { code: "DestinationAccessDenied".into(), message: format!("Character {char_id} is not authorized to move items into {new_loc}") });
                }
                Ok(())
            });
            if let Err(e) = auth_move_ok {
                return Some(Err(e));
            }

            let dest_is_ship_hold = matches!(flag_u16, 5 | 87 | 158 | 133..=151 | 179 | 181 | 182 | 185);
            if dest_is_ship_hold {
                let eval_cargo_cap = (flag_u16 == 5).then(|| {
                    ctx.get_or_rebuild_ship_fitting(char_id, new_loc).ok()
                        .map(|f| f.evaluated_context(&ctx.sde).ship.get_attribute(38, &ctx.sde))
                        .filter(|&c| c > 0.0)
                }).flatten();
                let cap_check = ctx.db.with_snapshot(|snap| {
                    let Some(moving_item) = snap.items.get(&item_id) else { return Ok(()); };
                    let Some(ship_item) = snap.items.get(&new_loc) else { return Ok(()); };
                    let Some(ship_type) = ctx.sde.types.get(&ship_item.type_id) else { return Ok(()); };
                    if ship_type.category_id != 6 { return Ok(()); }
                    let Some(item_type) = ctx.sde.types.get(&moving_item.type_id) else { return Ok(()); };
                    if !crate::services::fitting_saved_apply::is_item_allowed_in_ship_hold(flag_u16, item_type, ship_type) {
                        return Err(BridgeRpcError::user_error("InvalidItemTypeForHold"));
                    }
                    let hold_capacity = crate::services::fitting_saved_apply::get_ship_hold_capacity(flag_u16, ship_type, eval_cargo_cap);
                    if flag_u16 != 5 && hold_capacity <= 0.0 {
                        return Err(BridgeRpcError { code: "CapacityExceeded".into(), message: format!("Ship {} does not have a specialized hold for flag {flag_u16} (capacity 0 m³)", ship_type.name) });
                    }
                    if hold_capacity > 0.0 {
                        let used_vol: f64 = snap.items.values()
                            .filter(|i| i.location_id == new_loc && (i.flag as u16) == flag_u16 && i.item_id != item_id)
                            .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64)).sum();
                        let eff_qty = split_qty_opt.unwrap_or(moving_item.quantity);
                        let item_vol = effective_item_unit_volume(&ctx.sde, moving_item) * (eff_qty as f64);
                        if used_vol + item_vol > hold_capacity + 1e-4 {
                            let err_code = if looted_from_space { "NotEnoughCargoSpace" } else { "CapacityExceeded" };
                            return Err(BridgeRpcError { code: err_code.into(), message: format!("Hold capacity of {hold_capacity}m³ exceeded") });
                        }
                    }
                    Ok(())
                });
                if let Err(e) = cap_check { return Some(Err(e)); }
            }

            let mut did_split: Option<(i64, i32)> = None;
            if looted_from_space {
                let active_ship = session.ship_id.unwrap_or(new_loc);
                if let Some(split_qty) = split_qty_opt {
                    let orig_id = item_id;
                    let split_res = ctx.db.try_with_mut_snapshot(|snap| {
                        let orig = snap.items.get_mut(&item_id).ok_or_else(|| format!("Item {item_id} not found"))?;
                        if orig.singleton || split_qty >= orig.quantity { return Err("Cannot split".to_string()); }
                        orig.quantity -= split_qty;
                        orig.version += 1;
                        snap.next_entity_id += 1;
                        let new_id = snap.next_entity_id;
                        let split_rec = eve_db::InventoryItemRecord {
                            item_id: new_id, type_id: orig.type_id, owner_id: char_id, location_id: active_ship,
                            flag, quantity: split_qty, singleton: false, version: 1, ..Default::default()
                        };
                        snap.items.insert(new_id, split_rec.clone());
                        Ok(split_rec)
                    });
                    match split_res {
                        Ok(split_item) => { did_split = Some((orig_id, split_qty)); item_id = split_item.item_id; source_item_opt = Some(split_item); }
                        Err(e) => return Some(Err(BridgeRpcError { code: "SplitFailed".into(), message: e })),
                    }
                } else {
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(it) = snap.items.get_mut(&item_id) { it.owner_id = char_id; it.location_id = active_ship; it.flag = flag; it.version += 1; }
                    });
                    if let Some(src) = source_item_opt.as_mut() { src.owner_id = char_id; src.location_id = active_ship; src.flag = flag; }
                }
            } else if let Some(split_qty) = split_qty_opt {
                let orig_id = item_id;
                match ctx.db.split_stack(char_id, item_id, split_qty) {
                    Ok((_orig, split_item)) => { did_split = Some((orig_id, split_qty)); item_id = split_item.item_id; source_item_opt = Some(split_item); }
                    Err(e) => return Some(Err(BridgeRpcError { code: "SplitFailed".into(), message: e.to_string() })),
                }
            }

            if dest_is_ship_hold {
                // Stack merging if matching unstacked item exists at destination
                let merged_opt: Result<Option<eve_db::InventoryItemRecord>, eve_db::DbError> = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(moving) = snap.items.get(&item_id).cloned() else {
                        return Ok(None);
                    };
                    if moving.singleton
                        || moving.is_blueprint_copy
                        || !moving.dynamic_attributes.is_empty()
                        || moving.source_mutaplasmid_id.is_some()
                        || moving.flag == InventoryFlag::Locked
                        || snap.is_item_locked_in_contract_or_job(item_id)
                    {
                        return Ok(None);
                    }
                    let is_courier = |id: i64| snap.item_custom_names.get(&id).is_some_and(|s| s.starts_with("courier_cargo:"));
                    if is_courier(item_id) {
                        return Ok(None);
                    }
                    let existing_target = snap.items.values().find(|i| {
                        i.item_id != item_id
                            && i.location_id == new_loc
                            && i.flag == flag
                            && i.type_id == moving.type_id
                            && i.owner_id == moving.owner_id
                            && !i.singleton
                            && !i.is_blueprint_copy
                            && i.dynamic_attributes.is_empty()
                            && i.source_mutaplasmid_id.is_none()
                            && i.flag != InventoryFlag::Locked
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                            && !is_courier(i.item_id)
                    }).map(|i| i.item_id);
                    if let Some(target_iid) = existing_target {
                        let target_qty = snap
                            .items
                            .get(&target_iid)
                            .and_then(|t| t.quantity.checked_add(moving.quantity));
                        if let Some(new_qty) = target_qty {
                            snap.items.remove(&item_id);
                            let target_item = snap.items.get_mut(&target_iid).unwrap();
                            target_item.quantity = new_qty;
                            target_item.version += 1;
                            Ok(Some(target_item.clone()))
                        } else {
                            Ok(None)
                        }
                    } else {
                        Ok(None)
                    }
                });
                if let Ok(Some(merged_item)) = merged_opt {
                    let moved_tid = source_item_opt.as_ref().map(|i| i.type_id).unwrap_or(merged_item.type_id);
                    let moved_qty = source_item_opt.as_ref().map(|i| i.quantity).unwrap_or(1);
                    fitting_containers_ships::record_container_add_log(ctx, char_id, item_id, moved_tid, moved_qty, new_loc);
                    if let Some((cid, entry)) = pending_alsc_take {
                        fitting_containers_ships::write_container_ship_state(ctx, |st| {
                            st.alsc_logs.entry(cid).or_default().push(entry);
                        });
                    }
                    if looted_foreign_jetcan {
                        let now_unix = ctx.now_unix_secs();
                        let until = now_unix.saturating_add(900);
                        ctx.crimewatch
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .get_or_create(char_id)
                            .suspect_flag_until_unix = until;
                        if let Some(sys_id) = session.solar_system_id {
                            // V24 client derives `OnSuspectsAndCriminalsUpdate` from this.
                            let _ = ctx.notify_system_criminal_flag_updates(sys_id, &[], &[char_id], &[]);
                        }
                    }
                    if let Some(ship_id) = session.ship_id {
                        ctx.invalidate_ship_fitting(ship_id);
                    }
                    ctx.invalidate_ship_fitting(new_loc);
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnItemChange",
                        "charid",
                        vec![
                            PyRep::keyval([
                                ("itemID", PyRep::Integer(merged_item.item_id)), ("typeID", PyRep::Integer(merged_item.type_id as i64)),
                                ("locationID", PyRep::Integer(merged_item.location_id)), ("flagID", PyRep::Integer(merged_item.flag as i64)),
                                ("quantity", PyRep::Integer(merged_item.quantity as i64)),
                            ]),
                            PyRep::Dict(vec![(PyRep::Integer(5), PyRep::Integer(merged_item.quantity as i64))]),
                        ],
                    );
                    ctx.notify(crate::router::NotificationTarget::Character(char_id), "ProcessInventoryChange", "charid", vec![
                        PyRep::Integer(merged_item.item_id), PyRep::Integer(merged_item.location_id), PyRep::Integer(merged_item.flag as i64),
                    ]);
                    return Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(merged_item.item_id)), ("locationID", PyRep::Integer(merged_item.location_id)),
                        ("flagID", PyRep::Integer(merged_item.flag as i64)), ("quantity", PyRep::Integer(merged_item.quantity as i64)),
                        ("version", PyRep::Integer(merged_item.version as i64)), ("suspectFlagged", PyRep::Bool(looted_foreign_jetcan)),
                    ])));
                }
            }

            let old_loc_id = source_item_opt.as_ref().map(|i| i.location_id).unwrap_or(0);
            let old_flag_id = source_item_opt.as_ref().map(|i| i.flag as i64).unwrap_or(0);
            let move_res: Result<eve_db::InventoryItemRecord, String> = ctx
                .db
                .move_item_transactional(char_id, item_id, new_loc, flag, expected_version, idem_key)
                .map_err(|e| e.to_string())
                .or_else(|msg| {
                    if msg.contains("is not authorized") || msg.contains("is not owned by character") {
                        ctx.db.try_with_mut_snapshot(|snap| {
                            let it = snap.items.get_mut(&item_id).ok_or_else(|| format!("Item {item_id} not found"))?;
                            it.location_id = new_loc;
                            it.flag = flag;
                            if matches!(flag, InventoryFlag::Hangar | InventoryFlag::Cargo) {
                                it.owner_id = char_id;
                            }
                            it.version += 1;
                            Ok(it.clone())
                        })
                    } else {
                        Err(msg)
                    }
                });
            match move_res {
                Ok(updated) => {
                    fitting_containers_ships::record_container_add_log(
                        ctx,
                        char_id,
                        updated.item_id,
                        updated.type_id,
                        updated.quantity,
                        updated.location_id,
                    );
                    write_container_ship_state(ctx, |st| {
                        if let Some((cid, entry)) = pending_alsc_take {
                            st.alsc_logs.entry(cid).or_default().push(entry);
                        }
                        if matches!(flag_u16, 90 | 155) {
                            st.custom_item_flags.insert(item_id, flag_u16);
                        } else {
                            st.custom_item_flags.remove(&item_id);
                        }
                    });
                    if looted_foreign_jetcan {
                        let now_unix = ctx.now_unix_secs();
                        let until = now_unix.saturating_add(900);
                        ctx.crimewatch
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .get_or_create(char_id)
                            .suspect_flag_until_unix = until;
                        if let Some(sys_id) = session.solar_system_id {
                            // V24 client derives `OnSuspectsAndCriminalsUpdate` from this.
                            let _ = ctx.notify_system_criminal_flag_updates(sys_id, &[], &[char_id], &[]);
                        }
                    }
                    if let Some(ship_id) = session.ship_id {
                        ctx.invalidate_ship_fitting(ship_id);
                    }
                    ctx.invalidate_ship_fitting(new_loc);
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
                            PyRep::Dict(vec![
                                (PyRep::Integer(3), PyRep::Integer(old_loc_id)),
                                (PyRep::Integer(4), PyRep::Integer(old_flag_id)),
                            ]),
                        ],
                    );
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
                            ctx.invalidate_ship_fitting(old_loc_id);
                            let _ = ctx.get_or_rebuild_ship_fitting(char_id, old_loc_id);
                            // `OnModuleUnfitted` is derived client-side from the `OnItemChange`
                            // (ixLocationID/ixFlag change) already sent above for this move.
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
                Err(e) => {
                    if let Some((orig_id, sq)) = did_split {
                        ctx.db.with_mut_snapshot(|snap| {
                            snap.items.remove(&item_id);
                            if let Some(orig) = snap.items.get_mut(&orig_id) {
                                orig.quantity += sq;
                                orig.version = orig.version.saturating_sub(1);
                            }
                        });
                    }
                    Some(Err(BridgeRpcError {
                        code: "InventoryMoveFailed".into(),
                        message: e.to_string(),
                    }))
                }
            }
        }

        ("ship", "Jettison") | ("invbroker", "Jettison") => {
            Some(fitting_ship_bays::ship_jettison(ctx, session, args, kwargs).await)
        }

        ("ship", "Drop") => {
            Some(fitting_ship_bays::ship_drop(ctx, session, args, kwargs).await)
        }
        ("invbroker", "MultiAdd") => {
            Some(super::inventory_stacking::handle_multi_add(ctx, session, bound_entry, args, kwargs).await)
        }

        _ => {
            if let Some(res) = super::inventory_stacking::handle_stack_ops(ctx, session, bound_entry, service, method, args, kwargs) { return Some(res); }
            if let Some(res) = handle_dogma_im(ctx, session, bound_entry, service, method, args, kwargs).await { return Some(res); }
            if let Some(res) = handle_dogma_ops(ctx, session, bound_entry, service, method, args, kwargs).await { return Some(res); }
            if let Some(res) = handle_module_cycles(ctx, session, bound_entry, service, method, args, kwargs).await { return Some(res); }
            if let Some(res) = handle_mining_and_deactivate(ctx, session, bound_entry, service, method, args, kwargs).await { return Some(res); }
            if let Some(res) = handle_fitting_mgr(ctx, session, bound_entry, service, method, args, kwargs).await { return Some(res); }
            if let Some(res) = handle_ships_and_services(ctx, session, bound_entry, service, method, args, kwargs).await { return Some(res); }
            if let Some(res) = handle_containers_ships_and_fitting(ctx, session, bound_entry, service, method, args, kwargs).await { return Some(res); }
            if let Some(res) = handle_ships_mastery_and_fitting_ext(ctx, session, bound_entry, service, method, args, kwargs).await { return Some(res); }
            if let Some(res) = handle_superweapons_dbuffs_and_clones(ctx, session, bound_entry, service, method, args, kwargs).await { return Some(res); }
            if let Some(res) = handle_cyno_portal(ctx, session, service, method, args).await { return Some(res); }
            None
        }
    }
}

pub use super::fitting_saved_apply::{dna_to_fitting, fitting_to_dna, saved_fitting_to_dna};
