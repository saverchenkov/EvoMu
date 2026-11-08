//! `fitting_containers_ships_ops` — Extracted inventory broker, item locking,
//! encoded item decoding, and validated charge-unloading handlers (`#394`).

#![forbid(unsafe_code)]

use super::*;

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
        });
    }

    let char_skills = ctx.character_skill_levels(char_id);
    let (charge_ids, mod_ids_in_charge_slots) = ctx.db.with_snapshot(|snap| {
        let Some(ship) = snap.items.get(&ship_id) else {
            return Err(BridgeRpcError {
                code: "ShipNotFound".into(),
                message: format!("Ship {ship_id} not found"),
            });
        };
        let ship_cat = ctx.sde.types.get(&ship.type_id).map(|t| t.category_id).unwrap_or(0);
        if ship.owner_id != char_id || ship_cat != 6 {
            return Err(BridgeRpcError {
                code: "InvalidFittingTarget".into(),
                message: format!("Target {ship_id} is not a ship owned by character {char_id}"),
            });
        }
        if ship.flag == InventoryFlag::Locked || snap.is_item_locked_in_contract_or_job(ship_id) {
            return Err(BridgeRpcError {
                code: "ItemLocked".into(),
                message: format!("Ship {ship_id} is locked"),
            });
        }

        let charges: Vec<(i64, InventoryFlag, f64, i32)> = snap
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
                (it.item_id, it.flag, vol, it.quantity.max(0))
            })
            .collect();

        let occupied_flags: std::collections::HashSet<InventoryFlag> =
            charges.iter().map(|&(_, flg, _, _)| flg).collect();

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
                });
            }
        }

        let mut engine = eve_dogma::ShipFittingEngine::from_sde(ship_id, ship.type_id, &ctx.sde)
            .with_character_skills(char_skills);
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
            .map(|&(_, _, vol, qty)| vol * (qty as f64))
            .sum();
        if incoming_vol > 0.0 && existing_cargo_vol + incoming_vol > cargo_cap + 1e-4 {
            return Err(BridgeRpcError {
                code: "CapacityExceeded".into(),
                message: format!(
                    "Ship cargo capacity {cargo_cap} m³ exceeded: total {:.1} m³",
                    existing_cargo_vol + incoming_vol
                ),
            });
        }

        let c_ids: Vec<i64> = charges.into_iter().map(|(id, _, _, _)| id).collect();
        Ok((c_ids, mod_ids))
    })?;

    if let Some(sys_id) = session.solar_system_id {
        let sys = ctx.systems.get_or_spawn(sys_id);
        for &mid in &mod_ids_in_charge_slots {
            if sys.has_active_module_cycle_sync(ship_id, mid) {
                return Err(BridgeRpcError {
                    code: "ModuleIsActive".into(),
                    message: format!("Cannot unload charges while module {mid} is active"),
                });
            }
        }
    }

    ctx.db.with_mut_snapshot(|snap| {
        for &mid in &mod_ids_in_charge_slots {
            snap.module_active_charges.remove(&mid);
        }
        for cid in charge_ids {
            let Some(c_flag) = snap
                .items
                .get(&cid)
                .filter(|c| {
                    c.owner_id == char_id
                        && c.location_id == ship_id
                        && matches!(c.flag as u16, 11..=34)
                })
                .map(|c| c.flag)
            else {
                continue;
            };
            let _ = c_flag;
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
            let _ = snap.transfer_inventory_item(cid, spec);
        }
    });

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
                            }));
                        };
                        let Ok(u16_flg) = u16::try_from(flg) else {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidFittingFlag".into(),
                                message: format!("Invalid flag {flg}"),
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
                            }));
                        }
                        let qty_i64 = seq.get(2).and_then(|v| v.as_i64()).unwrap_or(1);
                        let Some(qty) = i32::try_from(qty_i64).ok().filter(|&q| q > 0) else {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidQuantity".into(),
                                message: format!("Invalid quantity {qty_i64}"),
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
                            }));
                        };
                        let Some(qty) = i32::try_from(qty_i64).ok().filter(|&q| q > 0) else {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidQuantity".into(),
                                message: format!("Invalid quantity {qty_i64}"),
                            }));
                        };
                        let Some(item_type) = ctx.sde.types.get(&tid_u32) else {
                            return Some(Err(BridgeRpcError {
                                code: "ItemNotFound".into(),
                                message: format!("Type {tid_u32} not found in SDE"),
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

        ("invbroker", "HasExistingPasswordSet") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let container_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
                .unwrap_or(0);
            let which = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let st = read_container_ship_state(ctx);
            let has_pw = st.get_container_password(container_id, which).is_some();
            Some(Ok(PyRep::Bool(has_pw)))
        }

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

        ("invbroker", "ListStations") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let blueprint_only = args
                .first()
                .and_then(|a| a.as_bool().or_else(|| a.as_i64().map(|v| v != 0)))
                .unwrap_or(false);
            let is_corp = args
                .get(1)
                .and_then(|a| a.as_bool().or_else(|| a.as_i64().map(|v| v != 0)))
                .unwrap_or(false);
            let target_owner_id = if is_corp {
                session.corporation_id.filter(|&cid| cid > 0).unwrap_or(char_id)
            } else {
                char_id
            };
            let rows = ctx.db.with_snapshot(|snap| {
                let mut counts_by_station: std::collections::BTreeMap<i64, (i64, i64, i64)> =
                    std::collections::BTreeMap::new();
                for item in snap.items.values() {
                    if item.owner_id != target_owner_id {
                        continue;
                    }
                    if blueprint_only
                        && !ctx
                            .sde
                            .types
                            .get(&item.type_id)
                            .is_some_and(|t| t.category_id == 9)
                    {
                        continue;
                    }
                    let loc_id = item.location_id;
                    let st_info = if (60_000_000..=69_999_999).contains(&loc_id) {
                        ctx.sde
                            .stations
                            .get(&loc_id)
                            .map(|st| (i64::from(st.solar_system_id), i64::from(st.station_type_id)))
                    } else if loc_id >= 1_000_000_000_000 {
                        snap.structures
                            .get(&loc_id)
                            .map(|st| (i64::from(st.solar_system_id), i64::from(st.type_id)))
                    } else {
                        None
                    };
                    if let Some((sys_id, st_type_id)) = st_info {
                        let entry = counts_by_station
                            .entry(loc_id)
                            .or_insert((sys_id, 0, st_type_id));
                        entry.1 += 1;
                    }
                }
                counts_by_station
                    .into_iter()
                    .map(|(station_id, (solar_system_id, item_count, st_type_id))| {
                        vec![
                            PyRep::Integer(station_id),
                            PyRep::Integer(solar_system_id),
                            PyRep::Integer(item_count),
                            PyRep::Integer(st_type_id),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["stationID", "solarSystemID", "itemCount", "typeID"],
                rows,
            )))
        }

        ("invbroker", "Reprocess") => {
            let (_, _) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.or(session.structure_id).is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Reprocessing requires being docked at a station or structure".into(),
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
            let alsc_locked = read_container_ship_state(ctx).alsc_locked_items;
            let locs: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                let mut set = std::collections::BTreeSet::new();
                for item in snap.items.values() {
                    let in_job = snap.industry_jobs.values().any(|j| j.status == "Active" && j.blueprint_item_id == item.item_id);
                    let vote_locked = corp_id > 0 && item.owner_id == corp_id && item.flag == InventoryFlag::Locked && !in_job;
                    let container_or_escrow = (item.owner_id == char_id || (corp_id > 0 && item.owner_id == corp_id))
                        && (alsc_locked.contains(&item.item_id) || snap.contracts.contains_key(&item.location_id));
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
            let alsc_locked = read_container_ship_state(ctx).alsc_locked_items;
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
                            && (alsc_locked.contains(&item.item_id) || snap.contracts.contains_key(&item.location_id));
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
                })?;
                let qty = v.as_i64().filter(|&q| q > 0).ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodityQuantity".into(),
                    message: format!("{field_name} quantities must be positive integers"),
                })?;
                let slot = out.entry(key).or_insert(0);
                *slot = slot.checked_add(qty as u64).ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodityQuantity".into(),
                    message: format!("{field_name} quantity overflow"),
                })?;
            }
            Ok(out)
        }
        Some(PyRep::KeyVal(entries)) => {
            for (k, v) in entries {
                let key = k.parse::<i64>().ok().filter(|&id| id > 0).ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodity".into(),
                    message: format!("{field_name} keys must be positive integer item/type IDs"),
                })?;
                let qty = v.as_i64().filter(|&q| q > 0).ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodityQuantity".into(),
                    message: format!("{field_name} quantities must be positive integers"),
                })?;
                let slot = out.entry(key).or_insert(0);
                *slot = slot.checked_add(qty as u64).ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodityQuantity".into(),
                    message: format!("{field_name} quantity overflow"),
                })?;
            }
            Ok(out)
        }
        _ => Err(BridgeRpcError {
            code: "InvalidTransferArguments".into(),
            message: format!("{field_name} must be a dict or None"),
        }),
    }
}

