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

pub(crate) fn item_to_packed_cells(ctx: &ServerContext, i: &eve_db::InventoryItemRecord) -> Vec<PyRep> {
    let (group_id, category_id) = ctx
        .sde
        .types
        .get(&i.type_id)
        .map(|t| (t.group_id as i64, t.category_id as i64))
        .unwrap_or((0, 0));
    let qty = if i.is_blueprint_copy {
        -2
    } else if i.singleton {
        -1
    } else {
        i.quantity as i64
    };
    let custom_name = ctx
        .db
        .with_snapshot(|snap| snap.item_custom_names.get(&i.item_id).cloned())
        .unwrap_or_default();
    vec![
        PyRep::Integer(i.item_id),
        PyRep::Integer(i.type_id as i64),
        PyRep::Integer(i.owner_id),
        PyRep::Integer(i.location_id),
        PyRep::Integer(i.flag as i64),
        PyRep::Integer(qty),
        PyRep::Integer(group_id),
        PyRep::Integer(category_id),
        PyRep::String(custom_name),
    ]
}

/// Mechanic roll in `[0, 1)` from the context RNG (seeded test RNG); `0.5` if the OS CSPRNG is unreadable.
pub(crate) fn csprng_unit_f64(ctx: &ServerContext) -> f64 {
    ctx.mechanic_rng.try_unit_f64().unwrap_or(0.5)
}

pub(crate) fn get_effective_corp_roles(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
) -> (bool, u64) {
    get_effective_corp_roles_at_location(ctx, corp_id, char_id, None)
}

