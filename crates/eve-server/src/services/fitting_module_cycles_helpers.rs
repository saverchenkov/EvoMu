//! Helpers for active module cycle execution (`feat:mining.survey-scanner`, `dogmaIM.Activate`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::system_actor::SystemHandle;
use eve_db::InventoryFlag;
use eve_dogma::{DogmaItemState, ShipFittingEngine};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, InvType};
use glam::DVec3;
use std::collections::HashMap;

pub(crate) fn resolve_snapshot_fitted_module(
    ctx: &ServerContext,
    ship_id: i64,
    module_id: i64,
) -> Option<eve_dogma::FittedModule> {
    ctx.db.with_snapshot(|snap| {
        let item = snap.items.get(&module_id)?;
        let flag_u16 = item.flag as u16;
        if item.location_id != ship_id
            || !matches!(flag_u16, 11..=34 | 92..=94 | 125..=132 | 164..=171)
        {
            return None;
        }
        let is_online = snap.module_online_states.get(&module_id).copied().unwrap_or(true);
        let charge_item = snap.items.values().find(|c| {
            c.location_id == ship_id && c.flag == item.flag && c.item_id != module_id && c.quantity > 0
        });
        let charge_tid = charge_item
            .map(|c| c.type_id)
            .or_else(|| snap.module_active_charges.get(&module_id).copied());
        let charge_qty = charge_item.map(|c| c.quantity.max(0) as u32).unwrap_or(100);
        let damage_hp = item.dynamic_attributes.get(&attr_ids::DAMAGE).copied().unwrap_or(0.0);
        let max_hp = item
            .dynamic_attributes
            .get(&attr_ids::HP_STRUCT)
            .copied()
            .or_else(|| {
                ctx.sde
                    .types
                    .get(&item.type_id)
                    .and_then(|t| t.default_attributes.get(&attr_ids::HP_STRUCT).copied())
            })
            .unwrap_or(40.0);
        Some(eve_dogma::FittedModule {
            item_id: module_id,
            type_id: item.type_id,
            flag_id: flag_u16,
            state: if is_online {
                eve_dogma::ModuleState::Online
            } else {
                eve_dogma::ModuleState::Offline
            },
            charge_type_id: charge_tid,
            charge_quantity: charge_qty,
            damage_hp,
            max_hp,
            dynamic_attribute_overrides: item.dynamic_attributes.clone(),
        })
    })
}

pub(crate) fn set_modules_reload_until(
    ctx: &ServerContext,
    module_ids: &[i64],
    reload_until_unix: u64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        for &mid in module_ids {
            snap.cluster_and_runtime
                .module_reload_until_unix
                .insert(mid, reload_until_unix);
        }
    });
}

pub(crate) fn check_module_reloading(
    ctx: &ServerContext,
    ship_id: i64,
    module_id: i64,
) -> Result<(), BridgeRpcError> {
    let now_unix = ctx.now_unix_secs();
    let reloading_until = ctx.db.with_snapshot(|snap| {
        let map = &snap.cluster_and_runtime.module_reload_until_unix;
        if let Some(&until) = map.get(&module_id) {
            if now_unix < until {
                return Some(until);
            }
        }
        if let Some(bank) = snap.weapon_banks.get(&ship_id).and_then(|b| b.get(&module_id)) {
            for &mid in bank {
                if let Some(&until) = map.get(&mid) {
                    if now_unix < until {
                        return Some(until);
                    }
                }
            }
        }
        None
    });
    if let Some(until) = reloading_until {
        return Err(BridgeRpcError {
            code: "ModuleIsReloading".into(),
            message: format!("Module {module_id} is reloading until {until} (now {now_unix})"),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_survey_scanner_activation(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &InvType,
    mod_state: &DogmaItemState,
) -> PyRep {
    let mut scan_range = mod_state.get_attribute(197, &ctx.sde);
    let has_197_mod = mod_state
        .modifiers
        .iter()
        .any(|m| m.target_attribute_id == 197);
    if scan_range <= 0.0 {
        scan_range = mod_type.default_attributes.get(&197).or_else(|| mod_type.default_attributes.get(&54)).copied().unwrap_or(15_000.0);
    }
    if !has_197_mod {
        let m197 = crate::services::mining_cycle::dbuff_multiplier(ctx, ship_id, mod_type, 197);
        scan_range *= if (m197 - 1.0).abs() > 1e-9 { m197 } else { crate::services::mining_cycle::dbuff_multiplier(ctx, ship_id, mod_type, 54) };
    }
    let mut ship_pos = DVec3::ZERO;
    if let Ok(Some((_, ship_ball))) = sys.get_ball_snapshot(ship_id).await {
        ship_pos = ship_ball.position;
    }
    let asteroids = ctx.db.get_asteroids_in_system(sys_id);
    let mut scanned_asteroids = Vec::new();
    for ast in asteroids {
        if ast.quantity <= 0 {
            continue;
        }
        let ast_pos = DVec3::new(ast.position.0, ast.position.1, ast.position.2);
        let dist = (ship_pos - ast_pos).length();
        if dist <= scan_range {
            let type_name = ctx
                .sde
                .types
                .get(&ast.type_id)
                .map(|t| t.name.clone())
                .unwrap_or_else(|| format!("Asteroid {}", ast.type_id));
            let vol = ctx.sde.types.get(&ast.type_id).map(|t| t.volume).unwrap_or(0.1);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnAsteroidRevealed",
                "character",
                vec![
                    PyRep::Integer(ast.asteroid_id),
                    PyRep::Integer(ast.type_id as i64),
                    PyRep::Integer(ast.quantity),
                ],
            );
            scanned_asteroids.push(PyRep::keyval([
                ("asteroidID", PyRep::Integer(ast.asteroid_id)),
                ("typeID", PyRep::Integer(ast.type_id as i64)),
                ("typeName", PyRep::String(type_name)),
                ("quantity", PyRep::Integer(ast.quantity)),
                ("initialQuantity", PyRep::Integer(ast.initial_quantity)),
                ("distance", PyRep::Real(dist)),
                ("volume", PyRep::Real(vol)),
            ]));
        }
    }
    PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("active", PyRep::Bool(true)),
        ("scanRange", PyRep::Real(scan_range)),
        ("count", PyRep::Integer(scanned_asteroids.len() as i64)),
        ("asteroidCount", PyRep::Integer(scanned_asteroids.len() as i64)),
        ("asteroids", PyRep::List(scanned_asteroids)),
    ])
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_specialized_module_result(
    method: &str,
    fit: &ShipFittingEngine,
    ctx: &ServerContext,
    module_id: i64,
    mod_type: &InvType,
    hic_bubble_radius_m: Option<f64>,
    warp_scramble_points: i32,
    active_burst_charge_tid: Option<u32>,
) -> Option<PyRep> {
    if method == "Overload" {
        let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
        Some(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("overloaded", PyRep::Bool(true)),
            ("effectiveMaxVelocity", PyRep::Real(max_vel)),
        ]))
    } else if mod_type.group_id == 899 {
        let weapon_timer_s = mod_type
            .default_attributes
            .get(&2037)
            .copied()
            .unwrap_or(60_000.0)
            / 1000.0;
        Some(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("active", PyRep::Bool(true)),
            ("scripted", PyRep::Bool(hic_bubble_radius_m.is_none())),
            (
                "bubbleRadius",
                hic_bubble_radius_m.map(PyRep::Real).unwrap_or(PyRep::None),
            ),
            ("warpScrambleStrength", PyRep::Integer(warp_scramble_points as i64)),
            ("disallowAssistance", PyRep::Bool(hic_bubble_radius_m.is_some())),
            ("weaponTimerSeconds", PyRep::Real(weapon_timer_s)),
        ]))
    } else if matches!(mod_type.group_id, 515 | 1770 | 4174) {
        Some(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("active", PyRep::Bool(true)),
            (
                "chargeTypeID",
                active_burst_charge_tid
                    .map(|c| PyRep::Integer(c as i64))
                    .unwrap_or(PyRep::None),
            ),
        ]))
    } else {
        None
    }
}

