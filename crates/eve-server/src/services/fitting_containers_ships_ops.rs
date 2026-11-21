//! `fitting_containers_ships_ops` — Extracted inventory broker, item locking,
//! encoded item decoding, and validated charge-unloading handlers (`#394`).

#![forbid(unsafe_code)]

use super::*;

pub(crate) use eve_db::{
    AlscLogEntryRecord as AlscLogEntry, ContainerConfigRecord as ContainerConfig,
    ShipAccessConfigRecord as ShipAccessConfig,
};
#[cfg(test)]
pub(crate) use eve_db::MAX_ALSC_LOGS_PER_CONTAINER;

#[inline]
pub(crate) fn map_kv_err(err: String) -> BridgeRpcError {
    <BridgeRpcError as eve_db::FromWalError>::from_wal_error(err)
}

#[inline]
pub(crate) fn container_password_snap(
    snap: &eve_db::WorldStateSnapshot,
    container_id: i64,
    which: i64,
) -> Option<String> {
    let sub_key = if which == 2 { "2" } else { "1" };
    snap.kv(eve_db::Ns::ContainerPassword)
        .get::<String>(container_id, sub_key)
        .filter(|s| !s.is_empty())
}

pub(crate) fn set_container_password_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    container_id: i64,
    which: i64,
    password: &str,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    let sub_key = if which == 2 { "2" } else { "1" };
    if password.is_empty() {
        snap.kv_mut(eve_db::Ns::ContainerPassword)
            .remove(container_id, sub_key);
        Ok(())
    } else {
        snap.kv_mut(eve_db::Ns::ContainerPassword)
            .set_with_unix(container_id, sub_key, &password, now_unix)
            .map_err(map_kv_err)
    }
}

#[inline]
pub(crate) fn container_config_snap(
    snap: &eve_db::WorldStateSnapshot,
    container_id: i64,
) -> ContainerConfig {
    snap.kv(eve_db::Ns::ContainerConfig)
        .get::<ContainerConfig>(container_id, "")
        .unwrap_or_default()
}

pub(crate) fn set_container_config_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    container_id: i64,
    cfg: &ContainerConfig,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    snap.kv_mut(eve_db::Ns::ContainerConfig)
        .set_with_unix(container_id, "", cfg, now_unix)
        .map_err(map_kv_err)
}

#[inline]
pub(crate) fn is_container_unlocked_snap(
    snap: &eve_db::WorldStateSnapshot,
    target_id: i64,
    char_id: i64,
) -> bool {
    snap.kv(eve_db::Ns::ContainerUnlockedChar)
        .get::<bool>(target_id, &char_id.to_string())
        .unwrap_or(false)
}

pub(crate) fn set_container_unlocked_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    target_id: i64,
    char_id: i64,
    unlocked: bool,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    let sub_key = char_id.to_string();
    if unlocked {
        snap.kv_mut(eve_db::Ns::ContainerUnlockedChar)
            .set_with_unix(target_id, sub_key, &true, now_unix)
            .map_err(map_kv_err)
    } else {
        snap.kv_mut(eve_db::Ns::ContainerUnlockedChar)
            .remove(target_id, &sub_key);
        Ok(())
    }
}

pub(crate) fn clear_container_unlocks_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    target_id: i64,
) -> usize {
    snap.kv_mut(eve_db::Ns::ContainerUnlockedChar)
        .clear_entity(target_id)
}

pub(crate) fn set_alsc_item_locked_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    item_id: i64,
    locked: bool,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    if locked {
        snap.kv_mut(eve_db::Ns::AlscLockedItem)
            .set_with_unix(item_id, "", &true, now_unix)
            .map_err(map_kv_err)
    } else {
        snap.kv_mut(eve_db::Ns::AlscLockedItem)
            .remove(item_id, "");
        Ok(())
    }
}

#[inline]
pub(crate) fn container_alsc_logs_snap(
    snap: &eve_db::WorldStateSnapshot,
    container_id: i64,
) -> Vec<AlscLogEntry> {
    snap.kv(eve_db::Ns::ContainerAlscLogs)
        .get::<Vec<AlscLogEntry>>(container_id, "")
        .unwrap_or_default()
}

pub(crate) fn append_alsc_log_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    container_id: i64,
    entry: AlscLogEntry,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    eve_db::push_alsc_log_in_kv(snap, container_id, entry, now_unix).map_err(map_kv_err)
}

#[inline]
pub(crate) fn ship_access_config_snap(
    snap: &eve_db::WorldStateSnapshot,
    ship_id: i64,
) -> ShipAccessConfig {
    snap.kv(eve_db::Ns::ShipAccessConfig)
        .get::<ShipAccessConfig>(ship_id, "")
        .unwrap_or_default()
}

#[inline]
pub(crate) fn ship_password_snap(
    snap: &eve_db::WorldStateSnapshot,
    ship_id: i64,
) -> Option<String> {
    ship_access_config_snap(snap, ship_id)
        .password
        .filter(|s| !s.is_empty())
}

pub(crate) fn set_ship_access_config_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    ship_id: i64,
    cfg: &ShipAccessConfig,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    snap.kv_mut(eve_db::Ns::ShipAccessConfig)
        .set_with_unix(ship_id, "", cfg, now_unix)
        .map_err(map_kv_err)
}

#[inline]
pub(crate) fn custom_item_flag_snap(
    snap: &eve_db::WorldStateSnapshot,
    item_id: i64,
) -> Option<u16> {
    snap.kv(eve_db::Ns::CustomItemFlag).get::<u16>(item_id, "")
}

pub(crate) fn set_custom_item_flag_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    item_id: i64,
    flag_u16: Option<u16>,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    if let Some(f) = flag_u16 {
        snap.kv_mut(eve_db::Ns::CustomItemFlag)
            .set_with_unix(item_id, "", &f, now_unix)
            .map_err(map_kv_err)
    } else {
        snap.kv_mut(eve_db::Ns::CustomItemFlag).remove(item_id, "");
        Ok(())
    }
}

pub(crate) fn apply_add_move_kv_side_effects_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    pending_alsc_take: Option<&(i64, AlscLogEntry)>,
    item_ids: &[i64],
    flag_opt: Option<u16>,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    if let Some((cid, entry)) = pending_alsc_take {
        append_alsc_log_snap(snap, *cid, entry.clone(), now_unix)?;
    }
    for &id in item_ids {
        if flag_opt.is_some() || custom_item_flag_snap(snap, id).is_some() {
            set_custom_item_flag_snap(snap, id, flag_opt, now_unix)?;
        }
    }
    Ok(())
}

fn validate_clear_charges_in_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    ship_id: i64,
    char_skills: &std::collections::HashMap<u32, u8>,
) -> Result<(Vec<i64>, Vec<i64>), BridgeRpcError> {
    let Some(ship) = snap.items.get(&ship_id) else {
        return Err(BridgeRpcError {
            code: "ShipNotFound".into(),
            message: format!("Ship {ship_id} not found"),
            client_error: None,
        });
    };
    let ship_cat = ctx.sde.types.get(&ship.type_id).map(|t| t.category_id).unwrap_or(0);
    if ship.owner_id != char_id || ship_cat != 6 {
        return Err(BridgeRpcError {
            code: "InvalidFittingTarget".into(),
            message: format!("Target {ship_id} is not a ship owned by character {char_id}"),
            client_error: None,
        });
    }
    let charges: Vec<(i64, InventoryFlag, f64, i32, bool)> = snap
        .items
        .values()
        .filter(|it| {
            it.owner_id == char_id
                && it.location_id == ship_id
                && matches!(it.flag as u16, 11..=34)
                && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.category_id == 8)
        })
        .map(|it| {
            let vol = ctx.sde.types.get(&it.type_id).map(|t| t.volume).unwrap_or(0.0);
            let locked = it.flag == InventoryFlag::Locked
                || snap.is_item_or_ancestor_locked_or_impounded(it.item_id)
                || snap.is_item_in_alsc_lock(it.item_id);
            (it.item_id, it.flag, vol, it.quantity.max(0), locked)
        })
        .collect();
    if ship.flag == InventoryFlag::Locked
        || snap.is_item_or_ancestor_locked_or_impounded(ship_id)
        || charges.iter().any(|&(_, _, _, _, locked)| locked)
    {
        return Err(BridgeRpcError {
            code: "ItemLocked".into(),
            message: format!("Ship {ship_id} is locked"),
            client_error: None,
        });
    }

    let occupied_flags: std::collections::HashSet<InventoryFlag> =
        charges.iter().map(|&(_, flg, _, _, _)| flg).collect();

    let mod_ids: Vec<i64> = snap
        .items
        .values()
        .filter(|it| {
            it.location_id == ship_id
                && occupied_flags.contains(&it.flag)
                && ctx.sde.types.get(&it.type_id).is_none_or(|t| t.category_id != 8)
        })
        .map(|it| it.item_id)
        .collect();

    for &mid in &mod_ids {
        if snap.module_active_states.contains(&mid) {
            return Err(BridgeRpcError {
                code: "ModuleIsActive".into(),
                message: format!("Cannot unload charges while module {mid} is active"),
                client_error: None,
            });
        }
    }

    let mut engine = eve_dogma::ShipFittingEngine::from_sde(ship_id, ship.type_id, &ctx.sde)
        .with_character_skills(char_skills.clone());
    for it in snap.items.values() {
        if it.location_id == ship_id
            && matches!(it.flag as u16, 11..=34 | 92..=94 | 125..=132)
            && ctx.sde.types.get(&it.type_id).is_none_or(|t| t.category_id != 8)
        {
            let online = snap.module_online_states.get(&it.item_id).copied().unwrap_or(true);
            let _ = engine.fit_module_with_state(it.item_id, it.type_id, it.flag as u16, online, &ctx.sde);
        }
    }
    let eval_ctx = engine.evaluated_context(&ctx.sde);
    let cargo_cap = eval_ctx
        .ship
        .get_attribute(eve_sde::attr_ids::CAPACITY, &ctx.sde)
        .max(0.0);
    let existing_cargo_vol: f64 = snap
        .items
        .values()
        .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
        .map(|i| {
            ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.0)
                * (i.quantity.max(0) as f64)
        })
        .sum();
    let incoming_vol: f64 = charges
        .iter()
        .map(|&(_, _, vol, qty, _)| vol * (qty as f64))
        .sum();
    if incoming_vol > 0.0 && existing_cargo_vol + incoming_vol > cargo_cap + 1e-4 {
        return Err(BridgeRpcError {
            code: "CapacityExceeded".into(),
            message: format!(
                "Ship cargo capacity {cargo_cap} m³ exceeded: total {:.1} m³",
                existing_cargo_vol + incoming_vol
            ),
            client_error: None,
        });
    }

    let c_ids: Vec<i64> = charges.into_iter().map(|(id, _, _, _, _)| id).collect();
    Ok((c_ids, mod_ids))
}

