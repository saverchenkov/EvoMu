//! `fitting_dogma_im` — `dogmaIM` inspection, fitting, targeting, weapon linking,ammo & `shipStanceMgr`.

#![forbid(unsafe_code)]

use super::*;

pub(crate) async fn handle_dogma_im(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // rpc-lint: allow tools/client-harness/run_journeys bridge probe calls GetShipAttributes
        ("dogmaIM", "GetAllInfo") | ("dogmaIM", "GetShipAttributes") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = bound_entry
                .map(|b| b.object_id)
                .filter(|&id| ctx.db.get_item(id).is_some())
                .or(session.ship_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };

            let fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            let eval_ctx = fit.evaluated_context(&ctx.sde);
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
                ship_attrs_map.insert(attr_id, val);
            }
            apply_active_dbuffs_and_system_effects_to_ship_attrs(ctx, ship_id, &mut ship_attrs_map);
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
                let is_mod_online = fit.modules.get(&m_state.item_id).map(|m| m.state != eve_dogma::ModuleState::Offline).unwrap_or(true);
                m_map.insert(2, if is_mod_online { 1.0 } else { 0.0 });
                crate::services::tactical_modules::apply_projected_rtc_to_module_attrs(ctx, ship_id, &mut m_map);
                let mut m_attrs: Vec<(PyRep, PyRep)> = m_map.into_iter().map(|(aid, v)| (PyRep::Integer(aid as i64), PyRep::Real(v))).collect();
                m_attrs.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
                module_entries.push((PyRep::Integer(m_state.item_id), PyRep::Dict(m_attrs)));
            }
            for m in fit.modules.values().filter(|m| m.state == eve_dogma::ModuleState::Offline) {
                let mut m_map: HashMap<u16, f64> = ctx.sde.types.get(&m.type_id).map(|t| t.default_attributes.clone()).unwrap_or_default();
                m_map.insert(2, 0.0);
                let mut m_attrs: Vec<(PyRep, PyRep)> = m_map.into_iter().map(|(aid, v)| (PyRep::Integer(aid as i64), PyRep::Real(v))).collect();
                m_attrs.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
                module_entries.push((PyRep::Integer(m.item_id), PyRep::Dict(m_attrs)));
            }

            let ship_type_id = fit.ship.type_id;
            let active_stance = fit
                .active_stance_type_id
                .or_else(|| eve_dogma::ShipFittingEngine::default_t3d_stance_type_id(ship_type_id))
                .map(|tid| PyRep::Integer(tid as i64))
                .unwrap_or(PyRep::None);

            ctx.store_ship_fitting(ship_id, fit);

            if method == "GetAllInfo" {
                Some(Ok(build_all_info(ctx, session, char_id, ship_id, ship_type_id, active_stance, entries, module_entries, args)))
            } else {
                Some(Ok(PyRep::Dict(entries)))
            }
        }

        ("shipStanceMgr", "SetShipStance") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (ship_id_opt, stance_type_id_opt) = if args.len() >= 2 {
                (
                    args.first().and_then(|a| a.as_i64()),
                    args.get(1).and_then(|a| a.as_i64()),
                )
            } else {
                (None, args.first().and_then(|a| a.as_i64()))
            };
            let Some(ship_id) = ship_id_opt
                .filter(|&id| id > 0)
                .or_else(|| bound_entry.map(|b| b.object_id))
                .or(session.ship_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let Some(stance_type_id) = stance_type_id_opt.filter(|&id| id > 0) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidShipStance".into(),
                    message: "SetShipStance requires a valid stance typeID".into(),
                }));
            };
            let now_ms = ctx.now_unix_secs().saturating_mul(1000);
            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            if fit.active_stance_type_id.is_none() {
                fit.active_stance_type_id =
                    eve_dogma::ShipFittingEngine::default_t3d_stance_type_id(fit.ship.type_id);
            }
            if let Err(e) = fit.switch_ship_stance(stance_type_id as u32, now_ms, &ctx.sde) {
                let code = if e.contains("Cooldown") {
                    "StanceSwitchCooldown"
                } else {
                    "InvalidShipStance"
                };
                return Some(Err(BridgeRpcError {
                    code: code.into(),
                    message: e,
                }));
            }
            ctx.store_ship_fitting(ship_id, fit);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnStanceActive",
                "charid",
                vec![PyRep::Integer(ship_id), PyRep::Integer(stance_type_id)],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnModuleAttributeChanges",
                "charid",
                vec![PyRep::Integer(ship_id)],
            );
            Some(Ok(PyRep::Integer(stance_type_id)))
        }

        ("dogmaIM", "ItemGetInfo")
        | ("invbroker", "GetItemInfo")
        | ("invbroker", "GetDynamicItemInfo")
        | ("dogmaIM", "GetDynamicItemInfo") => {
            let Some(raw_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "ItemGetInfo requires itemID or typeID".into(),
                }));
            };

            // 1. Check if raw_id is an item in the world DB or a launched space drone ball
            let item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&raw_id).cloned());
            let space_drone_opt = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space
                    .values()
                    .find(|d| d.ball_id == raw_id || d.drone_item_id == raw_id)
                    .cloned()
            });

            let (type_id, owner_id, dyn_attrs, location_id, qty, singleton) = if let Some(ref item) = item_opt {
                (
                    item.type_id,
                    PyRep::Integer(item.owner_id),
                    item.dynamic_attributes.clone(),
                    Some(item.location_id),
                    item.quantity,
                    item.singleton,
                )
            } else if let Some(ref sd) = space_drone_opt {
                (
                    sd.type_id,
                    PyRep::Integer(sd.owner_char_id),
                    HashMap::new(),
                    Some(sd.parent_ship_id),
                    1,
                    true,
                )
            } else {
                // If not found in items, raw_id is treated as a TypeId
                let tid = raw_id as TypeId;
                (tid, PyRep::None, HashMap::new(), None, 1, false)
            };

            let Some(show_info) = ctx.sde.show_info_for_type(type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "TypeNotFound".into(),
                    message: format!("Type {type_id} not found in SDE"),
                }));
            };

            // Merge default attributes with dynamic attributes
            let mut merged_attrs = show_info.default_attributes.clone();
            for (&attr_id, &val) in &dyn_attrs {
                merged_attrs.insert(attr_id, val);
            }
            let item_cat = ctx.sde.types.get(&type_id).map(|t| t.category_id).unwrap_or(0);
            if item_cat == 18 {
                let parent_opt = space_drone_opt
                    .as_ref()
                    .map(|sd| (sd.owner_char_id, sd.parent_ship_id))
                    .or_else(|| item_opt.as_ref().map(|it| (it.owner_id, it.location_id)));
                if let Some((p_owner, p_ship)) = parent_opt {
                    if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(p_owner, p_ship) {
                        let eval_ctx = fit.evaluated_context(&ctx.sde);
                        if let Some(d_state) = eval_ctx
                            .owned_items
                            .iter()
                            .find(|d| d.item_id == raw_id || d.type_id == type_id)
                        {
                            let all_aids: std::collections::BTreeSet<u16> = d_state
                                .base_attributes
                                .keys()
                                .copied()
                                .chain(d_state.modifiers.iter().map(|m| m.target_attribute_id))
                                .collect();
                            for aid in all_aids {
                                merged_attrs.insert(aid, d_state.get_attribute(aid, &ctx.sde));
                            }
                        }
                        let has_active_triage = fit.modules.values().any(|fm| {
                            matches!(fm.type_id, 27951 | 4294)
                                && matches!(
                                    fm.state,
                                    eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                                )
                        });
                        if has_active_triage {
                            merged_attrs.insert(64, 0.0);
                        }
                    }
                }
            } else if let Some(ref item) = item_opt {
                if item_cat == 6 {
                    if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(item.owner_id, item.item_id) {
                        let eval_ctx = fit.evaluated_context(&ctx.sde);
                        let all_aids: std::collections::BTreeSet<u16> = eval_ctx
                            .ship
                            .base_attributes
                            .keys()
                            .copied()
                            .chain(eval_ctx.ship.modifiers.iter().map(|m| m.target_attribute_id))
                            .collect();
                        for aid in all_aids {
                            merged_attrs.insert(aid, eval_ctx.ship.get_attribute(aid, &ctx.sde));
                        }
                        for (&attr_id, &val) in &dyn_attrs {
                            merged_attrs.insert(attr_id, val);
                        }
                        apply_active_dbuffs_and_system_effects_to_ship_attrs(ctx, item.item_id, &mut merged_attrs);
                    }
                } else if matches!(item.flag as u16, 11..=34 | 92..=94 | 125..=132) {
                    if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(item.owner_id, item.location_id) {
                        let eval_ctx = fit.evaluated_context(&ctx.sde);
                        if item_cat == 8 {
                            if let Some(c_state) = eval_ctx
                                .owned_items
                                .iter()
                                .find(|c| c.type_id == item.type_id)
                            {
                                let all_aids: std::collections::BTreeSet<u16> = c_state
                                    .base_attributes
                                    .keys()
                                    .copied()
                                    .chain(c_state.modifiers.iter().map(|m| m.target_attribute_id))
                                    .collect();
                                for aid in all_aids {
                                    merged_attrs.insert(aid, c_state.get_attribute(aid, &ctx.sde));
                                }
                            }
                        } else if let Some(m_state) = eval_ctx
                            .ship_located_items
                            .iter()
                            .find(|m| m.item_id == item.item_id)
                        {
                            let all_aids: std::collections::BTreeSet<u16> = m_state
                                .base_attributes
                                .keys()
                                .copied()
                                .chain(m_state.modifiers.iter().map(|m| m.target_attribute_id))
                                .collect();
                            for aid in all_aids {
                                merged_attrs.insert(aid, m_state.get_attribute(aid, &ctx.sde));
                            }
                            crate::services::tactical_modules::apply_projected_rtc_to_module_attrs(
                                ctx,
                                item.location_id,
                                &mut merged_attrs,
                            );
                        }
                        for &aid in &[3u16, 9, 84, 267, 268, 269, 270] {
                            if let Some(&v) = dyn_attrs.get(&aid) {
                                merged_attrs.insert(aid, v);
                            }
                        }
                    }
                }
            }

            let mut attr_entries: Vec<(PyRep, PyRep)> = merged_attrs
                .into_iter()
                .map(|(k, v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
                .collect();
            attr_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));

            let mut dyn_entries: Vec<(PyRep, PyRep)> = dyn_attrs
                .into_iter()
                .map(|(k, v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
                .collect();
            dyn_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));

            let var_reps: Vec<PyRep> = show_info.variations.into_iter().map(|v| PyRep::Integer(v as i64)).collect();
            let custom_name = ctx.db.with_snapshot(|snap| snap.item_custom_names.get(&raw_id).cloned()).unwrap_or_default();
            let now_ft = ctx.now_blueos_filetime_ticks();
            let inv_item = ctx.db.get_item(raw_id).map(|it| eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(item_to_packed_cells(ctx, &it))).unwrap_or(PyRep::None);

            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(raw_id)),
                ("invItem", inv_item),
                ("typeID", PyRep::Integer(show_info.type_id as i64)),
                ("typeName", PyRep::String(show_info.name)),
                ("customName", PyRep::String(custom_name)),
                ("groupID", PyRep::Integer(show_info.group_id as i64)),
                ("categoryID", PyRep::Integer(show_info.category_id as i64)),
                ("ownerID", owner_id),
                ("locationID", location_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("quantity", PyRep::Integer(qty as i64)),
                ("singleton", PyRep::Bool(singleton)),
                ("mass", PyRep::Real(show_info.mass)),
                ("volume", PyRep::Real(show_info.volume)),
                ("capacity", PyRep::Real(show_info.capacity)),
                ("portionSize", PyRep::Integer(show_info.portion_size as i64)),
                ("basePrice", PyRep::Real(show_info.base_price)),
                ("published", PyRep::Bool(show_info.published)),
                ("marketGroupID", show_info.market_group_id.map(|mg| PyRep::Integer(mg as i64)).unwrap_or(PyRep::None)),
                ("variationParentTypeID", show_info.variation_parent_type_id.map(|vp| PyRep::Integer(vp as i64)).unwrap_or(PyRep::None)),
                ("variations", PyRep::List(var_reps)),
                ("dynamicAttributes", PyRep::Dict(dyn_entries)),
                ("attributes", PyRep::Dict(attr_entries)),
                ("activeEffects", PyRep::Dict(Vec::new())),
                ("time", PyRep::Integer(now_ft)),
                ("wallclockTime", PyRep::Integer(now_ft)),
            ])))
        }

        ("dogmaIM", "DestroyFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DestroyFitting requires itemID".into(),
                }));
            };
            let loc_opt = ctx.db.with_mut_snapshot(|snap| {
                if let Some(item) = snap.items.get(&item_id) {
                    if item.owner_id != char_id {
                        return None;
                    }
                    let found = (
                        item.location_id,
                        item.flag as i64,
                        item.type_id as i64,
                        item.quantity as i64,
                    );
                    snap.items.remove(&item_id);
                    snap.module_online_states.remove(&item_id);
                    snap.module_active_states.remove(&item_id);
                    snap.module_active_charges.remove(&item_id);
                    Some(found)
                } else {
                    None
                }
            });
            if let Some((loc, flag, type_id, qty)) = loc_opt {
                ctx.invalidate_ship_fitting(loc);
                // V24 client derives `OnModuleUnfitted` from this `OnItemChange` (`ixFlag` change).
                let _ = ctx.notify_fitted_item_trashed(char_id, item_id, type_id, qty, loc, flag);
            }
            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(item_id)),
                ("destroyed", PyRep::Bool(true)),
            ])))
        }

        ("dogmaIM", "SetModuleOnline") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let module_id = args.get(1).and_then(|a| a.as_i64()).or_else(|| args.first().and_then(|a| a.as_i64())).unwrap_or(0);
            if ctx.db.with_snapshot(|s| s.structures.contains_key(&ship_id)) {
                return Some(
                    crate::services::station_structure::structure_lifecycle_rpc::handle_structure_module_online(
                        ctx, session, ship_id, module_id, true,
                    ),
                );
            }
            let owns_ship = ctx.db.with_snapshot(|s| s.items.get(&ship_id).is_some_and(|sh| sh.owner_id == char_id));
            if !owns_ship {
                return Some(Err(BridgeRpcError {
                    code: "ShipOwnershipViolation".into(),
                    message: format!("Ship {ship_id} is not owned by character {char_id}"),
                }));
            }

            if session.station_id.is_none() && session.structure_id.is_none() {
                if let Some(sys_id) = session.solar_system_id {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                        if cs.max_capacitor > 0.0 && cs.capacitor < 0.75 * cs.max_capacitor {
                            return Some(Err(BridgeRpcError {
                                code: "NotEnoughCapacitorToOnline".into(),
                                message: format!(
                                    "Capacitor level ({:.1} GJ / {:.1} GJ) is below 75% requirement to online modules in space",
                                    cs.capacitor, cs.max_capacitor
                                ),
                            }));
                        }
                    }
                }
            }

            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            if let Err(e) = fit.set_module_online(module_id, &ctx.sde) {
                return Some(Err(BridgeRpcError {
                    code: "FittingValidationFailed".into(),
                    message: e,
                }));
            }
            ctx.store_ship_fitting(ship_id, fit);
            ctx.db.with_mut_snapshot(|snap| {
                snap.module_online_states.insert(module_id, true);
            });
            ctx.invalidate_ship_fitting(ship_id);

            // V24 client derives `OnModuleOnlineChange` from the `isOnline` attribute change.
            let _ = ctx.notify_module_online_state(char_id, module_id, true);

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("online", PyRep::Bool(true)),
            ])))
        }

        ("dogmaIM", "TakeModuleOffline") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let module_id = args.get(1).and_then(|a| a.as_i64()).or_else(|| args.first().and_then(|a| a.as_i64())).unwrap_or(0);
            if ctx.db.with_snapshot(|s| s.structures.contains_key(&ship_id)) {
                return Some(
                    crate::services::station_structure::structure_lifecycle_rpc::handle_structure_module_online(
                        ctx, session, ship_id, module_id, false,
                    ),
                );
            }
            let owns_ship = ctx.db.with_snapshot(|s| s.items.get(&ship_id).is_some_and(|sh| sh.owner_id == char_id));
            if !owns_ship {
                return Some(Err(BridgeRpcError {
                    code: "ShipOwnershipViolation".into(),
                    message: format!("Ship {ship_id} is not owned by character {char_id}"),
                }));
            }

            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            if let Err(e) = fit.take_module_offline(module_id, &ctx.sde) {
                return Some(Err(BridgeRpcError {
                    code: "FittingValidationFailed".into(),
                    message: e,
                }));
            }
            ctx.store_ship_fitting(ship_id, fit);
            ctx.db.with_mut_snapshot(|snap| {
                snap.module_online_states.insert(module_id, false);
            });
            ctx.invalidate_ship_fitting(ship_id);
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.remove_module_cycle(ship_id, module_id).await;
            }

            // V24 client derives `OnModuleOnlineChange` from the `isOnline` attribute change.
            let _ = ctx.notify_module_online_state(char_id, module_id, false);

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("online", PyRep::Bool(false)),
            ])))
        }

        ("dogmaIM", "AddTarget") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let Some(target_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "AddTarget requires integer targetID".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError { code: "NoSolarSystem".into(), message: "Session is not in space".into() }));
            };
            if let Err(e) = crate::services::tourney_service::check_tourney_lock_restricted(ctx, sys_id, char_id) {
                return Some(Err(e));
            }
            let sys = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = sys.get_ball_snapshot(ship_id).await.unwrap_or(None) else {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Ship ball {ship_id} not found in system"),
                }));
            };
            let Some((_, target_ball)) = sys.get_ball_snapshot(target_id).await.unwrap_or(None) else {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Target ball {target_id} not found in system"),
                }));
            };

            let fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            if let Ok(Some(cs)) = sys.get_combat_state(ship_id).await {
                if cs.ecm_jammed_remaining_s > 0.0 {
                    return Some(Err(BridgeRpcError {
                        code: "ShipJammed".into(),
                        message: "Targeting systems are jammed by ECM".into(),
                    }));
                }
            }

            let max_range = fit.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, &ctx.sde);
            let dist = (ship_ball.position - target_ball.position).length();
            if dist > max_range {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Target distance ({dist:.1} m) exceeds maxTargetRange ({max_range:.1} m)"),
                }));
            }

            let triage_bonus: usize = fit
                .modules
                .values()
                .filter(|fm| {
                    matches!(
                        fm.state,
                        eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                    )
                })
                .map(|fm| match fm.type_id {
                    27951 => 4usize,
                    4294 => 5usize,
                    _ => 0usize,
                })
                .sum();
            let ship_max = (fit.ship.get_attribute(attr_ids::MAX_LOCKED_TARGETS, &ctx.sde).max(1.0) as usize) + triage_bonus;
            let tm_lvl = ctx.character_skill_levels(char_id).get(&3429).copied().unwrap_or(0);
            let atm_lvl = ctx.character_skill_levels(char_id).get(&3430).copied().unwrap_or(0);
            let max_locked = eve_dogma::compute_effective_max_locked_targets(ship_max, tm_lvl, atm_lvl) + triage_bonus;
            let current_locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
            if current_locks.len() >= max_locked {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: format!("Max locked targets exceeded ({max_locked})"),
                }));
            }

            let scan_res = fit.ship.get_attribute(attr_ids::SCAN_RESOLUTION, &ctx.sde).max(1.0);
            let target_sig = target_ball.radius.max(35.0);
            let lock_sec = eve_destiny::combat::compute_lock_time_seconds(scan_res, target_sig);
            let lock_time_ms = lock_sec * 1000.0;

            let lock = eve_destiny::TargetLock {
                target_id,
                lock_time_ms,
                remaining_lock_ms: lock_time_ms,
                locked: false,
                max_range_m: Some(max_range),
            };
            if let Err(e) = sys.add_target_lock(ship_id, lock).await {
                return Some(Err(BridgeRpcError {
                    code: "TargetingFailed".into(),
                    message: e,
                }));
            }

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnTarget",
                "charid",
                vec![PyRep::Integer(target_id), PyRep::String("locking".into())],
            );
            // V24 `godma.Godma.OnMultiEvent` collects `OnTarget` events and derives `OnTargets`.
            let _ = ctx.notify_target_events(char_id, &[("add", target_id, "")]);

            // Passive Targeter (`group_id == 82`, `feat:modules.passive-targeter`) suppresses `"targetedBy"` yellow-box notification!
            let has_active_passive_targeter = fit.modules.values().any(|fm| {
                matches!(
                    fm.state,
                    eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                ) && ctx.sde.types.get(&fm.type_id).is_some_and(|t| t.group_id == 82)
            });
            if !has_active_passive_targeter {
                if let Some(target_owner_id) = ctx
                    .db
                    .with_snapshot(|s| s.items.get(&target_id).map(|i| i.owner_id))
                    .filter(|&oid| oid != char_id && oid > 0)
                {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(target_owner_id),
                        "OnTarget",
                        "charid",
                        vec![PyRep::Integer(ship_id), PyRep::String("targetedBy".into())],
                    );
                }
            }

            Some(Ok(PyRep::keyval([
                ("targetID", PyRep::Integer(target_id)),
                ("locked", PyRep::Bool(false)),
                ("lockTimeMs", PyRep::Real(lock_time_ms)),
                ("passiveLock", PyRep::Bool(has_active_passive_targeter)),
            ])))
        }

        ("dogmaIM", "RemoveTarget") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let Some(target_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "RemoveTarget requires targetID".into(),
                }));
            };
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.remove_target_lock(ship_id, target_id).await;
            }
            crate::services::tactical_modules::clear_entosis_warmup_on_unlock(ctx, ship_id, Some(target_id));
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnTarget",
                "charid",
                vec![PyRep::Integer(target_id), PyRep::String("lost".into())],
            );
            Some(Ok(PyRep::Integer(target_id)))
        }

        ("dogmaIM", "ClearTargets") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.clear_target_locks(ship_id).await;
            }
            crate::services::tactical_modules::clear_entosis_warmup_on_unlock(ctx, ship_id, None);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnTargetClear",
                "charid",
                vec![],
            );
            Some(Ok(PyRep::None))
        }

        // parity: rpc:dogmaIM.GetTargets
        ("dogmaIM", "GetTargets") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() && session.structure_id.is_none() {
                if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let locks = sys.get_target_locks(ship_id).await.unwrap_or_default();
                    let ids = locks
                        .into_iter()
                        .map(|l| PyRep::Integer(l.target_id))
                        .collect();
                    return Some(Ok(PyRep::List(ids)));
                }
            }
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("dogmaIM", "GetTargeters") => {
            // const-ok: client targeting UI query, empty list when no active locks
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("dogmaIM", "LinkWeapons") | ("charFittingMgr", "LinkWeapons") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let master_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let slave_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError {
                        code: "ShipOwnershipViolation".into(),
                        message: format!("Ship {ship_id} is not owned by {char_id}"),
                    });
                }
                let banks = snap.weapon_banks.entry(ship_id).or_default();
                let bank = banks.entry(master_id).or_insert_with(|| vec![master_id]);
                if !bank.contains(&slave_id) {
                    bank.push(slave_id);
                }
                Ok(PyRep::None)
            });
            Some(res)
        }

        ("dogmaIM", "PeelAndLink") | ("charFittingMgr", "PeelAndLink") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let new_mod_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let target_bank_mod_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError {
                        code: "ShipOwnershipViolation".into(),
                        message: format!("Ship {ship_id} is not owned by {char_id}"),
                    });
                }
                let banks = snap.weapon_banks.entry(ship_id).or_default();
                let mut found_master = None;
                for (&m, list) in banks.iter() {
                    if m == target_bank_mod_id || list.contains(&target_bank_mod_id) {
                        found_master = Some(m);
                        break;
                    }
                }
                let master = found_master.unwrap_or(target_bank_mod_id);
                let list = banks.entry(master).or_insert_with(|| vec![master]);
                if !list.contains(&new_mod_id) {
                    list.push(new_mod_id);
                }
                Ok(PyRep::None)
            });
            Some(res)
        }

        ("dogmaIM", "MergeModuleGroups") | ("charFittingMgr", "MergeModuleGroups") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let master_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let other_master_id = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError {
                        code: "ShipOwnershipViolation".into(),
                        message: format!("Ship {ship_id} is not owned by {char_id}"),
                    });
                }
                let banks = snap.weapon_banks.entry(ship_id).or_default();
                let other_modules = banks.remove(&other_master_id).unwrap_or_else(|| vec![other_master_id]);
                let master_list = banks.entry(master_id).or_insert_with(|| vec![master_id]);
                for id in other_modules {
                    if !master_list.contains(&id) {
                        master_list.push(id);
                    }
                }
                Ok(PyRep::None)
            });
            Some(res)
        }

        ("dogmaIM", "UnlinkModule") | ("charFittingMgr", "UnlinkModule") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let mod_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError {
                        code: "ShipOwnershipViolation".into(),
                        message: format!("Ship {ship_id} is not owned by {char_id}"),
                    });
                }
                if let Some(banks) = snap.weapon_banks.get_mut(&ship_id) {
                    for list in banks.values_mut() {
                        list.retain(|&id| id != mod_id);
                    }
                    banks.retain(|k, v| *k != mod_id && v.len() > 1);
                }
                Ok(PyRep::None)
            });
            Some(res)
        }

        ("dogmaIM", "DestroyWeaponBank") | ("charFittingMgr", "DestroyWeaponBank") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let master_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError {
                        code: "ShipOwnershipViolation".into(),
                        message: format!("Ship {ship_id} is not owned by {char_id}"),
                    });
                }
                if let Some(banks) = snap.weapon_banks.get_mut(&ship_id) {
                    banks.remove(&master_id);
                }
                Ok(PyRep::None)
            });
            Some(res)
        }

        ("dogmaIM", "LinkAllWeapons") | ("charFittingMgr", "LinkAllWeapons") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError {
                        code: "ShipOwnershipViolation".into(),
                        message: format!("Ship {ship_id} is not owned by {char_id}"),
                    });
                }
                let mut weapons: Vec<i64> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.location_id == ship_id
                            && matches!(i.flag as u16, 27..=34)
                            && ctx.sde.types.get(&i.type_id).map(|t| t.category_id == 7).unwrap_or(false)
                    })
                    .map(|i| i.item_id)
                    .collect();
                weapons.sort();
                if let Some(&master) = weapons.first() {
                    let banks = snap.weapon_banks.entry(ship_id).or_default();
                    banks.insert(master, weapons);
                }
                Ok(PyRep::None)
            });
            Some(res)
        }

        ("dogmaIM", "UnlinkAllModules") | ("charFittingMgr", "UnlinkAllModules") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.items.get(&ship_id).is_some_and(|s| s.owner_id == char_id) {
                    return Err(BridgeRpcError {
                        code: "ShipOwnershipViolation".into(),
                        message: format!("Ship {ship_id} is not owned by {char_id}"),
                    });
                }
                snap.weapon_banks.remove(&ship_id);
                Ok(PyRep::None)
            });
            Some(res)
        }

        ("dogmaIM", "LoadAmmo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let owns_ship = ctx.db.with_snapshot(|s| s.items.get(&ship_id).is_some_and(|sh| sh.owner_id == char_id));
            if !owns_ship {
                return Some(Err(BridgeRpcError {
                    code: "ShipOwnershipViolation".into(),
                    message: format!("Ship {ship_id} is not owned by character {char_id}"),
                }));
            }
            let requested_module_ids: Vec<i64> = match args.get(1) {
                Some(PyRep::List(v) | PyRep::Tuple(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
                Some(v) => v.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            let charge_item_ids: Vec<i64> = match args.get(2) {
                Some(PyRep::List(v) | PyRep::Tuple(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
                Some(v) => v.as_i64().into_iter().collect(),
                None => Vec::new(),
            };

            let Some(&first_charge_id) = charge_item_ids.first() else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "LoadAmmo requires charge itemID".into(),
                }));
            };

            let charge_info_opt = ctx.db.with_snapshot(|snap| {
                snap.items.get(&first_charge_id).map(|i| (i.type_id, i.quantity, i.owner_id))
            });
            let Some((charge_type_id, avail_qty, charge_owner_id)) = charge_info_opt else {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Charge item {first_charge_id} not found"),
                }));
            };
            if charge_owner_id != char_id {
                return Some(Err(BridgeRpcError {
                    code: "ItemOwnershipViolation".into(),
                    message: format!("Charge item {first_charge_id} is not owned by character {char_id}"),
                }));
            }

            let charge_vol = ctx.sde.types.get(&charge_type_id).map(|t| t.volume.max(0.0001)).unwrap_or(0.0025);

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

            for &mod_id in &all_target_modules {
                if let Some(mod_item) = ctx.db.with_snapshot(|snap| snap.items.get(&mod_id).cloned()) {
                    if let (Some(wt), Some(ct)) = (ctx.sde.types.get(&mod_item.type_id), ctx.sde.types.get(&charge_type_id)) {
                        let aar_paste_ok = matches!(wt.group_id, 1199 | 1698) && ct.type_id == 28668;
                        let cap_booster_ok = matches!(wt.group_id, 76 | 1156 | 1697)
                            && ct.group_id == 87
                            && ct.volume <= wt.capacity.max(1.0) + 1e-6;
                        let wubble_probe_ok = wt.group_id == 589
                            && matches!(ct.group_id, 548 | 4088)
                            && ct.volume <= wt.capacity.max(1.0) + 1e-6;
                        if !ServerContext::is_charge_compatible_with_weapon(wt, ct)
                            && !aar_paste_ok
                            && !cap_booster_ok
                            && !wubble_probe_ok
                        {
                            return Some(Err(BridgeRpcError {
                                code: "IncompatibleAmmo".into(),
                                message: format!("Charge {} is incompatible with weapon {}", ct.name, wt.name),
                            }));
                        }
                        if matches!(ct.group_id, 482 | 663) {
                            if let Err(e) = ctx.db.with_snapshot(|snap| {
                                ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, charge_type_id)
                            }) {
                                return Some(Err(e));
                            }
                        }
                    }
                }
            }

            let mut rounds_per_mod = 1i32;
            let mut reload_time_ms = 10_000.0;
            if let Some(&first_mod_id) = all_target_modules.first() {
                if let Some(mod_item) = ctx.db.with_snapshot(|snap| snap.items.get(&first_mod_id).cloned()) {
                    if let Some(mod_t) = ctx.sde.types.get(&mod_item.type_id) {
                        let cap = mod_t.capacity.max(0.0);
                        if cap > 0.0 {
                            rounds_per_mod = (cap / charge_vol).floor() as i32;
                        }
                        if let Some(&rt) = mod_t.default_attributes.get(&1795) {
                            reload_time_ms = rt;
                        }
                        if mod_t.group_id == 1770 {
                            let cbs_lvl = ctx
                                .character_skill_levels(char_id)
                                .get(&43728)
                                .copied()
                                .unwrap_or(0)
                                .min(5);
                            reload_time_ms = 60_000.0 * (1.0 - 0.10 * (cbs_lvl as f64));
                        }
                    }
                }
            }
            if rounds_per_mod <= 0 {
                rounds_per_mod = 1;
            }

            let num_mods = (all_target_modules.len() as i32).max(1);
            if avail_qty < num_mods {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientQuantity".into(),
                    message: format!("Need at least {num_mods} charges, but only {avail_qty} available in stack"),
                }));
            }
            let actual_per_mod = rounds_per_mod.min(avail_qty / num_mods).max(1);
            let total_needed = actual_per_mod * num_mods;

            ctx.db.with_mut_snapshot(|snap| {
                if let Some(stack) = snap.items.get_mut(&first_charge_id) {
                    stack.quantity -= total_needed;
                }
                for &mod_id in &all_target_modules {
                    let Some(mod_item) = snap.items.get(&mod_id).cloned() else {
                        continue;
                    };
                    let slot_flag = mod_item.flag;
                    let prev_charges: Vec<i64> = snap
                        .items
                        .values()
                        .filter(|i| i.location_id == ship_id && i.flag == slot_flag && i.item_id != mod_id)
                        .map(|i| i.item_id)
                        .collect();

                    for pid in prev_charges {
                        if let Some(pi) = snap.items.get_mut(&pid) {
                            pi.flag = InventoryFlag::Cargo;
                        }
                    }

                    snap.next_entity_id += 1;
                    let new_charge_id = snap.next_entity_id;
                    snap.items.insert(
                        new_charge_id,
                        eve_db::InventoryItemRecord {
                            item_id: new_charge_id,
                            type_id: charge_type_id,
                            owner_id: char_id,
                            location_id: ship_id,
                            flag: slot_flag,
                            quantity: actual_per_mod,
                            singleton: false,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                }
            });

            ctx.invalidate_ship_fitting(ship_id);

            Some(Ok(PyRep::keyval([
                ("reloadTimeMs", PyRep::Real(reload_time_ms)),
                ("totalLoaded", PyRep::Integer(total_needed as i64)),
            ])))
        }

        ("dogmaIM", "UnloadAmmo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
            let owns_ship = ctx.db.with_snapshot(|s| s.items.get(&ship_id).is_some_and(|sh| sh.owner_id == char_id));
            if !owns_ship {
                return Some(Err(BridgeRpcError {
                    code: "ShipOwnershipViolation".into(),
                    message: format!("Ship {ship_id} is not owned by character {char_id}"),
                }));
            }
            let requested_module_ids: Vec<i64> = match args.get(1) {
                Some(PyRep::List(v) | PyRep::Tuple(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
                Some(v) => v.as_i64().into_iter().collect(),
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

            ctx.db.with_mut_snapshot(|snap| {
                for &mod_id in &all_target_modules {
                    let Some(mod_item) = snap.items.get(&mod_id).cloned() else {
                        continue;
                    };
                    let slot_flag = mod_item.flag;
                    let charges: Vec<i64> = snap
                        .items
                        .values()
                        .filter(|i| i.location_id == ship_id && i.flag == slot_flag && i.item_id != mod_id)
                        .map(|i| i.item_id)
                        .collect();
                    for cid in charges {
                        if let Some(ci) = snap.items.get_mut(&cid) {
                            ci.flag = InventoryFlag::Cargo;
                        }
                    }
                }
            });

            ctx.invalidate_ship_fitting(ship_id);
            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}

/// Builds the `dogmaIM.GetAllInfo(primeCharacter, primeShip, primeStructure)` `KeyVal` consumed by
/// `godma.Prime` / `godma.ProcessAllInfo` (`eve/client/script/environment/godma.pyj:2785-2861`).
#[allow(clippy::too_many_arguments)]
fn build_all_info(
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

    let ship_inv_row = if let Some(mut it) = ctx.db.get_item(ship_id) {
        it.singleton = true;
        header.build_dbrow(item_to_packed_cells(ctx, &it))
    } else {
        let (grp, cat) = ctx
            .sde
            .types
            .get(&ship_type_id)
            .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
            .unwrap_or((25, 6));
        let loc = session.station_id.or(session.solar_system_id.map(i64::from)).unwrap_or(0);
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
            ("attributes", PyRep::Dict(entries.clone())),
            ("activeEffects", PyRep::Dict(Vec::new())),
            ("time", PyRep::Integer(now_ft)),
            ("wallclockTime", PyRep::Integer(now_ft)),
            ("description", PyRep::String(String::new())),
        ]),
    )];
    for (mod_id_rep, mod_attrs_rep) in &module_entries {
        if let Some(mod_id) = mod_id_rep.as_i64() {
            if let Some(mut mod_item) = ctx.db.get_item(mod_id) {
                mod_item.singleton = true;
                let inv_row = header.build_dbrow(item_to_packed_cells(ctx, &mod_item));
                ship_info_entries.push((
                    PyRep::Integer(mod_id),
                    PyRep::keyval([
                        ("itemID", PyRep::Integer(mod_id)),
                        ("invItem", inv_row),
                        ("attributes", mod_attrs_rep.clone()),
                        ("activeEffects", PyRep::Dict(Vec::new())),
                        ("time", PyRep::Integer(now_ft)),
                        ("wallclockTime", PyRep::Integer(now_ft)),
                        ("description", PyRep::String(String::new())),
                    ]),
                ));
            }
        }
    }

    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "ProcessGodmaPrimeLocation",
        "charid",
        vec![PyRep::Integer(ship_id), PyRep::Dict(ship_info_entries.clone())],
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
        // `const.flagPilot = 57` (`_LoadItem` skips `FitItemToLocation` for `flagPilot`).
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
        // `DogmaLocation.__init__` concatenates `(-1,) + charBrain[1:]` and `ApplyBrainEffects`
        // unpacks `(version, charEffects, shipEffects, structureEffects)`.
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
    let ship_state = PyRep::Tuple(vec![
        PyRep::Dict(Vec::new()),
        PyRep::Dict(Vec::new()),
        PyRep::Dict(Vec::new()),
        PyRep::Dict(Vec::new()),
    ]);

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