pub(crate) fn handle_unload_ammo(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
    let owns_ship = ctx.db.with_snapshot(|s| s.items.get(&ship_id).is_some_and(|sh| sh.owner_id == char_id));
    if !owns_ship {
        return Err(BridgeRpcError {
            code: "ShipOwnershipViolation".into(),
            message: format!("Ship {ship_id} is not owned by character {char_id}"),
        });
    }
    let requested_module_ids: Vec<i64> = match args.get(1) {
        Some(v) => {
            let ids = crate::services::combat_crimewatch::extract_id_collection(Some(v));
            if ids.is_empty() {
                v.as_i64().into_iter().collect()
            } else {
                ids
            }
        }
        None => Vec::new(),
    };

    let all_target_modules: Vec<i64> = ctx.db.with_snapshot(|snap| {
        let mut out = Vec::new();
        for &req_id in &requested_module_ids {
            let resolved_mod_id = if snap.items.contains_key(&req_id) {
                req_id
            } else if let Some(found) = snap.items.values().find(|i| i.location_id == ship_id && (i.flag as i64) == req_id) {
                found.item_id
            } else {
                req_id
            };
            if let Some(bank) = snap.weapon_banks.get(&ship_id).and_then(|b| b.get(&resolved_mod_id)) {
                for &m in bank {
                    if !out.contains(&m) {
                        out.push(m);
                    }
                }
            } else if !out.contains(&resolved_mod_id) {
                out.push(resolved_mod_id);
            }
        }
        out
    });

    let (dest_loc_id, dest_owner_opt, dest_flag_raw_opt) = match args.get(2) {
        None | Some(PyRep::None) => (ship_id, None, None),
        Some(PyRep::Integer(loc_id)) if *loc_id > 0 => (*loc_id, None, None),
        Some(PyRep::Tuple(elems) | PyRep::List(elems)) => {
            let Some(loc_id) = elems.first().and_then(|e| e.as_i64()).filter(|&id| id > 0) else {
                return Err(BridgeRpcError { code: "InvalidDestination".into(), message: "Unload destination locationID must be a positive integer".into() });
            };
            let parse_flag = |rep: Option<&PyRep>| -> Result<Option<i64>, BridgeRpcError> {
                match rep {
                    None | Some(PyRep::None) => Ok(None),
                    Some(e) => e.as_i64().map(Some).ok_or_else(|| BridgeRpcError { code: "InvalidInventoryFlag".into(), message: "Invalid unload destination flag".into() }),
                }
            };
            match elems.len() {
                1 => (loc_id, None, None),
                2 => (loc_id, None, parse_flag(elems.get(1))?),
                _ => {
                    let owner_opt = match elems.get(1) {
                        None | Some(PyRep::None) => None,
                        Some(e) => Some(e.as_i64().filter(|&id| id > 0).ok_or_else(|| BridgeRpcError { code: "ItemOwnershipViolation".into(), message: "Invalid unload destination ownerID".into() })?),
                    };
                    (loc_id, owner_opt, parse_flag(elems.get(2))?)
                }
            }
        }
        Some(_) => return Err(BridgeRpcError { code: "InvalidDestination".into(), message: "Invalid unload destination".into() }),
    };
    if let Some(dest_owner) = dest_owner_opt {
        if dest_owner != char_id {
            return Err(BridgeRpcError { code: "ItemOwnershipViolation".into(), message: format!("Destination owner {dest_owner} does not match character {char_id}") });
        }
    }
    if let Some(rf) = dest_flag_raw_opt {
        if !matches!(rf, 4 | 5 | 64 | 133..=151 | 155 | 177 | 179 | 181 | 182 | 185) || (dest_loc_id == ship_id && matches!(rf, 4 | 64)) {
            return Err(BridgeRpcError { code: "InvalidInventoryFlag".into(), message: format!("Invalid unload destination flag {rf}") });
        }
    }

    let eval_cargo_cap = (dest_loc_id == ship_id)
        .then(|| {
            ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok().map(|f| f.ship.get_attribute(attr_ids::CAPACITY, &ctx.sde))
                .or_else(|| ctx.db.with_snapshot(|s| s.items.get(&ship_id).and_then(|sh| ctx.sde.types.get(&sh.type_id)).map(|t| t.capacity)))
        })
        .flatten();

    let item_diffs = ctx.db.try_with_mut_snapshot(|snap| {
        let mut is_owned_container_target = false;
        let dest_flag = if dest_loc_id == ship_id {
            dest_flag_raw_opt.map(|rf| InventoryFlag::from_u16(rf as u16)).unwrap_or(InventoryFlag::Cargo)
        } else {
            let ch_dock = session.station_id.or(session.structure_id).or_else(|| snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id)));
            let is_docked_target = ch_dock == Some(dest_loc_id);
            if is_docked_target {
                if dest_flag_raw_opt.is_some_and(|rf| rf != 4) {
                    return Err(BridgeRpcError { code: "InvalidInventoryFlag".into(), message: "Station/structure unload requires Hangar flag (4)".into() });
                }
                InventoryFlag::Hangar
            } else {
                let Some(d) = snap.items.get(&dest_loc_id) else {
                    return Err(BridgeRpcError { code: "InvalidDestination".into(), message: format!("Unload destination {dest_loc_id} is not accessible to character {char_id}") });
                };
                let d_grp = ctx.sde.types.get(&d.type_id).map(|t| t.group_id).unwrap_or(0);
                if d.owner_id != char_id
                    || (d.location_id != ship_id && !(ch_dock.is_some() && Some(d.location_id) == ch_dock))
                    || !crate::services::fitting_containers_ships::is_container_group(d_grp, d.type_id)
                    || d_grp == crate::services::mobile_deployables::GROUP_CRAB_BEACON
                    || d_grp == 1247
                {
                    return Err(BridgeRpcError { code: "InvalidDestination".into(), message: format!("Unload destination {dest_loc_id} is not an accessible container") });
                }
                if !d.singleton && d_grp != crate::services::mobile_deployables::GROUP_MOBILE_DEPOT {
                    return Err(BridgeRpcError { code: "ContainerNotAssembled".into(), message: format!("Target container {dest_loc_id} must be assembled before storing items") });
                }
                if dest_flag_raw_opt.is_some_and(|rf| !matches!(rf, 5 | 64)) {
                    return Err(BridgeRpcError { code: "InvalidInventoryFlag".into(), message: "Container unload requires Cargo (5) or Unlocked (64) flag".into() });
                }
                crate::services::fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, d)?;
                let st = crate::services::fitting_containers_ships::read_container_ship_state_snap(snap);
                if st.container_passwords.get(&dest_loc_id).is_some_and(|pw| !pw.is_empty() && !st.unlocked_containers.contains(&(dest_loc_id, char_id))) {
                    return Err(BridgeRpcError { code: "ALSCPasswordRequired".into(), message: format!("Password required to move items into container {dest_loc_id}") });
                }
                is_owned_container_target = true;
                if dest_flag_raw_opt == Some(64) { InventoryFlag::Unlocked } else { InventoryFlag::Cargo }
            }
        };
        let ship_type = snap.items.get(&ship_id).and_then(|sh| ctx.sde.types.get(&sh.type_id));
        let dest_cap: Option<f64> = if dest_loc_id == ship_id {
            let hold_cap = if dest_flag == InventoryFlag::Cargo {
                eval_cargo_cap.unwrap_or(0.0)
            } else {
                ship_type.map(|st| crate::services::fitting_saved_apply::get_ship_hold_capacity(dest_flag as u16, st, eval_cargo_cap)).unwrap_or(0.0)
            };
            if hold_cap <= 0.0 {
                return Err(BridgeRpcError { code: "NotEnoughCargoSpace".into(), message: "Not enough cargo space to unload ammo".into() });
            }
            Some(hold_cap)
        } else if is_owned_container_target {
            let cont_cap = snap.items.get(&dest_loc_id).and_then(|d| {
                d.dynamic_attributes.get(&attr_ids::CAPACITY).copied().filter(|&c| c > 0.0).or_else(|| {
                    ctx.sde.types.get(&d.type_id).map(|t| if t.capacity > 0.0 { t.capacity } else { t.default_attributes.get(&attr_ids::CAPACITY).copied().unwrap_or(0.0) })
                })
            }).unwrap_or(0.0);
            if cont_cap <= 0.0 {
                return Err(BridgeRpcError { code: "NotEnoughCargoSpace".into(), message: "Not enough cargo space to unload ammo".into() });
            }
            Some(cont_cap)
        } else {
            None
        };
        let mut diffs: Vec<(Option<eve_db::InventoryItemRecord>, Option<eve_db::InventoryItemRecord>)> = Vec::new();
        let mut used_cargo: f64 = if dest_cap.is_some() {
            snap.items.values()
                .filter(|i| i.location_id == dest_loc_id && (dest_loc_id != ship_id || i.flag == dest_flag))
                .map(|i| crate::services::fitting_containers_ships::effective_item_unit_volume(&ctx.sde, i) * (i.quantity.max(1) as f64))
                .sum()
        } else {
            0.0
        };

        for &mod_id in &all_target_modules {
            let Some(mod_item) = snap.items.get(&mod_id).cloned() else { continue };
            if mod_item.location_id != ship_id || !matches!(mod_item.flag as u16, 11..=34 | 125..=132) { continue; }
            let slot_flag = mod_item.flag;
            let charges: Vec<(i64, u32, f64)> = snap.items.values()
                .filter(|i| i.location_id == ship_id && i.flag == slot_flag && i.item_id != mod_id && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 8))
                .map(|i| (i.item_id, i.type_id, crate::services::fitting_containers_ships::effective_item_unit_volume(&ctx.sde, i) * (i.quantity.max(1) as f64)))
                .collect();
            if !charges.is_empty() && snap.module_active_states.contains(&mod_id) {
                return Err(BridgeRpcError::user_error("ModuleIsActive"));
            }
            for (cid, charge_tid, vol) in charges {
                if dest_loc_id == ship_id {
                    if let (Some(st), Some(ct)) = (ship_type, ctx.sde.types.get(&charge_tid)) {
                        if !crate::services::fitting_saved_apply::is_item_allowed_in_ship_hold(dest_flag as u16, ct, st) {
                            return Err(BridgeRpcError::user_error("InvalidItemTypeForHold"));
                        }
                    }
                }
                if let Some(cap) = dest_cap {
                    if cap > 0.0 && used_cargo + vol > cap + 1e-4 {
                        return Err(BridgeRpcError { code: "NotEnoughCargoSpace".into(), message: "Not enough cargo space to unload ammo".into() });
                    }
                }
                if let Some(ci) = snap.items.get_mut(&cid) {
                    let pre = ci.clone();
                    ci.location_id = dest_loc_id;
                    ci.flag = dest_flag;
                    ci.singleton = false;
                    ci.version += 1;
                    diffs.push((Some(pre), Some(ci.clone())));
                    used_cargo += vol;
                }
            }
        }
        if !diffs.is_empty() {
            crate::services::fitting_containers_ships::write_container_ship_state_snap(snap, |st| {
                for (_, post_opt) in &diffs {
                    if let Some(post) = post_opt {
                        if dest_loc_id == ship_id && dest_flag_raw_opt == Some(155) {
                            st.custom_item_flags.insert(post.item_id, 155);
                        } else {
                            st.custom_item_flags.remove(&post.item_id);
                        }
                    }
                }
            });
        }
        Ok(diffs)
    })?;

    let expelled_sublocs: Vec<(InventoryFlag, u32, i32)> = item_diffs
        .iter()
        .filter_map(|(pre_opt, _)| {
            pre_opt.as_ref().filter(|pre| {
                pre.location_id == ship_id
                    && matches!(pre.flag as u16, 11..=34 | 125..=132)
                    && pre.quantity > 0
                    && ctx.sde.types.get(&pre.type_id).is_some_and(|t| t.category_id == 8)
            })
        })
        .map(|pre| (pre.flag, pre.type_id, pre.quantity))
        .collect();
    for (pre, post) in item_diffs {
        ctx.notify_item_mutation(char_id, pre.as_ref(), post.as_ref());
    }
    for (old_flag, charge_type_id, old_qty) in expelled_sublocs {
        emit_charge_sublocation_quantity_change(
            ctx,
            char_id,
            ship_id,
            old_flag,
            charge_type_id,
            old_qty,
            0,
        );
    }
    crate::services::notify_emitters_seasons_tactical::emit_bank_ammo_changed_if_grouped(
        ctx,
        char_id,
        ship_id,
        &all_target_modules,
    );
    ctx.invalidate_ship_fitting(ship_id);
    Ok(PyRep::None)
}

pub(crate) fn charge_sublocation_key(
    ship_id: i64,
    flag: InventoryFlag,
    charge_type_id: u32,
) -> PyRep {
    PyRep::Tuple(vec![
        PyRep::Integer(ship_id),
        PyRep::Integer(i64::from(flag as u16)),
        PyRep::Integer(i64::from(charge_type_id)),
    ])
}