pub(crate) async fn handle_clear_charges(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = args
        .first()
        .and_then(|a| a.as_i64())
        .filter(|&id| id > 0)
        .or_else(|| bound_entry.map(|b| b.object_id).filter(|&id| id > 0))
        .or(session.ship_id)
        .unwrap_or(0);
    if ship_id <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidFittingTarget".into(),
            message: "ClearCharges requires a valid shipID".into(),
            client_error: None,
        });
    }

    let char_skills = ctx.character_skill_levels(char_id);
    let (pre_charge_ids, mod_ids_in_charge_slots) = ctx
        .db
        .with_snapshot(|snap| validate_clear_charges_in_snap(ctx, snap, char_id, ship_id, &char_skills))?;
    if pre_charge_ids.is_empty() && mod_ids_in_charge_slots.is_empty() {
        return Ok(PyRep::None);
    }

    if let Some(sys_id) = session.solar_system_id {
        let sys = ctx.systems.get_or_spawn(sys_id);
        for &mid in &mod_ids_in_charge_slots {
            if sys.has_active_module_cycle_sync(ship_id, mid) {
                return Err(BridgeRpcError {
                    code: "ModuleIsActive".into(),
                    message: format!("Cannot unload charges while module {mid} is active"),
                    client_error: None,
                });
            }
        }
    }

    ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
        let (charge_ids, mod_ids) =
            validate_clear_charges_in_snap(ctx, snap, char_id, ship_id, &char_skills)?;
        for &mid in &mod_ids {
            snap.module_active_charges.remove(&mid);
        }
        for cid in charge_ids {
            if !snap.items.get(&cid).is_some_and(|c| {
                c.owner_id == char_id
                    && c.location_id == ship_id
                    && matches!(c.flag as u16, 11..=34)
            }) {
                continue;
            }
            if let Some(it) = snap.items.get_mut(&cid) {
                it.singleton = false;
            }
            let mut spec = eve_db::TransferItemSpec::move_whole(
                Some(char_id),
                Some(char_id),
                Some(char_id),
                ship_id,
                InventoryFlag::Cargo,
            );
            spec.reject_fitted = false;
            spec.merge_at_destination = true;
            spec.active_ship_id = Some(ship_id);
            snap.transfer_inventory_item(cid, spec)
                .map_err(|e| BridgeRpcError::custom_info(e.to_string()))?;
        }
        Ok(())
    })?;

    ctx.invalidate_ship_fitting(ship_id);
    Ok(PyRep::None)
}