fn handle_import_export_with_planet(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    use crate::formulas::compute_poco_export_tax;
    use crate::services::industry_pi::{
        is_launchpad_pin, load_orbital_settings, load_pi_network, pi_commodity_tier_base_cost,
        pin_contents_volume_m3, pin_storage_capacity_m3, resolve_character_orbital_access_and_tax,
        resolve_orbital_and_planet, save_pi_network,
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
        });
    }

    let import_map = parse_pi_xfer_entries(import_rep, "importData")?;
    let export_map = parse_pi_xfer_entries(export_rep, "exportData")?;
    if import_map.is_empty() && export_map.is_empty() {
        return Err(BridgeRpcError {
            code: "InvalidTransferArguments".into(),
            message: "ImportExportWithPlanet requires non-empty importData or exportData".into(),
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
                })?;
            resolve_orbital_and_planet(snap, found_pid)
        };

        let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
            return Err(BridgeRpcError {
                code: "ColonyNotFound".into(),
                message: format!("No colony found for character {char_id} on planet {planet_id}"),
            });
        };
        if current_sys_id != Some(col.solar_system_id) {
            return Err(BridgeRpcError {
                code: "ColonyNotInCurrentSystem".into(),
                message: format!(
                    "Planetary colony {planet_id} is in solar system {}, not {:?}",
                    col.solar_system_id, current_sys_id
                ),
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
            })?;
        if !is_launchpad_pin(&ctx.sde, net.pins[pin_idx].type_id) {
            return Err(BridgeRpcError {
                code: "InvalidPinTypeForLaunch".into(),
                message: format!(
                    "Pin {spaceport_pin_id} (type {}) is not a planetary launchpad/spaceport",
                    net.pins[pin_idx].type_id
                ),
            });
        }

        let customs_office_loc_id = orbital_id_opt.unwrap_or(planet_id);
        let is_co_hangar_item = |it: &eve_db::InventoryItemRecord| -> bool {
            it.owner_id == char_id
                && it.quantity > 0
                && it.flag != InventoryFlag::Locked
                && !snap.is_item_locked_in_contract_or_job(it.item_id)
                && ((it.location_id == customs_office_loc_id
                    || it.location_id == planet_id
                    || orbital_id_opt.is_some_and(|oid| it.location_id == oid))
                    && it.flag == InventoryFlag::Hangar
                    || ship_id_opt.is_some_and(|sid| it.location_id == sid)
                        && matches!(
                            it.flag,
                            InventoryFlag::Cargo
                                | InventoryFlag::SpecializedPlanetaryCommoditiesHold
                        ))
        };

        // Plan & validate imports (`item_id -> qty` or `type_id -> qty`)
        enum ImportSource {
            ByItemId(i64, TypeId, u64),
            ByTypeId(TypeId, u64, Vec<i64>),
        }
        let mut planned_imports = Vec::with_capacity(import_map.len());
        let mut import_vol_m3 = 0.0_f64;
        let mut import_base_isk = 0.0_f64;
        for (&key_id, &qty) in &import_map {
            let _ = i32::try_from(qty).map_err(|_| BridgeRpcError {
                code: "InvalidCommodityQuantity".into(),
                message: format!("Import quantity {qty} overflows i32"),
            })?;
            let tid = if let Some(item) = snap.items.get(&key_id) {
                if item.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Import item {key_id} not found or not owned"),
                    });
                }
                if item.flag == InventoryFlag::Locked
                    || snap.is_item_locked_in_contract_or_job(item.item_id)
                {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!("Import item {key_id} is locked"),
                    });
                }
                if !is_co_hangar_item(item) {
                    return Err(BridgeRpcError {
                        code: "ColocationRequired".into(),
                        message: format!(
                            "Import item {key_id} must be in customs office hangar or active ship cargo"
                        ),
                    });
                }
                if (item.quantity as u64) < qty {
                    return Err(BridgeRpcError {
                        code: "InsufficientCommodityQuantity".into(),
                        message: format!(
                            "Import item {key_id} has quantity {}, cannot import {qty}",
                            item.quantity
                        ),
                    });
                }
                planned_imports.push(ImportSource::ByItemId(key_id, item.type_id, qty));
                item.type_id
            } else if let Some(tid) =
                u32::try_from(key_id).ok().filter(|t| ctx.sde.types.contains_key(t))
            {
                let mut matching_ids: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|it| it.type_id == tid && is_co_hangar_item(it))
                    .map(|it| it.item_id)
                    .collect();
                matching_ids.sort();
                let avail: u64 = matching_ids
                    .iter()
                    .filter_map(|iid| snap.items.get(iid))
                    .map(|it| it.quantity.max(0) as u64)
                    .sum();
                if avail == 0 {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!(
                            "Commodity type {tid} not found in customs office hangar or ship cargo"
                        ),
                    });
                }
                if avail < qty {
                    return Err(BridgeRpcError {
                        code: "InsufficientCommodityQuantity".into(),
                        message: format!(
                            "Available quantity ({avail}) of type {tid} is less than import quantity ({qty})"
                        ),
                    });
                }
                planned_imports.push(ImportSource::ByTypeId(tid, qty, matching_ids));
                tid
            } else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Import commodity/item {key_id} not found"),
                });
            };
            let unit_vol = ctx
                .sde
                .types
                .get(&tid)
                .map(|t| t.volume.max(0.005))
                .unwrap_or(0.38);
            import_vol_m3 += unit_vol * (qty as f64);
            import_base_isk += pi_commodity_tier_base_cost(&ctx.sde, tid) * (qty as f64);
        }

        // Plan & validate exports (`type_id -> qty`)
        let mut planned_exports: Vec<(TypeId, u64)> = Vec::with_capacity(export_map.len());
        let mut export_vol_m3 = 0.0_f64;
        let mut export_base_isk = 0.0_f64;
        for (&tid_raw, &qty) in &export_map {
            let tid = u32::try_from(tid_raw)
                .ok()
                .filter(|t| ctx.sde.types.contains_key(t))
                .ok_or_else(|| BridgeRpcError {
                    code: "InvalidCommodity".into(),
                    message: format!("Unknown commodity typeID {tid_raw}"),
                })?;
            let _ = i32::try_from(qty).map_err(|_| BridgeRpcError {
                code: "InvalidCommodityQuantity".into(),
                message: format!("Export quantity {qty} overflows i32"),
            })?;
            let avail = net.pins[pin_idx].contents.get(&tid).copied().unwrap_or(0);
            if avail < qty {
                return Err(BridgeRpcError {
                    code: "InsufficientCommoditiesForRouting".into(),
                    message: format!(
                        "Spaceport pin {spaceport_pin_id} has {avail} of type {tid}, cannot export {qty}"
                    ),
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
            planned_exports.push((tid, qty));
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
                });
            }
        }

        // POCO/Skyhook access & tax calculation
        let (player_poco_rate, _) =
            resolve_character_orbital_access_and_tax(snap, char_id, planet_id)?;
        let customs_lvl = snap
            .skills
            .get(&char_id)
            .and_then(|m| m.get(&33467))
            .map(|s| s.level.clamp(0, 5))
            .unwrap_or(0);
        let npc_customs_tax_rate = if ctx
            .sde
            .solar_systems
            .get(&col.solar_system_id)
            .map(|s| s.security_status >= 0.45)
            .unwrap_or(true)
        {
            0.10
        } else {
            0.0
        };
        let import_tax_isk = if import_base_isk > 0.0 {
            0.5 * compute_poco_export_tax(import_base_isk, npc_customs_tax_rate, customs_lvl, player_poco_rate)
        } else {
            0.0
        };
        let export_tax_isk = if export_base_isk > 0.0 {
            compute_poco_export_tax(export_base_isk, npc_customs_tax_rate, customs_lvl, player_poco_rate)
        } else {
            0.0
        };
        let total_tax_isk = import_tax_isk + export_tax_isk;
        let tax_cents = IskCents::from_f64_isk(total_tax_isk);
        if tax_cents.0 > 0 {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                let new_bal = ch.balance.checked_sub(tax_cents).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: format!(
                        "Insufficient ISK to pay POCO import/export tax ({total_tax_isk:.2} ISK)"
                    ),
                })?;
                ch.balance = new_bal;
                snap.append_journal(
                    char_id,
                    "CustomOfficeTax",
                    tax_cents,
                    new_bal,
                    format!("POCO import/export tax on planet {planet_id}"),
                );
            }
            if let Some(cfg) = load_orbital_settings(snap, planet_id) {
                let player_share =
                    IskCents::from_f64_isk((0.5 * import_base_isk + export_base_isk) * player_poco_rate);
                if player_share.0 > 0 {
                    if let Some(corp) = snap.corporations.get_mut(&cfg.owner_corp_id) {
                        corp.credit_division(1, player_share);
                    }
                }
            }
        }

        // Apply import deductions & pin additions
        for src in planned_imports {
            match src {
                ImportSource::ByItemId(iid, tid, qty) => {
                    let q_i32 = qty as i32;
                    let Some(it_owner) = snap.items.get(&iid).map(|it| it.owner_id) else { continue };
                    let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), it_owner);
                    opts.reject_singleton = false;
                    if snap.consume_exact_item_quantity(iid, q_i32, opts).is_ok() {
                        *net.pins[pin_idx].contents.entry(tid).or_insert(0) += qty;
                    }
                }
                ImportSource::ByTypeId(tid, qty, matching_ids) => {
                    let mut rem = qty as i32;
                    for iid in matching_ids {
                        let Some((it_owner, avail)) = snap.items.get(&iid).map(|it| (it.owner_id, it.quantity)) else { continue };
                        let take = rem.min(avail);
                        if take <= 0 { continue; }
                        let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), it_owner);
                        opts.reject_singleton = false;
                        if snap.consume_exact_item_quantity(iid, take, opts).is_ok() {
                            rem -= take;
                        }
                        if rem == 0 {
                            break;
                        }
                    }
                    *net.pins[pin_idx].contents.entry(tid).or_insert(0) += qty;
                }
            }
        }

        // Apply export deductions & customs office hangar spawns/merges
        for (tid, qty) in planned_exports {
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
                .find(|it| {
                    it.owner_id == char_id
                        && it.location_id == customs_office_loc_id
                        && it.flag == InventoryFlag::Hangar
                        && it.type_id == tid
                        && !it.singleton
                })
                .map(|it| it.item_id);
            if let Some(eid) = existing_id {
                if let Some(stack) = snap.items.get_mut(&eid) {
                    stack.quantity = stack.quantity.saturating_add(qty as i32);
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
                        quantity: qty as i32,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
        }

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
        });
    }
    if container_id <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidArguments".into(),
            message: "RetrievePassword requires a valid containerID".into(),
        });
    }

    let now_unix = ctx.now_unix_secs();
    let pw_type_str = if which == 2 { "config" } else { "general" };
    let pw_opt = ctx.db.try_with_mut_snapshot(|snap| -> Result<Option<String>, BridgeRpcError> {
        let item = snap.items.get(&container_id).ok_or_else(|| BridgeRpcError {
            code: "ItemNotFound".into(),
            message: format!("Secure container {container_id} not found or not owned"),
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
                });
            }
            let (is_ceo, corp_roles) =
                snap.effective_corp_roles_at(corp_id, char_id, None, session.corp_role, ctx.now_unix());
            let eff_role = session.corp_role | corp_roles;
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
                });
            }
        }
        let item_tid = item.type_id;
        let grp_id = ctx.sde.types.get(&item_tid).map(|t| t.group_id).unwrap_or(0);
        if !matches!(grp_id, 340 | 448) {
            return Err(BridgeRpcError {
                code: "ContainerDoesNotSupportPassword".into(),
                message: format!("Container {container_id} (group {grp_id}) does not support passwords"),
            });
        }

        let mut st = read_container_ship_state_snap(snap);
        let pw = st
            .get_container_password(container_id, which)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        if which == 1 && pw.is_some() {
            st.unlocked_containers.insert((container_id, char_id));
        }
        st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry {
            timestamp_unix: now_unix,
            actor_id: char_id,
            action: "ALSCActionRetrievePassword".into(),
            item_id: container_id,
            item_type_id: item_tid,
            quantity: 1,
        });
        save_container_ship_state_snap(snap, now_unix, &st);
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

#[cfg(test)]
#[path = "fitting_containers_ships_ops_cov_unit_tests.rs"]
mod fitting_containers_ships_ops_cov_unit_tests;