pub(crate) fn build_charge_sublocation_attrs(
    ctx: &ServerContext,
    c_item: &eve_db::InventoryItemRecord,
    fit_opt: Option<&eve_dogma::ShipFittingEngine>,
) -> Vec<(PyRep, PyRep)> {
    let mut c_map: HashMap<u16, f64> = ctx
        .sde
        .types
        .get(&c_item.type_id)
        .map(|t| t.default_attributes.clone())
        .unwrap_or_default();
    if let Some(fit) = fit_opt {
        let slot_flag = c_item.flag as u16;
        let launcher_id_opt = fit
            .modules
            .values()
            .find(|m| m.flag_id == slot_flag)
            .map(|m| m.item_id);
        let charge_state_opt = launcher_id_opt
            .and_then(|lid| fit.charge_state(lid, &ctx.sde))
            .or_else(|| {
                fit.evaluated_charges
                    .values()
                    .find(|c| c.type_id == c_item.type_id)
                    .cloned()
            });
        if let Some(c_state) = charge_state_opt {
            let all_aids: std::collections::BTreeSet<u16> = c_state
                .base_attributes
                .keys()
                .copied()
                .chain(c_state.modifiers.iter().map(|m| m.target_attribute_id))
                .collect();
            for aid in all_aids {
                c_map.insert(aid, c_state.get_attribute(aid, &ctx.sde));
            }
        }
    }
    for (&aid, &val) in &c_item.dynamic_attributes {
        c_map.insert(aid, val);
    }
    c_map.insert(attr_ids::QUANTITY, c_item.quantity as f64);
    let mut c_attrs: Vec<(PyRep, PyRep)> = c_map
        .into_iter()
        .map(|(aid, v)| (PyRep::Integer(aid as i64), PyRep::Real(v)))
        .collect();
    c_attrs.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
    c_attrs
}

pub(crate) fn build_charge_sublocation_keyval(
    subloc_key: PyRep,
    c_attrs: Vec<(PyRep, PyRep)>,
    now_ft: i64,
) -> PyRep {
    PyRep::keyval([
        ("itemID", subloc_key),
        ("invItem", PyRep::None),
        ("attributes", PyRep::Dict(c_attrs)),
        ("activeEffects", PyRep::Dict(Vec::new())),
        ("time", PyRep::Integer(now_ft)),
        ("wallclockTime", PyRep::Integer(now_ft)),
        ("description", PyRep::String(String::new())),
    ])
}

/// Emits `OnGodmaPrimeItem` for a charge sublocation `(ship_id, flag_id, charge_type_id)` so
/// client `godma.StateManager.UpdateItem` populates `sublocationsByLocationFlag` immediately.
pub(crate) fn emit_godma_charge_sublocation_prime(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    c_item: &eve_db::InventoryItemRecord,
) {
    if ship_id <= 0 || c_item.quantity <= 0 {
        return;
    }
    let fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
    let c_attrs = build_charge_sublocation_attrs(ctx, c_item, fit_opt.as_ref());
    let subloc_key = charge_sublocation_key(ship_id, c_item.flag, c_item.type_id);
    let now_ft = ctx.now_blueos_filetime_ticks();
    let keyval = build_charge_sublocation_keyval(subloc_key, c_attrs, now_ft);
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnGodmaPrimeItem",
        "charid",
        vec![PyRep::Integer(ship_id), keyval],
    );
}

/// Emits `OnModuleAttributeChanges` for `const.attributeQuantity (805)` on a charge sublocation
/// `(ship_id, flag_id, charge_type_id)`. Passing `new_qty == 0` triggers `SubLoc.quantityExpelled`.
pub(crate) fn emit_charge_sublocation_quantity_change(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    flag: InventoryFlag,
    charge_type_id: u32,
    old_qty: i32,
    new_qty: i32,
) {
    if ship_id <= 0 {
        return;
    }
    let subloc_key = charge_sublocation_key(ship_id, flag, charge_type_id);
    let now_ft = ctx.now_blueos_filetime_ticks();
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnModuleAttributeChanges",
        "charid",
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::String("OnModuleAttributeChange".into()),
            PyRep::Integer(char_id),
            subloc_key,
            PyRep::Integer(i64::from(attr_ids::QUANTITY)),
            PyRep::Integer(now_ft),
            PyRep::Real(f64::from(new_qty)),
            PyRep::Real(f64::from(old_qty)),
            PyRep::None,
        ])])],
    );
}

pub(crate) fn evaluate_ship_dogma_attributes(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
) -> Result<(u32, PyRep, Vec<(PyRep, PyRep)>, Vec<(PyRep, PyRep)>), BridgeRpcError> {
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    let computed_ctx;
    let eval_ctx = match fit.persisted_context() {
        Some(c) => c,
        None => {
            computed_ctx = fit.evaluated_context(&ctx.sde);
            &computed_ctx
        }
    };
    let mut ship_attr_ids: std::collections::BTreeSet<u16> = eval_ctx
        .ship
        .base_attributes
        .keys()
        .copied()
        .chain(eval_ctx.ship.modifiers.iter().map(|m| m.target_attribute_id))
        .collect();
    ship_attr_ids.insert(attr_ids::CPU_LOAD);
    ship_attr_ids.insert(attr_ids::POWER_LOAD);
    let mut ship_attrs_map: HashMap<u16, f64> = HashMap::with_capacity(ship_attr_ids.len());
    for attr_id in ship_attr_ids {
        let val = eval_ctx.ship.get_attribute(attr_id, &ctx.sde);
        ship_attrs_map.insert(attr_id, val);
    }
    let dyn_attrs = ctx
        .db
        .with_snapshot(|s| s.items.get(&ship_id).map(|it| it.dynamic_attributes.clone()))
        .unwrap_or_default();
    for (attr_id, val) in dyn_attrs {
        if matches!(attr_id, 3 | 18 | 264 | 265 | 266)
            || !eval_ctx
                .ship
                .modifiers
                .iter()
                .any(|m| m.target_attribute_id == attr_id)
        {
            ship_attrs_map.insert(attr_id, val);
        }
    }
    let already_applied_dbuffs: Vec<u32> =
        fit.gang_bursts.iter().filter_map(|b| b.dbuff_id).collect();
    super::super::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs_excluding(
        ctx,
        ship_id,
        &already_applied_dbuffs,
        &mut ship_attrs_map,
    );
    let mut entries: Vec<(PyRep, PyRep)> = ship_attrs_map
        .into_iter()
        .map(|(k, v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
        .collect();
    entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));

    let mut module_entries = Vec::with_capacity(fit.modules.len());
    for m_state in &eval_ctx.ship_located_items {
        let attr_ids_set: std::collections::BTreeSet<u16> = m_state
            .base_attributes
            .keys()
            .copied()
            .chain(m_state.modifiers.iter().map(|m| m.target_attribute_id))
            .collect();
        let mut m_map: HashMap<u16, f64> = attr_ids_set
            .into_iter()
            .map(|aid| (aid, m_state.get_attribute(aid, &ctx.sde)))
            .collect();
        let is_mod_online = fit
            .modules
            .get(&m_state.item_id)
            .map(|m| m.state != eve_dogma::ModuleState::Offline)
            .unwrap_or(true);
        m_map.insert(2, if is_mod_online { 1.0 } else { 0.0 });
        crate::services::tactical_modules::apply_projected_rtc_to_module_attrs(
            ctx,
            ship_id,
            &mut m_map,
        );
        let mut m_attrs: Vec<(PyRep, PyRep)> = m_map
            .into_iter()
            .map(|(aid, v)| (PyRep::Integer(aid as i64), PyRep::Real(v)))
            .collect();
        m_attrs.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
        module_entries.push((PyRep::Integer(m_state.item_id), PyRep::Dict(m_attrs)));
    }
    for m in fit
        .modules
        .values()
        .filter(|m| m.state == eve_dogma::ModuleState::Offline)
    {
        let mut m_map: HashMap<u16, f64> = ctx
            .sde
            .types
            .get(&m.type_id)
            .map(|t| t.default_attributes.clone())
            .unwrap_or_default();
        m_map.insert(2, 0.0);
        let mut m_attrs: Vec<(PyRep, PyRep)> = m_map
            .into_iter()
            .map(|(aid, v)| (PyRep::Integer(aid as i64), PyRep::Real(v)))
            .collect();
        m_attrs.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
        module_entries.push((PyRep::Integer(m.item_id), PyRep::Dict(m_attrs)));
    }
    let loaded_charges: Vec<eve_db::InventoryItemRecord> = ctx.db.with_snapshot(|snap| {
        let mut list: Vec<eve_db::InventoryItemRecord> = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && matches!(i.flag as u16, 11..=34 | 125..=132)
                    && i.quantity > 0
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 8)
            })
            .cloned()
            .collect();
        list.sort_by_key(|i| i.item_id);
        list
    });
    for c_item in loaded_charges {
        let c_attrs = build_charge_sublocation_attrs(ctx, &c_item, Some(&fit));
        module_entries.push((
            charge_sublocation_key(ship_id, c_item.flag, c_item.type_id),
            PyRep::Dict(c_attrs),
        ));
    }

    let ship_type_id = fit.ship.type_id;
    let active_stance = fit
        .active_stance_type_id
        .or_else(|| eve_dogma::ShipFittingEngine::default_t3d_stance_type_id(ship_type_id))
        .map(|tid| PyRep::Integer(tid as i64))
        .unwrap_or(PyRep::None);

    ctx.store_ship_fitting(ship_id, fit);
    Ok((ship_type_id, active_stance, entries, module_entries))
}

