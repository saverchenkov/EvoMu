//! `fitting_mining_lasers` — `Deactivate` (mining stop via `mining_cycle`), `StopOverload`, rack overload, nanite repair & `miningScanMgr`.

#![forbid(unsafe_code)]

use super::*;

pub(crate) async fn handle_mining_and_deactivate(
    ctx: &ServerContext,
    session: &mut SessionState,
    _bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("dogmaIM", "Deactivate") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = crate::router::RpcArgs::positional(args);
            let module_id = match rpc.req_positive_i64(0, "moduleID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let effect_name = match rpc.opt_str(1, "effectName") {
                Ok(v) => v.unwrap_or("defaultEffect").to_string(),
                Err(_) => "defaultEffect".to_string(),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                    client_error: None,
                }));
            };
            if effect_name == "online" {
                let is_structure =
                    ctx.db.with_snapshot(|snap| snap.structures.contains_key(&ship_id));
                if is_structure {
                    return Some(
                        crate::services::notify_emitters_skills_trade_godma::handle_dogma_take_module_offline(
                            ctx,
                            session,
                            &[PyRep::Integer(ship_id), PyRep::Integer(module_id)],
                        )
                        .await,
                    );
                }
                if let Err(e) = validate_ship_and_fitted_module(ctx, char_id, ship_id, module_id) {
                    return Some(Err(e));
                }
                let prepared = match crate::services::notify_emitters_skills_trade_godma::prepare_dogma_take_module_offline(
                    ctx,
                    session,
                    char_id,
                    ship_id,
                    module_id,
                )
                .await
                {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let _ = crate::services::mining_cycle::deactivate(
                    ctx,
                    char_id,
                    Some(ship_id),
                    module_id,
                )
                .await;
                return Some(
                    crate::services::notify_emitters_skills_trade_godma::commit_dogma_take_module_offline(
                        ctx,
                        session,
                        char_id,
                        ship_id,
                        module_id,
                        prepared,
                    )
                    .await,
                );
            }
            // Validate ship, fitted-module ownership, and active space cloak BEFORE mutating any
            // mining cycle, EWAR, remote-assist, or module_active_states state so a rejected call
            // or cloaked pilot cannot stop cycles or alter fitting/ballpark state. Note: active
            // modules cycling before cloak (e.g. MWD+cloak) plus groups 330/481/4117 may stop cleanly.
            let (mod_type_id, mod_group_id) =
                match validate_ship_and_fitted_module(ctx, char_id, ship_id, module_id) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            let is_cloak_exempt_group = matches!(mod_group_id, Some(330 | 481 | 4117));
            if !is_cloak_exempt_group
                && !is_module_currently_active_sync(
                    ctx,
                    session.solar_system_id,
                    char_id,
                    ship_id,
                    module_id,
                )
            {
                if let Err(e) = super::fitting_module_cycles::enforce_space_active_cloak_module_guard(
                    ctx,
                    session,
                    ship_id,
                )
                .await
                {
                    return Some(Err(e));
                }
            }
            // Mining modules stop their MN-04 cycle (delivering a short cycle's share of ore); only
            // the pilot whose active ship runs the cycle may stop it.
            match crate::services::mining_cycle::deactivate(ctx, char_id, Some(ship_id), module_id).await {
                Some(Ok(_)) => {
                    return Some(Ok(PyRep::keyval([
                        ("moduleID", PyRep::Integer(module_id)),
                        ("active", PyRep::Bool(false)),
                    ])));
                }
                Some(Err(e)) => return Some(Err(e)),
                None => {}
            }
            let mod_type_opt = Some(mod_type_id);
            let is_mining = mod_type_opt
                .and_then(|tid| ctx.sde.types.get(&tid))
                .map(|t| matches!(t.group_id, 54 | 464 | 483 | 737 | 2004 | 4138) || t.default_attributes.contains_key(&77))
                .unwrap_or(false);

            let mut removed_cycle_delay = None;
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                if sys.is_module_mjd_or_mjfg_spooling_sync(ship_id, module_id) {
                    return Some(Err(BridgeRpcError {
                        code: "CantDeactivateWhileSpooling".into(),
                        message: "Cannot deactivate Micro Jump Drive or Micro Jump Field Generator while spooling".into(),
                        client_error: None,
                    }));
                }
                let is_cap_mjfg = mod_type_opt
                    .and_then(|tid| ctx.sde.types.get(&tid))
                    .is_some_and(|t| t.group_id == 1533 && t.name.contains("Capital"));
                if is_cap_mjfg
                    && sys
                        .get_combat_state(ship_id)
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|cs| cs.mjfg_spool_remaining_s > 0.0)
                {
                    return Some(Err(BridgeRpcError {
                        code: "CantDeactivateWhileSpooling".into(),
                        message: "Cannot deactivate Capital Micro Jump Field Generator while spooling".into(),
                        client_error: None,
                    }));
                }
                if sys.request_cycle_deactivation(ship_id, module_id).await.unwrap_or(false)
                {
                    ctx.emit_typed(
                        crate::router::NotificationTarget::Character(char_id),
                        crate::router::notify_registry::OnModulesDeactivating {
                            module_ids: vec![module_id],
                        },
                    );
                    let max_vel = ctx
                        .fit(char_id, ship_id)
                        .map(|f| f.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde))
                        .unwrap_or(0.0);
                    return Some(Ok(PyRep::keyval([
                        ("moduleID", PyRep::Integer(module_id)),
                        ("active", PyRep::Bool(false)),
                        ("overloaded", PyRep::Bool(false)),
                        ("effectiveMaxVelocity", PyRep::Real(max_vel)),
                    ])));
                }
                removed_cycle_delay = sys.remove_module_cycle(ship_id, module_id).await.ok().flatten();
            }
            let mut fit = match ctx.fit(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let db_key = crate::services::tactical_modules::db_ptr_key(&ctx.db);
            let removed_entosis_module = {
                let mut tst = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
                if tst.entosis_active_links.get(&(db_key, ship_id)).is_some_and(|l| l.module_id == module_id) {
                    tst.entosis_active_links.remove(&(db_key, ship_id)).map(|link| {
                        tst.entosis_target_cycles.remove(&(db_key, link.target_id, ship_id));
                        link.module_id
                    })
                } else {
                    None
                }
            };
            if let Some(entosis_mod_id) = removed_entosis_module {
                if let Some(sys_id) = session.solar_system_id {
                    ctx.systems
                        .unregister_external_active_module(sys_id, ship_id, entosis_mod_id);
                } else {
                    ctx.systems
                        .unregister_external_active_module_in_loaded_systems(ship_id, entosis_mod_id);
                }
            }
            let deact_res = fit.deactivate_module(module_id, &ctx.sde);
            if let Err(e) = deact_res {
                if removed_cycle_delay.is_none() && !is_mining && removed_entosis_module.is_none() {
                    return Some(Err(BridgeRpcError { code: "DogmaDeactivateFailed".into(), message: e.to_string(), client_error: None }));
                }
            }
            let affected_ewar_ships = crate::services::notify_emitters_space_dungeons::flush_module_ewar_end(
                ctx,
                char_id,
                session.solar_system_id,
                ship_id,
                module_id,
            );
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                for tid in affected_ewar_ships {
                    let tp_mult = ctx.effective_target_painter_multiplier(tid);
                    sys.try_recompute_combat_attributes(tid, |t_cs| {
                        t_cs.tp_sig_multiplier = (tp_mult > 1.0 + 1e-6).then_some(tp_mult);
                    });
                }
            }
            crate::services::tactical_modules::remove_projected_remote_assistance(ctx, ship_id, module_id);
            crate::services::combat_weapons_spool::clear_entropic_spool(ctx, ship_id, module_id);

            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnModulesDeactivating {
                    module_ids: vec![module_id],
                },
            );
            if is_mining {
                let (effect_id, guid, is_offensive) =
                    crate::services::mining_cycle::resolve_mining_effect_id_and_fx(
                        ctx,
                        mod_type_opt,
                        &effect_name,
                        None,
                    );
                if let Some(sys_id) = session.solar_system_id.filter(|&s| s > 0) {
                    let charge_type_id = fit.modules.get(&module_id).and_then(|m| m.charge_type_id);
                    crate::services::mining_cycle::emit_mining_godma_and_special_fx(
                        ctx,
                        char_id,
                        ship_id,
                        sys_id,
                        module_id,
                        mod_type_opt,
                        None,
                        charge_type_id,
                        effect_id,
                        &guid,
                        is_offensive,
                        false,
                        0,
                    );
                } else {
                    ctx.notify_godma_ship_effect(
                        crate::router::NotificationTarget::Character(char_id),
                        module_id,
                        effect_id as i32,
                        false,
                        0,
                        0,
                    );
                }
            } else {
                let name_opt = (!effect_name.is_empty() && effect_name != "defaultEffect")
                    .then_some(effect_name.as_str());
                let effect_id = mod_type_opt
                    .and_then(|tid| ctx.effect_id_for_module(tid, name_opt))
                    .or_else(|| {
                        name_opt.and_then(|name| {
                            ctx.sde
                                .effects
                                .values()
                                .find(|e| e.name == name)
                                .map(|e| i64::from(e.effect_id))
                        })
                    })
                    .unwrap_or(match effect_name.as_str() {
                        "online" => 16,
                        "miningLaser" => 67,
                        "miningClouds" => 2726,
                        "mining" => 17,
                        _ => 0,
                    });
                ctx.notify_godma_ship_effect(
                    crate::router::NotificationTarget::Character(char_id),
                    module_id,
                    effect_id as i32,
                    false,
                    0,
                    0,
                );
                if let Some(tid) = mod_type_opt {
                    if let Some((sfx_guid, is_offensive)) =
                        crate::router::binding::special_fx_guid_for_module_sde(
                            &ctx.sde,
                            tid,
                            effect_id,
                            name_opt,
                        )
                    {
                        let now = ctx.now_system_sim_filetime_ticks_for_ship(char_id, ship_id);
                        if let Some(scene) = session
                            .solar_system_id
                            .and_then(crate::router::notify_registry::BallparkScene::try_solar_system)
                        {
                            ctx.emit_typed(
                                crate::router::NotificationTarget::Character(char_id),
                                crate::router::notify_registry::OnSpecialFX {
                                    scene,
                                    ship_id,
                                    module_id: Some(module_id),
                                    module_type_id: Some(tid as i64),
                                    target_id: None,
                                    other_type_id: None,
                                    guid: sfx_guid,
                                    is_offensive,
                                    start: 0,
                                    active: 0,
                                    duration: 0.0,
                                    repeat: 0,
                                    start_time: now,
                                    time_from_start: 0.0,
                                    graphic_info: None,
                                },
                            );
                        }
                    }
                }
            }

            ctx.db.with_mut_snapshot(|snap| {
                snap.module_active_states.remove(&module_id);
                snap.module_active_charges.remove(&module_id);
            });
            if let Some(m_entry) = fit.modules.get(&module_id) {
                let mod_state = fit.build_effective_module_state(module_id, m_entry.type_id, false, &ctx.sde);
                let react_delay_ms = mod_state.get_attribute(669, &ctx.sde);
                let is_cycle_gated = ctx
                    .sde
                    .types
                    .get(&m_entry.type_id)
                    .is_some_and(|t| super::fitting_module_cycles::is_cycle_gated_module_group(t.group_id));
                let delay_s = if is_cycle_gated {
                    removed_cycle_delay.unwrap_or(0.0).max(react_delay_ms / 1000.0)
                } else if react_delay_ms > 0.0 {
                    react_delay_ms / 1000.0
                } else {
                    0.0
                };
                if delay_s > 0.0 {
                    ctx.record_module_reactivation_delay(ship_id, module_id, delay_s);
                }
            }
            ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
            ctx.invalidate_and_push_fit(ship_id);
            let max_vel = ctx
                .fit(char_id, ship_id)
                .map(|f| f.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde))
                .unwrap_or_else(|_| fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde));
            if let Some(sys_id) = session.solar_system_id {
                crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
            }

            if mod_type_opt
                .and_then(|tid| ctx.sde.types.get(&tid))
                .is_some_and(|t| t.group_id == 1815)
            {
                let mut sync_ship_ids =
                    super::fitting_scanners_superweapons::clear_ship_dbuffs_by_source_module(
                        ctx, module_id,
                    );
                if let Some(sys_id) = session.solar_system_id.filter(|&s| s > 0) {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let balls = sys.list_balls().await.unwrap_or_default();
                    let dyn_ship_ids: Vec<i64> = balls
                        .iter()
                        .filter(|b| {
                            b.id > 0
                                && (ctx.sde.types.get(&b.type_id).is_some_and(|t| t.category_id == 6)
                                    || ctx.db.with_snapshot(|s| {
                                        s.items
                                            .get(&b.id)
                                            .and_then(|it| ctx.sde.types.get(&it.type_id))
                                            .is_some_and(|t| t.category_id == 6)
                                    }))
                        })
                        .map(|b| b.id)
                        .collect();
                    ctx.db.with_mut_snapshot(|snap| {
                        for &sid in &dyn_ship_ids {
                            if let Some(s_item) = snap.items.get_mut(&sid) {
                                for aid in [37u16, 55, 109, 110, 111, 113, 263, 265, 267, 268, 269, 270, 271, 272, 273, 274, 552] {
                                    s_item.dynamic_attributes.remove(&aid);
                                }
                            }
                        }
                    });
                    sync_ship_ids.extend(dyn_ship_ids);
                    sync_ship_ids.sort_unstable();
                    sync_ship_ids.dedup();
                    super::fitting_scanners_superweapons::sync_ship_dbuffs_and_destiny(
                        ctx,
                        &sys,
                        &sync_ship_ids,
                        ctx.now_unix_secs(),
                    )
                    .await;
                } else if !sync_ship_ids.is_empty() {
                    super::fitting_scanners_superweapons::sync_ship_dbuffs_and_destiny_sync(
                        ctx,
                        &sync_ship_ids,
                        ctx.now_unix_secs(),
                    );
                }
            }

            if is_mining {
                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("active", PyRep::Bool(false)),
                ])))
            } else {
                Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("overloaded", PyRep::Bool(false)),
                    ("effectiveMaxVelocity", PyRep::Real(max_vel)),
                ])))
            }
        }

        ("dogmaIM", "StopOverload") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = crate::router::RpcArgs::positional(args);
            let module_id = match rpc.req_positive_i64(0, "moduleID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                    client_error: None,
                }));
            };
            let mut fit = match ctx.fit(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            if !fit.modules.contains_key(&module_id) {
                return Some(Err(BridgeRpcError::module_not_found(
                    module_id,
                    Some(ship_id),
                    None,
                    false,
                    format!("Module {module_id} is not fitted to active ship {ship_id}"),
                )));
            }
            let was_cycling = if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                sys.update_module_overload(ship_id, module_id, false).await.unwrap_or(false)
            } else {
                false
            };
            fit.pending_overload_modules.remove(&module_id);
            if !was_cycling {
                if let Some(fm) = fit.modules.get_mut(&module_id) {
                    if fm.state == eve_dogma::ModuleState::Overloaded {
                        fm.state = eve_dogma::ModuleState::Online;
                    }
                }
                fit.recalculate_stats(&ctx.sde);
                ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
                ctx.invalidate_and_push_fit(ship_id);
                if let Some(sys_id) = session.solar_system_id.filter(|&s| s > 0) {
                    crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
                }
            }
            let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
            let heat_id = crate::services::fitting_cycles_overheat::resolve_module_heat_id(ctx, &fit, module_id);

            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnHeatRemoved { heat_id, module_id },
            );

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("overloaded", PyRep::Bool(false)),
                ("effectiveMaxVelocity", PyRep::Real(max_vel)),
            ])))
        }

        ("dogmaIM", "OverloadRack") | ("dogmaIM", "StopOverloadRack") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                    client_error: None,
                }));
            };
            if method == "OverloadRack"
                && (session.station_id.is_some() || session.structure_id.is_some())
            {
                return Some(Err(
                    crate::services::inventory_fitting::fitting_module_cycles::ship_docked_rpc_error(
                        "Cannot overload modules while docked",
                    ),
                ));
            }
            let arg_val = match crate::router::RpcArgs::positional(args).req_non_negative_i64(0, "rackOrModuleID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut fit = match ctx.fit(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let target_rack_idx: usize = if let Some(fm) = fit.modules.get(&arg_val) {
                let Some(idx) = crate::services::fitting_cycles_overheat::rack_index_for_flag(fm.flag_id) else {
                    return Some(Err(BridgeRpcError::module_not_found(
                        arg_val,
                        Some(ship_id),
                        Some(fm.type_id as i64),
                        false,
                        format!("Module {arg_val} is not fitted in a thermodynamic rack"),
                    )));
                };
                idx
            } else if matches!(arg_val, 0 | 1 | 2) {
                arg_val as usize
            } else if let Some(idx) = u16::try_from(arg_val)
                .ok()
                .and_then(crate::services::fitting_cycles_overheat::rack_index_for_flag)
            {
                idx
            } else {
                return Some(Err(BridgeRpcError::module_not_found(
                    arg_val,
                    Some(ship_id),
                    None,
                    false,
                    format!("Module or rack {arg_val} not found on ship {ship_id}"),
                )));
            };
            let overloaded = method == "OverloadRack";
            if overloaded
                && session
                    .solar_system_id
                    .is_some_and(|sid| ctx.systems.get_or_spawn(sid).has_any_active_nanite_repair_sync(ship_id))
            {
                return Some(Err(BridgeRpcError {
                    code: "CannotOverheatWhileRepairing".into(),
                    message: "UserError: Cannot overheat modules while in-space Nanite Repair Paste repair is active".into(),
                    client_error: None,
                }));
            }
            let cycle_overload_states: std::collections::HashMap<i64, (bool, Option<bool>)> =
                if let Some(sys_id) = session.solar_system_id.filter(|&s| s > 0) {
                    let sys = ctx.systems.get_or_spawn(sys_id);
                    let st = sys.actor_state.read().unwrap_or_else(|e| e.into_inner());
                    st.active_module_cycles
                        .iter()
                        .filter(|c| c.ship_id == ship_id)
                        .map(|c| (c.module_id, (c.is_overloaded, c.pending_overloaded)))
                        .collect()
                } else {
                    std::collections::HashMap::new()
                };
            let thermo_lvl = if overloaded {
                crate::services::fitting_cycles_overheat::pilot_thermodynamics_level(ctx, char_id)
            } else {
                0
            };
            let mut notified_modules = Vec::new();
            let mut cycling_modules = Vec::new();
            let mut pending_to_insert = Vec::new();
            for (&mid, fm) in fit.modules.iter_mut() {
                let Some(r_idx) = crate::services::fitting_cycles_overheat::rack_index_for_flag(fm.flag_id) else {
                    continue;
                };
                if r_idx != target_rack_idx || fm.state == eve_dogma::ModuleState::Offline {
                    continue;
                }
                if overloaded {
                    if crate::services::fitting_cycles_overheat::is_module_burned_out(ctx, fm, mid)
                        || !crate::services::fitting_cycles_overheat::can_overload_module(ctx, fm.type_id, thermo_lvl)
                    {
                        continue;
                    }
                    if let Some(&(cyc_ovl, pend_ovl)) = cycle_overload_states.get(&mid) {
                        if (!cyc_ovl && pend_ovl != Some(true)) || (cyc_ovl && pend_ovl == Some(false)) {
                            cycling_modules.push(mid);
                            notified_modules.push(mid);
                        }
                    } else if fm.state == eve_dogma::ModuleState::Online {
                        fm.state = eve_dogma::ModuleState::Overloaded;
                        pending_to_insert.push(mid);
                        notified_modules.push(mid);
                    } else if fm.state == eve_dogma::ModuleState::Active {
                        cycling_modules.push(mid);
                        notified_modules.push(mid);
                    }
                } else if let Some(&(cyc_ovl, pend_ovl)) = cycle_overload_states.get(&mid) {
                    if (cyc_ovl && pend_ovl != Some(false)) || (!cyc_ovl && pend_ovl == Some(true)) {
                        cycling_modules.push(mid);
                        notified_modules.push(mid);
                    }
                } else if fm.state == eve_dogma::ModuleState::Overloaded {
                    cycling_modules.push(mid);
                    notified_modules.push(mid);
                }
            }
            fit.pending_overload_modules.extend(pending_to_insert);
            notified_modules.sort_unstable();
            let sync_idle_overload_state = |fit: &mut eve_dogma::ShipFittingEngine, mid: i64| {
                if overloaded {
                    if let Some(fm) = fit.modules.get_mut(&mid) {
                        if fm.state == eve_dogma::ModuleState::Active {
                            fm.state = eve_dogma::ModuleState::Overloaded;
                            fit.pending_overload_modules.insert(mid);
                        }
                    }
                } else {
                    fit.pending_overload_modules.remove(&mid);
                    if let Some(fm) = fit.modules.get_mut(&mid) {
                        if fm.state == eve_dogma::ModuleState::Overloaded {
                            fm.state = eve_dogma::ModuleState::Online;
                        }
                    }
                }
            };
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                for &mid in &cycling_modules {
                    let was_cycling = sys.update_module_overload(ship_id, mid, overloaded).await.unwrap_or(false);
                    if !was_cycling {
                        sync_idle_overload_state(&mut fit, mid);
                    }
                }
            } else {
                for &mid in &cycling_modules {
                    sync_idle_overload_state(&mut fit, mid);
                }
            }
            fit.recalculate_stats(&ctx.sde);
            ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
            ctx.invalidate_and_push_fit(ship_id);
            let heat_id = crate::services::fitting_cycles_overheat::rack_heat_attr_id_for_rack(target_rack_idx);
            for &mid in &notified_modules {
                if overloaded {
                    ctx.emit_typed(
                        crate::router::NotificationTarget::Character(char_id),
                        crate::router::notify_registry::OnHeatAdded {
                            heat_id,
                            module_id: mid,
                        },
                    );
                } else {
                    ctx.emit_typed(
                        crate::router::NotificationTarget::Character(char_id),
                        crate::router::notify_registry::OnHeatRemoved {
                            heat_id,
                            module_id: mid,
                        },
                    );
                }
            }

            Some(Ok(PyRep::List(
                notified_modules.into_iter().map(PyRep::Integer).collect(),
            )))
        }

        ("dogmaIM", "RepairModule") | ("dogmaIM", "InitiateModuleRepair") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                    client_error: None,
                }));
            };
            if session.station_id.is_some() || session.structure_id.is_some() || session.solar_system_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "CannotRepairModuleWhileDocked".into(),
                    message: "In-space Nanite Repair Paste module repair requires being in space".into(),
                    client_error: None,
                }));
            }
            let sys_id = session.solar_system_id.unwrap();
            let sys = ctx.systems.get_or_spawn(sys_id);
            let module_id = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| {
                    args.first().and_then(|a| match a {
                        PyRep::List(v) | PyRep::Tuple(v) => v.first().and_then(|x| x.as_i64()),
                        other => other.as_i64(),
                    })
                })
                .unwrap_or(0);
            if sys.has_active_nanite_repair_sync(ship_id, module_id) {
                return Some(Err(BridgeRpcError {
                    code: "ModuleAlreadyRepairing".into(),
                    message: format!("Module {module_id} is already undergoing nanite repair"),
                    client_error: None,
                }));
            }
            if sys.has_active_overloaded_cycle_sync(ship_id) {
                return Some(Err(BridgeRpcError {
                    code: "CannotRepairWhileOverloaded".into(),
                    message: "UserError: Cannot initiate Nanite Repair Paste repair while another module on the ship is overloaded".into(),
                    client_error: None,
                }));
            }

            let mod_check = ctx.db.with_snapshot(|snap| {
                let Some(m_item) = snap.items.get(&module_id) else {
                    return Err(BridgeRpcError::module_not_found(
                        module_id,
                        Some(ship_id),
                        None,
                        false,
                        format!("Module {module_id} not found"),
                    ));
                };
                if m_item.owner_id != char_id
                    || m_item.location_id != ship_id
                    || !matches!(m_item.flag as u16, 11..=34 | 92..=94 | 125..=132)
                {
                    return Err(BridgeRpcError {
                        code: "ModuleOwnershipViolation".into(),
                        message: format!(
                            "Module {module_id} is not fitted on active ship {ship_id} owned by character {char_id}"
                        ),
                        client_error: None,
                    });
                }
                let active = snap.module_active_states.contains(&module_id);
                // parity: feat:modules.emergency-hull-energizer
                let is_ehe = matches!(m_item.type_id, 40714..=40718)
                    || ctx
                        .sde
                        .types
                        .get(&m_item.type_id)
                        .is_some_and(|t| t.effect_ids.contains(&6484));
                if is_ehe {
                    return Err(BridgeRpcError {
                        code: "ModuleBurnedOut".into(),
                        message: "UserError: Capital Emergency Hull Energizer cannot be repaired with Nanite Repair Paste".into(),
                        client_error: None,
                    });
                }
                let dmg = m_item
                    .dynamic_attributes
                    .get(&attr_ids::DAMAGE)
                    .copied()
                    .unwrap_or(0.0);
                let hp = m_item
                    .dynamic_attributes
                    .get(&attr_ids::HP_STRUCT)
                    .copied()
                    .or_else(|| {
                        ctx.sde
                            .types
                            .get(&m_item.type_id)
                            .and_then(|t| t.default_attributes.get(&attr_ids::HP_STRUCT).copied())
                    })
                    .unwrap_or(40.0);
                let sk = snap.skills.get(&char_id);
                let thermo_lvl = sk.and_then(|m| m.get(&28164)).map(|r| r.level).unwrap_or(0);
                let op_lvl = sk.and_then(|m| m.get(&28879)).map(|r| r.level).unwrap_or(0);
                let int_lvl = sk.and_then(|m| m.get(&28880)).map(|r| r.level).unwrap_or(0);
                Ok((active, dmg, hp, m_item.type_id, thermo_lvl, op_lvl, int_lvl))
            });
            let (is_active, cur_dmg, max_hp, mod_type_id, thermo_lvl, nanite_op_lvl, nanite_interfacing_lvl) =
                match mod_check {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };

            if is_active {
                return Some(Err(BridgeRpcError {
                    code: "CannotRepairActiveModule".into(),
                    message: "Module must be deactivated before initiating nanite repair".into(),
                    client_error: None,
                }));
            }
            if cur_dmg >= max_hp - 1e-6 {
                return Some(Err(BridgeRpcError {
                    code: "ModuleBurnedOut".into(),
                    message: "Completely burned-out modules cannot be repaired with Nanite Repair Paste in space".into(),
                    client_error: None,
                }));
            }
            if cur_dmg <= 1e-6 {
                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("pasteConsumed", PyRep::Integer(0)),
                    ("damageToRepair", PyRep::Real(0.0)),
                ])));
            }

            let (paste_needed, repair_rate_hp_per_s, duration_s) = compute_nanite_repair_params(
                ctx.sde.types.get(&mod_type_id),
                cur_dmg,
                max_hp,
                thermo_lvl,
                nanite_op_lvl,
                nanite_interfacing_lvl,
            );

            let deduct_res: Result<i32, BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(m_item) = snap.items.get(&module_id) else {
                    return Err(BridgeRpcError::module_not_found(
                        module_id,
                        Some(ship_id),
                        None,
                        false,
                        format!("Module {module_id} not found"),
                    ));
                };
                if m_item.owner_id != char_id
                    || m_item.location_id != ship_id
                    || !matches!(m_item.flag as u16, 11..=34 | 92..=94 | 125..=132)
                {
                    return Err(BridgeRpcError {
                        code: "ModuleOwnershipViolation".into(),
                        message: format!(
                            "Module {module_id} is not fitted on active ship {ship_id} owned by character {char_id}"
                        ),
                        client_error: None,
                    });
                }
                let initial_deduct = if session.solar_system_id.is_some() {
                    1_i32.min(paste_needed.max(1))
                } else {
                    paste_needed.max(1)
                };
                let avail: i32 = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && i.location_id == ship_id
                            && i.flag == InventoryFlag::Cargo
                            && i.type_id == 28668
                            && i.quantity > 0
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                    })
                    .map(|i| i.quantity)
                    .sum();
                if avail < initial_deduct {
                    return Err(BridgeRpcError {
                        code: "NotEnoughNanitePaste".into(),
                        message: format!(
                            "Need {initial_deduct} Nanite Repair Paste (type 28668) in ship cargo"
                        ),
                        client_error: None,
                    });
                }
                let taken = crate::services::tactical_module_mechanics::deduct_paste_from_cargo_in_snap(
                    snap,
                    char_id,
                    ship_id,
                    initial_deduct,
                );
                Ok(taken)
            });
            let initial_consumed = match deduct_res {
                Ok(c) => c,
                Err(e) => return Some(Err(e)),
            };

            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys
                    .start_nanite_repair(crate::system_actor::ActiveNaniteRepair {
                        char_id,
                        ship_id,
                        module_id,
                        initial_damage_hp: cur_dmg,
                        paste_total_needed: paste_needed.max(1),
                        paste_consumed_so_far: initial_consumed,
                        duration_s,
                        elapsed_s: 0.0,
                    })
                    .await;
            } else {
                ctx.db.with_mut_snapshot(|snap| {
                    if let Some(m_item) = snap.items.get_mut(&module_id) {
                        if m_item.owner_id == char_id
                            && m_item.location_id == ship_id
                            && matches!(m_item.flag as u16, 11..=34 | 92..=94 | 125..=132)
                        {
                            m_item.dynamic_attributes.remove(&attr_ids::DAMAGE);
                        }
                    }
                });
            }

            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnMultiModuleRepairStarted {
                    module_ids: vec![PyRep::Integer(module_id)],
                },
            );

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("pasteConsumed", PyRep::Integer(initial_consumed as i64)),
                ("totalPasteRequired", PyRep::Integer(paste_needed.max(1) as i64)),
                ("damageToRepair", PyRep::Real(cur_dmg)),
                ("repairRateHpPerSec", PyRep::Real(repair_rate_hp_per_s)),
            ])))
        }

        ("dogmaIM", "StopRepairModule") | ("dogmaIM", "StopModuleRepair") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                    client_error: None,
                }));
            };
            let module_id = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .unwrap_or(0);
            let mut stopped = false;
            let mut rem_dmg_out = 0.0;
            let mut paste_refunded = 0i32;
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                if let Some((rem_dmg, refund)) = sys
                    .stop_nanite_repair(ship_id, module_id)
                    .await
                    .ok()
                    .flatten()
                {
                    stopped = true;
                    rem_dmg_out = rem_dmg;
                    paste_refunded = ctx.db.with_mut_snapshot(|snap| {
                        if let Some(m_item) = snap.items.get_mut(&module_id) {
                            if rem_dmg <= 1e-6 {
                                m_item.dynamic_attributes.remove(&attr_ids::DAMAGE);
                            } else {
                                m_item.dynamic_attributes.insert(attr_ids::DAMAGE, rem_dmg);
                            }
                        }
                        crate::services::tactical_module_mechanics::refund_paste_to_cargo_capped_in_snap(
                            snap,
                            &ctx.sde,
                            char_id,
                            ship_id,
                            refund,
                        )
                    });
                    ctx.invalidate_and_push_fit(ship_id);
                }
            }
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnRepairDone {
                    item_ids: vec![module_id],
                },
            );
            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("stopped", PyRep::Bool(stopped)),
                ("remainingDamage", PyRep::Real(rem_dmg_out)),
                ("pasteRefunded", PyRep::Integer(paste_refunded as i64)),
            ])))
        }

        ("miningScanMgr", "perform_scan") => {
            Some(handle_mining_survey_scan(ctx, session, args, kwargs).await)
        }


        _ => None,
    }
}

