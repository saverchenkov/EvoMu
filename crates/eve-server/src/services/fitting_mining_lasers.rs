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
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
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
            let effect_name = args.get(1).and_then(|a| a.as_str()).unwrap_or("defaultEffect").to_string();
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            // Validate ship and fitted-module ownership BEFORE mutating any EWAR, remote-assist,
            // cycle, or module_active_states state so a target cannot deactivate an attacker's module.
            let mod_type_opt = match ctx.db.with_snapshot(|snap| {
                let Some(ship_item) = snap.items.get(&ship_id) else {
                    return Err(BridgeRpcError {
                        code: "ShipNotFound".into(),
                        message: format!("Active ship {ship_id} not found"),
                    });
                };
                if ship_item.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "ShipOwnershipViolation".into(),
                        message: format!("Ship {ship_id} is not owned by character {char_id}"),
                    });
                }
                let Some(mod_item) = snap.items.get(&module_id) else {
                    return Err(BridgeRpcError {
                        code: "ModuleNotFound".into(),
                        message: format!("Module {module_id} not found"),
                    });
                };
                let flag_u16 = mod_item.flag as u16;
                let is_fitted_slot = matches!(flag_u16, 11..=34 | 125..=129 | 158 | 159..=163);
                if mod_item.owner_id != char_id || mod_item.location_id != ship_id || !is_fitted_slot {
                    return Err(BridgeRpcError {
                        code: "ModuleNotOwnedByShip".into(),
                        message: format!("Module {module_id} is not fitted to active ship {ship_id}"),
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

            let mut removed_cycle_delay = None;
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                removed_cycle_delay = sys.remove_module_cycle(ship_id, module_id).await.ok().flatten();
            }
            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let deact_res = fit.deactivate_module(module_id, &ctx.sde);
            if let Err(e) = deact_res {
                if removed_cycle_delay.is_none() && !is_mining {
                    return Some(Err(BridgeRpcError { code: "DogmaDeactivateFailed".into(), message: e.to_string() }));
                }
            }
            let affected_ewar_ships = crate::services::notify_emitters_batch3_space::flush_module_profile_and_ewar_end(
                ctx,
                char_id,
                session.solar_system_id,
                ship_id,
                module_id,
            );
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                for tid in affected_ewar_ships {
                    if let Ok(Some(mut t_cs)) = sys.get_combat_state(tid).await {
                        let base_sig = match t_cs.base_signature_radius {
                            Some(b) => b,
                            None => sys.get_ball_snapshot(tid)
                                .await
                                .ok()
                                .flatten()
                                .and_then(|(_, b)| {
                                    ctx.sde
                                        .types
                                        .get(&b.type_id)
                                        .and_then(|t| t.default_attributes.get(&552).copied())
                                        .or(Some(b.radius.max(35.0)))
                                })
                                .unwrap_or(t_cs.signature_radius),
                        };
                        t_cs.signature_radius =
                            ctx.effective_target_signature_radius(tid, base_sig);
                        let _ = sys.set_combat_state(tid, t_cs).await;
                    }
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
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnGodmaShipEffect",
                "charid",
                vec![PyRep::Integer(module_id), PyRep::String(effect_name), PyRep::Bool(false)],
            );

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
            let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);
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
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys.update_module_overload(ship_id, module_id, false).await;
            }
            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            if let Some(fm) = fit.modules.get_mut(&module_id) {
                if fm.state == eve_dogma::ModuleState::Overloaded {
                    fm.state = eve_dogma::ModuleState::Active;
                }
            }
            fit.recalculate_stats(&ctx.sde);
            let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);
            if let Some(sys_id) = session.solar_system_id {
                crate::services::space_destiny::sync_ship_cloak_and_mjfg_from_fitting(ctx, char_id, sys_id, ship_id).await;
            }

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
                vec![PyRep::Integer(module_id)],
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
                }));
            };
            let arg_val = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
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
            let mut affected_modules = Vec::new();
            for (&mid, fm) in fit.modules.iter_mut() {
                let r_idx = match fm.flag_id {
                    27..=34 => 0,
                    19..=26 => 1,
                    11..=18 => 2,
                    _ => continue,
                };
                if r_idx == target_rack_idx && fm.state != eve_dogma::ModuleState::Offline {
                    affected_modules.push(mid);
                    if overloaded && fm.state == eve_dogma::ModuleState::Active {
                        fm.state = eve_dogma::ModuleState::Overloaded;
                    } else if !overloaded && fm.state == eve_dogma::ModuleState::Overloaded {
                        fm.state = eve_dogma::ModuleState::Active;
                    }
                }
            }
            fit.recalculate_stats(&ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);

            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                for &mid in &affected_modules {
                    let _ = sys.update_module_overload(ship_id, mid, overloaded).await;
                }
            }
            let notif_name = if overloaded { "OnHeatAdded" } else { "OnHeatRemoved" };
            for &mid in &affected_modules {
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
                    PyRep::List(affected_modules.into_iter().map(PyRep::Integer).collect()),
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
                }));
            };
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

            let mod_check = ctx.db.with_snapshot(|snap| {
                let Some(m_item) = snap.items.get(&module_id) else {
                    return Err(BridgeRpcError {
                        code: "ModuleNotFound".into(),
                        message: format!("Module {module_id} not found"),
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
                let base_cost = ctx
                    .sde
                    .types
                    .get(&m_item.type_id)
                    .and_then(|t| t.default_attributes.get(&1231).copied())
                    .unwrap_or(4.0)
                    .max(1.0);
                let sk = snap.skills.get(&char_id);
                let op_lvl = sk.and_then(|m| m.get(&28879)).map(|r| r.level).unwrap_or(0);
                let int_lvl = sk.and_then(|m| m.get(&28880)).map(|r| r.level).unwrap_or(0);
                Ok((active, dmg, hp, base_cost, op_lvl, int_lvl))
            });
            let (is_active, cur_dmg, max_hp, base_paste_cost, nanite_op_lvl, nanite_interfacing_lvl) =
                match mod_check {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };

            if is_active {
                return Some(Err(BridgeRpcError {
                    code: "CannotRepairActiveModule".into(),
                    message: "Module must be deactivated before initiating nanite repair".into(),
                }));
            }
            if cur_dmg >= max_hp - 1e-6 {
                return Some(Err(BridgeRpcError {
                    code: "ModuleBurnedOut".into(),
                    message: "Completely burned-out modules cannot be repaired with Nanite Repair Paste in space".into(),
                }));
            }
            if cur_dmg <= 1e-6 {
                return Some(Ok(PyRep::keyval([
                    ("moduleID", PyRep::Integer(module_id)),
                    ("pasteConsumed", PyRep::Integer(0)),
                    ("damageToRepair", PyRep::Real(0.0)),
                ])));
            }

            let effective_paste_per_hp =
                (base_paste_cost / max_hp.max(1.0)) * (1.0 - 0.05 * (nanite_op_lvl as f64));
            let paste_needed =
                eve_dogma::thermodynamics::nanite_paste_required(cur_dmg, effective_paste_per_hp)
                    .max(1) as i32;

            let deduct_res: Result<(), BridgeRpcError> = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(m_item) = snap.items.get(&module_id) else {
                    return Err(BridgeRpcError {
                        code: "ModuleNotFound".into(),
                        message: format!("Module {module_id} not found"),
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
                    });
                }
                let paste_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.location_id == ship_id
                            && i.flag == InventoryFlag::Cargo
                            && i.type_id == 28668
                            && i.quantity >= paste_needed
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                    })
                    .map(|i| i.item_id);
                let Some(pid) = paste_item_id else {
                    return Err(BridgeRpcError {
                        code: "NotEnoughNanitePaste".into(),
                        message: format!(
                            "Need {paste_needed} Nanite Repair Paste (type 28668) in ship cargo"
                        ),
                    });
                };
                let mut rem = 0;
                if let Some(p_item) = snap.items.get_mut(&pid) {
                    p_item.quantity -= paste_needed;
                    p_item.version += 1;
                    rem = p_item.quantity;
                }
                if rem == 0 {
                    snap.items.remove(&pid);
                }
                Ok(())
            });
            if let Err(e) = deduct_res {
                return Some(Err(e));
            }

            let repair_rate_hp_per_s = 0.67 * (1.0 + 0.20 * (nanite_interfacing_lvl as f64));
            let duration_s = (cur_dmg / repair_rate_hp_per_s).max(1.0);
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                let _ = sys
                    .start_nanite_repair(crate::system_actor::ActiveNaniteRepair {
                        ship_id,
                        module_id,
                        initial_damage_hp: cur_dmg,
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
                ("pasteConsumed", PyRep::Integer(paste_needed as i64)),
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
                }));
            };
            let module_id = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .unwrap_or(0);
            let mut stopped = false;
            if let Some(sys_id) = session.solar_system_id {
                let sys = ctx.systems.get_or_spawn(sys_id);
                stopped = sys
                    .stop_nanite_repair(ship_id, module_id)
                    .await
                    .ok()
                    .flatten()
                    .is_some();
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
            ])))
        }

        ("miningScanMgr", "perform_scan") => {
            if args.is_empty() && kwargs.is_empty() {
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