fn emit_godma_prime_location_and_ship_state(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    entries: &[(PyRep, PyRep)],
    module_entries: &[(PyRep, PyRep)],
    now_ft: i64,
    header: &eve_proto::DBRowDescriptor,
) -> (Vec<(PyRep, PyRep)>, PyRep) {
    let ship_inv_row = if let Some(mut it) = ctx.db.get_item(ship_id) {
        it.singleton = true;
        header.build_dbrow(super::super::item_to_packed_cells(ctx, &it))
    } else {
        let (grp, cat) = ctx
            .sde
            .types
            .get(&ship_type_id)
            .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
            .unwrap_or((25, 6));
        let loc = session
            .station_id
            .or(session.structure_id)
            .or(session.solar_system_id.map(i64::from))
            .unwrap_or(0);
        header.build_dbrow(vec![
            PyRep::Integer(ship_id),
            PyRep::Integer(i64::from(ship_type_id)),
            PyRep::Integer(char_id),
            PyRep::Integer(loc),
            PyRep::Integer(4),
            PyRep::Integer(-1),
            PyRep::Integer(grp),
            PyRep::Integer(cat),
            PyRep::String(String::new()),
        ])
    };
    let mut ship_info_entries = vec![(
        PyRep::Integer(ship_id),
        PyRep::keyval([
            ("itemID", PyRep::Integer(ship_id)),
            ("invItem", ship_inv_row),
            ("attributes", PyRep::Dict(entries.to_vec())),
            ("activeEffects", PyRep::Dict(Vec::new())),
            ("time", PyRep::Integer(now_ft)),
            ("wallclockTime", PyRep::Integer(now_ft)),
            ("description", PyRep::String(String::new())),
        ]),
    )];
    let active_states = ctx.db.with_snapshot(|snap| snap.module_active_states.clone());
    let build_active_effect_line = |mod_id: i64, effect_id: i64, dur_ms: f64| -> PyRep {
        PyRep::List(vec![
            PyRep::Integer(mod_id),
            PyRep::Integer(char_id),
            PyRep::Integer(ship_id),
            PyRep::None,
            PyRep::None,
            PyRep::List(Vec::new()),
            PyRep::Integer(effect_id),
            PyRep::Integer(now_ft),
            PyRep::Real(dur_ms),
            PyRep::Integer(0),
        ])
    };
    let mut subloc_cache: std::collections::BTreeMap<u16, (u32, i32)> =
        std::collections::BTreeMap::new();
    for (mod_id_rep, mod_attrs_rep) in module_entries {
        if let Some(mod_id) = mod_id_rep.as_i64() {
            if let Some(mut mod_item) = ctx.db.get_item(mod_id) {
                let is_charge = ctx
                    .sde
                    .types
                    .get(&mod_item.type_id)
                    .is_some_and(|t| t.category_id == 8);
                mod_item.singleton = !is_charge;
                let inv_row = header.build_dbrow(super::super::item_to_packed_cells(ctx, &mod_item));
                let is_online = !is_charge
                    && matches!(mod_attrs_rep, PyRep::Dict(pairs) if pairs.iter().any(|(k, v)| k.as_i64() == Some(2) && v.as_f64().is_some_and(|val| val > 0.5)));
                let mut active_effects = Vec::new();
                if !is_charge {
                    if let Some(type_rec) = ctx.sde.types.get(&mod_item.type_id) {
                        if is_online && type_rec.effect_ids.contains(&16) {
                            active_effects.push((
                                PyRep::Integer(16),
                                build_active_effect_line(mod_id, 16, -1.0),
                            ));
                        }
                        if active_states.contains(&mod_id) {
                            if let Some(def_eff) = type_rec.default_effect_id.filter(|&eid| eid != 16) {
                                let dur_ms = match mod_attrs_rep {
                                    PyRep::Dict(pairs) => pairs
                                        .iter()
                                        .find(|(k, _)| k.as_i64() == Some(73))
                                        .and_then(|(_, v)| v.as_f64())
                                        .filter(|&d| d > 0.0)
                                        .unwrap_or(1_000.0),
                                    _ => 1_000.0,
                                };
                                active_effects.push((
                                    PyRep::Integer(i64::from(def_eff)),
                                    build_active_effect_line(mod_id, i64::from(def_eff), dur_ms),
                                ));
                            }
                        }
                    }
                }
                ship_info_entries.push((
                    PyRep::Integer(mod_id),
                    PyRep::keyval([
                        ("itemID", PyRep::Integer(mod_id)),
                        ("invItem", inv_row),
                        ("attributes", mod_attrs_rep.clone()),
                        ("activeEffects", PyRep::Dict(active_effects)),
                        ("time", PyRep::Integer(now_ft)),
                        ("wallclockTime", PyRep::Integer(now_ft)),
                        ("description", PyRep::String(String::new())),
                    ]),
                ));
            }
        } else if let PyRep::Tuple(parts) = mod_id_rep {
            if parts.len() == 3 {
                if let (Some(loc_id), Some(flag_i64), Some(type_i64)) =
                    (parts[0].as_i64(), parts[1].as_i64(), parts[2].as_i64())
                {
                    if loc_id == ship_id && flag_i64 > 0 && type_i64 > 0 {
                        let flag_u16 = flag_i64 as u16;
                        let qty = match mod_attrs_rep {
                            PyRep::Dict(pairs) => pairs
                                .iter()
                                .find(|(k, _)| k.as_i64() == Some(i64::from(attr_ids::QUANTITY)))
                                .and_then(|(_, v)| v.as_f64())
                                .map(|q| q.round() as i32)
                                .unwrap_or(1),
                            _ => 1,
                        };
                        if !subloc_cache.contains_key(&flag_u16) && qty > 0 {
                            let c_attrs = match mod_attrs_rep {
                                PyRep::Dict(pairs) => pairs.clone(),
                                _ => Vec::new(),
                            };
                            let subloc_key = PyRep::Tuple(parts.clone());
                            ship_info_entries.push((
                                subloc_key.clone(),
                                build_charge_sublocation_keyval(subloc_key, c_attrs, now_ft),
                            ));
                            subloc_cache.insert(flag_u16, (type_i64 as u32, qty));
                        }
                    }
                }
            }
        }
    }
    let extra_charges: Vec<eve_db::InventoryItemRecord> = ctx.db.with_snapshot(|snap| {
        let mut list: Vec<eve_db::InventoryItemRecord> = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == ship_id
                    && matches!(i.flag as u16, 11..=34 | 125..=132)
                    && i.quantity > 0
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| t.category_id == 8)
            })
            .cloned()
            .collect();
        list.sort_by_key(|i| i.item_id);
        list
    });
    for c_item in extra_charges {
        let flag_u16 = c_item.flag as u16;
        if subloc_cache.contains_key(&flag_u16) {
            continue;
        }
        let c_attrs = build_charge_sublocation_attrs(ctx, &c_item, None);
        let subloc_key = charge_sublocation_key(ship_id, c_item.flag, c_item.type_id);
        ship_info_entries.push((
            subloc_key.clone(),
            build_charge_sublocation_keyval(subloc_key, c_attrs, now_ft),
        ));
        subloc_cache.insert(flag_u16, (c_item.type_id, c_item.quantity));
    }

    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "ProcessGodmaPrimeLocation",
        "charid",
        vec![PyRep::Integer(ship_id), PyRep::Dict(ship_info_entries.clone())],
    );

    let subloc_cache_dict = if subloc_cache.is_empty() {
        Vec::new()
    } else {
        let subloc_header = eve_proto::DBRowDescriptor::new([
            ("locationID", eve_proto::DbColumnType::I8),
            ("flagID", eve_proto::DbColumnType::I4),
            ("typeID", eve_proto::DbColumnType::I4),
            ("quantity", eve_proto::DbColumnType::I4),
        ]);
        let flag_rows: Vec<(PyRep, PyRep)> = subloc_cache
            .into_iter()
            .map(|(flag_u16, (charge_tid, qty))| {
                let row = subloc_header.build_dbrow(vec![
                    PyRep::Integer(ship_id),
                    PyRep::Integer(i64::from(flag_u16)),
                    PyRep::Integer(i64::from(charge_tid)),
                    PyRep::Integer(i64::from(qty)),
                ]);
                (PyRep::Integer(i64::from(flag_u16)), row)
            })
            .collect();
        vec![(PyRep::Integer(ship_id), PyRep::Dict(flag_rows))]
    };

    let wb_data = ctx.db.with_snapshot(|snap| {
        crate::services::combat_crimewatch::combat_weapons_spool::build_weapon_banks_dict(
            snap, ship_id,
        )
    });
    let ship_state = PyRep::Tuple(vec![
        PyRep::Dict(Vec::new()),
        PyRep::Dict(subloc_cache_dict),
        wb_data,
        PyRep::Dict(Vec::new()),
    ]);
    (ship_info_entries, ship_state)
}

