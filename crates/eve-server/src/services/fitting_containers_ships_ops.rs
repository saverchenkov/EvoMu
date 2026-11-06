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
            let Some((type_id, qty)) = snap
                .items
                .get(&cid)
                .filter(|c| {
                    c.owner_id == char_id
                        && c.location_id == ship_id
                        && matches!(c.flag as u16, 11..=34)
                })
                .map(|c| (c.type_id, c.quantity.max(1)))
            else {
                continue;
            };
            let existing_cargo_stack_id = snap
                .items
                .values()
                .find(|it| {
                    it.item_id != cid
                        && it.owner_id == char_id
                        && it.location_id == ship_id
                        && it.flag == InventoryFlag::Cargo
                        && it.type_id == type_id
                        && !it.singleton
                })
                .map(|it| it.item_id);
            if let Some(target_id) = existing_cargo_stack_id {
                if let Some(stack) = snap.items.get_mut(&target_id) {
                    stack.quantity += qty;
                    stack.version += 1;
                }
                snap.items.remove(&cid);
            } else if let Some(charge_item) = snap.items.get_mut(&cid) {
                charge_item.flag = InventoryFlag::Cargo;
                charge_item.singleton = false;
                charge_item.version += 1;
            }
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
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut failed: Vec<PyRep> = Vec::new();
            if let Some(PyRep::List(items) | PyRep::Tuple(items)) = args.get(1) {
                for entry in items {
                    if let PyRep::Tuple(t) | PyRep::List(t) = entry {
                        if t.len() >= 2 {
                            failed.push(PyRep::Tuple(vec![t[0].clone(), t[1].clone()]));
                        }
                    }
                }
            }
            if failed.is_empty() {
                failed.push(PyRep::Tuple(vec![PyRep::Integer(440), PyRep::Integer(1)]));
            }
            Some(Ok(PyRep::List(failed)))
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
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx.db.get_items_for_character(char_id);
            Some(Ok(PyRep::None))
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
            let mut rows: Vec<Vec<PyRep>> = items
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
            if rows.is_empty() {
                rows = items
                    .iter()
                    .map(|it| item_to_packed_cells(ctx, it))
                    .collect();
            }
            Some(Ok(DBRowDescriptor::inv_item_header().build_crowset(rows)))
        }

        ("invbroker", "ListStations") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                let mut counts_by_station: std::collections::BTreeMap<i64, (i64, i64, i64)> =
                    std::collections::BTreeMap::new();
                for item in snap.items.values() {
                    if item.owner_id != char_id {
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
                if counts_by_station.is_empty() {
                    let st_id = session.station_id.unwrap_or(60_003_760);
                    let sys_id = i64::from(session.solar_system_id.unwrap_or(30_000_142));
                    let st_type_id = ctx
                        .sde
                        .stations
                        .get(&st_id)
                        .map(|s| i64::from(s.station_type_id))
                        .unwrap_or(1529);
                    counts_by_station.insert(st_id, (sys_id, 1, st_type_id));
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
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx.db.get_items_for_character(char_id);
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("invbroker", "RetrievePassword") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let _ = ctx.db.get_items_for_character(char_id);
            Some(Ok(PyRep::None))
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