pub(crate) fn get_effective_corp_roles_at_location(
    ctx: &ServerContext,
    corp_id: i64,
    char_id: i64,
    location_id: Option<i64>,
) -> (bool, u64) {
    crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
    ctx.db.with_snapshot(|snap| {
        let (is_ceo, hq_station_id, mut role_mask) = snap
            .corporations
            .get(&corp_id)
            .map(|c| {
                (
                    c.ceo_id == char_id,
                    c.hq_station_id.unwrap_or(60_003_760),
                    c.member_roles.get(&char_id).copied().unwrap_or(0),
                )
            })
            .unwrap_or((false, 60_003_760, 0));
        let raw_loc = location_id
            .or_else(|| {
                snap.characters
                    .get(&char_id)
                    .and_then(|ch| ch.station_id.or(ch.structure_id))
            })
            .unwrap_or(0);
        let loc_station_id = snap
            .items
            .get(&raw_loc)
            .and_then(|it| {
                (it.location_id >= 60_000_000 || snap.structures.contains_key(&it.location_id))
                    .then_some(it.location_id)
            })
            .unwrap_or(raw_loc);
        let base_id = snap
            .corp_governance
            .get(&corp_id)
            .and_then(|g| g.member_bases.get(&char_id).copied())
            .unwrap_or(0);
        if let Some(ra) = snap.corp_role_assignments.get(&(corp_id, char_id)) {
            role_mask |= ra.roles;
            if loc_station_id > 0 {
                if loc_station_id == hq_station_id {
                    role_mask |= ra.roles_at_hq;
                }
                if base_id > 0 && loc_station_id == base_id {
                    role_mask |= ra.roles_at_base;
                }
                if loc_station_id != hq_station_id && (base_id <= 0 || loc_station_id != base_id) {
                    role_mask |= ra.roles_at_other;
                }
            }
        }
        (is_ceo, role_mask)
    })
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
                if !matches!(rf, 0 | 4 | 5 | 11..=34 | 87 | 90 | 92..=94 | 125..=151 | 155 | 164..=171 | 179 | 181 | 182 | 185) {
                    return Some(Err(BridgeRpcError { code: "InvalidInventoryFlag".into(), message: format!("Invalid or restricted inventory flag {rf}") }));
                }
            }
            let flag_u16 = raw_flag_opt.map(|v| v as u16).unwrap_or(InventoryFlag::Cargo as u16);
            let flag = match flag_u16 {
                90 => InventoryFlag::SpecializedShipHold,
                155 => InventoryFlag::Cargo,
                other => InventoryFlag::from_u16(other),
            };
            if let Err(e) = validate_container_and_bay_move(ctx, char_id, item_id, new_loc, flag_u16, kwargs) {
                return Some(Err(e));
            }
            let requested_qty_opt = kwargs
                .get("qty")
                .or_else(|| kwargs.get("quantity"))
                .and_then(|v| v.as_i64())
                .or_else(|| args.get(3).and_then(|a| a.as_i64()));
            let mut source_item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&item_id).cloned());
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
            let mut looted_foreign_jetcan = false;
            if let Some(src_item) = &source_item_opt {
                let space_container_opt = ctx.db.with_snapshot(|snap| {
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
                            Some(cont.owner_id != char_id || src_item.owner_id != char_id)
                        } else {
                            None
                        }
                    })
                });
                if let Some(is_foreign) = space_container_opt {
                    let active_ship = session.ship_id.unwrap_or(new_loc);
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some(it) = snap.items.get_mut(&item_id) {
                            it.owner_id = char_id;
                            it.location_id = active_ship;
                        }
                    });
                    if let Some(src) = source_item_opt.as_mut() {
                        src.owner_id = char_id;
                        src.location_id = active_ship;
                    }
                    looted_foreign_jetcan = is_foreign;
                }
            }
            if let (Some(req_qty), Some(src_item)) = (requested_qty_opt, source_item_opt.as_ref()) {
                if req_qty > 0
                    && !src_item.singleton
                    && !src_item.is_blueprint_copy
                    && (req_qty as i32) < src_item.quantity
                {
                    match ctx.db.split_stack(char_id, item_id, req_qty as i32) {
                        Ok((_orig, split_item)) => {
                            item_id = split_item.item_id;
                            source_item_opt = Some(split_item);
                        }
                        Err(e) => {
                            return Some(Err(BridgeRpcError {
                                code: "SplitFailed".into(),
                                message: e.to_string(),
                            }));
                        }
                    }
                }
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
                    if item.quantity != 1 {
                        return Err(BridgeRpcError {
                            code: "CannotFitStackedModule".into(),
                            message: format!(
                                "Item {item_id} has quantity {} and must be split to 1 before fitting",
                                item.quantity
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
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let ch_sys = snap.characters.get(&char_id).map(|c| c.solar_system_id as i64).unwrap_or(0);
                let ch_dock = snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id)).unwrap_or(0);
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
                            true
                        }
                    } else {
                        true
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
                            || (corp_id > 0 && parent.owner_id == corp_id);
                        if is_public_or_own {
                            parent.location_id == ch_sys || (ch_dock > 0 && parent.location_id == ch_dock)
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
                        true
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
                            dest.location_id == ch_sys || (ch_dock > 0 && dest.location_id == ch_dock)
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

            let dest_is_ship_hold = matches!(flag_u16, 5 | 133..=151 | 179 | 181 | 182 | 185);
            if dest_is_ship_hold {
                let eval_cargo_cap = if flag_u16 == 5 {
                    ctx.get_or_rebuild_ship_fitting(char_id, new_loc)
                        .ok()
                        .map(|f| f.evaluated_context(&ctx.sde).ship.get_attribute(38, &ctx.sde))
                        .filter(|&c| c > 0.0)
                } else {
                    None
                };
                let cap_check = ctx.db.with_snapshot(|snap| {
                    let Some(moving_item) = snap.items.get(&item_id) else { return Ok(()); };
                    let Some(ship_item) = snap.items.get(&new_loc) else { return Ok(()); };
                    let Some(ship_type) = ctx.sde.types.get(&ship_item.type_id) else { return Ok(()); };
                    if ship_type.category_id != 6 {
                        return Ok(());
                    }
                    let Some(item_type) = ctx.sde.types.get(&moving_item.type_id) else { return Ok(()); };

                    // Enforce specialized hold item-group / category restrictions (`feat:ships.special-holds`, `feat:mining.ore-mineral-holds`)
                    let allowed_in_hold = match flag_u16 {
                        5 => true,
                        133 => {
                            // SpecializedFuelBay: Fuel Blocks (group 1136), Ice Products (group 423), or Fuel (group 497)
                            matches!(item_type.group_id, 423 | 497 | 1136)
                        }
                        134 | 151 => {
                            // SpecializedOreHold / GeneralMiningHold: asteroid ores, moon ores, ice (cat 25), or gas (group 711 / 4168)
                            item_type.category_id == 25 || matches!(item_type.group_id, 711 | 4168)
                        }
                        135 => {
                            // SpecializedGasHold: harvestable/compressed gas (group 711 / 4168)
                            matches!(item_type.group_id, 711 | 4168)
                        }
                        136 => {
                            // SpecializedMineralHold: refined minerals (group 18)
                            item_type.group_id == 18
                        }
                        143 => {
                            // SpecializedAmmoHold: charges/ammo (category 8)
                            item_type.category_id == 8
                        }
                        148 => {
                            // SpecializedCommandCenterHold: Command Centers (group 1027)
                            item_type.group_id == 1027
                        }
                        149 => {
                            // SpecializedPlanetaryCommoditiesHold: planetary resources/commodities (cat 42 / 43)
                            matches!(item_type.category_id, 42 | 43)
                        }
                        179 => {
                            // SpecializedMobileDepotHold: Mobile Depots (group 1246)
                            item_type.group_id == 1246
                        }
                        181 => {
                            // SpecializedIceHold: raw/compressed ice (group 465)
                            item_type.group_id == 465
                        }
                        150 | 185 => {
                            // InfrastructureHold (`specialColonyResourcesHoldCapacity` attr 5646): Upwell/Starbase/Sov structures, upgrades, orbitals, PI, colony reagents, fuel
                            matches!(item_type.category_id, 23 | 39 | 40 | 41 | 42 | 43 | 46 | 65 | 66 | 2143)
                                || matches!(item_type.group_id, 423 | 1136 | 4729)
                        }
                        182 => {
                            if ship_type.default_attributes.get(&5646).copied().unwrap_or(0.0) > 0.0 {
                                matches!(item_type.category_id, 23 | 39 | 40 | 41 | 42 | 43 | 46 | 65 | 66 | 2143)
                                    || matches!(item_type.group_id, 423 | 1136 | 4729)
                            } else {
                                item_type.category_id == 25
                            }
                        }
                        _ => true,
                    };
                    if !allowed_in_hold {
                        return Err(BridgeRpcError {
                            code: "InvalidItemTypeForHold".into(),
                            message: format!("Item {} (type {}) cannot be placed in specialized hold flag {}", moving_item.item_id, item_type.type_id, flag_u16),
                        });
                    }

                    let hold_capacity = match flag_u16 {
                        133 => ship_type.default_attributes.get(&1549).copied().unwrap_or(0.0),
                        134 | 151 => ship_type.default_attributes.get(&1556).copied()
                            .or_else(|| ship_type.default_attributes.get(&1770).copied())
                            .unwrap_or(0.0),
                        136 => ship_type.default_attributes.get(&1558).copied().unwrap_or(0.0),
                        135 => ship_type.default_attributes.get(&1557).copied()
                            .or_else(|| ship_type.default_attributes.get(&1556).copied())
                            .unwrap_or(0.0),
                        179 => ship_type.default_attributes.get(&5325).copied().unwrap_or(0.0),
                        181 => ship_type.default_attributes.get(&3136).copied()
                            .or_else(|| ship_type.default_attributes.get(&1556).copied())
                            .unwrap_or(0.0),
                        150 | 185 => ship_type.default_attributes.get(&5646).copied().unwrap_or(0.0),
                        182 => ship_type.default_attributes.get(&5646).copied()
                            .or_else(|| ship_type.default_attributes.get(&3227).copied())
                            .unwrap_or(0.0),
                        143 => ship_type.default_attributes.get(&1573).copied().unwrap_or(0.0),
                        148 => ship_type.default_attributes.get(&1646).copied().unwrap_or(5_000.0),
                        149 => ship_type.default_attributes.get(&1653).copied().unwrap_or(0.0),
                        5 => eval_cargo_cap.unwrap_or(ship_type.capacity),
                        _ => 0.0,
                    };
                    if flag_u16 != 5 && hold_capacity <= 0.0 {
                        return Err(BridgeRpcError {
                            code: "CapacityExceeded".into(),
                            message: format!(
                                "Ship {} does not have a specialized hold for flag {flag_u16} (capacity 0 m³)",
                                ship_type.name
                            ),
                        });
                    }
                    if hold_capacity > 0.0 {
                        let used_vol: f64 = snap.items.values()
                            .filter(|i| i.location_id == new_loc && (i.flag as u16) == flag_u16 && i.item_id != item_id)
                            .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                            .sum();
                        let item_vol = effective_item_unit_volume(&ctx.sde, moving_item) * (moving_item.quantity as f64);
                        if used_vol + item_vol > hold_capacity + 1e-4 {
                            return Err(BridgeRpcError { code: "CapacityExceeded".into(), message: format!("Hold capacity of {hold_capacity}m³ exceeded") });
                        }
                    }
                    Ok(())
                });
                if let Err(e) = cap_check {
                    return Some(Err(e));
                }

                // Stack merging if matching unstacked item exists at destination
                let merged_opt: Result<Option<eve_db::InventoryItemRecord>, eve_db::DbError> = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(moving) = snap.items.get(&item_id).cloned() else {
                        return Ok(None);
                    };
                    if moving.singleton || moving.is_blueprint_copy {
                        return Ok(None);
                    }
                    let existing_target = snap.items.values().find(|i| {
                        i.item_id != item_id
                            && i.location_id == new_loc
                            && i.flag == flag
                            && i.type_id == moving.type_id
                            && !i.singleton
                            && !i.is_blueprint_copy
                    }).map(|i| i.item_id);
                    if let Some(target_iid) = existing_target {
                        snap.items.remove(&item_id);
                        let target_item = snap.items.get_mut(&target_iid).unwrap();
                        target_item.quantity += moving.quantity;
                        target_item.version += 1;
                        Ok(Some(target_item.clone()))
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
                                ("itemID", PyRep::Integer(merged_item.item_id)),
                                ("typeID", PyRep::Integer(merged_item.type_id as i64)),
                                ("locationID", PyRep::Integer(merged_item.location_id)),
                                ("flagID", PyRep::Integer(merged_item.flag as i64)),
                                ("quantity", PyRep::Integer(merged_item.quantity as i64)),
                            ]),
                            PyRep::Dict(vec![(
                                PyRep::Integer(5),
                                PyRep::Integer(merged_item.quantity as i64),
                            )]),
                        ],
                    );
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "ProcessInventoryChange",
                        "charid",
                        vec![
                            PyRep::Integer(merged_item.item_id),
                            PyRep::Integer(merged_item.location_id),
                            PyRep::Integer(merged_item.flag as i64),
                        ],
                    );
                    return Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(merged_item.item_id)),
                        ("locationID", PyRep::Integer(merged_item.location_id)),
                        ("flagID", PyRep::Integer(merged_item.flag as i64)),
                        ("quantity", PyRep::Integer(merged_item.quantity as i64)),
                        ("version", PyRep::Integer(merged_item.version as i64)),
                        ("suspectFlagged", PyRep::Bool(looted_foreign_jetcan)),
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
                Err(e) => Some(Err(BridgeRpcError {
                    code: "InventoryMoveFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("ship", "Jettison") | ("invbroker", "Jettison") => {
            Some(fitting_ship_bays::ship_jettison(ctx, session, args, kwargs).await)
        }

        ("ship", "Drop") => {
            Some(fitting_ship_bays::ship_drop(ctx, session, args, kwargs).await)
        }


        ("invbroker", "MultiAdd") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list) | PyRep::Tuple(list)) => {
                    list.iter().filter_map(|v| v.as_i64()).collect()
                }
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            };
            let Some(new_loc) = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
                .or(session.ship_id)
                .or(session.station_id)
                .or(session.structure_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLocation".into(),
                    message: "MultiAdd requires target location".into(),
                }));
            };
            let flag = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()))
                .map(|v| InventoryFlag::from_u16(v as u16))
                .unwrap_or(InventoryFlag::Cargo);
            let mut moved = Vec::with_capacity(item_ids.len());
            for item_id in item_ids {
                if let Err(e) = validate_container_and_bay_move(ctx, char_id, item_id, new_loc, flag as u16, kwargs) {
                    return Some(Err(e));
                }
                match ctx
                    .db
                    .move_item_transactional(char_id, item_id, new_loc, flag, None, None)
                {
                    Ok(upd) => moved.push(PyRep::Integer(upd.item_id)),
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "InventoryMoveFailed".into(),
                            message: e.to_string(),
                        }));
                    }
                }
            }
            Some(Ok(PyRep::keyval([
                ("movedCount", PyRep::Integer(moved.len() as i64)),
                ("items", PyRep::List(moved)),
            ])))
        }

        _ => {
            if let Some(res) = super::inventory_stacking::handle_stack_ops(ctx, session, bound_entry, service, method, args, kwargs) {
                return Some(res);
            }
            if let Some(res) = handle_dogma_im(ctx, session, bound_entry, service, method, args, kwargs).await {
                return Some(res);
            }
            if let Some(res) = handle_module_cycles(ctx, session, bound_entry, service, method, args, kwargs).await {
                return Some(res);
            }
            if let Some(res) = handle_mining_and_deactivate(ctx, session, bound_entry, service, method, args, kwargs).await {
                return Some(res);
            }
            if let Some(res) = handle_fitting_mgr(ctx, session, bound_entry, service, method, args, kwargs).await {
                return Some(res);
            }
            if let Some(res) = handle_ships_and_services(ctx, session, bound_entry, service, method, args, kwargs).await {
                return Some(res);
            }
            if let Some(res) = handle_containers_ships_and_fitting(ctx, session, bound_entry, service, method, args, kwargs).await {
                return Some(res);
            }
            if let Some(res) = handle_ships_mastery_and_fitting_ext(ctx, session, bound_entry, service, method, args, kwargs).await {
                return Some(res);
            }
            if let Some(res) = handle_superweapons_dbuffs_and_clones(ctx, session, bound_entry, service, method, args, kwargs).await {
                return Some(res);
            }
            if let Some(res) = handle_cyno_portal(ctx, session, service, method, args).await {
                return Some(res);
            }
            None
        }
    }
}