pub(crate) fn prime_ship_on_board(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    ship_id: i64,
) -> PyRep {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let header = eve_proto::DBRowDescriptor::inv_item_header();
    let (ship_type_id, entries, module_entries) =
        match evaluate_ship_dogma_attributes(ctx, char_id, ship_id) {
            Ok((tid, _stance, entries, module_entries)) => (tid, entries, module_entries),
            Err(_) => {
                let tid = ctx
                    .db
                    .get_item(ship_id)
                    .map(|it| it.type_id)
                    .unwrap_or(0);
                (tid, Vec::new(), Vec::new())
            }
        };
    let (_ship_info_entries, ship_state) = emit_godma_prime_location_and_ship_state(
        ctx,
        session,
        char_id,
        ship_id,
        ship_type_id,
        &entries,
        &module_entries,
        now_ft,
        &header,
    );
    ship_state
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_all_info(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    active_stance: PyRep,
    entries: Vec<(PyRep, PyRep)>,
    module_entries: Vec<(PyRep, PyRep)>,
    args: &[PyRep],
) -> PyRep {
    let now_ft = ctx.now_blueos_filetime_ticks();
    let header = eve_proto::DBRowDescriptor::inv_item_header();
    let prime_char = !matches!(args.first(), Some(PyRep::Bool(false) | PyRep::Integer(0) | PyRep::None));
    let prime_ship = !matches!(args.get(1), Some(PyRep::Bool(false) | PyRep::Integer(0) | PyRep::None));

    let (ship_info_entries, ship_state) = emit_godma_prime_location_and_ship_state(
        ctx,
        session,
        char_id,
        ship_id,
        ship_type_id,
        &entries,
        &module_entries,
        now_ft,
        &header,
    );

    let char_info = if prime_char {
        let char_type_id = 1373u32;
        let (cha, int, mem, per, wil) = ctx.db.with_snapshot(|snap| {
            crate::services::character_skills::effective_character_attributes(&ctx.sde, snap, char_id)
        });
        let (char_grp, char_cat) = ctx
            .sde
            .types
            .get(&char_type_id)
            .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
            .unwrap_or((1, 1));
        let char_inv_row = header.build_dbrow(vec![
            PyRep::Integer(char_id),
            PyRep::Integer(i64::from(char_type_id)),
            PyRep::Integer(char_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(57),
            PyRep::Integer(-1),
            PyRep::Integer(char_grp),
            PyRep::Integer(char_cat),
            PyRep::String(String::new()),
        ]);
        let mut char_attrs_map = ctx
            .sde
            .types
            .get(&char_type_id)
            .map(|t| t.default_attributes.clone())
            .unwrap_or_default();
        for (aid, val) in [(164u16, cha), (165, int), (166, mem), (167, per), (168, wil)] {
            char_attrs_map.insert(aid, val);
        }
        let (drones_lv, target_mgmt_lv, adv_target_mgmt_lv) = ctx.db.with_snapshot(|snap| {
            let sk = snap.skills.get(&char_id);
            let d = sk
                .and_then(|m| m.get(&3436))
                .map(|s| f64::from(s.level.min(5)))
                .unwrap_or(0.0);
            let tm = sk
                .and_then(|m| m.get(&3429))
                .map(|s| f64::from(s.level.min(5)))
                .unwrap_or(0.0);
            let atm = sk
                .and_then(|m| m.get(&3430))
                .map(|s| f64::from(s.level.min(5)))
                .unwrap_or(0.0);
            (d, tm, atm)
        });
        let (max_active_drones, max_locked_targets, drone_ctrl_dist) =
            if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                let d = fit.get_character_effective_attribute(352, &ctx.sde).max(drones_lv);
                let t = fit
                    .get_character_effective_attribute(192, &ctx.sde)
                    .max(2.0 + target_mgmt_lv + adv_target_mgmt_lv);
                let c = fit.get_character_effective_attribute(458, &ctx.sde).max(20_000.0);
                (d, t, c)
            } else {
                (drones_lv, 2.0 + target_mgmt_lv + adv_target_mgmt_lv, 20_000.0)
            };
        char_attrs_map.insert(352, max_active_drones);
        char_attrs_map.insert(192, max_locked_targets);
        char_attrs_map.insert(458, drone_ctrl_dist);
        let mut char_attrs: Vec<(PyRep, PyRep)> = char_attrs_map
            .into_iter()
            .map(|(k, v)| (PyRep::Integer(i64::from(k)), PyRep::Real(v)))
            .collect();
        char_attrs.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
        let char_row = PyRep::keyval([
            ("itemID", PyRep::Integer(char_id)),
            ("invItem", char_inv_row),
            ("attributes", PyRep::Dict(char_attrs)),
            ("activeEffects", PyRep::Dict(Vec::new())),
            ("time", PyRep::Integer(now_ft)),
            ("wallclockTime", PyRep::Integer(now_ft)),
            ("description", PyRep::String(String::new())),
        ]);
        let char_brain = PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::Tuple(Vec::new()),
            PyRep::Tuple(Vec::new()),
            PyRep::Tuple(Vec::new()),
        ]);
        PyRep::Tuple(vec![PyRep::Dict(vec![(PyRep::Integer(char_id), char_row)]), char_brain])
    } else {
        PyRep::None
    };

    let ship_info = if prime_ship { PyRep::Dict(ship_info_entries) } else { PyRep::None };

    PyRep::keyval([
        ("shipID", PyRep::Integer(ship_id)),
        ("charID", PyRep::Integer(char_id)),
        ("activeStance", active_stance),
        ("shipAttributes", PyRep::Dict(entries)),
        ("moduleAttributes", PyRep::Dict(module_entries)),
        ("structureInfo", PyRep::None),
        ("shipInfo", ship_info),
        ("charInfo", char_info),
        ("shipModifiedCharAttribs", PyRep::None),
        ("systemWideEffectsOnShip", PyRep::Dict(Vec::new())),
        ("shipState", ship_state),
    ])
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_ecm_or_burst_jammer_activation(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    mod_type: &InvType,
    mod_state: &DogmaItemState,
    target_id_opt: Option<i64>,
    duration_s: f64,
) -> Result<(), BridgeRpcError> {
    // SDE sensor pairing: `idx 0..4` matches jammer attrs `[238 Grav, 239 Ladar, 240 Mag, 241 Radar]`
    // against target sensor attrs `[211 Grav, 209 Ladar, 210 Mag, 208 Radar]`.
    const SENSOR_ATTRS: [u16; 4] = [211, 209, 210, 208];

    if mod_type.group_id == 201 {
        let Some(target_id) = target_id_opt else {
            let _ = sys.remove_module_cycle(ship_id, module_id).await;
            return Err(BridgeRpcError {
                code: "TargetRequired".into(),
                message: "ECM module requires a locked target".into(),
            });
        };
        let mut distance_m = 0.0;
        if let (Ok(Some((_, ship_ball))), Ok(Some((_, target_ball)))) = (
            sys.get_ball_snapshot(ship_id).await,
            sys.get_ball_snapshot(target_id).await,
        ) {
            distance_m = (ship_ball.position - target_ball.position).length();
        }
        let target_type_id_opt = ctx
            .db
            .with_snapshot(|snap| snap.items.get(&target_id).map(|i| (i.owner_id, i.type_id)));
        let mut best_sensor_idx = 0usize;
        let mut target_sensor_strength = 0.0;
        if let Some((t_owner, t_tid)) = target_type_id_opt {
            if let Ok(t_fit) = ctx.get_or_rebuild_ship_fitting(t_owner, target_id) {
                for (idx, &s_attr) in SENSOR_ATTRS.iter().enumerate() {
                    let val = t_fit.ship.get_attribute(s_attr, &ctx.sde);
                    if val > target_sensor_strength {
                        target_sensor_strength = val;
                        best_sensor_idx = idx;
                    }
                }
            } else if let Some(t_type) = ctx.sde.types.get(&t_tid) {
                for (idx, &s_attr) in SENSOR_ATTRS.iter().enumerate() {
                    let val = t_type.default_attributes.get(&s_attr).copied().unwrap_or(0.0);
                    if val > target_sensor_strength {
                        target_sensor_strength = val;
                        best_sensor_idx = idx;
                    }
                }
            }
        } else if let Ok(Some((_, target_ball))) = sys.get_ball_snapshot(target_id).await {
            if let Some(t_type) = ctx.sde.types.get(&target_ball.type_id) {
                for (idx, &s_attr) in SENSOR_ATTRS.iter().enumerate() {
                    let val = t_type.default_attributes.get(&s_attr).copied().unwrap_or(0.0);
                    if val > target_sensor_strength {
                        target_sensor_strength = val;
                        best_sensor_idx = idx;
                    }
                }
            }
        }
        if target_sensor_strength <= 0.0 {
            target_sensor_strength = 12.0;
        }
        let jam_attr_id = 238u16 + (best_sensor_idx as u16);
        let jam_strength = mod_state.get_attribute(jam_attr_id, &ctx.sde);
        let optimal_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
        let falloff_range = mod_state.get_attribute(attr_ids::FALLOFF, &ctx.sde);
        let jam_chance = eve_dogma::compute_ecm_jam_chance(
            jam_strength,
            target_sensor_strength,
            distance_m,
            optimal_range,
            falloff_range,
        );
        let roll = super::csprng_unit_f64(ctx);
        let jam_duration_s = if duration_s > 0.0 { duration_s } else { 20.0 };
        if roll < jam_chance && jam_chance > 0.0 {
            let _ = sys.apply_ecm_jam(target_id, jam_duration_s).await;
            crate::services::notify_emitters_space_dungeons::emit_ecm_jam_landed(
                ctx,
                sys_id,
                char_id,
                ship_id,
                module_id,
                target_id,
                jam_duration_s,
            );
        }
    }

    if mod_type.group_id == 80 || mod_type.group_id == 353 {
        let burst_range = eve_dogma::compute_burst_jammer_radius_m(mod_type, mod_state, &ctx.sde);
        let sys_sec = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.modified_system_securities.get(&sys_id).copied()
        })
        .unwrap_or_else(|| {
            ctx.sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0)
        });
        let ship_pos = sys
            .get_ball_snapshot(ship_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
            .unwrap_or(DVec3::ZERO);
        let all_balls = sys.list_balls().await.unwrap_or_default();
        let mut unprovoked_neutral_victims: Vec<i64> = Vec::new();
        let mut legal_pvp_victims: Vec<i64> = Vec::new();
        let mut ship_candidates: Vec<(i64, usize, f64)> = Vec::new();
        let now_unix = ctx.now_unix_secs();
        for b in &all_balls {
            if b.id == ship_id || (b.position - ship_pos).length() > burst_range + 1e-3 {
                continue;
            }
            // Burst Jammer affects ONLY uncloaked, non-EWAR-immune Ships (`category_id == 6`) —
            // never wrecks, jetcans, drones, fighters, deployables, or structures (`R-BURST-ECM-01`).
            let is_ship_ball = ctx
                .sde
                .types
                .get(&b.type_id)
                .is_some_and(|t| t.category_id == 6);
            if !is_ship_ball {
                continue;
            }
            let cs_opt = sys.get_combat_state(b.id).await.ok().flatten();
            if cs_opt.as_ref().is_some_and(|cs| cs.is_cloaked) {
                continue;
            }
            let victim_char_opt = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .find(|c| c.active_ship_id == b.id && c.solar_system_id == sys_id)
                    .map(|c| c.character_id)
                    .or_else(|| {
                        snap.items
                            .get(&b.id)
                            .map(|i| i.owner_id)
                            .filter(|oid| snap.characters.contains_key(oid))
                    })
            });
            if let Some(v_char) = victim_char_opt {
                if crate::services::space_destiny::is_location_change_invulnerable(ctx, v_char, now_unix) {
                    continue;
                }
                if v_char != char_id {
                    let legal = {
                        let cw = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner());
                        cw.states.get(&v_char).is_some_and(|vs| {
                            vs.is_criminal(now_unix) || vs.is_suspect(now_unix)
                        })
                    } || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                        rt.has_limited_engagement_with(char_id, v_char, now_unix)
                            || rt.has_limited_engagement_with(v_char, char_id, now_unix)
                    }) || ctx.db.with_snapshot(|snap| {
                        crate::services::combat_crimewatch::is_legal_war_or_suspect_target(
                            snap, char_id, v_char, now_unix,
                        )
                    });
                    if !legal {
                        if !unprovoked_neutral_victims.contains(&v_char) {
                            unprovoked_neutral_victims.push(v_char);
                        }
                    } else if !legal_pvp_victims.contains(&v_char) {
                        legal_pvp_victims.push(v_char);
                    }
                }
            }
            let mut best_sensor_idx = 0usize;
            let mut target_sensor_strength = 0.0;
            let owner_for_fit = victim_char_opt.or_else(|| {
                ctx.db
                    .with_snapshot(|snap| snap.items.get(&b.id).map(|i| i.owner_id))
            });
            if let Some(t_owner) = owner_for_fit {
                if let Ok(t_fit) = ctx.get_or_rebuild_ship_fitting(t_owner, b.id) {
                    for (idx, &s_attr) in SENSOR_ATTRS.iter().enumerate() {
                        let val = t_fit.ship.get_attribute(s_attr, &ctx.sde);
                        if val > target_sensor_strength {
                            target_sensor_strength = val;
                            best_sensor_idx = idx;
                        }
                    }
                }
            }
            if target_sensor_strength <= 0.0 {
                if let Some(t_type) = ctx.sde.types.get(&b.type_id) {
                    for (idx, &s_attr) in SENSOR_ATTRS.iter().enumerate() {
                        let val = t_type.default_attributes.get(&s_attr).copied().unwrap_or(0.0);
                        if val > target_sensor_strength {
                            target_sensor_strength = val;
                            best_sensor_idx = idx;
                        }
                    }
                }
            }
            if target_sensor_strength <= 0.0 {
                target_sensor_strength = 12.0;
            }
            ship_candidates.push((b.id, best_sensor_idx, target_sensor_strength));
        }

        if !unprovoked_neutral_victims.is_empty() && sys_sec > 0.0 {
            let safety = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.safety_levels.get(&char_id).copied()
            })
            .unwrap_or(crate::services::combat_crimewatch::SafetyLevel::Red);
            if (sys_sec >= 0.45 && safety != crate::services::combat_crimewatch::SafetyLevel::Red)
                || (sys_sec < 0.45 && safety == crate::services::combat_crimewatch::SafetyLevel::Green)
            {
                let _ = sys.remove_module_cycle(ship_id, module_id).await;
                return Err(BridgeRpcError {
                    code: "SafetyBlocked".into(),
                    message: "Safety setting prevents activating Burst Jammer near neutral capsuleers in empire space".into(),
                });
            }
        }
        let jam_strengths = [
            mod_state.get_attribute(238, &ctx.sde),
            mod_state.get_attribute(239, &ctx.sde),
            mod_state.get_attribute(240, &ctx.sde),
            mod_state.get_attribute(241, &ctx.sde),
        ];
        let mut ship_eval_overrides = HashMap::with_capacity(ship_candidates.len());
        for (bid, s_idx, s_str) in ship_candidates {
            let ship_roll = super::csprng_unit_f64(ctx);
            ship_eval_overrides.insert(bid, (s_idx, s_str, ship_roll));
        }
        let jam_duration_s = if duration_s > 0.0 { duration_s } else { 20.0 };
        let jammed_ids = sys
            .apply_ecm_burst_ships(
                ship_id,
                burst_range,
                jam_strengths,
                jam_duration_s,
                0.5,
                Some(ship_eval_overrides),
            )
            .await
            .unwrap_or_default();
        for &jid in &jammed_ids {
            crate::services::notify_emitters_space_dungeons::emit_ecm_jam_landed(
                ctx,
                sys_id,
                char_id,
                ship_id,
                module_id,
                jid,
                jam_duration_s,
            );
            let jammed_owner_opt = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .find(|c| c.active_ship_id == jid)
                    .map(|c| c.character_id)
                    .or_else(|| {
                        snap.items
                            .get(&jid)
                            .map(|i| i.owner_id)
                            .filter(|oid| snap.characters.contains_key(oid))
                    })
            });
            if let Some(j_char) = jammed_owner_opt {
                ctx.notify(
                    crate::router::NotificationTarget::Character(j_char),
                    "OnTargetClear",
                    "charid",
                    vec![],
                );
            }
        }
        for &v_char in &legal_pvp_victims {
            crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                ctx, char_id, v_char, now_unix,
            );
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            let _ = cw.handle_combat_aggression(
                char_id, v_char, ship_id, sys_id, sys_sec, true, now_unix,
            );
            cw.get_or_create(v_char).pvp_timer_until_unix = now_unix.saturating_add(900);
        }
        for &v_char in &unprovoked_neutral_victims {
            crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
                ctx, char_id, v_char, now_unix,
            );
            let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
            cw.get_or_create(v_char).pvp_timer_until_unix = now_unix.saturating_add(900);
            let _ = cw.handle_combat_aggression(
                char_id,
                v_char,
                ship_id,
                sys_id,
                sys_sec,
                sys_sec <= 0.0,
                now_unix,
            );
        }
        if !unprovoked_neutral_victims.is_empty() && sys_sec > 0.0 {
            crate::services::combat_crimewatch::emit_crimewatch_timer_notifications(
                ctx, char_id, now_unix,
            );
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_pre_notify_instant_module_effects(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &InvType,
    mod_state: &DogmaItemState,
    is_overloaded: bool,
    target_id_opt: Option<i64>,
    duration_s: f64,
    kwargs: &HashMap<String, PyRep>,
) -> Result<Option<PyRep>, BridgeRpcError> {
    if matches!(mod_type.group_id, 201 | 80 | 353) {
        handle_ecm_or_burst_jammer_activation(
            ctx,
            sys,
            sys_id,
            char_id,
            ship_id,
            module_id,
            mod_type,
            mod_state,
            target_id_opt,
            duration_s,
        )
        .await?;
        return Ok(None);
    }
    if matches!(mod_type.group_id, 72 | 650) {
        if let Ok(Some((_, attacker_ball))) = sys.get_ball_snapshot(ship_id).await {
            let sys_sec = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.modified_system_securities.get(&sys_id).copied()
            })
            .unwrap_or_else(|| {
                ctx.sde
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| s.security_status)
                    .unwrap_or(0.0)
            });
            let now_unix = ctx.now_unix_secs();
            let mut m_eff = m.clone();
            if is_overloaded {
                m_eff.state = eve_dogma::ModuleState::Overloaded;
            }
            let action_res = if mod_type.group_id == 650 {
                crate::services::combat_crimewatch::activate_salvager_or_tractor_module(
                    ctx,
                    char_id,
                    ship_id,
                    sys_id,
                    module_id,
                    &m_eff,
                    mod_type,
                    &attacker_ball,
                    target_id_opt,
                    kwargs,
                )
                .await
            } else {
                crate::services::combat_crimewatch::activate_aoe_or_defender_module_with_cap(
                    ctx,
                    char_id,
                    ship_id,
                    sys_id,
                    sys_sec,
                    now_unix,
                    module_id,
                    &m_eff,
                    mod_type,
                    &attacker_ball,
                    target_id_opt,
                    kwargs,
                    false,
                )
                .await
            };
            if let Some(res) = action_res {
                return res.map(Some);
            }
        }
    }
    Ok(None)
}