pub(crate) async fn handle_ext_tail(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("invbroker", "FitFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Fitting requires being docked".into(),
                    client_error: None,
                }));
            };
            if station_id < 1_000_000_000_000
                && !crate::services::station_structure::npc_station_offers_service(
                    &ctx.sde,
                    station_id,
                    17,
                )
            {
                return Some(Err(BridgeRpcError {
                    code: "StationServiceUnavailable".into(),
                    message: format!("Station {station_id} does not offer fitting facilities"),
                    client_error: None,
                }));
            }
            let Some(ship_id) = kwargs
                .get("shipID")
                .and_then(|v| v.as_i64())
                .filter(|&id| id > 0)
                .or_else(|| args.first().and_then(|v| v.as_i64()).filter(|&id| id > 0))
                .or(session.ship_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFittingTarget".into(),
                    message: "FitFitting requires a valid shipID".into(),
                    client_error: None,
                }));
            };

            let mut desired_items: Vec<(eve_sde::TypeId, InventoryFlag, i32)> = Vec::new();
            let mut expected_ship_type_id: Option<u32> = None;

            if let Some(fid) = kwargs
                .get("fittingID")
                .and_then(|v| v.as_i64())
                .or_else(|| args.get(1).and_then(|v| v.as_i64()))
            {
                let Some(saved) = ctx.db.with_snapshot(|s| s.saved_fittings.get(&fid).cloned())
                else {
                    return Some(Err(BridgeRpcError {
                        code: "FittingNotFound".into(),
                        message: format!("Fitting {fid} not found"),
                        client_error: None,
                    }));
                };
                let authorized = saved.owner_id == char_id
                    || session
                        .corporation_id
                        .is_some_and(|cid| cid > 0 && saved.owner_id == cid)
                    || session
                        .alliance_id
                        .is_some_and(|aid| aid > 0 && saved.owner_id == aid)
                    || crate::services::inventory_fitting::fitting_mgr_service::is_community_fitting_owner(
                        saved.owner_id,
                    );
                if !authorized {
                    return Some(Err(BridgeRpcError {
                        code: "AccessDenied".into(),
                        message: format!("Not authorized to access fitting {fid}"),
                        client_error: None,
                    }));
                }
                desired_items = saved.items;
                if saved.ship_type_id > 0 {
                    expected_ship_type_id = Some(saved.ship_type_id);
                }
            } else if let Some(PyRep::List(entries) | PyRep::Tuple(entries)) =
                kwargs.get("fitData").or_else(|| args.get(1))
            {
                let (mut occupied_slots, low_cap, med_cap, hi_cap, rig_cap, sub_cap) =
                    ctx.db.with_snapshot(|snap| {
                        let mut occ = std::collections::HashSet::<u16>::new();
                        let ship_type_id = snap
                            .items
                            .get(&ship_id)
                            .map(|s| s.type_id)
                            .unwrap_or(0);
                        for it in snap.items.values() {
                            if it.location_id == ship_id
                                && matches!(it.flag as u16, 11..=34 | 92..=94 | 125..=132)
                                && ctx
                                    .sde
                                    .types
                                    .get(&it.type_id)
                                    .map(|t| t.category_id)
                                    != Some(8)
                            {
                                occ.insert(it.flag as u16);
                            }
                        }
                        let mut engine =
                            eve_dogma::ShipFittingEngine::from_sde(ship_id, ship_type_id, &ctx.sde);
                        for it in snap.items.values() {
                            if it.location_id == ship_id && matches!(it.flag as u16, 125..=132) {
                                let _ = engine.fit_module_with_state(
                                    it.item_id,
                                    it.type_id,
                                    it.flag as u16,
                                    true,
                                    &ctx.sde,
                                );
                            }
                        }
                        let low = engine
                            .ship
                            .get_attribute(eve_sde::attr_ids::LOW_SLOTS, &ctx.sde)
                            .max(0.0) as u16;
                        let med = engine
                            .ship
                            .get_attribute(eve_sde::attr_ids::MED_SLOTS, &ctx.sde)
                            .max(0.0) as u16;
                        let hi = engine
                            .ship
                            .get_attribute(eve_sde::attr_ids::HI_SLOTS, &ctx.sde)
                            .max(0.0) as u16;
                        let rig = engine
                            .ship
                            .get_attribute(eve_sde::attr_ids::RIG_SLOTS, &ctx.sde)
                            .max(0.0) as u16;
                        let sub = if ctx
                            .sde
                            .types
                            .get(&ship_type_id)
                            .is_some_and(|t| t.group_id == 963)
                        {
                            4_u16
                        } else {
                            0_u16
                        };
                        (occ, low, med, hi, rig, sub)
                    });

                for entry in entries {
                    let (PyRep::Tuple(seq) | PyRep::List(seq)) = entry else {
                        continue;
                    };
                    if seq.len() >= 3 {
                        let (Some(tid), Some(flg)) = (
                            seq.first().and_then(|v| v.as_i64()),
                            seq.get(1).and_then(|v| v.as_i64()),
                        ) else {
                            continue;
                        };
                        let Ok(tid_u32) = u32::try_from(tid) else {
                            return Some(Err(BridgeRpcError {
                                code: "ItemNotFound".into(),
                                message: format!("Invalid typeID {tid}"),
                                client_error: None,
                            }));
                        };
                        let Ok(u16_flg) = u16::try_from(flg) else {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidFittingFlag".into(),
                                message: format!("Invalid flag {flg}"),
                                client_error: None,
                            }));
                        };
                        let is_allowed_flag = matches!(
                            u16_flg,
                            5 | 87
                                | 11..=34
                                | 92..=94
                                | 125..=132
                                | 158
                                | 133..=151
                                | 177
                                | 179
                                | 181
                                | 182
                                | 185
                        );
                        if !is_allowed_flag {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidFittingFlag".into(),
                                message: format!("Flag {flg} is not a valid fitting destination"),
                                client_error: None,
                            }));
                        }
                        let qty_i64 = seq.get(2).and_then(|v| v.as_i64()).unwrap_or(1);
                        let Some(qty) = i32::try_from(qty_i64).ok().filter(|&q| q > 0) else {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidQuantity".into(),
                                message: format!("Invalid quantity {qty_i64}"),
                                client_error: None,
                            }));
                        };
                        if matches!(u16_flg, 11..=34 | 92..=94 | 125..=132)
                            && ctx.sde.types.get(&tid_u32).map(|t| t.category_id) != Some(8)
                        {
                            occupied_slots.insert(u16_flg);
                        }
                        desired_items.push((tid_u32, InventoryFlag::from_u16(u16_flg), qty));
                    } else if seq.len() == 2 {
                        let (Some(tid), Some(qty_i64)) = (
                            seq.first().and_then(|v| v.as_i64()),
                            seq.get(1).and_then(|v| v.as_i64()),
                        ) else {
                            continue;
                        };
                        let Ok(tid_u32) = u32::try_from(tid) else {
                            return Some(Err(BridgeRpcError {
                                code: "ItemNotFound".into(),
                                message: format!("Invalid typeID {tid}"),
                                client_error: None,
                            }));
                        };
                        let Some(qty) = i32::try_from(qty_i64).ok().filter(|&q| q > 0) else {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidQuantity".into(),
                                message: format!("Invalid quantity {qty_i64}"),
                                client_error: None,
                            }));
                        };
                        let Some(item_type) = ctx.sde.types.get(&tid_u32) else {
                            return Some(Err(BridgeRpcError {
                                code: "ItemNotFound".into(),
                                message: format!("Type {tid_u32} not found in SDE"),
                                client_error: None,
                            }));
                        };
                        if item_type.category_id == 18 {
                            desired_items.push((tid_u32, InventoryFlag::DroneBay, qty));
                            continue;
                        }
                        if item_type.category_id == 8 {
                            desired_items.push((tid_u32, InventoryFlag::Cargo, qty));
                            continue;
                        }
                        let effs = &item_type.effect_ids;
                        let slot_range = if effs.contains(&11) {
                            Some(11..(11 + low_cap.min(8)))
                        } else if effs.contains(&13) {
                            Some(19..(19 + med_cap.min(8)))
                        } else if effs.contains(&12) {
                            Some(27..(27 + hi_cap.min(8)))
                        } else if effs.contains(&2663) {
                            Some(92..(92 + rig_cap.min(3)))
                        } else if effs.contains(&3772) {
                            Some(125..(125 + sub_cap.min(4)))
                        } else {
                            None
                        };
                        if let Some(range) = slot_range {
                            for _ in 0..qty {
                                let Some(free_flag) =
                                    range.clone().find(|f| !occupied_slots.contains(f))
                                else {
                                    return Some(Err(BridgeRpcError {
                                        code: "UserError".into(),
                                        message: "SlotIndexExceedsHullSlots".into(),
                                        client_error: None,
                                    }));
                                };
                                occupied_slots.insert(free_flag);
                                desired_items.push((tid_u32, InventoryFlag::from_u16(free_flag), 1));
                            }
                        } else {
                            desired_items.push((tid_u32, InventoryFlag::Cargo, qty));
                        }
                    }
                }
            }

            let (_fitted_count, missing, _moved) =
                match crate::services::fitting_saved_apply::validate_and_apply_fitting(
                    ctx,
                    char_id,
                    station_id,
                    &[ship_id],
                    expected_ship_type_id,
                    &desired_items,
                    true,
                ) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            let mut missing_rows: Vec<(eve_sde::TypeId, i64)> = missing.into_iter().collect();
            missing_rows.sort_by_key(|&(tid, _)| tid);
            Some(Ok(PyRep::List(
                missing_rows
                    .into_iter()
                    .map(|(tid, qty)| {
                        PyRep::Tuple(vec![
                            PyRep::Integer(tid as i64),
                            PyRep::Integer(qty),
                        ])
                    })
                    .collect(),
            )))
        }

        ("invbroker", "GetAssetWorth") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (asset_worth, plex_worth) = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|it| it.owner_id == char_id)
                    .fold((0.0, 0.0), |(assets, plex), it| {
                        let val = ctx
                            .sde
                            .types
                            .get(&it.type_id)
                            .map(|t| t.base_price)
                            .unwrap_or(0.0)
                            * (it.quantity.max(1) as f64);
                        if it.type_id == 44_992 {
                            (assets, plex + val)
                        } else {
                            (assets + val, plex)
                        }
                    })
            });
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Real(asset_worth),
                PyRep::Real(plex_worth),
            ])))
        }

        ("invbroker", "GetPriceForItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(l) | PyRep::Tuple(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            };
            let total: f64 = ctx.db.with_snapshot(|snap| {
                item_ids
                    .iter()
                    .filter_map(|id| snap.items.get(id))
                    .filter(|it| it.owner_id == char_id)
                    .map(|it| {
                        ctx.sde
                            .types
                            .get(&it.type_id)
                            .map(|t| t.base_price)
                            .unwrap_or(0.0)
                            * (it.quantity.max(1) as f64)
                    })
                    .sum()
            });
            Some(Ok(PyRep::Real(total)))
        }

        ("invbroker", "HasExistingPasswordSet") => Some(handle_has_existing_password_set(
            ctx,
            session,
            bound_entry,
            args,
            kwargs,
        )),

        ("invbroker", "ImportExportWithPlanet") => {
            Some(handle_import_export_with_planet(ctx, session, bound_entry, args, kwargs))
        }

        ("invbroker", "ListIncludingContainers") | ("invbroker", "ListStationItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let st_filter = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
                .filter(|&id| id >= 60_000_000);
            let items = ctx.db.get_items_for_character(char_id);
            let rows: Vec<Vec<PyRep>> = items
                .iter()
                .filter(|it| match st_filter {
                    Some(st_id) => {
                        it.location_id == st_id
                            || items
                                .iter()
                                .any(|p| p.item_id == it.location_id && p.location_id == st_id)
                    }
                    None => true,
                })
                .map(|it| item_to_packed_cells(ctx, it))
                .collect();
            Some(Ok(DBRowDescriptor::inv_item_header().build_crowset(rows)))
        }

        ("invbroker", "ListStations") => Some(
            crate::services::character_skills::character_clones_implants::handle_list_stations(
                ctx, session, args, kwargs,
            ),
        ),

        ("invbroker", "Reprocess") => {
            let (_, _) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.or(session.structure_id).is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Reprocessing requires being docked at a station or structure".into(),
                    client_error: None,
                }));
            }
            let has_items = match args.first() {
                Some(PyRep::List(list)) => list.iter().any(|a| a.as_i64().is_some()),
                Some(first) => first.as_i64().is_some(),
                None => false,
            };
            if !has_items {
                return Some(Ok(PyRep::List(Vec::new())));
            }
            crate::services::space_entity_mining::handle(
                ctx,
                session,
                "reprocessingSvc",
                "Reprocess",
                args,
                bound_entry,
            )
            .await
        }

        ("invbroker", "RetrievePassword") => {
            Some(handle_retrieve_password(ctx, session, bound_entry, args, kwargs))
        }

        ("itemLocking", "GetLockedItemLocations") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = session.corporation_id.unwrap_or(0);
            let locs: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                let mut set = std::collections::BTreeSet::new();
                for item in snap.items.values() {
                    let in_job = snap.industry_jobs.values().any(|j| j.status == "Active" && j.blueprint_item_id == item.item_id);
                    let vote_locked = corp_id > 0 && item.owner_id == corp_id && item.flag == InventoryFlag::Locked && !in_job;
                    let container_or_escrow = (item.owner_id == char_id || (corp_id > 0 && item.owner_id == corp_id))
                        && (snap.is_item_in_alsc_lock(item.item_id) || snap.contracts.contains_key(&item.location_id));
                    if vote_locked || container_or_escrow {
                        set.insert(item.location_id);
                    }
                }
                set.into_iter().map(PyRep::Integer).collect()
            });
            Some(Ok(PyRep::List(locs)))
        }

        ("itemLocking", "GetItemsByLocation") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = session.corporation_id.unwrap_or(0);
            let loc_id = args.first().and_then(|a| a.as_i64());
            let locked_entries: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|item| {
                        if !loc_id.map(|l| item.location_id == l).unwrap_or(true) {
                            return false;
                        }
                        let in_job = snap.industry_jobs.values().any(|j| j.status == "Active" && j.blueprint_item_id == item.item_id);
                        let vote_locked = corp_id > 0 && item.owner_id == corp_id && item.flag == InventoryFlag::Locked && !in_job;
                        let container_or_escrow = (item.owner_id == char_id || (corp_id > 0 && item.owner_id == corp_id))
                            && (snap.is_item_in_alsc_lock(item.item_id) || snap.contracts.contains_key(&item.location_id));
                        vote_locked || container_or_escrow
                    })
                    .map(|item| (
                        PyRep::Integer(item.item_id),
                        PyRep::keyval([
                            ("itemID", PyRep::Integer(item.item_id)), ("typeID", PyRep::Integer(item.type_id as i64)),
                            ("locationID", PyRep::Integer(item.location_id)), ("ownerID", PyRep::Integer(item.owner_id)), ("flagID", PyRep::Integer(item.flag as i64)),
                        ]),
                    ))
                    .collect()
            });
            Some(Ok(PyRep::Dict(locked_entries)))
        }

        ("encodedItems", "DecodeItem") => {
            let (_, _char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let (item_id_opt, fallback_tid_opt) = match args.first() {
                Some(PyRep::Integer(id)) => (Some(*id), args.get(1).and_then(|a| a.as_i64())),
                Some(PyRep::Tuple(v) | PyRep::List(v)) => (v.first().and_then(|x| x.as_i64()), v.get(1).and_then(|x| x.as_i64())),
                Some(PyRep::String(s)) => {
                    let mut parts = s.split(':');
                    (parts.next().and_then(|x| x.trim().parse::<i64>().ok()), parts.next().and_then(|x| x.trim().parse::<i64>().ok()))
                }
                _ => (kwargs.get("itemID").and_then(|v| v.as_i64()), kwargs.get("typeID").and_then(|v| v.as_i64())),
            };
            let Some(raw_id) = item_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DecodeItem requires itemID or encoded link".into(),
                    client_error: None,
                }));
            };
            let item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&raw_id).cloned());
            let type_id = item_opt
                .as_ref()
                .map(|i| i.type_id)
                .or_else(|| fallback_tid_opt.map(|t| t as TypeId))
                .unwrap_or(raw_id as TypeId);
            let Some(type_info) = ctx.sde.types.get(&type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "TypeNotFound".into(),
                    message: format!("Encoded item type {type_id} not found"),
                    client_error: None,
                }));
            };
            let custom_name = ctx.db.with_snapshot(|snap| snap.item_custom_names.get(&raw_id).cloned()).unwrap_or_default();
            let mut merged_attrs = type_info.default_attributes.clone();
            let mut dyn_entries = Vec::new();
            let mut muta_id_rep = PyRep::None;
            let mut is_bpc = false;
            let mut owner_rep = PyRep::None;
            if let Some(ref item) = item_opt {
                owner_rep = PyRep::Integer(item.owner_id);
                is_bpc = item.is_blueprint_copy;
                if let Some(m) = item.source_mutaplasmid_id { muta_id_rep = PyRep::Integer(m as i64); }
                for (&aid, &val) in &item.dynamic_attributes {
                    merged_attrs.insert(aid, val);
                    dyn_entries.push((PyRep::Integer(aid as i64), PyRep::Real(val)));
                }
            }
            let attr_entries: Vec<(PyRep, PyRep)> = merged_attrs
                .into_iter().map(|(k, v)| (PyRep::Integer(k as i64), PyRep::Real(v))).collect();
            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(raw_id)), ("typeID", PyRep::Integer(type_id as i64)),
                ("typeName", PyRep::String(type_info.name.clone())), ("customName", PyRep::String(custom_name)),
                ("ownerID", owner_rep), ("isBlueprintCopy", PyRep::Bool(is_bpc)),
                ("sourceMutaplasmidID", muta_id_rep), ("dynamicAttributes", PyRep::Dict(dyn_entries)),
                ("attributes", PyRep::Dict(attr_entries)),
            ])))
        }

        _ => handle_ships_mastery_and_fitting_ext(ctx, session, bound_entry, service, method, args, kwargs).await,
    }
}