async fn handle_mining_survey_scan(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let docked_err = || {
        crate::services::inventory_fitting::fitting_module_cycles::ship_docked_rpc_error(
            "Cannot perform survey scan while docked",
        )
    };
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(docked_err());
    }
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "No active ship in session".into(),
            client_error: None,
        });
    };
    if session.solar_system_id.is_none()
        && ctx.db.with_snapshot(|s| {
            s.characters
                .get(&char_id)
                .is_some_and(|c| c.station_id.is_some() || c.structure_id.is_some())
        })
    {
        return Err(docked_err());
    }
    let (sys_id, ship_pos) = if let Some((id, handle)) = session
        .solar_system_id
        .filter(|&s| s > 0)
        .and_then(|id| ctx.systems.get_handle(id).map(|h| (id, h)))
    {
        handle
            .get_ball_snapshot(ship_id)
            .await
            .ok()
            .flatten()
            .map(|(_, ball)| (id, ball.position))
    } else {
        None
    }
    .ok_or_else(|| {
        BridgeRpcError::ship_not_in_space(
            ship_id,
            Some(char_id),
            session.solar_system_id,
            "perform_scan",
            "Active ship is not in space",
        )
    })?;
    let explicit_mod_id =
        crate::router::RpcArgs::new(args, kwargs).opt_positive_i64(0, "moduleID")?;

    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    let online_states = ctx.db.with_snapshot(|s| s.module_online_states.clone());
    let is_survey_scanner = |type_id: u32| {
        ctx.sde
            .types
            .get(&type_id)
            .is_some_and(|t| t.group_id == 49 || t.default_attributes.contains_key(&197))
    };
    let mut matching_scanners: Vec<&eve_dogma::FittedModule> = fit
        .modules
        .values()
        .filter(|m| {
            (11..=34).contains(&m.flag_id)
                && explicit_mod_id.map_or(true, |mid| m.item_id == mid)
                && is_survey_scanner(m.type_id)
        })
        .collect();
    matching_scanners.sort_by_key(|m| m.item_id);

    let is_online = |m: &eve_dogma::FittedModule| {
        m.state != eve_dogma::ModuleState::Offline
            && online_states.get(&m.item_id).copied().unwrap_or(true)
    };
    let scanner_type_id = if let Some(online) = matching_scanners.iter().copied().find(|m| is_online(m)) {
        online.type_id
    } else {
        let offline_mod_id = matching_scanners.first().map(|m| m.item_id).or_else(|| {
            ctx.db.get_items_for_character(char_id).into_iter().find_map(|i| {
                let flag = i.flag as u16;
                (i.location_id == ship_id
                    && (11..=34).contains(&flag)
                    && explicit_mod_id.map_or(true, |mid| i.item_id == mid)
                    && is_survey_scanner(i.type_id))
                .then_some(i.item_id)
            })
        });
        if let Some(mid) = offline_mod_id {
            return Err(BridgeRpcError {
                code: "ModuleOffline".into(),
                message: format!("Survey scanner {mid} is offline"),
                client_error: None,
            });
        }
        return Err(BridgeRpcError::module_not_found(
            explicit_mod_id.unwrap_or(0),
            Some(ship_id),
            None,
            false,
            "No survey scanner fitted to ship",
        ));
    };

    let scanner_type = ctx.sde.types.get(&scanner_type_id);
    let base_scan_range = scanner_type
        .and_then(|t| t.default_attributes.get(&197).copied().or_else(|| t.default_attributes.get(&54).copied()))
        .unwrap_or(15_000.0);
    let range_mult = super::fitting_scanners_superweapons::get_aggregated_ship_dbuffs(
        ctx,
        ship_id,
        ctx.now_unix_secs(),
    )
    .get(&23)
    .map(|(v, _)| 1.0 + v / 100.0)
    .unwrap_or(1.0);
    let scan_range = base_scan_range * range_mult;

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
            ctx.emit_typed(
                crate::router::NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnAsteroidRevealedDetailed {
                    asteroid_id: ast.asteroid_id,
                    type_id: ast.type_id as i64,
                    quantity: ast.quantity,
                },
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

    Ok(PyRep::keyval([
        ("scanRange", PyRep::Real(scan_range)),
        ("count", PyRep::Integer(scanned_asteroids.len() as i64)),
        ("asteroidCount", PyRep::Integer(scanned_asteroids.len() as i64)),
        ("asteroids", PyRep::List(scanned_asteroids)),
    ]))
}