async fn deactivate_repeating_cycle_after_action(
    ctx: &ServerContext,
    sys: &SystemHandle,
    owner_char_id: Option<i64>,
    ship_id: i64,
    module_id: i64,
    effect_name: &str,
) {
    let _ = sys.remove_module_cycle(ship_id, module_id).await;
    let _ = crate::services::notify_emitters_space_dungeons::flush_module_ewar_end(
        ctx,
        owner_char_id.unwrap_or(0),
        Some(sys.solar_system_id),
        ship_id,
        module_id,
    );
    crate::services::tactical_modules::remove_projected_remote_assistance(ctx, ship_id, module_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&module_id);
        snap.module_active_charges.remove(&module_id);
    });
    if let Some(cid) = owner_char_id {
        if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(cid, ship_id) {
            let _ = fit.deactivate_module(module_id, &ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);
        }
        ctx.notify(
            crate::router::NotificationTarget::Character(cid),
            "OnGodmaShipEffect",
            "charid",
            vec![
                PyRep::Integer(module_id),
                PyRep::String(effect_name.to_string()),
                PyRep::Bool(false),
            ],
        );
    }
}

/// Executes cycle-end wreck salvaging (`group 1122`) and auto-repeating (`repeat != 0`) AoE / tractor
/// cycle-start pulses queued by `step_ballpark_and_cycles` (`R-AOE-SALVAGE-CYCLE-01`).
pub(crate) async fn execute_pending_cycle_actions(ctx: &ServerContext, sys_id: u32) {
    let actions = ctx.systems.take_pending_cycle_actions(sys_id);
    execute_pending_cycle_actions_list(ctx, sys_id, actions).await;
}