fn parse_pi_xfer_entries(
    rep: Option<&PyRep>,
    field_name: &str,
) -> Result<std::collections::BTreeMap<i64, u64>, BridgeRpcError> {
    let mut out = std::collections::BTreeMap::new();
    match rep {
        None | Some(PyRep::None) => Ok(out),
        Some(PyRep::Dict(entries)) => {
            for (k, v) in entries {
                let key = k.as_i64().filter(|&id| id > 0).ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodity".into(),
                    message: format!("{field_name} keys must be positive integer item/type IDs"),
                    client_error: None,
                })?;
                let qty = v.as_i64().filter(|&q| q > 0).ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodityQuantity".into(),
                    message: format!("{field_name} quantities must be positive integers"),
                    client_error: None,
                })?;
                let slot = out.entry(key).or_insert(0);
                *slot = slot.checked_add(qty as u64).ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodityQuantity".into(),
                    message: format!("{field_name} quantity overflow"),
                    client_error: None,
                })?;
            }
            Ok(out)
        }
        Some(PyRep::KeyVal(entries)) => {
            for (k, v) in entries {
                let key = k.parse::<i64>().ok().filter(|&id| id > 0).ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodity".into(),
                    message: format!("{field_name} keys must be positive integer item/type IDs"),
                    client_error: None,
                })?;
                let qty = v.as_i64().filter(|&q| q > 0).ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodityQuantity".into(),
                    message: format!("{field_name} quantities must be positive integers"),
                    client_error: None,
                })?;
                let slot = out.entry(key).or_insert(0);
                *slot = slot.checked_add(qty as u64).ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodityQuantity".into(),
                    message: format!("{field_name} quantity overflow"),
                    client_error: None,
                })?;
            }
            Ok(out)
        }
        _ => Err(BridgeRpcError {
            code: "InvalidTransferArguments".into(),
            message: format!("{field_name} must be a dict or None"),
            client_error: None,
        }),
    }
}

struct PlannedPocoImports {
    consumptions: Vec<(i64, i32)>,
    pin_additions: Vec<(TypeId, u64)>,
    total_vol_m3: f64,
    total_base_isk: f64,
}

fn is_in_poco_or_ship_import_location(
    it: &eve_db::InventoryItemRecord,
    customs_office_loc_id: i64,
    planet_id: i64,
    orbital_id_opt: Option<i64>,
    ship_id_opt: Option<i64>,
) -> bool {
    let in_poco_hangar = (it.location_id == customs_office_loc_id
        || it.location_id == planet_id
        || orbital_id_opt.is_some_and(|oid| it.location_id == oid))
        && it.flag == InventoryFlag::Hangar;
    let in_ship_cargo = ship_id_opt.is_some_and(|sid| it.location_id == sid)
        && matches!(
            it.flag,
            InventoryFlag::Cargo | InventoryFlag::SpecializedPlanetaryCommoditiesHold
        );
    in_poco_hangar || in_ship_cargo
}

