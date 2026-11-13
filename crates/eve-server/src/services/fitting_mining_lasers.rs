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
    _kwargs: &HashMap<String, PyRep>,
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
            if effect_name == "online" {
                let Some(ship_id) = session.ship_id else {
                    return Some(Err(BridgeRpcError {
                        code: "NoActiveShip".into(),
                        message: "No active ship in session".into(),
                        client_error: None,
                    }));
                };
                let _ = crate::services::mining_cycle::deactivate(ctx, char_id, Some(ship_id), module_id).await;
                return Some(
                    crate::services::notify_emitters_skills_trade_godma::handle_dogma_take_module_offline(
                        ctx,
                        session,
                        &[PyRep::Integer(ship_id), PyRep::Integer(module_id)],
                    )
                    .await,
                );
            }
            // Mining modules stop their MN-04 cycle (delivering a short cycle's share of ore); only
            // the pilot whose active ship runs the cycle may stop it.
            match crate::services::mining_cycle::deactivate(ctx, char_id, session.ship_id, module_id).await {
                Some(Ok(_)) => {
                    return Some(Ok(PyRep::keyval([
                        ("moduleID", PyRep::Integer(module_id)),
                        ("active", PyRep::Bool(false)),
                    ])));
                }
                Some(Err(e)) => return Some(Err(e)),
                None => {}
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                    client_error: None,
                }));
            };
            // Validate ship and fitted-module ownership BEFORE mutating any EWAR, remote-assist,
            // cycle, or module_active_states state so a target cannot deactivate an attacker's module.
            let mod_type_opt = match ctx.db.with_snapshot(|snap| {
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
                    return Err(BridgeRpcError {
                        code: "ModuleNotFound".into(),
                        message: format!("Module {module_id} not found"),
                        client_error: None,
                    });
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
            }) {
                Ok(tid) => Some(tid),
                Err(e) => return Some(Err(e)),
            };
            let is_mining = mod_type_opt
                .and_then(|tid| ctx.sde.types.get(&tid))
                .map(|t| matches!(t.group_id, 54 | 464 | 483 | 737 | 2004 | 4138) || t.default_attributes.contains_key(&77))
                .unwrap_or(false);

            let is_paid_cycle_deferred = mod_type_opt
                .and_then(|tid| ctx.sde.types.get(&tid))
                .is_some_and(|t| {
                    matches!(t.group_id, 62 | 63 | 68 | 325 | 585 | 1199 | 1698)
                });
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
                if is_paid_cycle_deferred
                    && sys.request_cycle_deactivation(ship_id, module_id).await.unwrap_or(false)
                {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnModulesDeactivating",
                        "charid",
                        vec![PyRep::List(vec![PyRep::Integer(module_id)])],
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
            let removed_entosis = {
                let mut tst = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
                if tst.entosis_active_links.get(&(db_key, ship_id)).is_some_and(|l| l.module_id == module_id) {
                    if let Some(link) = tst.entosis_active_links.remove(&(db_key, ship_id)) {
                        tst.entosis_target_cycles.remove(&(db_key, link.target_id, ship_id));
                    }
                    true
                } else {
                    false
                }
            };
            let deact_res = fit.deactivate_module(module_id, &ctx.sde);
            if let Err(e) = deact_res {
                if removed_cycle_delay.is_none() && !is_mining && !removed_entosis {
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

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnModulesDeactivating",
                "charid",
                vec![PyRep::List(vec![PyRep::Integer(module_id)])],
            );
            if is_mining {
                let (effect_id, guid, is_offensive) =
                    crate::services::mining_cycle::resolve_mining_effect_id_and_fx(
                        ctx,
                        mod_type_opt,
                        &effect_name,
                        None,
                    );
                if let Some(sys_id) = session.solar_system_id {
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
                        let now = ctx.now_blueos_filetime_ticks();
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnSpecialFX",
                            "charid",
                            vec![
                                PyRep::Integer(ship_id),
                                PyRep::Integer(module_id),
                                PyRep::Integer(tid as i64),
                                PyRep::None,
                                PyRep::None,
                                PyRep::String(sfx_guid),
                                PyRep::Bool(is_offensive),
                                PyRep::Integer(0),
                                PyRep::Integer(0),
                                PyRep::Real(0.0),
                                PyRep::Integer(0),
                                PyRep::Integer(now),
                                PyRep::Real(0.0),
                                PyRep::None,
                            ],
                        );
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
                let is_cycle_gated = ctx.sde.types.get(&m_entry.type_id).is_some_and(|t| {
                    matches!(t.group_id, 40 | 1156 | 41 | 1697 | 67 | 71 | 76 | 72 | 1122 | 650 | 80 | 353)
                });
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
                if let Some(sys_id) = session.solar_system_id {
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
            let was_cycling = if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                sys.update_module_overload(ship_id, module_id, false).await.unwrap_or(false)
            } else {
                false
            };
            let mut fit = match ctx.fit(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            if !was_cycling {
                if let Some(fm) = fit.modules.get_mut(&module_id) {
                    if fm.state == eve_dogma::ModuleState::Overloaded {
                        fm.state = eve_dogma::ModuleState::Online;
                    }
                }
                fit.recalculate_stats(&ctx.sde);
                ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
                ctx.invalidate_and_push_fit(ship_id);
                if let Some(sys_id) = session.solar_system_id {
                    crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
                }
            }
            let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnHeatRemoved",
                "charid",
                vec![PyRep::Integer(module_id)],
            );
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnModuleAttributeChanges",
                "charid",
                vec![PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::String("OnModuleAttributeChange".into()),
                    PyRep::Integer(char_id),
                    PyRep::Integer(module_id),
                    PyRep::Integer(eve_sde::attr_ids::DAMAGE as i64),
                    PyRep::Integer(ctx.now_blueos_filetime_ticks()),
                    PyRep::Real(0.0),
                    PyRep::Real(0.0),
                    PyRep::None,
                ])])],
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
            let arg_val = match crate::router::RpcArgs::positional(args).req_non_negative_i64(0, "rackOrModuleID") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut fit = match ctx.fit(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let target_rack_idx: usize = if let Some(fm) = fit.modules.get(&arg_val) {
                match fm.flag_id {
                    27..=34 => 0,
                    19..=26 => 1,
                    _ => 2,
                }
            } else {
                match arg_val as u16 {
                    27..=34 | 0 => 0,
                    19..=26 | 1 => 1,
                    _ => 2,
                }
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
            let mut notified_modules = Vec::new();
            let mut cycling_modules = Vec::new();
            for (&mid, fm) in fit.modules.iter_mut() {
                let r_idx = match fm.flag_id {
                    27..=34 => 0,
                    19..=26 => 1,
                    11..=18 => 2,
                    _ => continue,
                };
                if r_idx != target_rack_idx || fm.state == eve_dogma::ModuleState::Offline {
                    continue;
                }
                if overloaded {
                    if fm.state == eve_dogma::ModuleState::Online {
                        fm.state = eve_dogma::ModuleState::Overloaded;
                        notified_modules.push(mid);
                    } else if fm.state == eve_dogma::ModuleState::Active {
                        cycling_modules.push(mid);
                        notified_modules.push(mid);
                    }
                } else if fm.state == eve_dogma::ModuleState::Overloaded {
                    cycling_modules.push(mid);
                    notified_modules.push(mid);
                }
            }
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                for &mid in &cycling_modules {
                    let was_cycling = sys.update_module_overload(ship_id, mid, overloaded).await.unwrap_or(false);
                    if !overloaded && !was_cycling {
                        if let Some(fm) = fit.modules.get_mut(&mid) {
                            if fm.state == eve_dogma::ModuleState::Overloaded {
                                fm.state = eve_dogma::ModuleState::Online;
                            }
                        }
                    }
                }
            } else if !overloaded {
                for &mid in &cycling_modules {
                    if let Some(fm) = fit.modules.get_mut(&mid) {
                        if fm.state == eve_dogma::ModuleState::Overloaded {
                            fm.state = eve_dogma::ModuleState::Online;
                        }
                    }
                }
            }
            fit.recalculate_stats(&ctx.sde);
            ctx.persist_fit_inputs(ship_id, &fit.persisted_inputs());
            ctx.invalidate_and_push_fit(ship_id);
            let notif_name = if overloaded { "OnHeatAdded" } else { "OnHeatRemoved" };
            for &mid in &notified_modules {
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    notif_name,
                    "charid",
                    vec![PyRep::Integer(mid)],
                );
            }

            Some(Ok(PyRep::keyval([
                ("rackIndex", PyRep::Integer(target_rack_idx as i64)),
                ("overloaded", PyRep::Bool(overloaded)),
                (
                    "moduleIDs",
                    PyRep::List(notified_modules.into_iter().map(PyRep::Integer).collect()),
                ),
            ])))
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
                    return Err(BridgeRpcError {
                        code: "ModuleNotFound".into(),
                        message: format!("Module {module_id} not found"),
                        client_error: None,
                    });
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
                    return Err(BridgeRpcError {
                        code: "ModuleNotFound".into(),
                        message: format!("Module {module_id} not found"),
                        client_error: None,
                    });
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

            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnMultiModuleRepairStarted",
                "charid",
                vec![PyRep::List(vec![PyRep::Integer(module_id)])],
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
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnRepairDone",
                "charid",
                vec![PyRep::List(vec![PyRep::Integer(module_id)])],
            );
            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("stopped", PyRep::Bool(stopped)),
                ("remainingDamage", PyRep::Real(rem_dmg_out)),
                ("pasteRefunded", PyRep::Integer(paste_refunded as i64)),
            ])))
        }

        ("miningScanMgr", "perform_scan") => {
            if args.is_empty() && _kwargs.is_empty() {
                return None;
            }
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
            let items = ctx.db.get_items_for_character(char_id);
            let explicit_mod_id = args.first().and_then(|a| a.as_i64());
            let scanner_opt = items.iter().find(|i| {
                i.location_id == ship_id
                    && (i.flag as u16) >= 11
                    && (i.flag as u16) <= 34
                    && explicit_mod_id.map(|mid| i.item_id == mid).unwrap_or(true)
                    && ctx.sde.types.get(&i.type_id).map(|t| t.group_id == 49 || t.default_attributes.contains_key(&197)).unwrap_or(false)
            });
            let Some(scanner) = scanner_opt else {
                return Some(Err(BridgeRpcError {
                    code: "ModuleNotFound".into(),
                    message: "No survey scanner fitted to ship".into(),
                    client_error: None,
                }));
            };
            let scanner_type = ctx.sde.types.get(&scanner.type_id);
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

            let sys_id = session.solar_system_id.unwrap_or(0);
            let mut ship_pos = DVec3::ZERO;
            if sys_id > 0 {
                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                if let Ok(Some((_, ship_ball))) = sys_handle.get_ball_snapshot(ship_id).await {
                    ship_pos = ship_ball.position;
                }
            }

            let asteroids = ctx.db.get_asteroids_in_system(sys_id);
            let mut scanned_asteroids = Vec::new();
            for ast in asteroids {
                if ast.quantity <= 0 {
                    continue;
                }
                let ast_pos = DVec3::new(ast.position.0, ast.position.1, ast.position.2);
                let dist = if ship_pos != DVec3::ZERO {
                    (ship_pos - ast_pos).length()
                } else {
                    0.0
                };
                if dist <= scan_range || ship_pos == DVec3::ZERO {
                    let type_name = ctx.sde.types.get(&ast.type_id).map(|t| t.name.clone()).unwrap_or_else(|| format!("Asteroid {}", ast.type_id));
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

            Some(Ok(PyRep::keyval([
                ("scanRange", PyRep::Real(scan_range)),
                ("count", PyRep::Integer(scanned_asteroids.len() as i64)),
                ("asteroidCount", PyRep::Integer(scanned_asteroids.len() as i64)),
                ("asteroids", PyRep::List(scanned_asteroids)),
            ])))
        }


        _ => None,
    }
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

#[cfg(test)]
#[path = "fitting_mining_lasers_tests.rs"]
mod fitting_mining_lasers_tests;