pub(crate) async fn execute_pending_cycle_actions_list(
    ctx: &ServerContext,
    sys_id: u32,
    actions: Vec<crate::system_actor::PendingCycleAction>,
) {
    if actions.is_empty() {
        return;
    }
    let sys = ctx.systems.get_or_spawn(sys_id);
    let empty_kw = HashMap::new();
    for action in actions {
        match action {
            crate::system_actor::PendingCycleAction::SalvageEnd {
                char_id,
                ship_id,
                module_id,
                module_type_id,
                wreck_id,
                repeat,
                effect_name,
                next_cycle_cap_deducted,
                deducted_cap_gj,
            } => {
                let res = crate::combat::drones_salvage_tractor::execute_wreck_salvage_cycle(
                    ctx,
                    sys_id,
                    char_id,
                    ship_id,
                    wreck_id,
                    Some(module_type_id),
                )
                .await;
                if repeat == 0 {
                    if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                        let _ = fit.deactivate_module(module_id, &ctx.sde);
                        ctx.store_ship_fitting(ship_id, fit);
                    }
                } else {
                    let keep_repeating = matches!(
                        &res,
                        Ok(rep)
                            if rep.dict_get("salvaged").and_then(|v| v.as_bool()) == Some(false)
                                && rep.dict_get("reason").and_then(|v| v.as_str())
                                    == Some("CycleRollFailed")
                    );
                    if keep_repeating {
                        let dur_s = ctx
                            .sde
                            .types
                            .get(&module_type_id)
                            .and_then(|t| t.default_attributes.get(&73).copied())
                            .unwrap_or(10_000.0)
                            / 1000.0;
                        ctx.record_module_reactivation_delay(ship_id, module_id, dur_s);
                    } else {
                        deactivate_repeating_cycle_after_action(
                            ctx,
                            &sys,
                            Some(char_id),
                            ship_id,
                            module_id,
                            &effect_name,
                        )
                        .await;
                        if next_cycle_cap_deducted && deducted_cap_gj > 0.0 {
                            if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                                let _ = sys
                                    .set_capacitor(
                                        ship_id,
                                        (cs.capacitor + deducted_cap_gj).min(cs.max_capacitor),
                                    )
                                    .await;
                            }
                        }
                    }
                }
            }
            crate::system_actor::PendingCycleAction::LanceBeamFired {
                attacker_char_id,
                attacker_ship_id,
                attacker_ship_type_id,
                module_id: _,
                module_type_id,
                hits,
            } => {
                let now_unix = ctx.now_unix_secs();
                let mut msg_tuples = Vec::new();
                for (victim_id, applied_dmg) in hits {
                    crate::services::combat_crimewatch::record_killmail_assist_action(
                        ctx,
                        victim_id,
                        attacker_char_id,
                        attacker_ship_type_id,
                        module_type_id,
                        applied_dmg,
                        false,
                        now_unix,
                    );
                    let dmg_dict = PyRep::Dict(vec![
                        (PyRep::String("damage".into()), PyRep::Real(applied_dmg)),
                        (PyRep::String("source".into()), PyRep::Integer(attacker_ship_id)),
                        (PyRep::String("attackerID".into()), PyRep::Integer(attacker_ship_id)),
                        (PyRep::String("target".into()), PyRep::Integer(victim_id)),
                        (PyRep::String("target_ID".into()), PyRep::Integer(victim_id)),
                        (
                            PyRep::String("owner".into()),
                            PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(attacker_char_id)]),
                        ),
                        (PyRep::String("weapon".into()), PyRep::Integer(module_type_id as i64)),
                        (PyRep::String("hitQuality".into()), PyRep::Integer(5)),
                        (PyRep::String("attackType".into()), PyRep::String("me".into())),
                        (PyRep::String("isBanked".into()), PyRep::Bool(false)),
                    ]);
                    ctx.notify(
                        crate::router::NotificationTarget::Character(attacker_char_id),
                        "OnDamageMessage",
                        "charid",
                        vec![dmg_dict.clone()],
                    );
                    crate::services::notify_emitters_ess_bounties::emit_combat_message(
                        ctx,
                        attacker_char_id,
                        if applied_dmg > 0.0 { "AttackHit" } else { "AttackMiss" },
                        victim_id,
                        applied_dmg,
                        module_type_id,
                        false,
                    );
                    msg_tuples.push(PyRep::Tuple(vec![PyRep::String("AttackHit".into()), dmg_dict]));
                    let victim_char_opt = ctx.db.with_snapshot(|snap| {
                        snap.characters
                            .values()
                            .find(|c| c.active_ship_id == victim_id)
                            .map(|c| c.character_id)
                            .or_else(|| {
                                snap.items
                                    .get(&victim_id)
                                    .map(|i| i.owner_id)
                                    .filter(|oid| snap.characters.contains_key(oid))
                            })
                    });
                    if let Some(victim_char_id) = victim_char_opt {
                        crate::services::notify_emitters_ess_bounties::emit_combat_message(
                            ctx,
                            victim_char_id,
                            if applied_dmg > 0.0 { "AttackedBy" } else { "AttackMissBy" },
                            attacker_ship_id,
                            applied_dmg,
                            module_type_id,
                            false,
                        );
                        let vic_dict = PyRep::Dict(vec![
                            (PyRep::String("damage".into()), PyRep::Real(applied_dmg)),
                            (PyRep::String("source".into()), PyRep::Integer(attacker_ship_id)),
                            (PyRep::String("attackerID".into()), PyRep::Integer(attacker_ship_id)),
                            (PyRep::String("target".into()), PyRep::Integer(victim_id)),
                            (PyRep::String("target_ID".into()), PyRep::Integer(victim_id)),
                            (
                                PyRep::String("owner".into()),
                                PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(attacker_char_id)]),
                            ),
                            (PyRep::String("weapon".into()), PyRep::Integer(module_type_id as i64)),
                            (PyRep::String("hitQuality".into()), PyRep::Integer(5)),
                            (PyRep::String("attackType".into()), PyRep::String("other".into())),
                            (PyRep::String("isBanked".into()), PyRep::Bool(false)),
                        ]);
                        ctx.notify(
                            crate::router::NotificationTarget::Character(victim_char_id),
                            "OnDamageMessage",
                            "charid",
                            vec![vic_dict.clone()],
                        );
                        ctx.notify(
                            crate::router::NotificationTarget::Character(victim_char_id),
                            "OnDamageMessages",
                            "charid",
                            vec![PyRep::List(vec![PyRep::Tuple(vec![
                                PyRep::String("AttackedBy".into()),
                                vic_dict,
                            ])])],
                        );
                    }
                }
                if !msg_tuples.is_empty() {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(attacker_char_id),
                        "OnDamageMessages",
                        "charid",
                        vec![PyRep::List(msg_tuples)],
                    );
                }
            }
            crate::system_actor::PendingCycleAction::CycleWeaponDamage {
                attacker_char_id,
                attacker_ship_id,
                attacker_ship_type_id,
                target_id,
                target_char_id,
                module_type_id,
                applied_dmg,
                notified,
            } => {
                if let Some(acid) = attacker_char_id.filter(|&c| c > 0) {
                    crate::services::combat_crimewatch::record_killmail_assist_action(
                        ctx,
                        target_id,
                        acid,
                        attacker_ship_type_id,
                        module_type_id,
                        applied_dmg,
                        false,
                        ctx.now_unix_secs(),
                    );
                    let active_slot_charges: Vec<eve_db::InventoryItemRecord> =
                        ctx.db.with_snapshot(|snap| {
                            let weapon_flags: Vec<InventoryFlag> = snap
                                .items
                                .values()
                                .filter(|i| {
                                    i.location_id == attacker_ship_id
                                        && i.type_id == module_type_id
                                        && snap.module_active_states.contains(&i.item_id)
                                })
                                .map(|i| i.flag)
                                .collect();
                            snap.items
                                .values()
                                .filter(|i| {
                                    i.location_id == attacker_ship_id
                                        && weapon_flags.contains(&i.flag)
                                        && i.type_id != module_type_id
                                        && i.quantity > 0
                                        && ctx
                                            .sde
                                            .types
                                            .get(&i.type_id)
                                            .is_some_and(|t| t.category_id == 8)
                                })
                                .cloned()
                                .collect()
                        });
                    for chg in active_slot_charges {
                        ctx.emit_item_change_routed(
                            acid,
                            &chg,
                            vec![(9, PyRep::Integer(i64::from(chg.quantity.saturating_add(1))))],
                        );
                        emit_charge_sublocation_quantity_change(
                            ctx,
                            acid,
                            attacker_ship_id,
                            chg.flag,
                            chg.type_id,
                            chg.quantity.saturating_add(1),
                            chg.quantity,
                        );
                    }
                }
                if !notified && applied_dmg > 0.0 {
                    let owner_cid = attacker_char_id.unwrap_or(0);
                    if let Some(acid) = attacker_char_id.filter(|&c| c > 0) {
                        let dmg_dict = PyRep::Dict(vec![
                            (PyRep::String("damage".into()), PyRep::Real(applied_dmg)),
                            (PyRep::String("source".into()), PyRep::Integer(attacker_ship_id)),
                            (PyRep::String("attackerID".into()), PyRep::Integer(attacker_ship_id)),
                            (PyRep::String("target".into()), PyRep::Integer(target_id)),
                            (PyRep::String("target_ID".into()), PyRep::Integer(target_id)),
                            (
                                PyRep::String("owner".into()),
                                PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(acid)]),
                            ),
                            (PyRep::String("weapon".into()), PyRep::Integer(module_type_id as i64)),
                            (PyRep::String("hitQuality".into()), PyRep::Integer(3)),
                            (PyRep::String("attackType".into()), PyRep::String("me".into())),
                            (PyRep::String("isWrecking".into()), PyRep::Bool(false)),
                            (PyRep::String("isBanked".into()), PyRep::Bool(false)),
                        ]);
                        ctx.notify(
                            crate::router::NotificationTarget::Character(acid),
                            "OnDamageMessage",
                            "charid",
                            vec![dmg_dict.clone()],
                        );
                        ctx.notify(
                            crate::router::NotificationTarget::Character(acid),
                            "OnDamageMessages",
                            "charid",
                            vec![PyRep::List(vec![PyRep::Tuple(vec![
                                PyRep::String("AttackHit".into()),
                                dmg_dict,
                            ])])],
                        );
                    }
                    if let Some(vcid) = target_char_id.filter(|&c| c > 0 && c != owner_cid) {
                        let vic_dict = PyRep::Dict(vec![
                            (PyRep::String("damage".into()), PyRep::Real(applied_dmg)),
                            (PyRep::String("source".into()), PyRep::Integer(attacker_ship_id)),
                            (PyRep::String("attackerID".into()), PyRep::Integer(attacker_ship_id)),
                            (PyRep::String("target".into()), PyRep::Integer(target_id)),
                            (PyRep::String("target_ID".into()), PyRep::Integer(target_id)),
                            (
                                PyRep::String("owner".into()),
                                PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Integer(owner_cid)]),
                            ),
                            (PyRep::String("weapon".into()), PyRep::Integer(module_type_id as i64)),
                            (PyRep::String("hitQuality".into()), PyRep::Integer(3)),
                            (PyRep::String("attackType".into()), PyRep::String("other".into())),
                            (PyRep::String("isWrecking".into()), PyRep::Bool(false)),
                            (PyRep::String("isBanked".into()), PyRep::Bool(false)),
                        ]);
                        ctx.notify(
                            crate::router::NotificationTarget::Character(vcid),
                            "OnDamageMessage",
                            "charid",
                            vec![vic_dict.clone()],
                        );
                        ctx.notify(
                            crate::router::NotificationTarget::Character(vcid),
                            "OnDamageMessages",
                            "charid",
                            vec![PyRep::List(vec![PyRep::Tuple(vec![
                                PyRep::String("AttackedBy".into()),
                                vic_dict,
                            ])])],
                        );
                    }
                }
            }
            crate::system_actor::PendingCycleAction::RepeatCycleStart {
                char_id,
                ship_id,
                module_id,
                module_type_id,
                target_id,
                duration_s,
                is_overloaded,
                effect_name,
            } => {
                ctx.record_module_reactivation_delay(ship_id, module_id, duration_s);
                let Some(mod_type) = ctx.sde.types.get(&module_type_id).cloned() else {
                    continue;
                };
                let fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
                let mod_state = fit_opt
                    .as_ref()
                    .map(|f| {
                        f.build_effective_module_state(
                            module_id,
                            module_type_id,
                            is_overloaded,
                            &ctx.sde,
                        )
                    })
                    .unwrap_or_else(|| DogmaItemState::from_sde(char_id, module_type_id, &ctx.sde));
                let m_eff = fit_opt
                    .as_ref()
                    .and_then(|f| f.modules.get(&module_id).cloned())
                    .unwrap_or(eve_dogma::FittedModule {
                        item_id: module_id,
                        type_id: module_type_id,
                        flag_id: 27,
                        state: if is_overloaded {
                            eve_dogma::ModuleState::Overloaded
                        } else {
                            eve_dogma::ModuleState::Active
                        },
                        charge_type_id: None,
                        charge_quantity: 0,
                        damage_hp: 0.0,
                        max_hp: 40.0,
                        dynamic_attribute_overrides: HashMap::new(),
                    });
                let pulse_res = run_pre_notify_instant_module_effects(
                    ctx, &sys, sys_id, char_id, ship_id, module_id, &m_eff, &mod_type, &mod_state, is_overloaded, target_id, duration_s, &empty_kw,
                ).await;
                if pulse_res.is_err() {
                    deactivate_repeating_cycle_after_action(ctx, &sys, Some(char_id), ship_id, module_id, &effect_name).await;
                }
            }
            crate::system_actor::PendingCycleAction::SyncOverloadState { char_id, ship_id, module_id, is_overloaded } => {
                if let Some(cid) = char_id.or_else(|| ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.owner_id))) {
                    if let Ok(mut fit) = ctx.get_or_rebuild_ship_fitting(cid, ship_id) {
                        if let Some(fm) = fit.modules.get_mut(&module_id).filter(|m| m.state != eve_dogma::ModuleState::Offline) {
                            fm.state = if is_overloaded { eve_dogma::ModuleState::Overloaded } else { eve_dogma::ModuleState::Active };
                        }
                        fit.recalculate_stats(&ctx.sde);
                        ctx.store_ship_fitting(ship_id, fit);
                        crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(ctx, cid, sys_id, ship_id).await;
                    }
                }
            }
            crate::system_actor::PendingCycleAction::InvalidateShipFitting { char_id, ship_id } => {
                ctx.ship_fittings.write().unwrap_or_else(|e| e.into_inner()).remove(&ship_id);
                if let Some(cid) = char_id.or_else(|| ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.owner_id))) {
                    let _ = ctx.get_or_rebuild_ship_fitting(cid, ship_id);
                    crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(ctx, cid, sys_id, ship_id).await;
                }
            }
        }
    }
}