/// Computes `(paste_needed, repair_rate_hp_per_s, duration_s)` for in-space Nanite Repair Paste
/// repair (`feat:modules.nanite-repair-paste-in-space`) from SDE attributes:
/// - `shipBrokenModuleRepairCostMultiplier` (attr `1277`, default `0.5` paste per HP of damage)
/// - `shipBrokenRepairCostMultiplierBonus` (attr `1294` on `Nanite Operation 28879`, `-5%/lvl`)
/// - `moduleRepairRate` (attr `1267`, default `10.0` HP/s)
/// - `moduleRepairRateBonus` (attr `1295` on `Nanite Interfacing 28880`, `+20%/lvl`)
pub(crate) fn compute_nanite_repair_params(
    mod_type: Option<&eve_sde::InvType>,
    cur_dmg: f64,
    _max_hp: f64,
    _thermo_lvl: u8,
    nanite_op_lvl: u8,
    nanite_interfacing_lvl: u8,
) -> (i32, f64, f64) {
    let cost_per_hp = mod_type
        .and_then(|t| t.default_attributes.get(&1277).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(0.5);
    let skill_cost_mult = 1.0 - 0.05 * (nanite_op_lvl.min(5) as f64);
    let raw_paste = cur_dmg.max(0.0) * cost_per_hp * skill_cost_mult;
    let paste_needed = if cur_dmg <= 0.0 {
        0
    } else {
        (raw_paste - 1e-9).ceil().max(1.0) as i32
    };
    let base_rate = mod_type
        .and_then(|t| t.default_attributes.get(&1267).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(10.0);
    let repair_rate_hp_per_s = base_rate * (1.0 + 0.20 * (nanite_interfacing_lvl.min(5) as f64));
    let duration_s = (cur_dmg / repair_rate_hp_per_s).max(1.0);
    (paste_needed, repair_rate_hp_per_s, duration_s)
}

/// Validates ship ownership and fitted-module ownership read-only and returns `(type_id, group_id)`.
fn validate_ship_and_fitted_module(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    module_id: i64,
) -> Result<(u32, Option<u32>), BridgeRpcError> {
    let mod_type_id = ctx.db.with_snapshot(|snap| {
        let Some(ship_item) = snap.items.get(&ship_id) else {
            return Err(BridgeRpcError {
                code: "ShipNotFound".into(),
                message: format!("Active ship {ship_id} not found"),
                client_error: None,
            });
        };
        if ship_item.owner_id != char_id {
            return Err(BridgeRpcError {
                code: "ShipOwnershipViolation".into(),
                message: format!("Ship {ship_id} is not owned by character {char_id}"),
                client_error: None,
            });
        }
        let Some(mod_item) = snap.items.get(&module_id) else {
            return Err(BridgeRpcError::module_not_found(
                module_id,
                Some(ship_id),
                None,
                false,
                format!("Module {module_id} not found"),
            ));
        };
        let flag_u16 = mod_item.flag as u16;
        let is_fitted_slot = matches!(flag_u16, 11..=34 | 125..=129 | 158 | 159..=163);
        if mod_item.owner_id != char_id || mod_item.location_id != ship_id || !is_fitted_slot {
            return Err(BridgeRpcError {
                code: "ModuleNotOwnedByShip".into(),
                message: format!("Module {module_id} is not fitted to active ship {ship_id}"),
                client_error: None,
            });
        }
        Ok(mod_item.type_id)
    })?;
    let mod_group_id = ctx.sde.types.get(&mod_type_id).map(|t| t.group_id);
    Ok((mod_type_id, mod_group_id))
}

/// Canonical synchronous check for whether `module_id` is currently active in the system
/// actor's active-module registry (`active_module_cycles` or `external_active_modules`),
/// without reading stale DB or service-map state (`#1486`, `PVE-133`).
pub(crate) fn is_module_currently_active_sync(
    ctx: &ServerContext,
    solar_system_id: Option<u32>,
    _char_id: i64,
    ship_id: i64,
    module_id: i64,
) -> bool {
    solar_system_id.is_some_and(|sys_id| ctx.systems.is_module_active_sync(sys_id, ship_id, module_id))
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(async move {
        let res = if service == "dogmaIM" && method == "Deactivate" {
            if let Some(burst_res) =
                crate::services::corp_registry_ops::handle_fleet_burst_fallback(
                    ctx, session, service, method, args,
                )
                .await
            {
                Some(burst_res)
            } else {
                handle_mining_and_deactivate(ctx, session, bound_entry, service, method, args, kwargs)
                    .await
            }
        } else {
            handle_mining_and_deactivate(ctx, session, bound_entry, service, method, args, kwargs)
                .await
        };
        if matches!(&res, Some(Ok(_))) && method == "Deactivate" {
            crate::services::character_skill_ops::post_inventory_fitting_hook(
                ctx,
                session,
                bound_entry,
                method,
                args,
            )
            .await;
        }
        res
    })
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("miningScanMgr", "perform_scan", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("dogmaIM", "Deactivate", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("dogmaIM", "StopOverload", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("dogmaIM", "OverloadRack", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("dogmaIM", "StopOverloadRack", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("dogmaIM", "RepairModule", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("dogmaIM", "InitiateModuleRepair", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("dogmaIM", "StopRepairModule", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("dogmaIM", "StopModuleRepair", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
];

#[cfg(test)]
#[path = "fitting_mining_lasers_tests.rs"]
mod fitting_mining_lasers_tests;