fn plan_poco_import_transfers(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    import_map: &std::collections::BTreeMap<i64, u64>,
    customs_office_loc_id: i64,
    planet_id: i64,
    orbital_id_opt: Option<i64>,
    ship_id_opt: Option<i64>,
    commodity_opts: &eve_db::ConsumeExactOptions,
) -> Result<PlannedPocoImports, BridgeRpcError> {
    use crate::services::industry_pi::{
        ensure_valid_pi_commodity_type, is_valid_pi_commodity_type, pi_commodity_tier_base_cost,
    };

    let mut consumptions = Vec::new();
    let mut pin_additions = Vec::with_capacity(import_map.len());
    let mut reserved_by_item: HashMap<i64, i32> = HashMap::new();
    let mut total_vol_m3 = 0.0_f64;
    let mut total_base_isk = 0.0_f64;

    for (&key_id, &qty) in import_map {
        let qty_i32 = i32::try_from(qty).map_err(|_| BridgeRpcError {
            code: "InvalidCommodityQuantity".into(),
            message: format!("Import quantity {qty} overflows i32"),
            client_error: None,
        })?;
        let tid = if let Some(item) = snap.items.get(&key_id) {
            if item.owner_id != char_id {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Import item {key_id} not found or not owned"),
                    client_error: None,
                });
            }
            ensure_valid_pi_commodity_type(&ctx.sde, item.type_id)?;
            if item.flag == InventoryFlag::Locked
                || snap.is_item_or_ancestor_locked_or_impounded(item.item_id)
            {
                return Err(BridgeRpcError {
                    code: "ItemLocked".into(),
                    message: format!("Import item {key_id} is locked"),
                    client_error: None,
                });
            }
            if !is_in_poco_or_ship_import_location(
                item,
                customs_office_loc_id,
                planet_id,
                orbital_id_opt,
                ship_id_opt,
            ) {
                return Err(BridgeRpcError {
                    code: "ColocationRequired".into(),
                    message: format!(
                        "Import item {key_id} must be in customs office hangar or active ship cargo"
                    ),
                    client_error: None,
                });
            }
            snap.validate_item_for_consumption(item, commodity_opts)
                .map_err(|err| BridgeRpcError {
                    code: "InvalidCommodityItemState".into(),
                    message: format!("Import item {key_id} cannot be imported ({err})"),
                    client_error: None,
                })?;
            let reserved = reserved_by_item.get(&key_id).copied().unwrap_or(0);
            let remaining_avail = u64::try_from(item.quantity.saturating_sub(reserved)).unwrap_or(0);
            if remaining_avail < qty {
                return Err(BridgeRpcError {
                    code: "InsufficientCommodityQuantity".into(),
                    message: format!(
                        "Import item {key_id} has quantity {}, cannot import {qty}",
                        remaining_avail
                    ),
                    client_error: None,
                });
            }
            reserved_by_item.insert(key_id, reserved.saturating_add(qty_i32));
            consumptions.push((key_id, qty_i32));
            pin_additions.push((item.type_id, qty));
            item.type_id
        } else {
            let Some(tid) = u32::try_from(key_id)
                .ok()
                .filter(|&t| ctx.sde.types.contains_key(&t) || is_valid_pi_commodity_type(&ctx.sde, t))
            else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Import commodity/item {key_id} not found"),
                    client_error: None,
                });
            };
            ensure_valid_pi_commodity_type(&ctx.sde, tid)?;

            let mut matching_ids: Vec<i64> = Vec::new();
            let mut first_locked_id: Option<i64> = None;
            let mut first_invalid_state_err: Option<String> = None;
            for it in snap.items.values() {
                if it.owner_id != char_id
                    || it.type_id != tid
                    || it.quantity <= 0
                    || !is_in_poco_or_ship_import_location(
                        it,
                        customs_office_loc_id,
                        planet_id,
                        orbital_id_opt,
                        ship_id_opt,
                    )
                {
                    continue;
                }
                if it.flag == InventoryFlag::Locked
                    || snap.is_item_or_ancestor_locked_or_impounded(it.item_id)
                {
                    if first_locked_id.is_none() {
                        first_locked_id = Some(it.item_id);
                    }
                    continue;
                }
                if let Err(err) = snap.validate_item_for_consumption(it, commodity_opts) {
                    if first_invalid_state_err.is_none() {
                        first_invalid_state_err = Some(err.to_string());
                    }
                    continue;
                }
                matching_ids.push(it.item_id);
            }
            matching_ids.sort();

            let avail_u128: u128 = matching_ids
                .iter()
                .filter_map(|iid| snap.items.get(iid))
                .filter_map(|it| {
                    let res = reserved_by_item.get(&it.item_id).copied().unwrap_or(0);
                    u64::try_from(it.quantity.saturating_sub(res)).ok()
                })
                .map(u128::from)
                .sum();
            if avail_u128 < u128::from(qty) {
                if let Some(reason) = first_invalid_state_err {
                    return Err(BridgeRpcError {
                        code: "InvalidCommodityItemState".into(),
                        message: format!(
                            "Commodity type {tid} cannot be imported because matching items are non-stackable, locked, or modified ({reason})"
                        ),
                        client_error: None,
                    });
                }
                if let Some(locked_id) = first_locked_id {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!(
                            "Commodity type {tid} stack {locked_id} is locked"
                        ),
                        client_error: None,
                    });
                }
                if avail_u128 == 0 {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!(
                            "Commodity type {tid} not found in customs office hangar or ship cargo"
                        ),
                        client_error: None,
                    });
                }
                return Err(BridgeRpcError {
                    code: "InsufficientCommodityQuantity".into(),
                    message: format!(
                        "Available quantity ({avail_u128}) of type {tid} is less than import quantity ({qty})"
                    ),
                    client_error: None,
                });
            }

            let mut rem = qty;
            for iid in matching_ids {
                if rem == 0 {
                    break;
                }
                let Some(it) = snap.items.get(&iid) else {
                    continue;
                };
                let res = reserved_by_item.get(&iid).copied().unwrap_or(0);
                let avail_i32 = it.quantity.saturating_sub(res);
                let Ok(avail_u64) = u64::try_from(avail_i32) else {
                    continue;
                };
                let take_u64 = avail_u64.min(rem);
                if take_u64 == 0 {
                    continue;
                }
                let take_i32 = i32::try_from(take_u64).map_err(|_| BridgeRpcError {
                    code: "InvalidCommodityQuantity".into(),
                    message: format!("Import quantity {take_u64} overflows i32"),
                    client_error: None,
                })?;
                reserved_by_item.insert(iid, res.saturating_add(take_i32));
                consumptions.push((iid, take_i32));
                rem -= take_u64;
            }
            if rem > 0 {
                return Err(BridgeRpcError {
                    code: "InsufficientCommodityQuantity".into(),
                    message: format!("Unable to allocate {qty} units of commodity type {tid}"),
                    client_error: None,
                });
            }
            pin_additions.push((tid, qty));
            tid
        };

        let unit_vol = ctx
            .sde
            .types
            .get(&tid)
            .map(|t| t.volume.max(0.005))
            .unwrap_or(0.38);
        total_vol_m3 += unit_vol * (qty as f64);
        total_base_isk += pi_commodity_tier_base_cost(&ctx.sde, tid) * (qty as f64);
    }

    Ok(PlannedPocoImports {
        consumptions,
        pin_additions,
        total_vol_m3,
        total_base_isk,
    })
}