pub(crate) async fn evaluate_module_cycle_aggression_or_assistance(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    group_id: u32,
    target_id_opt: Option<i64>,
) -> Result<(), BridgeRpcError> {
    let Some(target_id) = target_id_opt else { return Ok(()); };
    if matches!(
        group_id,
        52 | 53 | 55 | 65 | 68 | 71 | 74 | 201 | 208 | 291 | 379 | 506..=512
            | 524 | 771 | 899 | 1245 | 1672 | 1674 | 1986 | 4060 | 4500 | 4805 | 4807
    ) {
        let now_unix = ctx.now_unix();
        let sys_sec = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.modified_system_securities.get(&sys_id).copied()
        }).unwrap_or_else(|| ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0));
        let trigger_lowsec_sentry = !(32_000_000..=33_000_000).contains(&sys_id)
            && sys_sec > 0.0
            && sys_sec < 0.45
            && {
                let (vcid_opt, is_mobile_or_jetcan) = ctx.db.with_snapshot(|snap| {
                    let vcid = if snap.characters.contains_key(&target_id) {
                        Some(target_id)
                    } else {
                        snap.characters.values().find(|c| c.active_ship_id == target_id).map(|c| c.character_id)
                            .or_else(|| snap.items.get(&target_id).and_then(|it| snap.characters.contains_key(&it.owner_id).then_some(it.owner_id)))
                    };
                    let anchored = snap.sp6_lifecycle.space_object_meta.get(&target_id).is_some_and(|m| m.anchor_start_unix.is_some());
                    let it_tid = snap.items.get(&target_id).map(|i| i.type_id).unwrap_or(0);
                    let is_mobile = crate::services::mobile_deployables::is_mobile_structure(&ctx.sde, it_tid);
                    (vcid, (it_tid == 23 && !anchored) || is_mobile)
                });
                if let Some(vcid) = vcid_opt.filter(|&v| v != char_id && !is_mobile_or_jetcan) {
                    let v_suspect_or_crim = {
                        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
                        let st = cw.get_or_create(vcid);
                        st.is_suspect(now_unix) || st.is_criminal(now_unix)
                    };
                    let has_le = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| rt.has_limited_engagement_with(vcid, char_id, now_unix));
                    let is_legal = ctx.db.with_snapshot(|snap| {
                        crate::services::combat_crimewatch::is_legal_war_or_suspect_target(snap, char_id, vcid, now_unix)
                    }) || v_suspect_or_crim || has_le;
                    !is_legal
                } else {
                    false
                }
            };
        let is_pod = sys.get_ball_snapshot(target_id).await.ok().flatten().is_some_and(|(_, b)| b.type_id == 670)
            || ctx.db.with_snapshot(|s| s.items.get(&target_id).is_some_and(|i| i.type_id == 670));
        crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_pvp_aggression(
            ctx, char_id, ship_id, sys_id, target_id, is_pod,
        )?;
        if trigger_lowsec_sentry {
            if let Some((_, ab)) = sys.get_ball_snapshot(ship_id).await.ok().flatten() {
                if let Some(origin) = crate::services::security_mgr::nearest_sentry_gun_origin(ctx, sys_id, ab.position) {
                    let _ = crate::services::combat_crimewatch::crimewatch_timers::step_sentry_gun_grid_aggression(
                        ctx, sys_id, char_id, ship_id, origin, true,
                    ).await;
                }
            }
        }
        Ok(())
    } else if matches!(group_id, 41 | 67 | 325 | 585 | 1697 | 1698 | 2018) {
        crate::services::combat_crimewatch::crimewatch_timers::evaluate_and_record_remote_assistance(
            ctx, char_id, ship_id, sys_id, target_id,
        )
    } else {
        Ok(())
    }
}

pub(crate) async fn try_handle_mercden_remote_shield_repair(
    ctx: &ServerContext,
    sys: &SystemHandle,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
    m: &eve_dogma::FittedModule,
    mod_type: &InvType,
    fit: &eve_dogma::ShipFittingEngine,
    method: &str,
    target_id: i64,
) -> Result<Option<PyRep>, BridgeRpcError> {
    let locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
    if !locks.iter().any(|l| l.target_id == target_id && l.locked) {
        return Err(BridgeRpcError { code: "TargetNotLocked".into(), message: format!("Target {target_id} is not locked") });
    }
    if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
        if cs.is_cloaked {
            return Err(BridgeRpcError { code: "Cloaked".into(), message: "Cannot activate module while cloaked".into() });
        }
        if cs.is_tethered {
            return Err(BridgeRpcError { code: "Tethered".into(), message: "Cannot activate remote assistance module while tethered".into() });
        }
    }
    let is_overloaded = method == "Overload" || m.state == eve_dogma::ModuleState::Overloaded;
    let mod_state = fit.build_effective_module_state(module_id, m.type_id, is_overloaded, &ctx.sde);
    let opt_range = mod_state.get_attribute(attr_ids::MAX_RANGE, &ctx.sde);
    let falloff = eve_dogma::get_module_falloff(&mod_state, &ctx.sde);
    let max_range = if opt_range > 0.0 { opt_range + falloff.max(0.0) } else { 40_000.0 };
    let attacker_pos = sys.get_ball_snapshot(ship_id).await.ok().flatten().map(|(_, b)| b.position);
    let target_pos = ctx.db.with_snapshot(|s| s.structures.get(&target_id).map(|st| DVec3::from_array(st.position)));
    if let (Some(apos), Some(tpos)) = (attacker_pos, target_pos) {
        if apos.distance(tpos) > max_range {
            return Err(BridgeRpcError { code: "TargetOutOfRange".into(), message: format!("Target {target_id} is out of module range") });
        }
    }
    let cap_need = mod_state.get_attribute(attr_ids::CAPACITOR_NEED, &ctx.sde);
    if cap_need > 0.0 {
        let cs = sys.get_combat_state(ship_id).await.ok().flatten();
        let cur_cap = cs.as_ref().map(|c| c.capacitor).unwrap_or(0.0);
        if cur_cap < cap_need {
            return Err(BridgeRpcError { code: "NotEnoughCapacitor".into(), message: "Not enough capacitor to activate module".into() });
        }
        let _ = sys.set_capacitor(ship_id, cur_cap - cap_need).await;
    }
    let rep_hp = mod_type.default_attributes.get(&68).copied().unwrap_or(1_500.0);
    let (pct, enabled) = crate::services::sov_gameplay::apply_mercenary_den_shield_repair(ctx, sys_id, ship_id, target_id, rep_hp).await?;
    let _ = (char_id, m.type_id);
    Ok(Some(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("targetID", PyRep::Integer(target_id)),
        ("shieldPct", PyRep::Integer(pct as i64)),
        ("denEnabled", PyRep::Bool(enabled)),
    ])))
}

/// Builds the V24 `activeEffects` dict for a fitted module (`const.effectOnline = 16` when online).
pub(crate) fn online_module_active_effects(
    mod_id: i64,
    char_id: i64,
    ship_id: i64,
    now_ft: i64,
    is_online: bool,
) -> PyRep {
    if !is_online {
        return PyRep::Dict(Vec::new());
    }
    PyRep::Dict(vec![(
        PyRep::Integer(16),
        PyRep::List(vec![
            PyRep::Integer(mod_id),
            PyRep::Integer(char_id),
            PyRep::Integer(ship_id),
            PyRep::None,
            PyRep::None,
            PyRep::List(Vec::new()),
            PyRep::Integer(16),
            PyRep::Integer(now_ft),
            PyRep::Real(-1.0),
            PyRep::Integer(0),
        ]),
    )])
}

#[allow(dead_code)]
fn online_module_active_effects_for_attrs(
    mod_id: i64,
    char_id: i64,
    ship_id: i64,
    now_ft: i64,
    mod_attrs_rep: &PyRep,
) -> PyRep {
    let is_online = matches!(
        mod_attrs_rep,
        PyRep::Dict(d) if d.iter().any(|(k, v)| k.as_i64() == Some(2) && v.as_f64().unwrap_or(0.0) > 0.0)
    );
    online_module_active_effects(mod_id, char_id, ship_id, now_ft, is_online)
}

#[cfg(test)]
#[path = "fitting_module_cycles_helpers_tests.rs"]
mod fitting_module_cycles_helpers_tests;

#[cfg(test)]
#[path = "fitting_module_cycles_helpers_edges_tests.rs"]
mod fitting_module_cycles_helpers_edges_tests;