/// Encodes a ship fitting into an EVE DNA fitting string (`INV-3`):
/// `<ship_type_id>:<type_id_1>;<qty_1>:<type_id_2>;<qty_2>:...::`
pub fn fitting_to_dna(ship_type_id: TypeId, items: &[(TypeId, i32)]) -> String {
    let mut counts: HashMap<TypeId, i32> = HashMap::new();
    let mut order: Vec<TypeId> = Vec::new();
    for &(type_id, qty) in items {
        if qty <= 0 {
            continue;
        }
        if !counts.contains_key(&type_id) {
            order.push(type_id);
        }
        *counts.entry(type_id).or_insert(0) += qty;
    }
    let mut out = format!("{ship_type_id}:");
    for type_id in order {
        let qty = counts[&type_id];
        out.push_str(&format!("{type_id};{qty}:"));
    }
    out.push(':');
    out
}

/// Encodes a `SavedFittingRecord` into an EVE DNA fitting string (`INV-3`).
pub fn saved_fitting_to_dna(fitting: &SavedFittingRecord) -> String {
    let items: Vec<(TypeId, i32)> = fitting.items.iter().map(|&(t, _, q)| (t, q)).collect();
    fitting_to_dna(fitting.ship_type_id, &items)
}

/// Decodes an EVE DNA fitting string into `(ship_type_id, Vec<(TypeId, i32)>)` (`INV-3`).
/// Supports standard format `<ship_type_id>:<type_id_1>;<qty_1>:<type_id_2>;<qty_2>:...::`
pub fn dna_to_fitting(dna: &str) -> Result<(TypeId, Vec<(TypeId, i32)>), String> {
    let trimmed = dna.trim();
    if trimmed.is_empty() {
        return Err("Empty DNA string".into());
    }
    let mut parts = trimmed.split(':');
    let ship_str = parts.next().ok_or("Missing ship type ID")?;
    let ship_type_id = ship_str
        .parse::<TypeId>()
        .map_err(|e| format!("Invalid ship type ID '{ship_str}': {e}"))?;

    let mut items = Vec::new();
    for part in parts {
        let token = part.trim();
        if token.is_empty() {
            continue;
        }
        let (type_str, qty_str) = match token.split_once(';') {
            Some((t, q)) => (t.trim(), q.trim()),
            None => (token, "1"),
        };
        let type_id = type_str
            .parse::<TypeId>()
            .map_err(|e| format!("Invalid item type ID '{type_str}': {e}"))?;
        let qty = qty_str
            .parse::<i32>()
            .map_err(|e| format!("Invalid quantity '{qty_str}': {e}"))?;
        if qty > 0 {
            items.push((type_id, qty));
        }
    }
    Ok((ship_type_id, items))
}