fn handle_import_export_with_planet(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    use crate::services::industry_pi::{
        commit_poco_commodity_tax, ensure_valid_pi_commodity_type, is_launchpad_pin,
        load_pi_network, pi_commodity_tier_base_cost, pin_contents_volume_m3,
        pin_storage_capacity_m3, prepare_poco_commodity_tax, resolve_orbital_and_planet,
        save_pi_network,
    };

    let (_, char_id) = ServerContext::require_character(session)?;

    let (co_id_opt, spaceport_pin_id, import_rep, export_rep) = if args.len() >= 3
        && args.first().and_then(|a| a.as_i64()).is_some()
        && args.get(1).and_then(|a| a.as_i64()).is_some()
        && matches!(
            args.get(2),
            Some(PyRep::Dict(_) | PyRep::KeyVal(_) | PyRep::None)
        ) {
        (
            args.first().and_then(|a| a.as_i64()),
            args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
            args.get(2),
            args.get(3),
        )
    } else {
        (
            bound_entry
                .map(|b| b.object_id)
                .or_else(|| kwargs.get("customsOfficeID").and_then(|v| v.as_i64())),
            args.first()
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("spaceportPinID").and_then(|v| v.as_i64()))
                .unwrap_or(0),
            args.get(1).or_else(|| kwargs.get("importData")),
            args.get(2).or_else(|| kwargs.get("exportData")),
        )
    };

    if spaceport_pin_id <= 0 {
        return Err(BridgeRpcError {
            code: "PinNotFound".into(),
            message: "ImportExportWithPlanet requires a valid spaceportPinID".into(),
            client_error: None,
        });
    }

    let import_map = parse_pi_xfer_entries(import_rep, "importData")?;
    let export_map = parse_pi_xfer_entries(export_rep, "exportData")?;
    if import_map.is_empty() && export_map.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidTransferArguments".into(),
            message: "ImportExportWithPlanet requires non-empty importData or exportData".into(),
            client_error: None,
        });
    }

    let current_sys_id = session.solar_system_id;
    let ship_id_opt = session.ship_id;
    let notify_loc_id = ctx.db.try_with_mut_snapshot(|snap| -> Result<i64, BridgeRpcError> {
        let (orbital_id_opt, planet_id) = if let Some(raw_co_id) = co_id_opt {
            let (orb_opt, pid) = resolve_orbital_and_planet(snap, raw_co_id);
            if snap.planetary_colonies.contains_key(&(char_id, pid)) {
                (orb_opt, pid)
            } else {
                // If bound to a customs office structure whose name is not `Orbital:<pid>`,
                // try locating the caller's colony containing `spaceport_pin_id` on that planet or system.
                let found_pid = snap
                    .planetary_colonies
                    .iter()
                    .find(|&(&(cid, _), col)| {
                        cid == char_id
                            && load_pi_network(snap, char_id, col.planet_id, col.command_center_level)
                                .pins
                                .iter()
                                .any(|p| p.pin_id == spaceport_pin_id)
                    })
                    .map(|(&(_, pid), _)| pid);
                let Some(matched_pid) = found_pid else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!(
                            "No planetary colony found for character {char_id} matching customs office {raw_co_id}"
                        ),
                        client_error: None,
                    });
                };
                if orb_opt.is_none()
                    && !snap.structures.get(&raw_co_id).is_some_and(|st| {
                        snap.planetary_colonies
                            .get(&(char_id, matched_pid))
                            .is_some_and(|c| c.solar_system_id == st.solar_system_id)
                    })
                    && raw_co_id != matched_pid
                {
                    return Err(BridgeRpcError {
                        code: "InvalidCustomsOffice".into(),
                        message: format!("Object {raw_co_id} is not a customs office for planet {matched_pid}"),
                        client_error: None,
                    });
                }
                (orb_opt.or_else(|| snap.structures.contains_key(&raw_co_id).then_some(raw_co_id)), matched_pid)
            }
        } else {
            let found_pid = snap
                .planetary_colonies
                .iter()
                .find(|&(&(cid, _), col)| {
                    cid == char_id
                        && load_pi_network(snap, char_id, col.planet_id, col.command_center_level)
                            .pins
                            .iter()
                            .any(|p| p.pin_id == spaceport_pin_id)
                })
                .map(|(&(_, pid), _)| pid)
                .ok_or_else(|| BridgeRpcError {
                    code: "ColonyNotFound".into(),
                    message: format!(
                        "No planetary colony found for character {char_id} and spaceport pin {spaceport_pin_id}"
                    ),
                    client_error: None,
                })?;
            resolve_orbital_and_planet(snap, found_pid)
        };

        let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
            return Err(BridgeRpcError {
                code: "ColonyNotFound".into(),
                message: format!("No colony found for character {char_id} on planet {planet_id}"),
                client_error: None,
            });
        };
        if current_sys_id != Some(col.solar_system_id) {
            return Err(BridgeRpcError {
                code: "ColonyNotInCurrentSystem".into(),
                message: format!(
                    "Planetary colony {planet_id} is in solar system {}, not {:?}",
                    col.solar_system_id, current_sys_id
                ),
                client_error: None,
            });
        }

        let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
        let pin_idx = net
            .pins
            .iter()
            .position(|p| p.pin_id == spaceport_pin_id)
            .ok_or_else(|| BridgeRpcError {
                code: "PinNotFound".into(),
                message: format!("Spaceport pin {spaceport_pin_id} not found on planet {planet_id}"),
                client_error: None,
            })?;
        if !is_launchpad_pin(&ctx.sde, net.pins[pin_idx].type_id) {
            return Err(BridgeRpcError {
                code: "InvalidPinTypeForLaunch".into(),
                message: format!(
                    "Pin {spaceport_pin_id} (type {}) is not a planetary launchpad/spaceport",
                    net.pins[pin_idx].type_id
                ),
                client_error: None,
            });
        }

        let customs_office_loc_id = orbital_id_opt.unwrap_or(planet_id);
        let commodity_opts = eve_db::ConsumeExactOptions {
            active_ship_id: ship_id_opt,
            ..eve_db::ConsumeExactOptions::for_character_commodity(char_id)
        };
        let planned_imports = plan_poco_import_transfers(
            ctx,
            snap,
            char_id,
            &import_map,
            customs_office_loc_id,
            planet_id,
            orbital_id_opt,
            ship_id_opt,
            &commodity_opts,
        )?;
        let import_vol_m3 = planned_imports.total_vol_m3;
        let import_base_isk = planned_imports.total_base_isk;

        // Plan & validate exports (`type_id -> qty`)
        let mut planned_exports: Vec<(TypeId, u64, i32)> = Vec::with_capacity(export_map.len());
        let mut export_vol_m3 = 0.0_f64;
        let mut export_base_isk = 0.0_f64;
        for (&tid_raw, &qty) in &export_map {
            let tid = u32::try_from(tid_raw).map_err(|_| BridgeRpcError {
                code: "InvalidCommodity".into(),
                message: format!("Unknown commodity typeID {tid_raw}"),
                client_error: None,
            })?;
            ensure_valid_pi_commodity_type(&ctx.sde, tid)?;
            let qty_i32 = i32::try_from(qty).map_err(|_| BridgeRpcError {
                code: "InvalidCommodityQuantity".into(),
                message: format!("Export quantity {qty} overflows i32"),
                client_error: None,
            })?;
            let avail = net.pins[pin_idx].contents.get(&tid).copied().unwrap_or(0);
            if avail < qty {
                return Err(BridgeRpcError {
                    code: "InsufficientCommoditiesForRouting".into(),
                    message: format!(
                        "Spaceport pin {spaceport_pin_id} has {avail} of type {tid}, cannot export {qty}"
                    ),
                    client_error: None,
                });
            }
            let unit_vol = ctx
                .sde
                .types
                .get(&tid)
                .map(|t| t.volume.max(0.005))
                .unwrap_or(0.38);
            export_vol_m3 += unit_vol * (qty as f64);
            export_base_isk += pi_commodity_tier_base_cost(&ctx.sde, tid) * (qty as f64);
            planned_exports.push((tid, qty, qty_i32));
        }

        // Capacity validation: launchpad (10,000 m3) and customs office hangar (35,000 m3)
        let pin_cap_m3 = pin_storage_capacity_m3(&ctx.sde, net.pins[pin_idx].type_id);
        let pin_cur_vol = pin_contents_volume_m3(&ctx.sde, &net.pins[pin_idx]);
        if import_vol_m3 > 0.0 && (pin_cur_vol - export_vol_m3).max(0.0) + import_vol_m3 > pin_cap_m3 + 1e-6 {
            return Err(BridgeRpcError {
                code: "LaunchpadCapacityExceeded".into(),
                message: format!(
                    "Cannot import {import_vol_m3:.1} m3 into spaceport {spaceport_pin_id} (capacity {pin_cap_m3:.0} m3)"
                ),
                client_error: None,
            });
        }
        if export_vol_m3 > 0.0 {
            let co_cur_vol: f64 = snap
                .items
                .values()
                .filter(|it| {
                    it.owner_id == char_id
                        && (it.location_id == customs_office_loc_id
                            || it.location_id == planet_id
                            || orbital_id_opt.is_some_and(|oid| it.location_id == oid))
                        && it.flag == InventoryFlag::Hangar
                })
                .map(|it| {
                    let v = ctx.sde.types.get(&it.type_id).map(|t| t.volume).unwrap_or(0.38);
                    v * (it.quantity.max(0) as f64)
                })
                .sum();
            const CUSTOMS_OFFICE_CAPACITY_M3: f64 = 35_000.0;
            if (co_cur_vol - import_vol_m3).max(0.0) + export_vol_m3 > CUSTOMS_OFFICE_CAPACITY_M3 + 1e-6 {
                return Err(BridgeRpcError {
                    code: "CustomsOfficeCapacityExceeded".into(),
                    message: format!(
                        "Cannot export {export_vol_m3:.1} m3 to customs office {customs_office_loc_id} (capacity {CUSTOMS_OFFICE_CAPACITY_M3:.0} m3)"
                    ),
                    client_error: None,
                });
            }
        }

        // Pre-validate POCO access and tax affordability read-only before mutating any state.
        let prepared_tax = prepare_poco_commodity_tax(
            ctx,
            snap,
            char_id,
            planet_id,
            col.solar_system_id,
            0.5 * import_base_isk + export_base_isk,
            1.0,
            "import/export",
        )?;

        // Apply import consumptions atomically (propagate errors so tx rolls back).
        for (iid, take_i32) in planned_imports.consumptions {
            snap.consume_exact_item_quantity(iid, take_i32, commodity_opts)
                .map_err(|err| BridgeRpcError {
                    code: "InvalidCommodityItemState".into(),
                    message: format!("Failed to consume import commodity item {iid}: {err}"),
                    client_error: None,
                })?;
        }
        for (tid, qty) in planned_imports.pin_additions {
            let entry = net.pins[pin_idx].contents.entry(tid).or_insert(0);
            *entry = entry.checked_add(qty).ok_or_else(|| BridgeRpcError {
                code: "InvalidCommodityQuantity".into(),
                message: format!("Launchpad commodity quantity overflow for type {tid}"),
                client_error: None,
            })?;
        }

        // Apply export deductions & customs office hangar spawns/merges into clean commodity stacks only.
        for (tid, qty, qty_i32) in planned_exports {
            if let Some(cnt) = net.pins[pin_idx].contents.get_mut(&tid) {
                if *cnt <= qty {
                    net.pins[pin_idx].contents.remove(&tid);
                } else {
                    *cnt -= qty;
                }
            }
            let existing_id = snap
                .items
                .values()
                .filter(|it| {
                    it.owner_id == char_id
                        && it.location_id == customs_office_loc_id
                        && it.flag == InventoryFlag::Hangar
                        && it.type_id == tid
                        && snap.is_clean_stackable_item(it)
                        && it.quantity.checked_add(qty_i32).is_some()
                })
                .min_by_key(|it| it.item_id)
                .map(|it| it.item_id);
            if let Some(eid) = existing_id {
                if let Some(stack) = snap.items.get_mut(&eid) {
                    stack.quantity = stack.quantity.checked_add(qty_i32).ok_or_else(|| {
                        BridgeRpcError {
                            code: "InvalidCommodityQuantity".into(),
                            message: format!(
                                "Export quantity {qty_i32} overflows customs office stack {eid}"
                            ),
                            client_error: None,
                        }
                    })?;
                    stack.version += 1;
                }
            } else {
                snap.next_entity_id += 1;
                let nid = snap.next_entity_id;
                snap.items.insert(
                    nid,
                    eve_db::InventoryItemRecord {
                        item_id: nid,
                        type_id: tid,
                        owner_id: char_id,
                        location_id: customs_office_loc_id,
                        flag: InventoryFlag::Hangar,
                        quantity: qty_i32,
                        singleton: false,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
        }

        commit_poco_commodity_tax(
            snap,
            char_id,
            planet_id,
            prepared_tax,
            format!("POCO import/export tax on planet {planet_id}"),
            "import/export",
        )?;
        save_pi_network(snap, char_id, planet_id, &net);
        Ok(customs_office_loc_id)
    })?;

    crate::services::notify_emitters_fleet_combat::emit_invalidate_cache_for_locations(
        ctx,
        crate::router::NotificationTarget::Character(char_id),
        &[notify_loc_id],
    );
    Ok(PyRep::None)
}

fn handle_has_existing_password_set(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let caller_loc = session
        .station_id
        .or(session.structure_id)
        .or_else(|| session.solar_system_id.map(i64::from));
    let (arg0, arg1) = match args {
        [a0, a1, ..] => (Some(a0), Some(a1)),
        [a0] => (Some(a0), None),
        [] => (None, None),
    };
    let explicit_container_arg = kwargs.contains_key("containerID")
        || arg0.and_then(|a| a.as_i64()).is_some_and(|v| {
            // Disambiguate bound 1-arg `HasExistingPasswordSet(which)` (including invalid slots
            // like `3` or `99`) from explicit `(container_id, which)` or large entity-ID calls.
            (args.len() >= 2 && v > 2) || v >= 1_000
        });
    let item_bound =
        bound_entry.filter(|b| !explicit_container_arg && Some(b.object_id) != caller_loc);
    let parse_which = |positional: Option<&PyRep>| -> i64 {
        match positional {
            Some(v) => v.as_i64().unwrap_or(0),
            None => match kwargs.get("which") {
                Some(kw_w) => kw_w.as_i64().unwrap_or(0),
                None => 1,
            },
        }
    };
    let (container_id, which) = if let Some(b) = item_bound {
        (b.object_id, parse_which(arg0))
    } else {
        let cid = kwargs
            .get("containerID")
            .and_then(|v| v.as_i64())
            .or_else(|| arg0.and_then(|a| a.as_i64()))
            .unwrap_or(0);
        (cid, parse_which(arg1))
    };
    if !matches!(which, 1 | 2) {
        return Err(BridgeRpcError {
            code: "InvalidPasswordType".into(),
            message: format!("Invalid container password slot {which}; expected 1 or 2"),
            client_error: None,
        });
    }
    let has_pw = ctx.db.with_snapshot(|snap| -> Result<bool, BridgeRpcError> {
        super::fitting_containers_ships::authorize_container_inspect_in_snap(
            ctx,
            session,
            snap,
            char_id,
            container_id,
            true,
        )?;
        Ok(container_password_snap(snap, container_id, which).is_some())
    })?;
    Ok(PyRep::Bool(has_pw))
}

fn handle_retrieve_password(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let caller_loc = session
        .station_id
        .or(session.structure_id)
        .or_else(|| session.solar_system_id.map(i64::from));
    let explicit_container_arg = args.first().and_then(|a| a.as_i64()).is_some_and(|v| v > 2)
        || kwargs.contains_key("containerID");
    let item_bound = bound_entry.filter(|b| !explicit_container_arg && Some(b.object_id) != caller_loc);
    let (container_id, which) = if let Some(b) = item_bound {
        let w = args
            .first()
            .and_then(|a| a.as_i64())
            .or_else(|| kwargs.get("which").and_then(|v| v.as_i64()))
            .unwrap_or(1);
        (b.object_id, w)
    } else {
        let cid = args
            .first()
            .and_then(|a| a.as_i64())
            .or_else(|| kwargs.get("containerID").and_then(|v| v.as_i64()))
            .unwrap_or(0);
        let w = args
            .get(1)
            .and_then(|a| a.as_i64())
            .or_else(|| kwargs.get("which").and_then(|v| v.as_i64()))
            .unwrap_or(1);
        (cid, w)
    };
    if !matches!(which, 1 | 2) {
        return Err(BridgeRpcError {
            code: "InvalidPasswordType".into(),
            message: format!("Invalid container password slot {which}; expected 1 or 2"),
            client_error: None,
        });
    }
    if container_id <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidArguments".into(),
            message: "RetrievePassword requires a valid containerID".into(),
            client_error: None,
        });
    }

    let now_unix = ctx.now_unix_secs();
    let pw_type_str = if which == 2 { "config" } else { "general" };
    let pw_opt = ctx.db.try_with_mut_snapshot(|snap| -> Result<Option<String>, BridgeRpcError> {
        let item = snap.items.get(&container_id).ok_or_else(|| BridgeRpcError {
            code: "ItemNotFound".into(),
            message: format!("Secure container {container_id} not found or not owned"),
            client_error: None,
        })?;
        let corp_id = session
            .corporation_id
            .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id))
            .unwrap_or(0);
        if item.owner_id != char_id {
            if corp_id == 0 || item.owner_id != corp_id {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Secure container {container_id} not found or not owned"),
                    client_error: None,
                });
            }
            let (is_ceo, eff_role) =
                snap.effective_corp_roles_at(corp_id, char_id, None, session.corp_role, ctx.now_unix());
            if !is_ceo
                && (eff_role
                    & (eve_db::corp_roles::ROLE_DIRECTOR
                        | eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG))
                    == 0
            {
                return Err(BridgeRpcError {
                    code: "CorpRoleRequired".into(),
                    message: "Corporation CEO/Director role required to retrieve container password"
                        .into(),
                    client_error: None,
                });
            }
        }
        let item_tid = item.type_id;
        let grp_id = ctx.sde.types.get(&item_tid).map(|t| t.group_id).unwrap_or(0);
        if !matches!(grp_id, 340 | 448) {
            return Err(BridgeRpcError {
                code: "ContainerDoesNotSupportPassword".into(),
                message: format!("Container {container_id} (group {grp_id}) does not support passwords"),
                client_error: None,
            });
        }

        let pw = container_password_snap(snap, container_id, which);
        if which == 1 && pw.is_some() {
            set_container_unlocked_snap(snap, container_id, char_id, true, now_unix)?;
        }
        append_alsc_log_snap(
            snap,
            container_id,
            AlscLogEntry {
                timestamp_unix: now_unix,
                actor_id: char_id,
                action: "ALSCActionRetrievePassword".into(),
                item_id: container_id,
                item_type_id: item_tid,
                quantity: 1,
            },
            now_unix,
        )?;
        if let Some((log_corp_id, _, loc_id, loc_flag)) =
            resolve_corp_container_log_target(snap, container_id)
        {
            snap.corp_container_logs.entry(log_corp_id).or_default().push(
                eve_db::CorpContainerLogEntry {
                    logged_at_unix: now_unix,
                    container_id,
                    container_type_id: item_tid as i32,
                    character_id: char_id,
                    location_id: loc_id,
                    location_flag: loc_flag,
                    action: "retrieve_password".into(),
                    password_type: Some(pw_type_str.into()),
                    type_id: None,
                    quantity: None,
                    old_config_bitmask: None,
                    new_config_bitmask: None,
                },
            );
        }
        Ok(pw)
    })?;

    Ok(pw_opt.map(PyRep::String).unwrap_or(PyRep::None))
}

/// Temporary `#[cfg(test)]` compatibility view over `entity_kv` (`#1180`).
/// Zero non-test code reads or compiles these shims. Minimal subset retained only for
/// 6 external test files outside `#1180` Track 2's file lock (`contracts_service_*`,
/// `industry_jobs_edges2_tests.rs`, `fitting_containers_ships_ops_*`).
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod legacy_container_ship_test_shim {
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::collections::HashSet;

    #[derive(Default, Clone, Serialize, Deserialize)]
    pub(crate) struct ContainerShipRuntimeState {
        pub container_passwords: HashMap<i64, String>,
        #[serde(default)]
        pub container_config_passwords: HashMap<i64, String>,
        pub unlocked_containers: HashSet<(i64, i64)>,
        pub alsc_locked_items: HashSet<i64>,
        pub alsc_logs: HashMap<i64, Vec<AlscLogEntry>>,
        pub ship_configs: HashMap<i64, ShipAccessConfig>,
        pub custom_item_flags: HashMap<i64, u16>,
        #[serde(default)]
        pub ship_passwords: HashMap<i64, String>,
        #[serde(default)]
        pub container_configs: HashMap<i64, ContainerConfig>,
    }

    impl ContainerShipRuntimeState {
        pub fn get_container_password(&self, cid: i64, which: i64) -> Option<&str> {
            (if which == 2 {
                &self.container_config_passwords
            } else {
                &self.container_passwords
            })
            .get(&cid)
            .map(String::as_str)
        }
        pub fn set_container_password(&mut self, cid: i64, which: i64, pw: String) {
            let map = if which == 2 {
                &mut self.container_config_passwords
            } else {
                &mut self.container_passwords
            };
            if pw.is_empty() {
                map.remove(&cid);
            } else {
                map.insert(cid, pw);
            }
        }
    }

    pub(crate) fn read_container_ship_state_snap(
        snap: &eve_db::WorldStateSnapshot,
    ) -> ContainerShipRuntimeState {
        let mut st = ContainerShipRuntimeState::default();
        for (k, row) in snap.kv(eve_db::Ns::ContainerPassword).iter_ns() {
            if let Ok(pw) = serde_json::from_str::<String>(&row.value_json) {
                if !pw.is_empty() {
                    if k.sub_key == "2" {
                        st.container_config_passwords.insert(k.entity_id, pw);
                    } else {
                        st.container_passwords.insert(k.entity_id, pw);
                    }
                }
            }
        }
        for (k, row) in snap.kv(eve_db::Ns::ContainerConfig).iter_ns() {
            if let Ok(cfg) = serde_json::from_str::<ContainerConfig>(&row.value_json) {
                st.container_configs.insert(k.entity_id, cfg);
            }
        }
        for (k, row) in snap.kv(eve_db::Ns::ShipAccessConfig).iter_ns() {
            if let Ok(cfg) = serde_json::from_str::<ShipAccessConfig>(&row.value_json) {
                if let Some(ref pw) = cfg.password {
                    if !pw.is_empty() {
                        st.ship_passwords.insert(k.entity_id, pw.clone());
                    }
                }
                st.ship_configs.insert(k.entity_id, cfg);
            }
        }
        for (k, row) in snap.kv(eve_db::Ns::ContainerUnlockedChar).iter_ns() {
            if matches!(serde_json::from_str::<bool>(&row.value_json), Ok(true)) {
                if let Ok(char_id) = k.sub_key.parse::<i64>() {
                    let is_ship_only = st.ship_configs.contains_key(&k.entity_id)
                        && !st.container_passwords.contains_key(&k.entity_id)
                        && !st.container_configs.contains_key(&k.entity_id);
                    if is_ship_only {
                        st.unlocked_containers.insert((char_id, k.entity_id));
                    } else {
                        st.unlocked_containers.insert((k.entity_id, char_id));
                    }
                }
            }
        }
        st.alsc_locked_items = snap.alsc_locked_item_ids();
        for (k, row) in snap.kv(eve_db::Ns::ContainerAlscLogs).iter_ns() {
            if let Ok(logs) = serde_json::from_str::<Vec<AlscLogEntry>>(&row.value_json) {
                if !logs.is_empty() {
                    st.alsc_logs.insert(k.entity_id, logs);
                }
            }
        }
        for (k, row) in snap.kv(eve_db::Ns::CustomItemFlag).iter_ns() {
            if let Ok(flag_u16) = serde_json::from_str::<u16>(&row.value_json) {
                st.custom_item_flags.insert(k.entity_id, flag_u16);
            }
        }
        st
    }

    pub(crate) fn save_container_ship_state_snap(
        snap: &mut eve_db::WorldStateSnapshot,
        now_unix: u64,
        state: &ContainerShipRuntimeState,
    ) {
        for ns in [
            eve_db::Ns::ContainerPassword,
            eve_db::Ns::ContainerConfig,
            eve_db::Ns::ContainerUnlockedChar,
            eve_db::Ns::ContainerAlscLogs,
            eve_db::Ns::AlscLockedItem,
            eve_db::Ns::ShipAccessConfig,
            eve_db::Ns::CustomItemFlag,
        ] {
            snap.kv_mut(ns).clear_ns();
        }
        for (&cid, pw) in &state.container_passwords {
            let _ = set_container_password_snap(snap, cid, 1, pw, now_unix);
        }
        for (&cid, pw) in &state.container_config_passwords {
            let _ = set_container_password_snap(snap, cid, 2, pw, now_unix);
        }
        for (&cid, cfg) in &state.container_configs {
            let _ = set_container_config_snap(snap, cid, cfg, now_unix);
        }
        let ship_ids: std::collections::BTreeSet<i64> = state
            .ship_configs
            .keys()
            .chain(state.ship_passwords.keys())
            .copied()
            .collect();
        for sid in ship_ids {
            let mut cfg = state.ship_configs.get(&sid).cloned().unwrap_or_default();
            if cfg.password.as_deref().is_none_or(str::is_empty) {
                if let Some(pw) = state.ship_passwords.get(&sid) {
                    if !pw.is_empty() {
                        cfg.password = Some(pw.clone());
                    }
                }
            }
            let _ = set_ship_access_config_snap(snap, sid, &cfg, now_unix);
        }
        for &(a, b) in &state.unlocked_containers {
            let is_ship_pair = !state.container_passwords.contains_key(&a)
                && !state.container_configs.contains_key(&a)
                && (state.ship_configs.contains_key(&b)
                    || state.ship_passwords.contains_key(&b)
                    || (snap.characters.contains_key(&a) && !snap.characters.contains_key(&b)));
            let (target_id, char_id) = if is_ship_pair { (b, a) } else { (a, b) };
            let _ = set_container_unlocked_snap(snap, target_id, char_id, true, now_unix);
        }
        for &iid in &state.alsc_locked_items {
            let _ = set_alsc_item_locked_snap(snap, iid, true, now_unix);
        }
        for (&cid, entries) in &state.alsc_logs {
            if !entries.is_empty() {
                let mut bounded = entries.clone();
                eve_db::trim_alsc_logs_to_budget(&mut bounded);
                let _ = snap
                    .kv_mut(eve_db::Ns::ContainerAlscLogs)
                    .set_with_unix(cid, "", &bounded, now_unix);
            }
        }
        for (&iid, &flag_u16) in &state.custom_item_flags {
            let _ = set_custom_item_flag_snap(snap, iid, Some(flag_u16), now_unix);
        }
    }

    pub(crate) fn write_container_ship_state_snap<R>(
        snap: &mut eve_db::WorldStateSnapshot,
        f: impl FnOnce(&mut ContainerShipRuntimeState) -> R,
    ) -> R {
        let mut state = read_container_ship_state_snap(snap);
        let out = f(&mut state);
        save_container_ship_state_snap(snap, 0, &state);
        out
    }

    pub(crate) fn write_container_ship_state<R>(
        ctx: &ServerContext,
        f: impl FnOnce(&mut ContainerShipRuntimeState) -> R,
    ) -> R {
        let now_unix = ctx.now_unix();
        ctx.db
            .try_with_mut_snapshot(|snap| -> Result<R, BridgeRpcError> {
                let mut state = read_container_ship_state_snap(snap);
                let out = f(&mut state);
                save_container_ship_state_snap(snap, now_unix, &state);
                Ok(out)
            })
            .expect("test entity_kv write")
    }
}

#[cfg(test)]
#[path = "fitting_containers_ships_ops_cov_unit_tests.rs"]
mod fitting_containers_ships_ops_cov_unit_tests;

#[cfg(test)]
#[path = "fitting_containers_ships_ops_edges_tests.rs"]
mod fitting_containers_ships_ops_edges_tests;

